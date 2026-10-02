use super::compaction::{COMPACTION_TEMPLATE, CutPlan, RETAIN_RATIO, plan_cut};
use super::error::AgentError;
use super::event::{AgentEvent, RunResult, StopReason};
use super::prompt::{SystemPrompt, SystemPromptContext};
use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::recall::{self, RecallStore};
use crate::session::SessionStore;
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
    // L1 估算 + L2 校准状态（见 `token` 模块）
    token_counter: token::HeuristicCounter,
    /// 召回存储（`docs/recall.md`）：被压缩掉对话段的内存存档 + BM25 检索。
    /// SDK 内部能力，应用层无感；recall 工具持有同一 `Arc` 的另一份引用。
    recall: Arc<RecallStore>,
    /// 会话日志（`docs/session.md`）：`None` 表示不落盘（行为与现状一致）。
    ///
    /// 持久化的是**原始 Message 全量日志**；召回库由它派生，不单独落盘。
    /// system 提示词不入日志——恢复时由 [`Agent::system_message`] 现生成。
    session: Option<Arc<dyn SessionStore>>,
}

#[bon::bon]
impl Agent {
    #[builder]
    pub fn new(
        model_config: adapter::ModelConfig,
        #[builder(default, into)] system_prompt: SystemPrompt,
        #[builder(into)] working_dir: Option<PathBuf>,
        #[builder(default)] mut messages: Vec<message::Message>,
        #[builder(default = tool::ToolManager::new())] mut tools: tool::ToolManager,
        #[builder(into)] compression_instruction: Option<String>,
        #[builder(into)] session: Option<Arc<dyn SessionStore>>,
    ) -> Result<Self, AgentError> {
        // 召回是 compaction 的自然配套（`docs/recall.md` 决策 2）：存储与工具共享 Arc，
        // recall 工具在此自动注册进 ToolManager，应用层完全无感。
        let recall = Arc::new(RecallStore::new());
        let _ = tools.register(recall::RecallTool::new(recall.clone()));

        // 恢复语义（`docs/session.md` 三.2）：`messages` 与 `session` 二者只有一个真相源。
        //   - `messages` 非空 → 以它为准（真相在调用方传入的消息里）；
        //   - `messages` 为空且有 `session` → 从日志恢复工作集与召回语料。
        if messages.is_empty()
            && let Some(store) = &session
        {
            Self::restore_from_session(&mut messages, &recall, store)?;
        }

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
            token_counter: token::HeuristicCounter::new(),
            recall,
            session,
        })
    }

    /// 从会话日志恢复工作集与召回语料（`docs/session.md` 三.2）。
    ///
    /// `Agent::new`（messages 为空时）与 [`Agent::switch_session`]（切换会话时）
    /// **共用这一套规则**——恢复与切换必须是同一套语义，否则切换出来的工作集
    /// 会与冷启动恢复不一致。
    ///
    /// 不置顶 system：调用方负责用 [`Agent::system_message`] 现生成。
    fn restore_from_session(
        messages: &mut Vec<message::Message>,
        recall: &RecallStore,
        store: &Arc<dyn SessionStore>,
    ) -> Result<(), AgentError> {
        let log = store.load()?;
        // 防御：日志不含 system，若混入则剔除（system 现生成）。
        let log: Vec<_> = log
            .into_iter()
            .filter(|m| !matches!(m, message::Message::System { .. }))
            .collect();
        // 召回语料 = 最后一条 summary 之前的原始消息（更早的 summary 不入库，
        // 由 `chunk_messages` 自动跳过）——`docs/session.md` 三.2。
        if let Some(last_summary) = log
            .iter()
            .rposition(|m| matches!(m, message::Message::ContextSummary { .. }))
        {
            recall.index(recall::chunk_messages(&log[..last_summary]));
        }
        *messages = log;
        Ok(())
    }

    /// 切换到另一份会话日志：换掉日志源，并按恢复规则重建工作集与召回语料。
    ///
    /// 这是应用层 `/session` 指令落地的唯一 SDK 接缝（与 `/model` 的
    /// [`Agent::set_model`] 对称）：模型配置、系统提示词、工作目录、工具、
    /// 压缩指令都**原样保留**，只有「当前会话」这一件事被替换。
    ///
    /// 语义与 `Agent::new` 的恢复路径完全一致：清空内存与召回库，从新日志
    /// 重建工作集（日志不含 system，这里现生成一条置顶）。切换后后续轮次的
    /// `append` 都落到新日志上。
    pub fn switch_session(&mut self, session: Arc<dyn SessionStore>) -> Result<(), AgentError> {
        // 先清干净：旧会话的 messages 与召回语料绝不能残留到新会话里。
        self.messages.clear();
        self.recall.clear();
        self.compression_pending = false;
        Self::restore_from_session(&mut self.messages, &self.recall, &session)?;
        self.session = Some(session);
        // 日志不含 system——按当前工作目录 / `Agent.md` 现生成一条置顶，
        // 与压缩重建、冷启动恢复同一套规则（`docs/session.md` 一.决策 3）。
        if let Some(system) = self.system_message() {
            self.messages.insert(0, system);
        }
        Ok(())
    }

    /// 重新生成系统提示词消息。压缩重建后由它把 system 置顶。
    ///
    /// 系统提示词不随对话变化，也不参与压缩——每次都从 `self.system_prompt` 现取，
    /// 因此 [`CompactParts::rebuild`] 无需（也不该）复制原消息里的 `System`。
    fn system_message(&self) -> Option<message::Message> {
        let content = self.system_prompt.resolve(&self.prompt_context());
        (!content.trim().is_empty()).then(|| message::Message::System { content })
    }

    /// 当前用于解析系统提示词的运行时上下文。
    fn prompt_context(&self) -> SystemPromptContext {
        SystemPromptContext {
            working_dir: self.working_dir.clone(),
        }
    }

    /// 记录一条消息：同时写入内存工作集与（若挂载了）会话日志。
    ///
    /// 所有对消息表的追加都走这里，保证日志与内存不分叉（`docs/session.md` 五.7）。
    /// system 不入日志——它由 [`Agent::system_message`] 现生成，恢复时重放，不持久化。
    ///
    /// 刻意做成"只借字段"的关联函数而非 `&mut self` 方法：`run_stream` 里
    /// `tools` 持有 `&self.tools` 的借用跨越请求，`&mut self` 会与之冲突。
    fn record(
        messages: &mut Vec<message::Message>,
        session: &Option<Arc<dyn SessionStore>>,
        message: message::Message,
    ) -> Result<(), AgentError> {
        if !matches!(message, message::Message::System { .. }) {
            if let Some(store) = session {
                store.append(&message)?;
            }
        }
        messages.push(message);
        Ok(())
    }

    /// 会话日志当前长度（不含 system 的话需减去置顶的那条）。
    ///
    /// rewind 时用它把日志截到与内存一致的长度：日志里不含 system，
    /// 而内存首条是 system，所以日志长度 = 内存长度 - (是否置顶 system)。
    fn session_len_for(&self, messages_len: usize) -> usize {
        let has_system = matches!(self.messages.first(), Some(message::Message::System { .. }));
        messages_len.saturating_sub(usize::from(has_system))
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

    /// 对话历史的只读视图。
    ///
    /// 供上层展示"当前 Agent 实际记得什么"，以及"回溯"时定位目标消息——
    /// 被压缩掉的历史不在这里，因此天然不可回溯（压缩边界之后才谈得上回溯）。
    pub fn messages(&self) -> &[message::Message] {
        &self.messages
    }

    /// 回溯到最后一轮用户消息之前，返回被丢弃的用户输入（供上层编辑 / 退回输入框）。
    ///
    /// 用于"打断"与"编辑重发"：回退掉这一轮的用户消息及其后可能已完成的
    /// assistant / tool 链，让会话回到该轮开始前的状态。没有用户消息时返回 `None`。
    ///
    /// **只作用于最后一条用户消息**（`docs/session.md` 一.决策 4）：这样回溯
    /// 永远只是 tail truncation，日志同步截尾即可，不会产生中间空洞。
    /// 若挂载了会话日志，同步截断到相同长度（日志不含 system，需换算）。
    pub fn rewind_last_user_turn(&mut self) -> Result<Option<String>, AgentError> {
        let Some(index) = self
            .messages
            .iter()
            .rposition(|m| matches!(m, message::Message::User { .. }))
        else {
            return Ok(None);
        };
        let content = match &self.messages[index] {
            message::Message::User { content } => content.clone(),
            _ => unreachable!("rposition 已保证是 User"),
        };
        if let Some(store) = &self.session {
            store.truncate(self.session_len_for(index))?;
        }
        self.messages.truncate(index);
        Ok(Some(content))
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
            let mut start_index = self.messages.len();

            let user_message = message::Message::User {
                content: task.into(),
            };
            Self::record(&mut self.messages, &self.session, user_message.clone())?;
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
                            Self::record(&mut self.messages, &self.session, response_message.clone())?;
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
                                        Self::record(&mut self.messages, &self.session, tool_message.clone())?;
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
        let mut parts = plan.split(&self.messages);

        // 1.5 召回入库 + 工具输出清空（`docs/recall.md` 四 / 五）：
        //     - 对话类消息（User / Assistant 文本）分块后送进召回库，原文无损保存；
        //     - 工具输出统一替换为占位标记——AI 走"重建"路径（重新执行获取当前状态）。
        //       这是 v0 有意的技术债：不可重建的调用（一次性快照）重跑拿不到当时结果。
        //     注意先入库再清空：recall.index 需要 Tool 的原始 content 做索引视图。
        self.recall
            .index(recall::chunk_messages(&parts.to_compress));
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
            .ok_or_else(|| AgentError::Compression("未配置压缩指令".into()))?;
        // 调用方给领域相关的取舍，SDK 追加结构模板（`docs/compaction.md` 5.3）：
        // 摘要始终是 XML 块，且禁止推演"下一步"。
        let content = format!("{instruction}\n\n{COMPACTION_TEMPLATE}");
        messages.push(message::Message::System { content });

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
                    let summary_message = rebuilt.first().expect("rebuild 至少产出摘要").clone();
                    let mut next = rebuilt;
                    if let Some(system) = self.system_message() {
                        next.insert(0, system);
                    }
                    // 压缩是**追加**一条摘要，不重写日志（`docs/session.md` 一.决策 4）：
                    // 日志里 [原始… 旧summary 更原始… 新summary] 全留着，
                    // 恢复时 active_messages 只认最后一条 summary。
                    if let Some(store) = &self.session {
                        store.append(&summary_message)?;
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

/// 工具结果统一清空为占位标记（`docs/recall.md` 第五节）。
///
/// **不能留空字符串**——模型会把空 content 误读为"执行成功但无输出"，
/// 占位标记才是"结果被省略、可重新执行获取"的显式信号。
/// 占位文案进 SDK 契约，措辞要稳定。
fn strip_tool_outputs(messages: &mut [message::Message]) {
    for msg in messages.iter_mut() {
        if let message::Message::Tool { content, .. } = msg {
            if let Some(text) = content {
                if !text.trim().is_empty() {
                    *text = "[工具结果已省略以节省上下文；如仍需要，请重新执行调用获取当前状态]".to_string();
                }
            }
        }
    }
}
