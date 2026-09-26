use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::tool;
use futures::StreamExt;
use std::fmt;

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

pub type AgentError = String;

// impl RunResult {
//     pub fn final_text(&self) -> Option<&str> {
//         todo!()
//     }
// }

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
    pub async fn run<F>(&mut self, task: &str, mut on_event: F) -> Result<RunResult, AgentError>
    where
        F: FnMut(AgentEvent),
    {
        let start_index = self.messages.len();

        self.messages.push(message::Message::User {
            content: task.into(),
        });

        let client = reqwest::Client::new();

        loop {
            let model_request = ModelRequest {
                messages: &self.messages,
                tools: &self.tools.definitions(),
            };
            let response = adapter::invoke(&client, &self.model_config, model_request).await?;
            if let message::Message::Assistant {
                content: Some(content),
                ..
            } = &response.message
            {
                on_event(AgentEvent::TextDetal(content.clone()));
            }
            let tool_messages = match &response.message {
                message::Message::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                    let mut tasks = futures::stream::FuturesUnordered::new();

                    for call in tool_calls {
                        on_event(AgentEvent::ToolStarted {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                        });

                        let tools = &self.tools;

                        tasks.push(async move {
                            let content = match tools.invoke(call).await {
                                Ok(output) => output.to_string(),
                                Err(error) => format!("工具执行失败: {error}"),
                            };
                            (call, content)
                        });
                    }

                    let mut tool_messages = Vec::with_capacity(tool_calls.len());
                    while let Some((call, content)) = tasks.next().await {
                        on_event(AgentEvent::ToolFinished {
                            call_id: call.id.clone(),
                            name: call.name.clone(),
                        });

                        tool_messages.push(message::Message::Tool {
                            tool_call_id: call.id.clone(),
                            content: Some(content),
                        })
                    }

                    tool_messages
                }
                _ => vec![],
            };

            let is_finished = tool_messages.is_empty();
            self.messages.push(response.message);
            self.messages.extend(tool_messages);

            if is_finished {
                return Ok(RunResult {
                    messages: self.messages[start_index..].to_vec(),
                    stop_reason: StopReason::Completed,
                });
            }
        }
    }

    pub async fn run_stream<F>(&mut self, task: &str, on_event: F) -> Result<RunResult, AgentError>
    where
        F: FnMut(AgentEvent),
    {
        let result = RunResult {
            messages: vec![],
            stop_reason: StopReason::Completed,
        };

        Ok(result)
    }
}
