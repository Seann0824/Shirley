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
#[derive(Debug)]
pub enum SandboxError {
    /// 后端在当前平台不可用（例如 Linux 上想用 sandbox-exec）。
    BackendUnavailable(String), // 括号里带一条说明文字
    /// 后端启动失败。
    Spawn(String),
    /// 后端不支持 spec 中要求的某项约束，且不允许降级。
    Unsupported(String),
    Io(std::io::Error), // 包一个底层 IO 错误
}

// `impl Display for X` 定义"X 怎么显示成给用户看的文字"。
// 这样就能用 {} 或 {} 格式化打印它。
impl std::fmt::Display for SandboxError {
    // fmt 是格式化器；-> fmt::Result 是"格式化是否成功"。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // match self 是"根据 self 是哪个变体，分别处理"。
        match self {
            Self::BackendUnavailable(m) => write!(f, "[沙盒不可用]: {m}"),
            Self::Spawn(m) => write!(f, "[沙盒启动失败]: {m}"),
            Self::Unsupported(m) => write!(f, "[沙盒不支持该约束]: {m}"),
            Self::Io(e) => write!(f, "[沙盒 IO 错误]: {e}"),
            // write! 是"把格式化后的文字写进 f"。
            // {m} 表示把变量 m 填进花括号。
        }
    }
}

// 声明"SandboxError 是一个标准错误类型"。
// 空的花括号 {} 表示"用默认实现就行，不用额外写代码"。
impl std::error::Error for SandboxError {}

// `From` 定义"怎么从 A 转成 B"。
// 这里：怎么从 io::Error 转成 SandboxError。
// 有了它，代码里用 `?` 遇到 io::Error 时能自动转成 SandboxError。
impl From<std::io::Error> for SandboxError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e) // 把 io::Error 包进 Io 变体
    }
}
