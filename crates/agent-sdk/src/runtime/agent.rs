use super::compaction::{CutPlan, RETAIN_RATIO, plan_cut};
use super::error::AgentError;
use super::event::{AgentEvent, RunResult, StopReason};
use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::token;
use crate::tool;
use futures::StreamExt;
use std::pin::Pin;

pub struct Agent {
    model_config: adapter::ModelConfig,
    /// 系统提示词单独持有：压缩重建时不从旧消息里复制，而是每次重新生成一条置顶。
    system_prompt: String,
    messages: Vec<message::Message>,
    tools: tool::ToolManager,
    compression_instruction: Option<String>,
    compression_pending: bool,
    // L1 估算 + L2 校准状态（见 `token` 模块）
    token_counter: token::HeuristicCounter,
}

#[bon::bon]
impl Agent {
    #[builder]
    pub fn new(
        model_config: adapter::ModelConfig,
        #[builder(default, into)] system_prompt: String,
        #[builder(default)] mut messages: Vec<message::Message>,
        #[builder(default = tool::ToolManager::new())] tools: tool::ToolManager,
        #[builder(into)] compression_instruction: Option<String>,
    ) -> Self {
        if !system_prompt.trim().is_empty() {
            messages.insert(
                0,
                message::Message::System {
                    content: system_prompt.clone(),
                },
            );
        }

        Self {
            model_config,
            system_prompt,
            messages,
            tools,
            compression_instruction,
            compression_pending: false,
            token_counter: token::HeuristicCounter::new(),
        }
    }

    /// 重新生成系统提示词消息。压缩重建后由它把 system 置顶。
    ///
    /// 系统提示词不随对话变化，也不参与压缩——每次都从 `self.system_prompt` 现取，
    /// 因此 [`CompactParts::rebuild`] 无需（也不该）复制原消息里的 `System`。
    fn system_message(&self) -> Option<message::Message> {
        (!self.system_prompt.trim().is_empty()).then(|| message::Message::System {
            content: self.system_prompt.clone(),
        })
    }
    pub async fn run(&mut self, task: &str) -> Result<RunResult, AgentError> {
        let mut events = self.run_stream(task);
        while let Some(event) = events.next().await {
            if let AgentEvent::Finished(result) = event? {
                return Ok(result);
            }
        }
        Err(AgentError::Other(
            "agent stream ended without a final result".into(),
        ))
    }

    pub fn run_stream<'a>(
        &'a mut self,
        task: &'a str,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, AgentError>> + Send + 'a>> {
        Box::pin(async_stream::try_stream! {
            let client = reqwest::Client::new();
            let mut total_usage = message::Usage::default();
            if self.compression_pending {
                yield AgentEvent::CompressionStarted;
                let (usage, summary) = self.compress_context(&client).await?;
                if let Some(summary) = summary {
                    yield AgentEvent::MessageAdded(summary);
                }
                yield AgentEvent::CompressionFinished;
                total_usage = total_usage + usage;
                yield AgentEvent::Usage(usage);
            }
            let start_index = self.messages.len();

            let user_message = message::Message::User {
                content: task.into(),
            };
            self.messages.push(user_message.clone());
            yield AgentEvent::MessageAdded(user_message);

            loop {
                if self.compression_pending {
                    yield AgentEvent::CompressionStarted;
                    let (usage, summary) = self.compress_context(&client).await?;
                    if let Some(summary) = summary {
                        yield AgentEvent::MessageAdded(summary);
                    }
                    yield AgentEvent::CompressionFinished;
                    total_usage = total_usage + usage;
                    yield AgentEvent::Usage(usage);
                }
                let active_messages = self.active_messages();
                let tools = self.tools.definitions();
                let model_request = ModelRequest {
                    messages: &active_messages,
                    tools: &tools,
                };
                let mut is_finished = false;
                let mut stream = adapter::invoke(&client, &self.model_config, model_request).await;
                while let Some(adapter_event) = stream.next().await {
                    let adapter_event = adapter_event.map_err(AgentError::Adapter)?;

                    match adapter_event {
                        adapter::AdapterEvent::Finished(response) => {
                            total_usage = total_usage + response.usage;
                            self.compression_pending = self.should_schedule_compression(&response.usage);
                            if let Some(limit_tokens) = self.model_config.context_window_tokens.filter(|&limit| limit > 0) {
                                yield AgentEvent::ContextUsage {
                                    used_tokens: response.usage.input_tokens.saturating_add(response.usage.output_tokens),
                                    limit_tokens,
                                };
                            }

                            yield AgentEvent::Usage(response.usage);

                            let response_message = response.message;
                            self.messages.push(response_message.clone());
                            yield AgentEvent::MessageAdded(response_message.clone());
                            let tool_messages = match &response_message {
                                message::Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                                    let mut tasks = futures::stream::FuturesUnordered::new();

                                    for call in tool_calls {
                                        yield AgentEvent::ToolStarted {
                                            call_id: call.id.clone(),
                                            name: call.name.clone(),
                                        };

                                        let tools = &self.tools;

                                        tasks.push(async move {
                                            let content = match tools.invoke(call).await {
                                                Ok(output) => output.to_string(),
                                                // TODO: 感觉这里不太合理，不过如果消费者是AI合理，外部消费者应该通过 AgentEvent 把错误信息传递出去
                                                Err(error) => error.to_string(),
                                            };
                                            (call, content)
                                        });
                                    }

                                    let mut tool_messages = Vec::with_capacity(tool_calls.len());
                                    while let Some((call, content)) = tasks.next().await {
                                        yield AgentEvent::ToolFinished {
                                            call_id: call.id.clone(),
                                            name: call.name.clone(),
                                        };

                                        let tool_message = message::Message::Tool {
                                            tool_call_id: call.id.clone(),
                                            content: Some(content),
                                        };
                                        self.messages.push(tool_message.clone());
                                        yield AgentEvent::MessageAdded(tool_message.clone());
                                        tool_messages.push(tool_message);
                                    }

                                    tool_messages
                                }
                                _ => vec![],
                            };
                            is_finished = tool_messages.is_empty();

                            if is_finished {
                                yield AgentEvent::Finished(RunResult {
                                    messages: self.messages[start_index..].to_vec(),
                                    stop_reason: StopReason::Completed,
                                    usage: total_usage,
                                });
                                break;
                            }
                        }
                        adapter::AdapterEvent::ReasoningDelta(delta) => {
                            yield AgentEvent::ReasoningDelta(delta);
                        }
                        adapter::AdapterEvent::ContentDelta(delta) => {
                            yield AgentEvent::ContentDelta(delta);
                        }
                    }
                }
                if is_finished {
                    break;
                }
            }
        })
    }

    /// 当前保留尾部的 token 预算：`context_window_tokens * 20%`。
    ///
    /// 未配置窗口（或配成 0）时返回 `None`，表示**不做压缩**。
    fn retain_budget(&self) -> Option<u64> {
        let window = self.model_config.context_window_tokens.filter(|&w| w > 0)?;
        Some(((window as f64) * RETAIN_RATIO).round() as u64)
    }

    /// 计算压缩切点，并提取用户最新提出的问题。
    ///
    /// 返回 `None` 的三种情况，语义都是"本轮不压缩"：
    /// 未配置上下文窗口、没有可压内容、或全部消息本来就能装进预算。
    fn compute_cut(&self) -> Option<CutPlan> {
        let budget = self.retain_budget()?;
        plan_cut(&self.messages, budget, &self.token_counter)
    }

    fn should_schedule_compression(&self, usage: &message::Usage) -> bool {
        let Some(limit) = self
            .model_config
            .context_window_tokens
            .filter(|&limit| limit > 0)
        else {
            return false;
        };
        if self
            .compression_instruction
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        {
            return false;
        }
        // 下一次请求会携带本轮输出；用整数比较避免浮点精度和溢出。
        let used = usage.input_tokens.saturating_add(usage.output_tokens);
        if (used as u128) * 100 >= (limit as u128) * 80 {
            return true;
        }
        false
    }

    fn active_messages(&self) -> Vec<message::Message> {
        let last_summary = self
            .messages
            .iter()
            .rposition(|msg| matches!(msg, message::Message::ContextSummary { .. }));
        match last_summary {
            None => self.messages.clone(),
            Some(index) => {
                let mut active: Vec<_> = self
                    .messages
                    .iter()
                    .take_while(|msg| matches!(msg, message::Message::System { .. }))
                    .cloned()
                    .collect();
                active.extend_from_slice(&self.messages[index..]);
                active
            }
        }
    }

    /// 压缩上下文：算切点 → 切三段 → 只对 `to_compress` 求摘要 → 重建消息。
    ///
    /// 对应 `docs/compaction.md` 5.1 的结构与 5.2 的"禁止摘要叠摘要"：
    ///
    /// ```text
    /// self.messages = [system（重新生成）] + [新 ContextSummary] + current_task + remain
    /// ```
    ///
    /// 与旧实现的区别：旧实现把摘要**追加在末尾**（等于把尾巴也吃掉），
    /// 且摘要器看到的是整段对话、拿不到"本轮任务是什么"，所以会忘任务、会编下一步。
    async fn compress_context(
        &mut self,
        client: &reqwest::Client,
    ) -> Result<(message::Usage, Option<message::Message>), AgentError> {
        // 1. 算切点并切三段。
        let Some(plan) = self.compute_cut() else {
            // 没有可压内容（窗口未配置 / 只剩背景 / 本来装得下）：不压缩，直接放行。
            self.compression_pending = false;
            return Ok((message::Usage::default(), None));
        };
        let parts = plan.split(&self.messages);

        // 2. 构造压缩请求：待压缩段 + 本轮任务 + 压缩指令。
        let mut messages = parts.to_compress.clone();
        // 显式告诉摘要器"本轮任务是什么"，避免它从历史里猜（猜就会编）。
        if let Some(task) = &parts.current_task {
            messages.push(task.clone());
        }
        messages.push(message::Message::System {
            content: self
                .compression_instruction
                .clone()
                .ok_or_else(|| AgentError::Compression("未配置压缩指令".into()))?,
        });

        let model_request = ModelRequest {
            messages: &messages,
            tools: &[],
        };

        let mut stream = adapter::invoke(&client, &self.model_config, model_request).await;
        while let Some(adapter_event) = stream.next().await {
            let adapter_event = adapter_event.map_err(AgentError::Adapter)?;
            match adapter_event {
                adapter::AdapterEvent::Finished(response) => {
                    let usage = response.usage;
                    let summary = match response.message {
                        message::Message::Assistant {
                            content: Some(content),
                            tool_calls,
                            ..
                        } if !content.trim().is_empty() && tool_calls.is_empty() => content,
                        _ => {
                            return Err(AgentError::Compression("压缩响应没有有效摘要".into()));
                        }
                    };

                    // 3. 重建：[新摘要] + current_task + remain，再把系统提示词重新生成置顶。
                    //    system 不参与压缩、也不从旧消息复制，每次重建现取一条。
                    let rebuilt = parts.rebuild(summary);
                    let summary_message = rebuilt
                        .first()
                        .expect("rebuild 至少产出摘要")
                        .clone();
                    let mut next = rebuilt;
                    if let Some(system) = self.system_message() {
                        next.insert(0, system);
                    }
                    self.messages = next;
                    self.compression_pending = false;

                    // 4. 用真实 input_tokens 校准估算系数（L2）。
                    //    估算口径必须与请求一致：待压缩段 + 任务 + 指令。
                    //    不含 system / tool schema —— 本轮压缩请求也没带它们。
                    self.token_counter
                        .calibrate(usage.input_tokens, token::count_messages(&messages));

                    return Ok((usage, Some(summary_message)));
                }
                adapter::AdapterEvent::ReasoningDelta(_)
                | adapter::AdapterEvent::ContentDelta(_) => {}
            }
        }

        Err(AgentError::Compression("无法获取压缩后的Usage".into()))
    }
}
