//! 最朴素的后端：直接起进程，**没有任何隔离**。
//!
//! 它的存在有两个意义：
//! 1. 让整条链路（spec → execute → output）先跑通、可测试。
//! 2. 作为"降级基线"——它会把所有无法满足的约束如实写进 `degraded`，
//!    上层看到 degraded 就知道"这次跑的是裸进程，别当真"。
//!
//! 绝不要用它执行不可信输入。

// Stdio：控制子进程的标准输入/输出/错误怎么接。
use std::process::Stdio;

// tokio 的异步 Command：用来启动子进程（异步 = 不阻塞等它跑完）。
use tokio::process::Command;

// 引入需要实现的接口和要用的类型。
use crate::sandbox::backend::{Capabilities, SandboxBackend, SandboxError};
use crate::sandbox::output::SandboxOutput;
use crate::sandbox::spec::{NetworkPolicy, SandboxSpec};

// 定义一个空结构体（没有字段）——它只是个"标记"，不需要存任何数据。
// derive(Default) 让 ProcessBackend::default() 能创建它。
#[derive(Default)]
pub struct ProcessBackend;

// `impl SandboxBackend for ProcessBackend` = "让 ProcessBackend 满足 SandboxBackend 这个契约"。
// 大括号里必须把 trait 要求的每个方法都实现出来。
impl SandboxBackend for ProcessBackend {
    // 实现 capabilities：报告自己"啥也隔离不了"。
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            filesystem_isolation: false, // 不能隔离文件系统
            network_isolation: false,    // 不能隔离网络
            resource_limits: false,      // 不能限制资源
            label: "none (bare process)", // 标签：裸进程
        }
    }

    // 实现 name：给个短名字。
    fn name(&self) -> &'static str {
        "process"
    }

    // 实现 execute：真正干活的地方。
    // async 表示这是异步函数；&self 借用自己；spec 借用规格来读。
    async fn execute(&self, spec: &SandboxSpec) -> Result<SandboxOutput, SandboxError> {
        // 防御性检查：如果没给程序名，直接报错返回。
        // is_empty() 判断字符串是否为空。
        if spec.program.is_empty() {
            // return Err(...) 表示"提前失败退出"。
            // "program 为空".into() 把 &str 转成 String。
            return Err(SandboxError::Spawn("program is empty".into()));
        }

        // 创建一个命令，要执行 spec.program。
        // &spec.program 是借用（不把 program 从 spec 里拿走）。
        let mut cmd = Command::new(&spec.program);

        // 把 spec.args 里的每个参数逐个传给命令。
        // &spec.args 借用整个参数列表。
        cmd.args(&spec.args);

        // kill_on_drop(true)：如果这个 cmd 对象被丢弃了，就杀掉子进程。
        // 这是超时兜底的关键——外层超时会把 future drop 掉，子进程随之被杀。
        cmd.kill_on_drop(true);

        // 环境变量默认不继承：先清空。
        // 避免宿主的 API key 等敏感变量泄漏给子进程。
        cmd.env_clear();

        // 再逐个注入 spec 里明确要求的环境变量。
        // `for (k, v) in &spec.env` 是"遍历列表里的每个键值对"。
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        // 如果调用方没自己设 PATH，就给一个最小的 PATH。
        // 否则程序会找不到自己的依赖（比如 python3 找不到）。
        // .iter() 遍历；.any(...) 判断"有没有任何一项满足条件"。
        if !spec.env.iter().any(|(k, _)| k == "PATH") {
            // 读宿主当前 PATH；读不到就用空字符串兜底。
            cmd.env("PATH", std::env::var("PATH").unwrap_or_default());
        }

        // 设置工作目录：优先用 spec.cwd，没有就用 spec.workspace_root。
        // .as_ref() 把 Option<PathBuf> 变成 Option<&PathBuf>（借用，不拿走）。
        // .or(...) 表示"前者有就用前者，没有就用后者"。
        if let Some(cwd) = spec.cwd.as_ref().or(spec.workspace_root.as_ref()) {
            cmd.current_dir(cwd); // 设置子进程的工作目录
        }

        // 标准输入接到 null（子进程读不到任何输入）。
        cmd.stdin(Stdio::null());
        // 标准输出/错误接成管道（这样我们能在父进程里读到它们的输出）。
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // 启动并等待子进程跑完，拿到输出。
        let output = cmd
            .output() // 异步等待，返回 Result<Output, io::Error>
            .await    // .await 表示"等这个异步操作完成"
            // 如果启动失败，把 io::Error 转成我们的 SandboxError::Spawn。
            // `?` 表示"如果是 Err 就提前返回"。
            .map_err(|e| SandboxError::Spawn(format!("failed to spawn {}: {e}", spec.program)))?;

        // 到这里进程跑完了。下面开始"如实上报降级项"。

        // 先建一个列表，放入两条"永远做不到"的事。
        // vec![...] 是创建列表的字面量写法。
        // .to_owned() 把 &str 变成 String。
        let mut degraded = vec![
            "no filesystem isolation: the process can access the host filesystem".to_owned(),
            "no resource limits: no memory/process-count caps applied".to_owned(),
        ];

        // 根据 spec 要求的网络策略，再补一条对应的降级说明。
        match spec.network {
            // 如果调用方要求断网，但我们做不到，就如实说。
            NetworkPolicy::Disabled => {
                degraded.push("no network isolation: NetworkPolicy::Disabled is not enforced".to_owned())
            }
            // 如果要求走代理，同样做不到，如实说。
            // `{ .. }` 表示"不关心里面 addr 的值，忽略它"。
            NetworkPolicy::Proxy { .. } => {
                degraded.push("no network isolation: proxy egress is not enforced".to_owned())
            }
        }

        // 如果调用方设了工作区根目录，说明它以为有文件系统约束——
        // 但我们只是把 cwd 设成那儿，并没有真的锁住文件系统，得说明。
        if spec.workspace_root.is_some() {
            degraded.push("workspace is only a cwd; not enforced by the filesystem".to_owned());
        }

        // 组装并返回结果。Ok(...) 表示成功。
        Ok(SandboxOutput {
            // 把子进程的 stdout 字节转成字符串。
            // from_utf8_lossy：遇到非法字节就替换成占位符，不报错。
            // .into_owned()：把借用的结果变成自己拥有的 String。
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            // .code() 取退出码；被信号杀死时是 None。
            exit_code: output.status.code(),
            // 这里永远填 false——因为超时是外层 Sandbox 统一判定的，
            // 进程能跑到这里就说明没超时。
            timed_out: false,
            // 用自己 capabilities 的标签。
            isolation: self.capabilities().label.to_owned(),
            // 把上面收集的降级项放进去。
            degraded,
        })
    }
}
