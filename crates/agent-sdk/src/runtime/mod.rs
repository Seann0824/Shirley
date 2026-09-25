use crate::ToolManager;
use crate::adapter;
use crate::adapter::ModelRequest;
use crate::message;
use crate::tool;

pub struct RunResult {
    // 本次调用新增的消息，按照发送顺序排序
    pub messages: Vec<message::Message>,

    // 停止的原因
    pub stop_reason: StopReason,
}

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

enum AgentEvent {
    TextDetal(String),
    MessageAdded(message::Message),
    ToolStarted {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolFinished {
        call_id: String,
        result: serde_json::Value,
    },
    Finished(RunResult),
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
        }
    }
    pub async fn run(&mut self, task: &str) -> Result<RunResult, AgentError> {
        // 1. client、mode_request、
        self.messages.push(message::Message::User {
            content: task.into(),
        });
        let client = reqwest::Client::new();
        let model_request = ModelRequest {
            messages: &self.messages,
            tools: &self.tools.definitions(),
        };
        // todo: 后面在做循环调用，根据 finish_reason 判断是否需要循环调用
        let response = adapter::invoke(&client, &self.model_config, model_request).await?;

        let result = RunResult {
            messages: vec![response.message],
            // 这里调用结束的原因，我理解一下，是否应该和实际的请求对其呢？
            stop_reason: StopReason::Completed,
        };
        Ok(result)
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
