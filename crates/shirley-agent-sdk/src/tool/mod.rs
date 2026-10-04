use std::{collections::HashMap, pin::Pin};

use serde::Serialize;

use crate::error::{ErrorKind, SdkError};
use crate::message;

/// 工具层错误。
///
/// 展示格式统一为 `[前缀]: 详情`，与 `AdapterError` / `SandboxError` /
/// `WorkspaceError` 一致；实现机制统一走 thiserror 派生，不再手写 `Display`。
///
/// 前缀刻意留在变体旁，而不是从 [`ErrorKind`] 推导：`ErrorKind` 是粗粒度的
/// 重试决策轴，多个变体归到同一个 kind（如这里的 `NotFoundError` 与
/// `ArgumentsError` 都是 `BadRequest`），模型需要靠前缀区分"工具不存在"和
/// "参数写错了"——后者它自己能改参数修好。
#[derive(Debug, Serialize, thiserror::Error)]
pub enum ToolError {
    #[error("[execution error]: {0}")]
    ExecutionError(String),

    #[error("[duplicate tool]: {0}")]
    RepetitionError(String),

    #[error("[tool not found]: {0}")]
    NotFoundError(String),

    #[error("[invalid arguments]: {0}")]
    ArgumentsError(String),
}

impl SdkError for ToolError {
    fn kind(&self) -> ErrorKind {
        match self {
            // 工具内部跑失败：模型应该看到细节并换策略，但重试同一份参数没用。
            Self::ExecutionError(_) => ErrorKind::ToolFailure,
            // 工具不存在 / 参数不合法 / 重复注册：属于调用方（或模型）的请求有问题。
            Self::RepetitionError(_) | Self::NotFoundError(_) | Self::ArgumentsError(_) => {
                ErrorKind::BadRequest
            }
        }
    }
}

pub type ToolName = String;

/// 工具执行结果。
///
/// 错误类型是 [`ToolError`] 而不是 [`crate::AgentError`]：工具层只负责"我这次跑成没跑成"，
/// 把它归到哪一层、要不要重试，是 runtime 的事。
/// 这与沙盒后端的做法一致——`ProcessBackend::execute` 只返回 `SandboxError`。
pub type ToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, ToolError>> + Send + 'a>>;

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

impl Default for ToolManager {
    fn default() -> Self {
        Self::new()
    }
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

    pub fn register(&mut self, tool: impl Tool + 'static) -> Result<(), ToolError> {
        // 1. 判断工具是否重复， 重复抛出错误
        let tool_name = &tool.definition().name;
        if self.tools.contains_key(tool_name) {
            return Err(ToolError::RepetitionError(format!(
                "tool already registered: {tool_name}"
            )));
        }

        // 2. 不重复，将工具添加到 self.tools
        self.tools.insert(tool_name.into(), Box::new(tool));

        Ok(())
    }

    /// 调用一个工具。
    ///
    /// 返回 `ToolError`：调用方（runtime）如果需要 `AgentError`，
    /// 用 `?` 自动收敛即可，不必在这里提前包装。
    pub async fn invoke(&self, input: &message::ToolCall) -> Result<serde_json::Value, ToolError> {
        let tool = self.tools.get(&input.name).ok_or_else(|| {
            ToolError::NotFoundError(input.name.clone())
        })?;

        let arguments = serde_json::from_str(&input.arguments).map_err(|error| {
            ToolError::ArgumentsError(format!("arguments is not valid JSON: {error}"))
        })?;

        // 交给工具处理自己的参数
        tool.invoke(arguments).await
    }
}
