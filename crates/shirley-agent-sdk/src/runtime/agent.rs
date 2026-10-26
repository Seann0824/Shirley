use super::compaction::{CompactionConfig, CutPlan, plan_cut};
use super::context::ContextProvider;
use super::error::AgentError;
use super::event::{AgentEvent, RunResult, StopReason};
use super::prompt::{SystemPrompt, SystemPromptContext};
use crate::SdkError;
use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::token;
use crate::tool;
use futures::StreamExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

pub struct Agent {
    model_config: adapter::ModelConfig,
    client: reqwest::Client,
    /// The system prompt is held separately: on a compaction rebuild it is not copied from the
    /// old messages, but regenerated and prepended each time.
    ///
    /// It can be a fixed string or a function — generated dynamically from
    /// [`SystemPromptContext`] (e.g. splicing in the current working directory and project
    /// guide). See [`super::prompt`].
    system_prompt: SystemPrompt,
    /// Runtime context for building the system prompt (the current working directory).
    ///
    /// A coding agent uses it to tell the model explicitly "where my working scope is", so the
    /// model doesn't blindly explore from the filesystem root (`plan.md`). `None` means the
    /// caller did not specify one.
    working_dir: Option<PathBuf>,
    messages: Vec<message::Message>,
    /// App-owned session identity, sent only to local model proxies for conversation reuse.
    request_session_id: Option<String>,
    /// User input and assistant deltas not yet committed when the stream is dropped, for the
    /// driver to flush into storage. The user turn is structured [`message::MessageContent`] so it can
    /// carry images alongside text.
    pending_user: Option<message::MessageContent>,
    pending_content: String,
    pending_reasoning: String,
    tools: tool::ToolManager,
    compaction_instruction: Option<String>,
    compaction_pending: bool,
    /// Compaction policy (trigger threshold / retain ratio / summary template). Defaults to the
    /// SDK built-in policy.
    compaction_config: CompactionConfig,
    /// Provider of the appended context at the end of each request (the application layer may
    /// inject one, e.g. a task ledger). The returned text is appended as a system message at the
    /// end of this turn's request — it does not enter `self.messages`, survives compaction, and
    /// leaves the prefix untouched (see [`super::context`]).
    context_provider: Option<Arc<dyn ContextProvider>>,
    /// Turns an image **reference** into something the provider can fetch, at request-build time.
    ///
    /// The application layer keeps the bytes in its own store and puts only an opaque id in the
    /// message ([`message::ImageSource::FileId`]). Resolving here means the session log stays a
    /// small reference instead of megabytes of base64, and the bytes have exactly one home. It
    /// rewrites the *request view* only: `self.messages` keeps the reference, so nothing is baked
    /// in and a restore / retry simply resolves again.
    image_resolver: Option<ImageResolver>,
    // L1 estimation + L2 calibration state (see the `token` module)
    token_counter: token::HeuristicCounter,
    /// Upper bound on model execution steps for a single user task (compaction does not reset
    /// this counter). `0` means **no cap** — the loop only ends when the model stops requesting
    /// tools.
    max_steps: usize,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `system_prompt` (a boxed closure) and `context_provider` (a trait object) are not
        // `Debug`; summarize them instead of deriving.
        f.debug_struct("Agent")
            .field("model_config", &self.model_config)
            .field("working_dir", &self.working_dir)
            .field("messages", &self.messages.len())
            .field("max_steps", &self.max_steps)
            .finish_non_exhaustive()
    }
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
        #[builder(into)] compaction_instruction: Option<String>,
        #[builder(default)] compaction_config: CompactionConfig,
        context_provider: Option<Arc<dyn ContextProvider>>,
        image_resolver: Option<ImageResolver>,
        #[builder(default = 64)] max_steps: usize,
    ) -> Result<Self, AgentError> {
        // `max_steps == 0` is allowed and means "no execution cap" (see the field docs).
        // Resolve once at construction against the working directory and prepend the System
        // message (an empty prompt is not prepended). Restored history contains no system, so it
        // is generated here — the same rule as a compaction rebuild.
        let resolved = system_prompt.resolve(&SystemPromptContext {
            working_dir: working_dir.clone(),
        });
        if !resolved.trim().is_empty() {
            messages.insert(0, message::Message::System { content: resolved });
        }

        Ok(Self {
            model_config,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(adapter::AdapterError::Transport)?,
            system_prompt,
            working_dir,
            messages,
            request_session_id: None,
            pending_user: None,
            pending_content: String::new(),
            pending_reasoning: String::new(),
            tools,
            compaction_instruction,
            compaction_pending: false,
            compaction_config,
            context_provider,
            image_resolver,
            token_counter: token::HeuristicCounter::new(),
            max_steps,
        })
    }

    /// Regenerate the system prompt message. After a compaction rebuild this prepends system.
    ///
    /// The system prompt does not change with the conversation and does not take part in
    /// compaction — it is taken fresh from `self.system_prompt` each time, so
    /// [`CompactParts::rebuild`] need not (and should not) copy any `System` from the original
    /// messages.
    fn system_message(&self) -> Option<message::Message> {
        let content = self.system_prompt.resolve(&self.prompt_context());
        (!content.trim().is_empty()).then_some(message::Message::System { content })
    }

    /// The runtime context currently used to resolve the system prompt.
    fn prompt_context(&self) -> SystemPromptContext {
        SystemPromptContext {
            working_dir: self.working_dir.clone(),
        }
    }

    /// The current model config (read-only).
    ///
    /// The application layer uses it to show "which model is in use now" and to preserve the
    /// other config when switching.
    pub fn model_config(&self) -> &adapter::ModelConfig {
        &self.model_config
    }

    /// Attach the application session identity without changing the model configuration.
    pub fn set_request_session_id(&mut self, session_id: Option<String>) {
        self.request_session_id = session_id;
    }

    /// Switch model: replace only the `model` field; the rest of the config (protocol /
    /// `base_url` / timeouts / window, etc.) is kept as-is.
    ///
    /// This is the only SDK seam for landing the application layer's `/model` command — the
    /// model is a hot-swappable runtime parameter and takes effect without rebuilding `Agent`
    /// (the next request uses the new model).
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model_config.model = model.into();
    }

    /// Replace the whole model config: hand down "model + protocol + endpoint" in one shot.
    ///
    /// This is the "hand down the model config in one shot" SDK seam
    /// (`docs/tool-calling-refactor.md` Step 3): the caller first uses a factory to compute a
    /// complete `ModelConfig`, then hands it over whole, with **no need to remember** the
    /// "first `set_model`, then `set_tool_calling`" order, and no risk of missing a protocol
    /// switch. `set_model` / `set_provider` remain small single-field seams, used as needed.
    pub fn set_model_config(&mut self, config: adapter::ModelConfig) {
        self.model_config = config;
    }

    /// Switch the model service connection (`base_url` + `api_key`): the rest of the config is
    /// kept as-is.
    ///
    /// This is the SDK seam for landing the application layer's `/login` command (symmetric with
    /// [`Agent::set_model`]): switching provider / endpoint needs no `Agent` rebuild and takes
    /// effect on the next request. Passing `None` for `api_key` means "no auth" (common for
    /// local services) and overwrites the old key — unlike "unchanged"; the caller decides
    /// whether to "keep the old value" or "actually clear it".
    pub fn set_provider(&mut self, base_url: impl Into<String>, api_key: Option<String>) {
        self.model_config.base_url = base_url.into();
        self.model_config.api_key = api_key;
    }

    /// Remove a registered tool (a runtime seam symmetric with [`ToolManager::register`]).
    ///
    /// Triggers the tool's `on_unregister` (= destroy), for dynamically enabling/disabling tools
    /// at runtime. Returns `Err` when the name is not registered. Same style as `set_model` /
    /// `set_provider`: no `Agent` rebuild.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when no tool with `name` is registered.
    pub fn unregister_tool(&mut self, name: &str) -> Result<(), AgentError> {
        self.tools.unregister(name).map_err(Into::into)
    }

    /// A read-only view of the conversation history.
    ///
    /// Lets upper layers show "what the Agent actually remembers now" and locate the target
    /// message for "rewind" — history that has been compacted away is not here, so it is
    /// naturally non-rewindable (rewinding is only meaningful after the compaction boundary).
    pub fn messages(&self) -> &[message::Message] {
        &self.messages
    }

    /// After dropping `run_stream`, save the unfinished messages and return the messages the
    /// application layer needs to append to the log. Repeated calls are empty; completed messages
    /// are not re-submitted. Tool calls with no returned result get a placeholder interrupted
    /// result, so the next turn doesn't carry an orphan `tool_call` and get rejected by the
    /// endpoint.
    pub fn finish_interrupted_turn(&mut self) -> Vec<message::Message> {
        let mut added = Vec::new();
        if let Some(content) = self.pending_user.take() {
            added.push(message::Message::User { content });
        }
        let content = std::mem::take(&mut self.pending_content);
        let reasoning = std::mem::take(&mut self.pending_reasoning);
        if !content.is_empty() || !reasoning.is_empty() {
            added.push(message::Message::Assistant {
                content: (!content.is_empty()).then_some(content),
                reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
                tool_calls: Vec::new(),
            });
        }
        // Only the tool group at the tail of the current message may be unfinished.
        if let Some(index) = self
            .messages
            .iter()
            .rposition(|message| !matches!(message, message::Message::Tool { .. }))
            && let message::Message::Assistant { tool_calls, .. } = &self.messages[index]
        {
            for call in tool_calls {
                let completed = self.messages[index + 1..].iter().any(|message| {
                    matches!(message, message::Message::Tool { tool_call_id, .. } if tool_call_id == &call.id)
                });
                if !completed {
                    added.push(message::Message::Tool {
                        tool_call_id: call.id.clone(),
                        content: Some(
                            "Tool call was interrupted while waiting; execution may have produced side effects — check the actual state first.".into(),
                        ),
                    });
                }
            }
        }
        self.messages.extend(added.iter().cloned());
        added
    }

    /// Truncate the conversation history to the first `len` messages, returning the truncated
    /// length.
    ///
    /// This is the **atomic capability** for rewinding: the SDK only "shortens the working set"
    /// and maintains internal invariants (never crosses the prepended system, clamps
    /// out-of-range), while **"where to truncate" is a business judgment** — e.g. "roll back the
    /// last user turn" is located by the upper layer via [`Agent::messages`] and then called
    /// (`docs/session.md` decision 4: truncate the tail only, no middle holes). Persistence
    /// truncates on its own by the truncated length (the SDK does not hold the session log).
    pub fn truncate_messages(&mut self, len: usize) -> usize {
        // The prepended system is an invariant maintained by construction / compaction rebuild
        // and must not be truncated away: use the count of leading consecutive system messages as
        // the lower bound. `clamp` also catches an out-of-range `len` (kept as-is when too long).
        let floor = self
            .messages
            .iter()
            .take_while(|m| matches!(m, message::Message::System { .. }))
            .count();
        let len = len.clamp(floor, self.messages.len());
        self.messages.truncate(len);
        self.messages.len()
    }

    /// Run one `ReAct` round to completion and return the aggregated result.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when the model request, tool execution, or context
    /// compaction fails, or when the event stream ends without a final result.
    pub async fn run(&mut self, task: &str) -> Result<RunResult, AgentError> {
        self.run_content(message::MessageContent::text(task)).await
    }

    /// Like [`Agent::run`], but the user turn is structured [`message::MessageContent`] (text plus
    /// image parts). The `&str` version delegates here.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] under the same conditions as [`Agent::run`].
    pub async fn run_content(
        &mut self,
        task: message::MessageContent,
    ) -> Result<RunResult, AgentError> {
        let mut events = self.run_stream_content(task);
        while let Some(event) = events.next().await {
            if let AgentEvent::Finished(result) = event? {
                return Ok(result);
            }
        }
        Err(AgentError::Other(
            "agent stream ended without a final result".into(),
        ))
    }

    /// One-shot completion: run **a single tool-less, non-streaming** model request for one
    /// prompt and return the assistant's text.
    ///
    /// A general primitive for the application layer to "do something small with one model call"
    /// (e.g. auto-generate a title for a new session). It **does not touch** `self.messages` /
    /// the session log / the recall store / the task ledger — it purely read-reuses the current
    /// `model_config`, so it can be called while holding `&self` without polluting the session.
    ///
    /// Unlike [`Agent::run`]: it doesn't drive the `ReAct` loop, send tools, or write storage;
    /// `stream` / `thinking` are turned off in place (a one-shot completion wants short text, so
    /// streaming and reasoning are pointless). It takes the `Assistant` text (`content`); if the
    /// model returned only reasoning (`reasoning_content`), it falls back to that; both empty
    /// returns an empty string.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError`] when the model request or response decoding fails.
    pub async fn complete(&self, prompt: &str) -> Result<String, AgentError> {
        let client = self.client.clone();
        // Clone the config and turn off streaming / reasoning: borrow only the model name and
        // endpoint, without touching `self`'s config.
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
            session_id: None,
        };

        let mut stream = adapter::invoke(&client, &config, request);
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
        self.run_stream_content(message::MessageContent::text(task))
    }

    /// Like [`Agent::run_stream`], but the user turn is structured [`message::MessageContent`] (text
    /// parts plus image parts). The `&str` version delegates here, so a text-only turn produces
    /// byte-identical bytes as before (the plain-string fast path keeps the KV prefix cache).
    ///
    /// # Panics
    ///
    /// Panics if the internal pending-user slot was not set by this method (it is
    /// set immediately before the stream starts, so this cannot happen in practice).
    pub fn run_stream_content(
        &mut self,
        task: message::MessageContent,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, AgentError>> + Send + '_>> {
        self.pending_user = Some(task);
        Box::pin(async_stream::try_stream! {
            let client = self.client.clone();
            let mut total_usage = message::Usage::default();
            if self.compaction_pending {
                yield AgentEvent::CompactionStarted;
                let (usage, summary) = self.compact_context(&client).await?;
                if let Some(summary) = summary {
                    yield AgentEvent::MessageAdded(summary);
                }
                yield AgentEvent::CompactionFinished;
                total_usage = total_usage + usage;
                yield AgentEvent::Usage(usage);
            }
            let mut start_index = self.messages.len();

            let user_message = message::Message::User {
                content: self.pending_user.take().expect("run_stream records the pending user"),
            };
            self.messages.push(user_message.clone());
            yield AgentEvent::MessageAdded(user_message);

            let mut steps = 0usize;
            let mut recoveries = 0usize;
            let mut resuming = false;
            loop {
                if self.max_steps != 0 && steps >= self.max_steps {
                    let notice = message::Message::Assistant {
                        content: Some(format!("Reached the execution limit for this turn ({} model calls); the task is not yet complete. The current conversation and tool results have been kept; review the existing conclusions and decide whether to continue.", self.max_steps)),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                    };
                    self.messages.push(notice.clone());
                    yield AgentEvent::MessageAdded(notice);
                    yield AgentEvent::Finished(RunResult {
                        messages: self.messages[start_index..].to_vec(),
                        stop_reason: StopReason::MaxStepsReached,
                        usage: total_usage,
                    });
                    break;
                }
                steps += 1;
                if self.compaction_pending {
                    yield AgentEvent::CompactionStarted;
                    let (usage, summary) = self.compact_context(&client).await?;
                    if let Some(summary) = summary {
                        yield AgentEvent::MessageAdded(summary);
                    }
                    yield AgentEvent::CompactionFinished;
                    total_usage = total_usage + usage;
                    yield AgentEvent::Usage(usage);

                    // Compaction rebuilds self.messages wholesale (far shorter than the original
                    // list), invalidating the absolute start_index — without clamping, the slice
                    // at Finished panics out of range ("range start index N out of range"). After
                    // clamping to the current length, the sliced [summary, task, remain..] is
                    // still "the new content visible this turn": summary is this turn's
                    // compaction product, remain is this turn's conversation and tool chain.
                    start_index = start_index.min(self.messages.len());
                }
                let mut active_messages = self.active_messages();
                if resuming {
                    active_messages.push(message::Message::System {
                        content: include_str!("../../prompts/stream_recovery.md").trim().to_owned(),
                    });
                }
                let tools = self.tools.definitions();
                let estimated = token::count_messages(&active_messages) + token::count_tools(&tools);
                let model_request = ModelRequest {
                    messages: &active_messages,
                    tools: &tools,
                    session_id: self.request_session_id.as_deref(),
                };
                let mut is_finished = false;
                let mut interrupted = None;
                let mut stream = adapter::invoke(&client, &self.model_config, model_request);
                while let Some(adapter_event) = stream.next().await {
                    let adapter_event = match adapter_event {
                        Ok(event) => event,
                        Err(error) => { interrupted = Some(error); break; }
                    };

                    match adapter_event {
                        adapter::AdapterEvent::Retrying { attempt, reason } => {
                            yield AgentEvent::Retrying { attempt, reason };
                        }
                        adapter::AdapterEvent::ImagesDropped { reason } => {
                            // Silent degradation: the images were dropped and the request
                            // re-sent as text. Tell the user in plain language (never surface a
                            // "does it support images" capability question).
                            let _ = reason;
                            yield AgentEvent::Notice(
                                "当前模型看不了图片，已去掉图片继续。".to_owned(),
                            );
                        }
                        adapter::AdapterEvent::Finished(response) => {
                            resuming = false;
                            self.token_counter.calibrate(response.usage.input_tokens, estimated);
                            self.pending_content.clear();
                            self.pending_reasoning.clear();
                            total_usage = total_usage + response.usage;
                            self.compaction_pending = self.should_schedule_compaction(&response.usage);
                            let response_message = response.message;
                            self.messages.push(response_message.clone());
                            yield AgentEvent::MessageAdded(response_message.clone());
                            if let Some(limit_tokens) = self.model_config.context_window_tokens.filter(|&limit| limit > 0) {
                                yield AgentEvent::ContextUsage {
                                    used_tokens: response.usage.input_tokens.saturating_add(response.usage.output_tokens),
                                    limit_tokens,
                                };
                            }

                            yield AgentEvent::Usage(response.usage);

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
                                        // ToolContext is Arc inside, so clone only bumps the
                                        // refcount; clone inside the loop instead of capturing
                                        // self into the async block. The context is held by
                                        // ToolManager (tools write it in on_register); here we
                                        // just take one clone and hand it down.
                                        let ctx = self.tools.context().clone();

                                        tasks.push(async move {
                                            let started = std::time::Instant::now();
                                            let (ok, content, error_kind, structured) =
                                                match tools.invoke(call, ctx).await {
                                                Ok(output) => {
                                                    // Keep the raw value: the flattened
                                                    // `content` is the model's view, the value is
                                                    // the UI's (docs/render-parts.md).
                                                    let content = value_to_content(&output);
                                                    (true, content, None, Some(output))
                                                }
                                                // The tool message carries the error text for the
                                                // model to read; the stable kind travels on the
                                                // event so the UI classifies structurally instead
                                                // of sniffing the text.
                                                Err(error) => {
                                                    let kind = crate::SdkError::kind(&error);
                                                    (
                                                        false,
                                                        message::MessageContent::text(error.to_string()),
                                                        Some(kind),
                                                        None,
                                                    )
                                                }
                                            };
                                            (
                                                call,
                                                ok,
                                                content,
                                                error_kind,
                                                started.elapsed().as_millis() as u64,
                                                structured,
                                            )
                                        });
                                    }

                                    let mut tool_messages = Vec::with_capacity(tool_calls.len());
                                    while let Some((
                                        call,
                                        ok,
                                        content,
                                        error_kind,
                                        elapsed_ms,
                                        structured,
                                    )) = tasks.next().await
                                    {
                                        let tool_message = message::Message::Tool {
                                            tool_call_id: call.id.clone(),
                                            content: Some(content.clone()),
                                        };
                                        self.messages.push(tool_message.clone());
                                        yield AgentEvent::MessageAdded(tool_message.clone());
                                        yield AgentEvent::ToolFinished {
                                            call_id: call.id.clone(),
                                            name: call.name.clone(),
                                            ok,
                                            output: content.to_string(),
                                            structured,
                                            error_kind,
                                            elapsed_ms,
                                        };

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
                            self.pending_reasoning.push_str(&delta);
                            yield AgentEvent::ReasoningDelta(delta);
                        }
                        adapter::AdapterEvent::ContentDelta(delta) => {
                            self.pending_content.push_str(&delta);
                            yield AgentEvent::ContentDelta(delta);
                        }
                    }
                }
                drop(stream);
                if let Some(error) = interrupted {
                    // No tools from an incomplete response have been dispatched. Commit only
                    // visible text, keep completed tool results, and ask for a continuation.
                    // Never replay a tool group or resend the original user task.
                    let has_partial = !self.pending_content.is_empty() || !self.pending_reasoning.is_empty();
                    if error.is_retryable() && has_partial && recoveries < 2 {
                        recoveries += 1;
                        for message in self.finish_interrupted_turn() {
                            yield AgentEvent::MessageAdded(message);
                        }
                        yield AgentEvent::Retrying { attempt: recoveries, reason: error.to_string() };
                        resuming = true;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                    Err(AgentError::Adapter(error))?;
                }
                if is_finished {
                    break;
                }
            }
        })
    }

    /// The current token budget for the retained tail: `context_window_tokens * 20%`.
    ///
    /// Returns `None` when no window is configured (or it is 0), meaning **no compaction**.
    fn retain_budget(&self) -> Option<u64> {
        let window = self.model_config.context_window_tokens.filter(|&w| w > 0)?;
        Some(((window as f64) * self.compaction_config.retain_ratio).round() as u64)
    }

    /// Compute the compaction cut point and extract the user's latest question.
    ///
    /// The three cases returning `None` all mean "no compaction this turn": no context window
    /// configured, nothing compressible, or all messages already fit in the budget.
    fn compute_cut(&self, messages: &[message::Message]) -> Option<CutPlan> {
        let budget = self.retain_budget()?;
        plan_cut(messages, budget, &self.token_counter)
    }

    fn should_schedule_compaction(&self, usage: &message::Usage) -> bool {
        let Some(limit) = self
            .model_config
            .context_window_tokens
            .filter(|&limit| limit > 0)
        else {
            return false;
        };
        if self
            .compaction_instruction
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        {
            return false;
        }
        // The next request will carry this turn's output. Use integer comparison to avoid float
        // precision and overflow: threshold = limit * trigger_ratio, turned into the integer test
        // (used / limit >= ratio).
        let used = usage.input_tokens.saturating_add(usage.output_tokens);
        let ratio = self.compaction_config.trigger_ratio;
        if !(ratio > 0.0 && ratio <= 1.0) {
            return false;
        }
        // Scale by 10000 for fixed-point comparison: 0.80 → 8000, avoiding f64 precision issues.
        let scaled = (ratio * 10_000.0).round() as u128;
        (used as u128) * 10_000 >= (limit as u128) * scaled
    }

    /// Normal execution and compaction share the same working set; messages already replaced by a
    /// summary in a restored log no longer participate. Only a copy is returned; the full log is
    /// still available for the UI and history viewing.
    fn active_history(&self) -> Vec<message::Message> {
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

    fn active_messages(&self) -> Vec<message::Message> {
        let mut active = self.active_history();
        // The appended context (injected by the application layer, e.g. a task ledger) is
        // **appended at the end** as a system message.
        //
        // Why the end:
        //   - it does not enter `self.messages`, compaction can't touch it, so it survives
        //     compaction;
        //   - appending at the tail leaves the preceding prefix untouched, so when the content is
        //     stable the prefix cache hits as usual (the Responses adapter keeps system in place
        //     precisely for this);
        //   - the content changes only when the provider decides it should, minimizing the change
        //     points.
        if let Some(provider) = &self.context_provider
            && let Some(text) = provider.context()
            && !text.trim().is_empty()
        {
            active.push(message::Message::System { content: text });
        }
        // A `file_id` must never reach an encoder: `chat_content` rejects it (the provider has no
        // way to look it up). Without a resolver only the **drop** half is active, which is
        // exactly the safe half -- a user attachment is never forwarded anyway, and an
        // unresolvable tool image is dropped rather than sent as a dangling id.
        let drop_only = |_id: &str| None;
        let resolver: &dyn Fn(&str) -> Option<String> = match &self.image_resolver {
            Some(resolver) => resolver.as_ref(),
            None => &drop_only,
        };
        for message in &mut active {
            resolve_image_refs(message, resolver);
        }
        // An assistant turn with neither text nor tool calls is rejected by every provider
        // ("content or tool_calls must be set"). It can only be the remains of an interrupted
        // round; drop it here as well, so a log written before the persistence-side guard existed
        // does not poison every later request.
        active.retain(|message| !is_unsendable_assistant(message));
        active
    }

    /// Compact the context: compute the cut → split into three segments → summarize only
    /// `to_compress` → rebuild the messages.
    ///
    /// Corresponds to the structure in `docs/compaction.md` 5.1 and "no summary-on-summary" in
    /// 5.2:
    ///
    /// ```text
    /// self.messages = [system (regenerated)] + [new ContextSummary] + current_task + remain
    /// ```
    ///
    /// Difference from the old implementation: the old one **appended** the summary at the end
    /// (effectively eating the tail too), and the summarizer saw the whole conversation without
    /// knowing "what this turn's task is", so it forgot the task and invented next steps.
    async fn compact_context(
        &mut self,
        client: &reqwest::Client,
    ) -> Result<(message::Usage, Option<message::Message>), AgentError> {
        // 1. Compute the cut and split into three segments.
        let history = self.active_history();
        let Some(mut plan) = self.compute_cut(&history) else {
            // Nothing compressible (no window / only background left / already fits): no
            // compaction, pass through.
            self.compaction_pending = false;
            return Ok((message::Usage::default(), None));
        };
        // The summary event is only appended to the log; the running original task may not be at
        // the tail of the post-summary log. Restore the most recent user task from the full
        // record, but do not bring back the already-compacted tool/reasoning history.
        if plan.current_task.is_none() {
            plan.current_task = self
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m, message::Message::User { .. }))
                .cloned();
        }
        let mut parts = plan.split(&history);
        // split can only locate a task within history; a task restored from the archived prefix
        // also needs to be kept at rebuild.
        if parts.current_task.is_some()
            && !history
                .iter()
                .any(|m| matches!(m, message::Message::User { .. }))
        {
            parts.task_extracted = true;
        }

        // 2. The old summary doesn't count against the tail budget, but must be an input to the
        // new summary; otherwise, when rebuild replaces the old summary, previously confirmed
        // conclusions are discarded too.
        let mut messages: Vec<_> = history[..plan.head]
            .iter()
            .filter(|m| matches!(m, message::Message::ContextSummary { .. }))
            .cloned()
            .collect();
        messages.extend(parts.to_compress.iter().cloned());
        // Explicitly tell the summarizer "what this turn's task is", so it doesn't guess from
        // history (guessing leads to inventing).
        if let Some(task) = &parts.current_task {
            messages.push(task.clone());
        }
        if let Some(text) = self
            .context_provider
            .as_ref()
            .and_then(|p| p.compaction_context())
            .filter(|text| !text.trim().is_empty())
        {
            messages.push(message::Message::System { content: text });
        }
        let instruction = self.compaction_instruction.clone().ok_or_else(|| {
            AgentError::Compaction("compression instruction is not configured".into())
        })?;
        // The caller supplies the domain-relevant trade-offs; the SDK appends the structural
        // template (`docs/compaction.md` 5.3): the summary is always XML blocks and speculation
        // about "next steps" is forbidden. The template can be overridden via
        // `compaction_config`.
        let content = format!("{instruction}\n\n{}", self.compaction_config.template);
        messages.push(message::Message::System { content });

        // The summary request is **text only**. An image reference here is nothing but a way to
        // fail: the ChatCompletions / Anthropic encoders reject a `file_id` (only the main request
        // view resolves or drops it), and inlining the bytes as a `data:` URL costs tens of
        // thousands of tokens for a picture the summary could not actually reproduce. Stripping
        // keeps the line naming the attachment, so the summary still knows a picture existed.
        // Only this copy is touched -- `self.messages` keeps the reference, and the rebuilt log
        // (task included) keeps it too.
        let mut messages = adapter::content::strip_images_with(
            &messages,
            adapter::content::IMAGE_OMITTED_FOR_SUMMARY,
        );

        // The compaction trigger ratio is not the capacity limit of the summary request. Reserve
        // output space from the window; estimation only decides how to shrink the request, not
        // replace the server's real token validation.
        if let Some(window) = self.model_config.context_window_tokens.filter(|w| *w > 0) {
            let reserve = self
                .model_config
                .max_output_tokens
                .map_or_else(|| (window / 10).max(1), u64::from);
            let budget = window.saturating_sub(reserve);
            // Prefer removing the repetitive, verbose reasoning drafts, keeping the text, the
            // ledger, and the tool evidence. Only the summary request's copy is touched; the
            // original record and retained tail are unaffected.
            if self.token_counter.estimate_messages(&messages) > budget {
                for message in &mut messages {
                    if let message::Message::Assistant {
                        reasoning_content, ..
                    } = message
                    {
                        *reasoning_content = None;
                    }
                }
            }
            trim_compaction_tool_outputs(&mut messages, budget, &self.token_counter);
        }

        let model_request = ModelRequest {
            messages: &messages,
            tools: &[],
            session_id: None,
        };

        let mut stream = adapter::invoke(client, &self.model_config, model_request);
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
                            return Err(AgentError::Compaction(
                                "compression response contained no usable summary".into(),
                            ));
                        }
                    };

                    // 3. Rebuild: [new summary] + current_task + remain, then regenerate the
                    //    system prompt and prepend it. system does not take part in compaction and
                    //    is not copied from the old messages; one is taken fresh each rebuild.
                    let rebuilt = parts.rebuild(summary);
                    let summary_message = rebuilt
                        .first()
                        .expect("rebuild always yields a summary")
                        .clone();
                    let mut next = rebuilt;
                    if let Some(system) = self.system_message() {
                        next.insert(0, system);
                    }
                    // Compaction rebuilds self.messages wholesale into [system?, new summary,
                    // task, remain]. The application layer writes storage itself based on the
                    // `MessageAdded(summary)` event (the SDK does not hold the session log).
                    self.messages = next;
                    self.compaction_pending = false;

                    // 4. Calibrate the estimation factor (L2) with the real input_tokens. The
                    //    estimation basis must match the request: to-compress segment + task +
                    //    instruction. Excludes system / tool schema — this compaction request
                    //    didn't carry them either.
                    self.token_counter
                        .calibrate(usage.input_tokens, token::count_messages(&messages));

                    return Ok((usage, Some(summary_message)));
                }
                adapter::AdapterEvent::ReasoningDelta(_)
                | adapter::AdapterEvent::ContentDelta(_)
                | adapter::AdapterEvent::Retrying { .. }
                | adapter::AdapterEvent::ImagesDropped { .. } => {}
            }
        }

        Err(AgentError::Compaction(
            "compression produced no usage report".into(),
        ))
    }
}

fn trim_compaction_tool_outputs(
    messages: &mut [message::Message],
    budget: u64,
    counter: &token::HeuristicCounter,
) {
    let raw_budget = (budget as f64 / counter.factor()) as u64;
    let mut total = token::count_messages(messages);
    let mut tools: Vec<_> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, msg)| match msg {
            // Only text-only results are excerpted; an image result (e.g. a
            // computer-use screenshot) is never truncated, so no observation is lost.
            message::Message::Tool {
                content: Some(content),
                ..
            } if content.is_text_only() => Some((i, token::count_content_parts(content))),
            _ => None,
        })
        .collect();
    tools.sort_unstable_by_key(|(_, cost)| std::cmp::Reverse(*cost));
    for (i, cost) in tools {
        if total <= raw_budget {
            break;
        }
        let message::Message::Tool {
            tool_call_id,
            content: Some(content),
        } = &mut messages[i]
        else {
            unreachable!()
        };
        let text = content.as_text();
        let marker = format!(
            "\n[tool result excerpt; middle omitted; original tool_call_id={tool_call_id}]\n"
        );
        // Small results are not trimmed; large results keep at least a 256-token excerpt
        // (including the omission marker).
        let target = cost.saturating_sub(total - raw_budget).max(256);
        let marker_cost = token::count_text(&marker);
        if target >= cost || target <= marker_cost {
            continue;
        }
        let keep = target - marker_cost;
        let prefix = text_prefix_for_tokens(&text, keep / 2);
        let suffix: String = text.chars().rev().collect();
        let suffix = text_prefix_for_tokens(&suffix, keep - keep / 2);
        let excerpt = format!(
            "{prefix}{marker}{}",
            suffix.chars().rev().collect::<String>()
        );
        let new_cost = token::count_text(&excerpt);
        if new_cost < cost {
            *content = message::MessageContent::text(excerpt);
            total = total.saturating_sub(cost - new_cost);
        }
    }
}

/// Convert a tool's JSON output into message content.
///
/// A tool may return **structured content**: a JSON array that deserializes into
/// [`message::MessageContent`] and carries at least one image (a computer-use screenshot). That is
/// kept structured so the image reaches the model. Everything else becomes text, using the
/// compact JSON rendering (`Value::to_string`) — for a plain string result that is the
/// quoted form the runtime always produced, so the request bytes (and the prefix cache) are
/// unchanged for every existing tool.
/// Turn a tool's raw return value into what the model reads.
///
/// Two envelope shapes are recognized; everything else is stringified as before:
///
/// 1. a `MessageContent` array carrying images -- handed to the model as multimodal content;
/// 2. an object with a string `content` field -- the tool is returning
///    `{ "content": <text for the model>, "parts": [<structured data for the UI>] }`.
///    Only `content` reaches the model: the parts are render-layer data (asset ids, links,
///    tables) and stringifying them would waste context and confuse the model.
///
/// The convention matters because the tool result is simultaneously the model's view and the
/// UI's source (`docs/render-parts.md`): a tool must be able to say "here is a one-line
/// description for you, and here is an image for the human" without inventing a second channel.
/// Turns a library id into something the provider can fetch (a `data:` URL).
///
/// Injected by the application layer: the SDK must not know where bytes live. Returning `None`
/// drops the reference instead of forwarding a dangling id.
pub type ImageResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Rewrite image references, **in the request view only**.
///
/// The two directions are deliberately different:
///
/// - A **tool result** is content the model asked for, so a reference there is resolved into
///   something the provider can fetch.
/// - A **user message** is not: the whole point of a resource library is that the model *decides*
///   what to read, so its reference must reach the provider as nothing at all. The reference still
///   lives in `self.messages`, where the application layer renders it as a card.
///
/// A reference that cannot be resolved is dropped rather than forwarded: a dangling provider-side
/// id turns "the local file is gone" into a confusing remote error. Text parts are always kept, so
/// a message never collapses to nothing.
fn resolve_image_refs(message: &mut message::Message, resolver: &dyn Fn(&str) -> Option<String>) {
    let (content, asked_for) = match message {
        message::Message::User { content } => (content, false),
        message::Message::Tool {
            content: Some(content),
            ..
        } => (content, true),
        _ => return,
    };
    if !content.has_images() {
        return;
    }
    let parts: Vec<message::ContentPart> = content
        .as_parts()
        .iter()
        .filter_map(|part| match part {
            message::ContentPart::Image(image) => match &image.source {
                message::ImageSource::FileId { file_id } => {
                    asked_for.then(|| resolver(file_id)).flatten().map(|url| {
                        message::ContentPart::Image(message::ImagePart {
                            source: message::ImageSource::Url { url },
                            detail: image.detail,
                        })
                    })
                }
                message::ImageSource::Url { .. } => Some(part.clone()),
            },
            _ => Some(part.clone()),
        })
        .collect();
    *content = message::MessageContent::from(parts);
}

/// True for an assistant message that carries neither text nor tool calls -- it has nothing to say
/// and no provider accepts it.
fn is_unsendable_assistant(message: &message::Message) -> bool {
    let message::Message::Assistant {
        content,
        reasoning_content,
        tool_calls,
    } = message
    else {
        return false;
    };
    if !tool_calls.is_empty() {
        return false;
    }
    let blank = |text: &Option<String>| text.as_deref().is_none_or(str::is_empty);
    blank(content) && blank(reasoning_content)
}

fn value_to_content(output: &serde_json::Value) -> message::MessageContent {
    if output.is_array()
        && let Ok(content) = serde_json::from_value::<message::MessageContent>(output.clone())
        && content.has_images()
    {
        return content;
    }
    if let Some(text) = output
        .as_object()
        .and_then(|object| object.get("content"))
        .and_then(serde_json::Value::as_str)
    {
        return message::MessageContent::text(text.to_string());
    }
    message::MessageContent::text(output.to_string())
}

fn text_prefix_for_tokens(text: &str, budget: u64) -> &str {
    // Each character's weight is an integer multiple of 1/4, accumulated with the same heuristic,
    // avoiding splitting UTF-8.
    let mut quarters = 0;
    for (i, ch) in text.char_indices() {
        quarters += (token::char_weight(ch) * 4.0) as u64;
        if quarters > budget.saturating_mul(4) {
            return &text[..i];
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test provider: returns fixed text.
    struct FixedProvider(&'static str);
    impl ContextProvider for FixedProvider {
        fn context(&self) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    /// Test provider: never injects.
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
            .system_prompt("You are Shirley")
            .maybe_context_provider(provider)
            .build()
            .unwrap()
    }

    /// With no provider, nothing is injected (avoiding a meaningless system message each turn).
    #[test]
    fn no_provider_injects_nothing() {
        let agent = agent_with(None);
        let active = agent.active_messages();
        assert_eq!(active.len(), 1, "only the prepended system");
        assert!(matches!(active[0], message::Message::System { .. }));
    }

    /// A provider returning `None` also injects nothing.
    #[test]
    fn provider_returning_none_injects_nothing() {
        let agent = agent_with(Some(Arc::new(EmptyProvider)));
        let active = agent.active_messages();
        assert_eq!(active.len(), 1, "only the prepended system");
    }

    /// The appended context is injected as the **last** system message and does not enter
    /// `self.messages` (so a compaction rebuild of `self.messages` can't touch it).
    #[test]
    fn context_is_appended_and_not_persisted() {
        let agent = agent_with(Some(Arc::new(FixedProvider("injected task ledger"))));

        // The appended context is not in the working set — neither compaction nor rewind can
        // touch it.
        assert!(
            !agent
                .messages
                .iter()
                .any(|m| matches!(m, message::Message::System { content } if content.contains("task ledger"))),
            "appended context must not be written into self.messages"
        );

        let active = agent.active_messages();
        let last = active.last().expect("at least one message");
        let message::Message::System { content } = last else {
            panic!("the last message should be the injected system");
        };
        assert_eq!(content, "injected task ledger");
        // The original prepended system is still first.
        assert!(
            matches!(&active[0], message::Message::System { content } if content == "You are Shirley")
        );
    }

    #[test]
    fn oversized_tool_evidence_is_excerpted_without_destroying_small_results() {
        let original = format!("important start{}critical end", "output".repeat(5000));
        let mut messages = vec![
            message::Message::Tool {
                tool_call_id: "large".into(),
                content: Some(original.into()),
            },
            message::Message::Tool {
                tool_call_id: "small".into(),
                content: Some("test failed: expected model".into()),
            },
        ];
        trim_compaction_tool_outputs(&mut messages, 600, &token::HeuristicCounter::new());
        let message::Message::Tool {
            content: Some(excerpt),
            ..
        } = &messages[0]
        else {
            panic!()
        };
        let excerpt = excerpt.as_text();
        assert!(excerpt.starts_with("important start"));
        assert!(excerpt.ends_with("critical end"));
        assert!(excerpt.contains("tool_call_id=large"));
        assert!(token::count_messages(&messages) <= 600);
        assert!(
            matches!(&messages[1], message::Message::Tool { content: Some(text), .. } if text == "test failed: expected model")
        );
    }

    #[test]
    fn tool_evidence_within_budget_is_unchanged() {
        let mut messages = vec![message::Message::Tool {
            tool_call_id: "a".into(),
            content: Some("modelName returned empty".into()),
        }];
        trim_compaction_tool_outputs(&mut messages, 1000, &token::HeuristicCounter::new());
        assert!(
            matches!(&messages[..], [message::Message::Tool { tool_call_id, content: Some(text) }] if tool_call_id == "a" && text == "modelName returned empty")
        );
    }

    fn text_of(message: &message::Message) -> String {
        let content = match message {
            message::Message::User { content }
            | message::Message::Tool {
                content: Some(content),
                ..
            } => content,
            _ => return String::new(),
        };
        content
            .as_parts()
            .iter()
            .filter_map(|part| match part {
                message::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn ref_image() -> message::ContentPart {
        message::ContentPart::Image(message::ImagePart {
            source: message::ImageSource::FileId {
                file_id: "abc123".into(),
            },
            detail: None,
        })
    }

    fn image_sources(message: &message::Message) -> Vec<message::ImageSource> {
        let content = match message {
            message::Message::User { content }
            | message::Message::Tool {
                content: Some(content),
                ..
            } => content,
            _ => return Vec::new(),
        };
        content
            .as_parts()
            .iter()
            .filter_map(|part| match part {
                message::ContentPart::Image(image) => Some(image.source.clone()),
                _ => None,
            })
            .collect()
    }

    /// The model must not see a picture the *user* merely attached: it only sees the text mention,
    /// and loads the picture through a tool when it decides to. A tool result is the other way
    /// round -- the model asked for it, so the reference is resolved.
    #[test]
    fn user_references_stay_unresolved_while_tool_references_resolve() {
        let resolver = |id: &str| Some(format!("data:image/png;base64,{id}"));

        let mut user = message::Message::User {
            content: message::MessageContent::from(vec![
                message::ContentPart::Text {
                    text: "look at this".into(),
                },
                ref_image(),
            ]),
        };
        resolve_image_refs(&mut user, &resolver);
        assert!(
            image_sources(&user).is_empty(),
            "a user attachment must not reach the provider"
        );
        assert_eq!(text_of(&user), "look at this", "the text survives");

        let mut tool = message::Message::Tool {
            tool_call_id: "call-1".into(),
            content: Some(message::MessageContent::from(vec![ref_image()])),
        };
        resolve_image_refs(&mut tool, &resolver);
        let sources = image_sources(&tool);
        assert_eq!(
            sources.len(),
            1,
            "a tool result the model asked for is resolved"
        );
        match &sources[0] {
            message::ImageSource::Url { url } => assert_eq!(url, "data:image/png;base64,abc123"),
            other => panic!("expected a resolved url, got {other:?}"),
        }
    }

    /// A reference that cannot be resolved is dropped rather than forwarded: a dangling provider
    /// id turns "the local file is gone" into a confusing remote error.
    #[test]
    fn unresolvable_references_are_dropped_not_forwarded() {
        let mut tool = message::Message::Tool {
            tool_call_id: "call-1".into(),
            content: Some(message::MessageContent::from(vec![
                message::ContentPart::Text { text: "x".into() },
                ref_image(),
            ])),
        };
        resolve_image_refs(&mut tool, &|_id| None);
        assert!(image_sources(&tool).is_empty());
        assert_eq!(text_of(&tool), "x");
    }
}
