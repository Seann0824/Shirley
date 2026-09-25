use std::{collections::HashMap, pin::Pin};

use crate::message;

pub type ToolError = String;

pub type ToolName = String;

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
            return Err(format!("{tool_name} 工具重复注册"));
        }

        // 2. 不重复，将工具添加到 self.tools
        self.tools.insert(tool_name.into(), Box::new(tool));

        Ok(())
    }

    pub async fn invoke(&self, input: &message::ToolCall) -> Result<serde_json::Value, ToolError> {
        let tool = self
            .tools
            .get(&input.name)
            .ok_or_else(|| format!("工具不存在: {}", &input.name))?;

        let arguments = serde_json::from_str(&input.arguments)
            .map_err(|error| format!("arguments 不是合法 JSON: {error}"))?;

        // 交给工具处理自己的参数
        tool.invoke(arguments).await
    }
}
