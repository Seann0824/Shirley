use std::{collections::HashMap, pin::Pin};

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

    pub async fn invoke(&self, input: ::serde_json::Value) -> Result<serde_json::Value, ToolError> {
        // 1. 根据输入找到工具
        // 2. 将参数传递进对应的工具invoke中，并将值返回
        let name = input
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "缺少字符串类型的 name".to_owned())?;

        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("工具不存在: {name}"))?;

        let arguments = input
            .get("arguments")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "缺少字符串类型的 arguments".to_owned())?;

        let arguments = serde_json::from_str(arguments)
            .map_err(|error| format!("arguments 不是合法 JSON: {error}"))?;

        // 交给工具处理自己的参数
        tool.invoke(arguments).await
    }
}
