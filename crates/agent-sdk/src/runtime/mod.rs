use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::tool;
use futures::StreamExt;
use std::fmt;
use std::pin::Pin;

#[derive(Debug)]
pub struct RunResult {
    // 本次调用新增的消息，按照发送顺序排序
    pub messages: Vec<message::Message>,

    // 停止的原因
    pub stop_reason: StopReason,

    pub usage: message::Usage,
}

impl RunResult {
    pub fn cache_hit_rate(&self) -> Option<f64> {
        self.usage.cache_hit_rate()
    }
}

#[derive(Debug)]
pub enum StopReason {
    Completed,
    MaxStepsReached,
    Cancelled,
}

#[derive(Debug)]
pub enum AgentError {
    ToolError(tool::ToolError),
    AdapterError(adapter::ModelError),
    CompressionError(String),
    Other(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolError(error) => write!(f, "{error}"),
            Self::AdapterError(error) | Self::Other(error) => write!(f, "{error}"),
            Self::CompressionError(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    TextDetal(String),
    MessageAdded(message::Message),
    CompressionStarted,
    CompressionFinished,
    ContextUsage { used_tokens: u64, limit_tokens: u64 },
    ToolStarted { call_id: String, name: String },
    ToolFinished { call_id: String, name: String },
    // 每次模型调用后上报，便于实时观察缓存命中
    Usage(message::Usage),
    Finished(RunResult),
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextDetal(text) => write!(f, "{text}"),
            Self::MessageAdded(message) => write!(f, "{message}"),
            Self::CompressionStarted => write!(f, "正在压缩上下文"),
            Self::CompressionFinished => write!(f, "上下文压缩完成"),
            Self::ContextUsage {
                used_tokens,
                limit_tokens,
            } => {
                let _ = write!(f, "上下文用量: {used_tokens}/{limit_tokens}");
                Ok(())
            }
            Self::ToolStarted { call_id, name } => {
                let _ = write!(f, "🔧 Tool Started [{call_id}]: {name}");
                Ok(())
            }
            Self::ToolFinished { call_id, name } => {
                let _ = write!(f, "✅ Tool Finished [{call_id}]: {name}");
                Ok(())
            }
            Self::Usage(usage) => {
                let hit = match usage.cache_hit_rate() {
                    Some(rate) => format!("{:.1}%", rate * 100.0),
                    None => "n/a".to_owned(),
                };
                let _ = write!(
                    f,
                    "📊 Usage: in={} (cached={}, hit={}) out={}",
                    usage.input_tokens,
                    usage.cached_tokens(),
                    hit,
                    usage.output_tokens
                );
                Ok(())
            }
            Self::Finished(result) => {
                let _ = write!(
                    f,
                    "🏁 Finished: {:?} ({} messages)",
                    result.stop_reason,
                    result.messages.len()
                );
                Ok(())
            }
        }
    }
}

pub struct Agent {
    model_config: adapter::ModelConfig,
    messages: Vec<message::Message>,
    tools: tool::ToolManager,
    compression_instruction: Option<String>,
    compression_pending: bool,
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
                    content: system_prompt,
                },
            );
        }

        Self {
            model_config,
            messages,
            tools,
            compression_instruction,
            compression_pending: false,
        }
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
            let usage = self.compress_context(&client).await?;
            yield AgentEvent::MessageAdded(self.messages.last().expect("compression added a summary").clone());
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
                let usage = self.compress_context(&client).await?;
                yield AgentEvent::MessageAdded(self.messages.last().expect("compression added a summary").clone());
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
            let response = adapter::invoke(&client, &self.model_config, model_request).await.map_err(|error| AgentError::AdapterError(error))?;

            total_usage = total_usage + response.usage;
            self.should_schedule_compression(&response.usage);
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

            let is_finished = tool_messages.is_empty();

            if is_finished {
                yield AgentEvent::Finished(RunResult {
                    messages: self.messages[start_index..].to_vec(),
                    stop_reason: StopReason::Completed,
                    usage: total_usage,
                });
                break;
            }
        }
        })
    }

    fn should_schedule_compression(&mut self, usage: &message::Usage) {
        let Some(limit) = self
            .model_config
            .context_window_tokens
            .filter(|&limit| limit > 0)
        else {
            return;
        };
        if self
            .compression_instruction
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
        {
            return;
        }
        // 下一次请求会携带本轮输出；用整数比较避免浮点精度和溢出。
        let used = usage.input_tokens.saturating_add(usage.output_tokens);
        if (used as u128) * 100 >= (limit as u128) * 80 {
            self.compression_pending = true;
        }
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

    async fn compress_context(
        &mut self,
        client: &reqwest::Client,
    ) -> Result<message::Usage, AgentError> {
        let mut messages = self.active_messages();
        messages.push(message::Message::System {
            content: self
                .compression_instruction
                .clone()
                .ok_or_else(|| AgentError::CompressionError("未配置压缩指令".into()))?,
        });
        let model_request = ModelRequest {
            messages: &messages,
            tools: &[],
        };
        let response = adapter::invoke(client, &self.model_config, model_request)
            .await
            .map_err(|error| AgentError::AdapterError(error))?;
        let summary = match response.message {
            message::Message::Assistant {
                content: Some(content),
                tool_calls,
                ..
            } if !content.trim().is_empty() && tool_calls.is_empty() => content,
            _ => return Err(AgentError::CompressionError("压缩响应没有有效摘要".into())),
        };
        self.messages
            .push(message::Message::ContextSummary { content: summary });
        self.compression_pending = false;
        Ok(response.usage)
    }
}
