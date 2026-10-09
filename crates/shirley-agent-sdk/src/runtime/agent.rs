use super::compaction::{CompressionConfig, CutPlan, plan_cut};
use super::context::ContextProvider;
use super::error::AgentError;
use super::event::{AgentEvent, RunResult, StopReason};
use super::prompt::{SystemPrompt, SystemPromptContext};
use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::token;
use crate::tool;
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::pin::Pin;

pub struct Agent {
    model_config: adapter::ModelConfig,
    /// 系统提示词单独持有：压缩重建时不从旧消息里复制，而是每次重新生成一条置顶。
    ///
    /// 它可以是一个固定字符串，也可以是一个函数——按 [`SystemPromptContext`]
    /// 动态生成（例如把当前工作目录、项目指南拼进去）。见 [`super::prompt`]。
    system_prompt: SystemPrompt,
    /// 构建系统提示词时的运行时上下文（当前工作目录）。
    ///
    /// coding agent 靠它把"我的工作范围在哪"明确告诉模型，避免模型从文件系统
    /// 根目录开始盲目探索（`plan.md`）。`None` 表示调用方没有指定。
    working_dir: Option<PathBuf>,
    messages: Vec<message::Message>,
    tools: tool::ToolManager,
    compression_instruction: Option<String>,
    compression_pending: bool,
    /// 压缩策略（触发阈值 / 保留比例 / 摘要模板）。默认即 SDK 内置策略。
    compression_config: CompressionConfig,
    /// 每轮请求末尾的附加上下文提供者（应用层可选注入，例如任务账本）。
    /// 返回的文本作为一条 system 消息追加在本轮请求末尾——不进 `self.messages`、
    /// 跨压缩存活、且不动前缀（见 [`super::context`]）。
    context_provider: Option<Arc<dyn ContextProvider>>,
    // L1 估算 + L2 校准状态（见 `token` 模块）
    token_counter: token::HeuristicCounter,
}

#[bon::bon]
impl Agent {
    #[builder]
    pub fn new(
        model_config: adapter::ModelConfig,
        #[builder(default, into)] system_prompt: SystemPrompt,
        #[builder(into)] working_dir: Option<PathBuf>,
        #[builder(default)] mut messages: Vec<message::Message>,
        #[builder(default = tool::ToolManager::new())] tools: tool::ToolManager,
        #[builder(into)] compression_instruction: Option<String>,
        #[builder(default)] compression_config: CompressionConfig,
        context_provider: Option<Arc<dyn ContextProvider>>,
    ) -> Result<Self, AgentError> {
        // 构造时就按工作目录解析一次，把 System 消息置顶（空提示词则不置顶）。
        // 恢复出的历史里不含 system，这里现生成——与压缩重建同一套规则。
        let resolved = system_prompt.resolve(&SystemPromptContext {
            working_dir: working_dir.clone(),
        });
        if !resolved.trim().is_empty() {
            messages.insert(0, message::Message::System { content: resolved });
        }

        Ok(Self {
            model_config,
            system_prompt,
            working_dir,
            messages,
            tools,
            compression_instruction,
            compression_pending: false,
            compression_config,
            context_provider,
            token_counter: token::HeuristicCounter::new(),
        })
    }

    /// 重新生成系统提示词消息。压缩重建后由它把 system 置顶。
    ///
    /// 系统提示词不随对话变化，也不参与压缩——每次都从 `self.system_prompt` 现取，
    /// 因此 [`CompactParts::rebuild`] 无需（也不该）复制原消息里的 `System`。
    fn system_message(&self) -> Option<message::Message> {
        let content = self.system_prompt.resolve(&self.prompt_context());
        (!content.trim().is_empty()).then_some(message::Message::System { content })
    }

    /// 当前用于解析系统提示词的运行时上下文。
    fn prompt_context(&self) -> SystemPromptContext {
        SystemPromptContext {
            working_dir: self.working_dir.clone(),
        }
    }

    /// 当前模型配置（只读）。
    ///
    /// 应用层用它展示"现在用的是哪个模型"，以及切换时保留其余配置。
    pub fn model_config(&self) -> &adapter::ModelConfig {
        &self.model_config
    }

    /// 切换模型：只替换 `model` 字段，其余配置（协议 / base_url / 超时 / 窗口等）原样保留。
    ///
    /// 这是应用层 `/model` 指令落地的唯一 SDK 接缝——模型是可热切换的运行参数，
    /// 不重建 `Agent` 也能生效（下一次请求即用新模型）。
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model_config.model = model.into();
    }

    /// 切换模型服务连接（`base_url` + `api_key`）：其余配置原样保留。
    ///
    /// 这是应用层 `/login` 指令落地的 SDK 接缝（与 [`Agent::set_model`] 对称）：
    /// 换一个供应商 / 端点不必重建 `Agent`，下一次请求即生效。`api_key` 传
    /// `None` 表示"无鉴权"（本地服务常见），会覆盖掉旧 key——与"未改动"不同，
    /// 调用方需自行决定是"保留旧值"还是"确实清空"。
    pub fn set_provider(&mut self, base_url: impl Into<String>, api_key: Option<String>) {
        self.model_config.base_url = base_url.into();
        self.model_config.api_key = api_key;
    }

    /// 移除一个已注册的工具（与 [`ToolManager::register`] 对称的运行时接缝）。
    ///
    /// 触发工具的 `on_unregister`（= destroy），用于运行期动态启停工具。
    /// 名字未注册时返回 `Err`。与 `set_model` / `set_provider` 同风格：
    /// 不重建 `Agent`。
    pub fn unregister_tool(&mut self, name: &str) -> Result<(), AgentError> {
        self.tools.unregister(name).map_err(Into::into)
    }

    /// 对话历史的只读视图。
    ///
    /// 供上层展示"当前 Agent 实际记得什么"，以及"回溯"时定位目标消息——
    /// 被压缩掉的历史不在这里，因此天然不可回溯（压缩边界之后才谈得上回溯）。
    pub fn messages(&self) -> &[message::Message] {
        &self.messages
    }

    /// 把对话历史尾截断到前 `len` 条，返回截断后的长度。
    ///
    /// 这是回溯 / 打断的**原子能力**：SDK 只负责"把工作集变短"并维护内部不变量
    /// （不越过置顶 system、越界自动钳制），**"截到哪"是业务判定**——例如"回退最后
    /// 一轮用户消息"由上层按 [`Agent::messages`] 自行定位后调用（`docs/session.md`
    /// 一.决策 4：只截尾，不产生中间空洞）。持久化由应用层据截断后的长度自行截尾
    /// （SDK 不持有会话日志）。
    pub fn truncate_messages(&mut self, len: usize) -> usize {
        // 置顶 system 是构造 / 压缩重建维护的不变量，不允许被截掉：以开头连续
        // 的 system 条数为下界。`clamp` 同时兜住 `len` 越界（超长则原样）。
        let floor = self
            .messages
            .iter()
            .take_while(|m| matches!(m, message::Message::System { .. }))
            .count();
        let len = len.clamp(floor, self.messages.len());
        self.messages.truncate(len);
        self.messages.len()
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

    /// 一次性补全：对单条 prompt 跑**一次无工具、非流式**的模型请求，返回助手正文。
    ///
    /// 这是给应用层「用一次模型调用做点小事」准备的通用原语（例如为新建会话自动
    /// 生成标题）。它**不碰** `self.messages` / 会话日志 / 召回库 / 任务账本——纯
    /// 只读地复用当前 `model_config`，因此可以在持有 `&self` 时调用，不会污染会话。
    ///
    /// 与 [`Agent::run`] 的区别：不驱动 ReAct 循环、不发工具、不落库；`stream` /
    /// `thinking` 被就地关掉（一次性补全要的是短文本，流式与推理无意义）。
    /// 取 `Assistant` 的正文（`content`）；若模型只回了推理内容（`reasoning_content`）
    /// 则回退取它；两者皆空返回空串。
    pub async fn complete(&self, prompt: &str) -> Result<String, AgentError> {
        let client = reqwest::Client::new();
        // 克隆一份配置并关掉流式 / 推理：只借模型名与端点，不改动 `self` 的���置。
        let mut config = self.model_config.clone();
        config.stream = false;
        config.thinking = false;

        let messages = vec![message::Message::User {
            content: prompt.into(),
        }];
        let tools: Vec<&tool::ToolDefinition> = Vec::new();
        let request = ModelRequest {
            messages: &messages,
            tools: &tools,
        };

        let mut stream = adapter::invoke(&client, &config, request).await;
        while let Some(event) = stream.next().await {
            if let adapter::AdapterEvent::Finished(response) = event.map_err(AgentError::Adapter)? {
                return Ok(match response.message {
                    message::Message::Assistant {
                        content,
                        reasoning_content,
                        ..
                    } => content
                        .filter(|text| !text.trim().is_empty())
                        .or(reasoning_content)
                        .unwrap_or_default(),
                    _ => String::new(),
                });
            }
        }
        Err(AgentError::Other(
            "completion stream ended without a result".into(),
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
            let mut start_index = self.messages.len();

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

                    // 压缩把 self.messages 整体重建（远短于原列表），
                    // start_index 这个绝对下标随之失效——不钳制会在
                    // Finished 处切片越界 panic（"range start index N out
                    // of range"）。钳到当前长度后，切出的 [summary, task,
                    // remain..] 仍是"本轮可见的新增内容"：summary 是本轮
                    // 压缩产物，remain 是本轮的对话与 tool 链。
                    start_index = start_index.min(self.messages.len());
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
                                            arguments: call.arguments.clone(),
                                        };

                                        let tools = &self.tools;
                                        // ToolContext 内部是 Arc，clone 只加引用计数；
                                        // 在循环里 clone 而不是把 self 捕获进 async 块。
                                        // 上下文由 ToolManager 持有（工具在 on_register
                                        // 里写入），这里只取一份 clone 分发下去。
                                        let ctx = self.tools.context().clone();

                                        tasks.push(async move {
                                            let started = std::time::Instant::now();
                                            let (ok, content) = match tools.invoke(call, ctx).await {
                                                Ok(output) => (true, output.to_string()),
                                                // TODO: 感觉这里不太合理，不过如果消费者是AI合理，外部消费者应该通过 AgentEvent 把错误信息传递出去
                                                Err(error) => (false, error.to_string()),
                                            };
                                            (call, ok, content, started.elapsed().as_millis() as u64)
                                        });
                                    }

                                    let mut tool_messages = Vec::with_capacity(tool_calls.len());
                                    while let Some((call, ok, content, elapsed_ms)) = tasks.next().await {
                                        yield AgentEvent::ToolFinished {
                                            call_id: call.id.clone(),
                                            name: call.name.clone(),
                                            ok,
                                            output: content.clone(),
                                            elapsed_ms,
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
        Some(((window as f64) * self.compression_config.retain_ratio).round() as u64)
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
        // 下一次请求会携带本轮输出。用整数比较避免浮点精度和溢出：
        // 阈值 = limit * trigger_ratio，转成 (used / limit >= ratio) 的整数判定。
        let used = usage.input_tokens.saturating_add(usage.output_tokens);
        let ratio = self.compression_config.trigger_ratio;
        if !(ratio > 0.0 && ratio <= 1.0) {
            return false;
        }
        // 放大 10000 倍做定点比较：0.80 → 8000，避免 f64 精度问题。
        let scaled = (ratio * 10_000.0).round() as u128;
        (used as u128) * 10_000 >= (limit as u128) * scaled
    }

    fn active_messages(&self) -> Vec<message::Message> {
        let last_summary = self
            .messages
            .iter()
            .rposition(|msg| matches!(msg, message::Message::ContextSummary { .. }));
        let mut active = match last_summary {
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
        };
        // 附加上下文（应用层注入，例如任务账本）作为一条 system 消息**追加在末尾**。
        //
        // 位置选末尾的理由：
        //   - 它不进 `self.messages`，压缩碰不到它，跨压缩存活；
        //   - 追加在尾部不动前面的前缀，内容稳定时前缀缓存照常命中
        //     （Responses 适配器把 system 原位保留，正是为了这一点）；
        //   - 内容只在提供者决定变化时才变，变化点被压到最小。
        if let Some(provider) = &self.context_provider
            && let Some(text) = provider.context()
            && !text.trim().is_empty()
        {
            active.push(message::Message::System { content: text });
        }
        active
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
        let mut parts = plan.split(&self.messages);

        // 1.5 工具输出清空：工具输出统一替换为占位标记——AI 走"重建"路径
        //     （重新执行获取当前状态）。这是 v0 有意的技术债：不可重建的调用
        //     （一次性快照）重跑拿不到当时结果。
        strip_tool_outputs(&mut parts.to_compress);

        // 2. 构造压缩请求：待压缩段 + 本轮任务 + 压缩指令。
        let mut messages = parts.to_compress.clone();
        // 显式告诉摘要器"本轮任务是什么"，避免它从历史里猜（猜就会编）。
        if let Some(task) = &parts.current_task {
            messages.push(task.clone());
        }
        let instruction = self
            .compression_instruction
            .clone()
            .ok_or_else(|| AgentError::Compression("compression instruction is not configured".into()))?;
        // 调用方给领域相关的取舍，SDK 追加结构模板（`docs/compaction.md` 5.3）：
        // 摘要始终是 XML 块，且禁止推演"下一步"。模板可由 `compression_config` 覆盖。
        let content = format!("{instruction}\n\n{}", self.compression_config.template);
        messages.push(message::Message::System { content });

        let model_request = ModelRequest {
            messages: &messages,
            tools: &[],
        };

        let mut stream = adapter::invoke(client, &self.model_config, model_request).await;
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
                            return Err(AgentError::Compression("compression response contained no usable summary".into()));
                        }
                    };

                    // 3. 重建：[新摘要] + current_task + remain，再把系统提示词重新生成置顶。
                    //    system 不参与压缩、也不从旧消息复制，每次重建现取一条。
                    let rebuilt = parts.rebuild(summary);
                    let summary_message = rebuilt.first().expect("rebuild always yields a summary").clone();
                    let mut next = rebuilt;
                    if let Some(system) = self.system_message() {
                        next.insert(0, system);
                    }
                    // 压缩把 self.messages 整体重建为 [system?, 新摘要, task, remain]。
                    // 应用层据 `MessageAdded(summary)` 事件自行落库（SDK 不持有会话日志）。
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

        Err(AgentError::Compression("compression produced no usage report".into()))
    }
}

/// 工具结果统一清空为占位标记（压缩时节省上下文）。
///
/// **不能留空字符串**——模型会把空 content 误读为"执行成功但无输出"，
/// 占位标记才是"结果被省略、可重新执行获取"的显式信号。
/// 占位文案进 SDK 契约，措辞要稳定。
fn strip_tool_outputs(messages: &mut [message::Message]) {
    for msg in messages.iter_mut() {
        if let message::Message::Tool { content, .. } = msg
            && let Some(text) = content
            && !text.trim().is_empty()
        {
            *text = "[tool result omitted to save context; re-run the call to get the current state if still needed]".to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用提供者：返回固定文本。
    struct FixedProvider(&'static str);
    impl ContextProvider for FixedProvider {
        fn context(&self) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    /// 测试用提供者：始终不注入。
    struct EmptyProvider;
    impl ContextProvider for EmptyProvider {
        fn context(&self) -> Option<String> {
            None
        }
    }

    fn agent_with(provider: Option<Arc<dyn ContextProvider>>) -> Agent {
        let config = adapter::ModelConfig::builder()
            .protocol(adapter::ModelProtocol::ChatCompletions)
            .base_url("http://localhost")
            .model("test")
            .build();
        Agent::builder()
            .model_config(config)
            .system_prompt("你是 Shirley")
            .maybe_context_provider(provider)
            .build()
            .unwrap()
    }

    /// 没有提供者时不注入任何东西（避免每轮多出一条无意义的 system）。
    #[test]
    fn no_provider_injects_nothing() {
        let agent = agent_with(None);
        let active = agent.active_messages();
        assert_eq!(active.len(), 1, "只有置顶 system");
        assert!(matches!(active[0], message::Message::System { .. }));
    }

    /// 提供者返回 `None` 时也不注入。
    #[test]
    fn provider_returning_none_injects_nothing() {
        let agent = agent_with(Some(Arc::new(EmptyProvider)));
        let active = agent.active_messages();
        assert_eq!(active.len(), 1, "只有置顶 system");
    }

    /// 附加上下文作为**最后一条** system 注入，且不进入 `self.messages`
    /// （因此压缩重建 `self.messages` 时碰不到它）。
    #[test]
    fn context_is_appended_and_not_persisted() {
        let agent = agent_with(Some(Arc::new(FixedProvider("注入的任务账本"))));

        // 附加上下文不在工作集里——压缩 / rewind 都动不到它。
        assert!(
            !agent
                .messages
                .iter()
                .any(|m| matches!(m, message::Message::System { content } if content.contains("任务账本"))),
            "附加上下文不应写入 self.messages"
        );

        let active = agent.active_messages();
        let last = active.last().expect("至少有一条");
        let message::Message::System { content } = last else {
            panic!("末条应是注入的 system");
        };
        assert_eq!(content, "注入的任务账本");
        // 置顶的原始 system 仍在最前。
        assert!(matches!(&active[0], message::Message::System { content } if content == "你是 Shirley"));
    }
}
