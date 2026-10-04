//! 进程沙盒。
//!
//! 设计意图（与 docs/security.md 一致）：
//!
//! 1. **边界在"能碰什么"，不在"命令字符串长什么样"。**
//!    所以 spec 用 program + args，而不是 `bash -c "..."`。
//! 2. **默认拒绝。** 网络默认断，环境变量默认不继承，写路径默认空。
//! 3. **可替换后端。** 隔离机制由 [`backend::SandboxBackend`] 提供，
//!    本地开发用 [`backend::ProcessBackend`]，生产换成 bwrap / sandbox-exec / microVM。
//! 4. **超时不可逃避。** 超时由 [`Sandbox`] 在最外层用 tokio 施加，
//!    后端即使想赖着不停也会被 drop + kill。
//! 5. **降级透明。** 后端做不到的约束必须写进 [`SandboxOutput::degraded`]，
//!    上层可据此拒绝结果，而不是被"看起来跑了"骗过去。
//!
//! 典型用法：
//!
//! ```no_run
//! use shirley_agent_sdk::sandbox::{Sandbox, SandboxSpec, backend::ProcessBackend};
//!
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let sandbox = Sandbox::new(ProcessBackend::default());
//! let spec = SandboxSpec::new("python3")
//!     .arg("-c")
//!     .arg("print('hi')")
//!     .workspace_root(".");
//! let out = sandbox.run(&spec).await?;
//! println!("{}", out.stdout);
//! # Ok(()) }
//! ```

// 声明三个子模块，各自对应一个文件。
pub mod backend; // backend/mod.rs
pub mod output;  // output.rs
pub mod spec;    // spec.rs

// 重新导出常用类型，让外面能少写一层路径。
// 比如写 sandbox::SandboxSpec 而不是 sandbox::spec::SandboxSpec。
pub use backend::{Capabilities, SandboxBackend, SandboxError};
pub use output::SandboxOutput;
pub use spec::{NetworkPolicy, SandboxSpec};

// 引入 Duration（时间段类型）。
use std::time::Duration;

/// 沙盒门面：持有一个后端，负责在最外层施加超时、组装结果。
///
/// `<B: SandboxBackend>` 是泛型参数：Sandbox 能装任意一种后端。
/// 这样换后端时，Sandbox 这层代码一个字都不用改。
pub struct Sandbox<B: SandboxBackend> {
    // 内部持有的后端，真正干活的。
    backend: B,
    /// 覆盖 spec 里的 timeout；None 表示用 spec 自己的。
    hard_timeout: Option<Duration>,
}

// `impl<B: SandboxBackend> Sandbox<B>`：给"任意后端的 Sandbox"添加方法。
impl<B: SandboxBackend> Sandbox<B> {
    // 构造：传入一个后端，得到一个 Sandbox。
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            hard_timeout: None, // 默认没有硬超时上限
        }
    }

    /// 设置一个不可被 spec 放宽的硬超时上限。
    // mut self + 返回 self：链式风格。
    pub fn with_hard_timeout(mut self, timeout: Duration) -> Self {
        self.hard_timeout = Some(timeout);
        self
    }

    // 把后端的能力透传出去，方便上层查询。
    pub fn capabilities(&self) -> Capabilities {
        self.backend.capabilities()
    }

    /// 执行一次。
    ///
    /// 超时在这里统一施加：即便后端内部也有超时，这里是最后一道保险。
    pub async fn run(&self, spec: &SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        // 决定这次实际用多长的超时。
        // match 根据 hard_timeout 有没有值，分两种处理。
        let effective = match self.hard_timeout {
            // 有硬上限：取"spec 要的"和"硬上限"里更小的那个。
            // .min(...) 返回两者中较小的值。
            Some(hard) => spec.timeout.min(hard),
            // 没硬上限：就用 spec 自己指定的。
            None => spec.timeout,
        };

        // 复制一份 spec，把超时改成上面算出的 effective。
        // clone() 复制；这样不改动调用方传进来的原始 spec。
        let mut effective_spec = spec.clone();
        effective_spec.timeout = effective;

        // 关键：用 tokio 的超时把后端执行包起来。
        // tokio::time::timeout(时长, future) 会在超时后中断 future。
        match tokio::time::timeout(effective, self.backend.execute(&effective_spec)).await {
            // 后端在超时前完成了，直接返回它的结果。
            Ok(result) => result,
            // 超时了（Err(_) 里是超时信息，我们不用它）。
            Err(_) => {
                // 超时：后端 future 被 drop，配合 backend 内的 kill_on_drop 完成清理。
                // 这里手工拼一个"超时结果"返回。
                Ok(SandboxOutput {
                    stdout: String::new(), // 没拿到输出
                    // 生成一句说明，比如 "命令执行超时（5s）"。
                    stderr: format!("command timed out after {}s", effective.as_secs()),
                    exit_code: None, // 被强杀，没有退出码
                    timed_out: true, // 标记超时
                    // 记录用的是哪个后端的标签。
                    isolation: self.backend.capabilities().label.to_owned(),
                    // 超时不算"隔离降级"，所以是空列表。
                    degraded: Vec::new(),
                })
            }
        }
    }
}
