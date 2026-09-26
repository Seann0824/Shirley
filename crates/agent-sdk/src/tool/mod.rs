use core::fmt;
use std::{collections::HashMap, pin::Pin};

use serde::Serialize;

use crate::{Agent, AgentError, message};

// 直接把错误格式化成消息，message，比如
// [什么错误]: 失败原因， 感觉应该有个工具函数来做
#[derive(Debug, Serialize)]
pub enum ToolError {
    ExecutionError(String),
    RepetitionError(String),
    NotFoundError(String),
    ArgumentsError(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutionError(msg) => write!(f, "[执行错误]: {msg}"),
            Self::RepetitionError(msg) => write!(f, "[重复注册错误]: {msg}"),
            Self::NotFoundError(msg) => write!(f, "[工具不存在]: {msg}"),
            Self::ArgumentsError(msg) => write!(f, "[参数错误]: {msg}"),
        }
    }
}

pub type ToolName = String;

pub type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, AgentError>> + Send + 'a>>;

pub struct ToolDefinition {
    pub name: String,

    pub description: String,

    pub parameters: serde_json::Value,
}

pub trait Tool: Send + Sync {
    // 获取名称、描述 和 参数Schema
    fn definition(&self) -> &ToolDefinition;

    // SDK 内部调用接受JSON, 通过识别到调用工具后，反序列化到对应的 Argument 类型
    fn invoke(&self, input: serde_json::Value) -> ToolFuture<'_>;
}
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
}

impl ToolManager {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn definitions(&self) -> Vec<&ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| tool.definition())
            .collect::<Vec<&ToolDefinition>>();

        definitions.sort_by(|a, b| a.name.cmp(&b.name));
        definitions
    }

    pub fn register(&mut self, tool: impl Tool + 'static) -> Result<(), AgentError> {
        // 1. 判断工具是否重复， 重复抛出错误
        let tool_name = &tool.definition().name;
        if self.tools.contains_key(tool_name) {
            return Err(AgentError::ToolError(ToolError::RepetitionError(format!(
                "{tool_name} 工具重复注册"
            ))));
        }

        // 2. 不重复，将工具添加到 self.tools
        self.tools.insert(tool_name.into(), Box::new(tool));

        Ok(())
    }

    pub async fn invoke(&self, input: &message::ToolCall) -> Result<serde_json::Value, AgentError> {
        let tool = self.tools.get(&input.name).ok_or_else(|| {
            AgentError::ToolError(ToolError::NotFoundError(format!(
                "工具不存在: {}",
                &input.name
            )))
        })?;

        let arguments = serde_json::from_str(&input.arguments).map_err(|error| {
            AgentError::ToolError(ToolError::ArgumentsError(format!(
                "arguments 不是合法 JSON: {error}"
            )))
        })?;

        // 交给工具处理自己的参数
        tool.invoke(arguments).await
    }
}
