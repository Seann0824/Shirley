//! 后端：把 [`SandboxSpec`] 落地为一次真实执行。
//!
//! 抽象出这一层，是为了让"隔离机制"可替换：
//! - [`ProcessBackend`]：无隔离，仅用于本地开发/测试，永远不要用于不可信输入。
//! - macOS 可接 `sandbox-exec`，Linux 可接 `bwrap` / `nsjail` / `runsc`。
//!
//! backend 的职责边界：**只负责执行与隔离**，不做权限决策（那是 policy 的事），
//! 也不负责超时（超时由 [`super::Sandbox`] 统一施加，确保后端无法逃避）。

// 引入要用到的类型。
use crate::sandbox::spec::SandboxSpec;   // 执行规格（要跑什么）
use crate::sandbox::output::SandboxOutput; // 执行结果

// `pub mod process;` 表示"这个目录下有个子模块 process"，
// 它会去找 backend/process.rs 文件。
pub mod process;

// 把 process 模块里的 ProcessBackend 重新导出，
// 这样外面能写 backend::ProcessBackend，而不用 backend::process::ProcessBackend。
pub use process::ProcessBackend;

/// 后端能力自描述。上层据此判断"我要求的隔离是否真的生效了"。
/// （注意：这是后端"静态声明"它能不能做，不是"这次执行"的实际结果。）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Copy：这个结构体小到可以直接复制（不用 move）
// PartialEq, Eq：能比较相等（能用 ==）
pub struct Capabilities {
    /// 能否提供文件系统隔离。
    pub filesystem_isolation: bool,
    /// 能否强制断网 / 代理出网。
    pub network_isolation: bool,
    /// 能否限制内存 / 进程数。
    pub resource_limits: bool,
    /// 隔离强度的人话标签，用于日志与审计。
    // &'static str：一个"活到程序结束"的字符串常量，比如 "none (bare process)"。
    pub label: &'static str,
}

/// 沙盒后端。
///
/// 注意这是同步 trait 描述能力，真正执行走 `execute`（async）。
/// 后端实现者必须保证：即使 `SandboxSpec` 里的限制无法实现，
/// 也要在 [`SandboxOutput`] 中如实上报，而不是静默忽略。
///
/// `trait` 是"接口/契约"：谁实现它，就必须提供下面这些方法。
/// `: Send + Sync` 表示"这个后端能在多线程间安全地共享"。
pub trait SandboxBackend: Send + Sync {
    // 返回这个后端的能力（能不能做文件系统隔离、网络隔离等）。
    // &self：读自己。
    fn capabilities(&self) -> Capabilities;

    /// 执行一次。返回 Ok 表示"进程跑完并拿到了输出"，不代表命令成功——
    /// 命令失败通过 [`SandboxOutput::exit_code`] 表达。
    /// 返回 Err 仅表示"沙盒本身无法执行"（如 backend 不可用）。
    ///
    /// `impl Future<...>` 是"异步函数"的返回类型（Rust 里 async fn 的底层形态）。
    /// `+ Send` 表示这个 Future 能跨线程传递。
    fn execute(
        &self,
        spec: &SandboxSpec, // 借用一份规格来读，不拿走
    ) -> impl std::future::Future<Output = Result<SandboxOutput, SandboxError>> + Send;
    // Result<A, B>：要么成功 Ok(A)，要么失败 Err(B)。

    /// 给这个后端一个短名字，用于审计日志。
    fn name(&self) -> &'static str;
}

/// 沙盒本身出错时的错误类型。`enum` = 多选一。
///
/// 展示格式统一为 `[前缀]: 详情`，实现走 thiserror 派生——
/// 与 `AdapterError` / `ToolError` / `WorkspaceError` 一致。
/// 前缀留在变体旁：`BackendUnavailable` 与 `Unsupported` 同属
/// `ErrorKind::Unsupported`，但前者是"这个平台没有"，后者是"这项约束不答应"，
/// 排障时要分得清。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// 后端在当前平台不可用（例如 Linux 上想用 sandbox-exec）。
    #[error("[沙盒不可用]: {0}")]
    BackendUnavailable(String),

    /// 后端启动失败。
    #[error("[沙盒启动失败]: {0}")]
    Spawn(String),

    /// 后端不支持 spec 中要求的某项约束，且不允许降级。
    #[error("[沙盒不支持该约束]: {0}")]
    Unsupported(String),

    /// 底层 IO 错误。`#[from]` 提供 `io::Error -> SandboxError` 的自动转换，
    /// 让调用点可以继续用 `?`；`#[source]` 保留错误链，便于向上追溯。
    #[error("[沙盒 IO 错误]: {0}")]
    Io(#[from] #[source] std::io::Error),
}

// 接入 SDK 统一错误契约：让上层能用同一个接口判断可重试性。
impl crate::error::SdkError for SandboxError {
    fn kind(&self) -> crate::error::ErrorKind {
        use crate::error::ErrorKind;
        match self {
            // 后端在当前平台不可用 / 不支持该约束：换实现或换配置，重试无用。
            Self::BackendUnavailable(_) | Self::Unsupported(_) => ErrorKind::Unsupported,
            // 启动失败与 IO 错误可能是瞬时的（fork 失败、资源紧张）。
            Self::Spawn(_) | Self::Io(_) => ErrorKind::Internal,
        }
    }
}
