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
}

#[derive(Debug)]
pub enum StopReason {
    Completed,
    MaxStepsReached,
    Cancelled,
}

#[derive(Debug)]
pub enum AgentError {
    // AdapterError(adapter::AdapterError),
    ToolError(tool::ToolError),
    AdapterError(adapter::ModelError),
    Other(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolError(error) => write!(f, "{error}"),
            Self::AdapterError(error) | Self::Other(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Debug)]
pub enum AgentEvent {
    TextDetal(String),
    MessageAdded(message::Message),
    ToolStarted { call_id: String, name: String },
    ToolFinished { call_id: String, name: String },
    Finished(RunResult),
}

impl fmt::Display for AgentEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TextDetal(text) => write!(f, "{text}"),
            Self::MessageAdded(message) => write!(f, "{message}"),
            Self::ToolStarted { call_id, name } => {
                let _ = write!(f, "🔧 Tool Started [{call_id}]: {name}");
                Ok(())
            }
            Self::ToolFinished { call_id, name } => {
                let _ = write!(f, "✅ Tool Finished [{call_id}]: {name}");
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
}

#[bon::bon]
impl Agent {
    #[builder]
    pub fn new(
        model_config: adapter::ModelConfig,
        #[builder(default, into)] system_prompt: String,
        #[builder(default)] mut messages: Vec<message::Message>,
        #[builder(default = tool::ToolManager::new())] tools: tool::ToolManager,
    ) -> Self {
        if !system_prompt.trim().is_empty() {
            messages.push(message::Message::System {
                content: system_prompt,
            });
        }

        Self {
            model_config,
            messages,
            tools,
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
    ) -> Pin<Box<dyn futures::Stream<Item = Result<AgentEvent, AgentError>> + 'a>> {
        Box::pin(async_stream::try_stream! {
        let start_index = self.messages.len();

        let user_message = message::Message::User {
            content: task.into(),
        };
        self.messages.push(user_message.clone());
        yield AgentEvent::MessageAdded(user_message);

        let client = reqwest::Client::new();

        loop {
            let model_request = ModelRequest {
                messages: &self.messages,
                tools: &self.tools.definitions(),
            };
            let response = adapter::invoke(&client, &self.model_config, model_request).await.map_err(|error| AgentError::AdapterError(error))?;
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
                });
                break;
            }
        }
        })
    }
}
