use std::any::{Any, TypeId};
use std::sync::Arc;
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

/// 工具运行时上下文。
///
/// 工具在 ReAct 循环里是**并发执行**的（`runtime/agent.rs` 用 `FuturesUnordered`），
/// 所以上下文不可能是 `&mut Session`——多个工具同时跑、同时要可变借用同一份会话
/// 必然冲突。唯一可行的形态是**内部可变性句柄**（`Arc<Mutex<Session>>` 之类）。
///
/// 本类型是类型擦除的容器：应用把任意 `Send + Sync + 'static` 数据用
/// [`ToolContext::with`] 塞进去，工具用 [`ToolContext::get`] 取回。存的是 `Arc`，
/// clone 只增加引用计数，可以低成本地按调用分发。
///
/// ```
/// use shirley_agent_sdk::ToolContext;
/// use std::sync::Mutex;
///
/// let ctx = ToolContext::new().with(Mutex::new(42_u32));
/// let value = ctx.get::<Mutex<u32>>().expect("session not provided");
/// assert_eq!(*value.lock().unwrap(), 42);
/// ```
#[derive(Clone, Default)]
pub struct ToolContext {
    inner: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// 存入一份应用数据（链式写法）。同一类型重复存入时后写入者覆盖。
    pub fn with<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.insert(value);
        self
    }

    /// 就地存入一份应用数据。供 `Tool::on_register` 等持有 `&mut ToolContext` 的
    /// 场合使用（链式的 [`ToolContext::with`] 会消费 `self`，在这里用不了）。
    /// 同一类型重复存入时后写入者覆盖。
    pub fn insert<T: Any + Send + Sync>(&mut self, value: T) {
        Arc::make_mut(&mut self.inner).insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// 取出应用数据。类型不匹配或未注入时返回 `None`。
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.inner
            .get(&TypeId::of::<T>())
            .and_then(|value| value.clone().downcast::<T>().ok())
    }

    /// 移除一份应用数据，返回被移除的值（不存在或类型不符时 `None`）。
    ///
    /// 供工具的 `on_unregister`（= destroy）清理自己注入的上下文项使用。
    /// 内部是 `Arc<HashMap>` + [`Arc::make_mut`] 写时复制：若此刻有在途的工具
    /// 调用持有旧 clone，`make_mut` 会先复制一份再改，**在途调用仍看到旧数据**
    /// （安全，无悬垂），只是新调用不再看到被移除的项。
    ///
    /// 注意按 [`TypeId`] 索引：两个工具若注入**同一类型**，其中一个注销会把
    /// 另一个的数据一并清掉。约定每个工具用独立 newtype 承载自己的状态；
    /// 跨工具共享的句柄不要由单个工具在 `on_unregister` 里移除。
    pub fn remove<T: Any + Send + Sync>(&mut self) -> Option<Arc<T>> {
        Arc::make_mut(&mut self.inner)
            .remove(&TypeId::of::<T>())
            .and_then(|value| value.downcast::<T>().ok())
    }
}

pub struct ToolDefinition {
    pub name: String,

    pub description: String,

    pub parameters: serde_json::Value,
}

pub trait Tool: Send + Sync {
    // 获取名称、描述 和 参数Schema
    fn definition(&self) -> &ToolDefinition;

    // SDK 内部调用接受JSON, 通过识别到调用工具后，反序列化到对应的 Argument 类型
    // ctx 是调用级运行时上下文，见 [`ToolContext`]。
    fn invoke(&self, input: serde_json::Value, ctx: ToolContext) -> ToolFuture<'_>;

    /// 注册（= created）。[`ToolManager::register`] 在查重通过后、插入前调用一次。
    ///
    /// 工具在这里 (a) 把共享运行时依赖写进上下文 / 存进 `self`；(b) 自检配置，
    /// 缺失即返回 `Err` —— **注册失败，工具不入表**，不会留下半注册状态。
    ///
    /// 默认空实现：无状态工具零改动。同步（注册发生在构造期，不引入 async）。
    fn on_register(&mut self, _ctx: &mut ToolContext) -> Result<(), ToolError> {
        Ok(())
    }

    /// 注销（= destroy）。[`ToolManager::unregister`] 在把工具移出表**之后**调用一次。
    ///
    /// 工具在这里释放自己注入的上下文项（如 `ctx.remove::<T>()`）。
    ///
    /// 语义：**best-effort 清理**。无论返回 `Ok` 还是 `Err`，工具都已从表里移除，
    /// `Err` 只用于告知调用方"清理失败"，不会让工具复活。
    /// 默认空实现：无状态工具零改动。
    fn on_unregister(&mut self, _ctx: &mut ToolContext) -> Result<(), ToolError> {
        Ok(())
    }
}
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
    /// 工具运行时上下文。所有权在 manager（不再由应用层单独持有 / 传入）：
    /// 工具在 `on_register` 里把依赖写进来，runtime 调用时 clone 分发，
    /// `on_unregister` 里清理。见 `docs/tool-lifecycle.md`。
    context: ToolContext,
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
            context: ToolContext::new(),
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

    pub fn register(&mut self, mut tool: impl Tool + 'static) -> Result<(), ToolError> {
        // 1. 判断工具是否重复， 重复抛出错误
        let tool_name = tool.definition().name.clone();
        if self.tools.contains_key(&tool_name) {
            return Err(ToolError::RepetitionError(format!(
                "tool already registered: {tool_name}"
            )));
        }

        // 2. created：查重通过后、插入前跑注册钩子。失败即不入表——
        //    不留"已初始化一半"的工具（见 `docs/tool-lifecycle.md` 4.3）。
        tool.on_register(&mut self.context)?;

        // 3. 不重复且初始化成功，将工具添加到 self.tools
        self.tools.insert(tool_name, Box::new(tool));

        Ok(())
    }

    /// 移除一个已注册的工具（= destroy 的触发路径）。
    ///
    /// 顺序：**先移出表 → 再回调 `on_unregister`**。即使回调报错，工具也不会复活，
    /// 因此不会留下"已调用 destroy 但仍在表里"的错乱状态。名字未注册时返回
    /// [`ToolError::NotFoundError`]。
    pub fn unregister(&mut self, name: &str) -> Result<(), ToolError> {
        let mut tool = self
            .tools
            .remove(name)
            .ok_or_else(|| ToolError::NotFoundError(name.to_string()))?;

        tool.on_unregister(&mut self.context)
    }

    /// 工具运行时上下文的只读视图（runtime 调用前 clone 分发用）。
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// 工具运行时上下文的可变视图：应用在注册前后向其中注入跨工具共享的句柄
    /// （每个工具私有的状态更适合放进它的 `on_register`，见 `docs/tool-lifecycle.md` 4.4）。
    pub fn context_mut(&mut self) -> &mut ToolContext {
        &mut self.context
    }

    /// 调用一个工具。
    ///
    /// 返回 `ToolError`：调用方（runtime）如果需要 `AgentError`，
    /// 用 `?` 自动收敛即可，不必在这里提前包装。
    pub async fn invoke(
        &self,
        input: &message::ToolCall,
        ctx: ToolContext,
    ) -> Result<serde_json::Value, ToolError> {
        let tool = self.tools.get(&input.name).ok_or_else(|| {
            ToolError::NotFoundError(input.name.clone())
        })?;

        let arguments = serde_json::from_str(&input.arguments).map_err(|error| {
            ToolError::ArgumentsError(format!("arguments is not valid JSON: {error}"))
        })?;

        // 交给工具处理自己的参数
        tool.invoke(arguments, ctx).await
    }
}
