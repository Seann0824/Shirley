//! 沙盒执行结果。
//!
//! 这里刻意保留 stderr / exit_code / 降级信息：
//! 模型需要看到失败细节才能自我纠正（见 docs/security.md 6.1），
//! 审计需要知道"当时隔离是否真的生效了"。

// 引入 serde 的 Serialize 宏：让这个结构体可以被序列化成 JSON。
// 用途：审计日志要把它写进文件、或者通过网络发给别的地方。
use serde::Serialize;

// derive 三个能力：
// - Debug：能用 {:?} 打印出来，方便调试
// - Clone：能复制一份
// - Serialize：能转成 JSON
#[derive(Debug, Clone, Serialize)]
pub struct SandboxOutput {
    // 子进程的标准输出（正常打印的东西）。
    pub stdout: String,
    // 子进程的标准错误（报错信息）。
    pub stderr: String,

    /// 进程退出码。被信号杀死时为 None。
    // Option<i32>：要么是某个整数 Some(0)/Some(3)，要么是 None（比如被强杀没有退出码）。
    pub exit_code: Option<i32>,

    /// 是否因超时被外层强制终止。
    pub timed_out: bool, // true / false

    /// 实际生效的隔离强度标签（来自 backend.capabilities().label）。
    pub isolation: String,

    /// 运行时无法满足、被降级处理的约束说明。
    /// 例如 ProcessBackend 会填"无文件系统隔离 / 无网络隔离"。
    /// 上层可据此决定是否拒绝这次结果（高危场景下应拒绝）。
    pub degraded: Vec<String>, // 一串"我没做到的事"的说明文字
}

// 给 SandboxOutput 添加两个便捷方法。
impl SandboxOutput {
    /// 是否满足"完全隔离"——没有任何降级项。
    // &self：借用自己来读（不拿走所有权，只是看一眼）。
    // -> bool：返回 true/false。
    pub fn is_fully_isolated(&self) -> bool {
        // degraded 是空列表 → 没有降级项 → 完全隔离。
        // .is_empty() 返回 true 当列表为空。
        self.degraded.is_empty()
    }

    /// 命令是否成功退出。
    pub fn success(&self) -> bool {
        // 两个条件都满足才算成功：
        // 1. 退出码正好是 0（exit_code 是 Some(0)）
        // 2. 不是超时
        // && 是"并且"。== 是"等于"。
        self.exit_code == Some(0) && !self.timed_out // ! 是"非"，!true 就是 false
    }
}
