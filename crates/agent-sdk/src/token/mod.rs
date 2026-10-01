//! Token 记账：压缩切点预算的前置。
//!
//! 依据 `docs/compaction.md` 第四节。核心约束是**够用，不追求绝对精确**：
//!
//! - `input_tokens` 是整个请求的真实总量，包含 system、tool schemas、role 包装与
//!   协议特殊 token，**无法精确分解到每条 message**。要精确分摊就得复刻 chat template，
//!   代价过高。而压缩选切点只需要**相对大小准确**——知道"砍到哪里能落进预算"就够了。
//! - 因此这里只做 L1（CJK 感知的启发式估算），并用 L2（真实 `input_tokens`）校准。
//! - L3 是 [`TokenCounter`]：留给未来的 `/count_tokens` 接口或本地词表，**本轮不实现**。
//!
//! 关键不变量：**估算函数一律返回"未校准"的原始值**，`factor` 只在聚合时统一回乘
//! （[`scaled`]）。否则校准会自我抵消——若 `count_message` 内部已经乘过 `factor`，
//! 那 `ratio = real / est` 恒等于 `1`，EMA 永远推不动 `factor`（见 [`HeuristicCounter::calibrate`]）。

use crate::message;
use std::sync::atomic::{AtomicU64, Ordering};

/// 单条消息的包装开销（role 字段、协议分隔符等）。
const ROLE_OVERHEAD: u64 = 4;

/// 每个 tool call 的包装开销（`id` / `type` / `function` 等固定字段）。
const TOOL_CALL_OVERHEAD: u64 = 8;

/// `Tool` 消息的包装开销（`tool_call_id` 之外的固定字段）。
const TOOL_MESSAGE_OVERHEAD: u64 = 4;

/// 估算一条文本占多少 token（未校准）。
///
/// 系数按字符类别分档（`docs/compaction.md` 4.3.1）：
///
/// | 类别 | 系数 | 依据 |
/// | --- | --- | --- |
/// | CJK | 1.0 | 汉字在 byte-level BPE 下 fertility ≈ 1.00x（arXiv:2106.00400 / 2608.26449） |
/// | ASCII | 0.25 | 英文 ≈ 1.2 tokens/word，平均词长 ≈ 4.7 字符（arXiv:2605.24718） |
/// | 其他 | 0.5 | 失败 BPE merge 会碎片化成单字节 token（arXiv:2607.24276），偏保守 |
///
/// CJK 取 1.0 是**宁可高估**（早触发压缩，落在安全侧），真实值可能在 0.5~1.0，
/// 由 [`HeuristicCounter::calibrate`] 拉回。
pub fn count_text(text: &str) -> u64 {
    let mut total = 0.0_f64;
    for ch in text.chars() {
        total += char_weight(ch);
    }
    total.ceil() as u64
}

/// 单个字符的 token 权重。分档依据见 [`count_text`]。
fn char_weight(ch: char) -> f64 {
    let code = ch as u32;
    if is_cjk(code) {
        1.0
    } else if code <= 0x7F {
        0.25
    } else {
        0.5
    }
}

/// 是否是 CJK 区段的字符（汉字、假名、谚文、CJK 标点）。
///
/// 记账口径：CJK 标点也按 1.0 计费——它确实占请求体积。
fn is_cjk(code: u32) -> bool {
    is_cjk_word_char(code) || matches!(code, 0x3000..=0x303F) // CJK 标点
}

/// 是否是 CJK **表意**字符（汉字、假名、谚文，不含标点）。
///
/// `pub(crate)`：`recall::tokenize` 复用同一份 Unicode 范围做切词信号，
/// 但它把标点当分隔符——分类必须只有一处定义，两个用途各自组合，
/// 避免"改了一处忘了另一处"的漂移。
pub(crate) fn is_cjk_word_char(code: u32) -> bool {
    matches!(
        code,
        0x4E00..=0x9FFF     // CJK 统一表意文字
        | 0x3040..=0x30FF   // 平假名 / 片假名
        | 0xAC00..=0xD7AF   // 谚文音节
    )
}

/// 估算单条消息占多少 token（未校准）。
///
/// 只统计**真正会被序列化进请求**的字段：`reasoning_content` 也在
/// `encode_messages` 里被写出，所以必须计入，否则含推理的助手消息会被系统性低估。
pub fn count_message(message: &message::Message) -> u64 {
    match message {
        message::Message::System { content }
        | message::Message::User { content }
        | message::Message::ContextSummary { content } => count_text(content) + ROLE_OVERHEAD,
        message::Message::Assistant {
            content,
            reasoning_content,
            tool_calls,
        } => {
            let mut total = ROLE_OVERHEAD;
            if let Some(content) = content {
                total += count_text(content);
            }
            if let Some(reasoning) = reasoning_content {
                total += count_text(reasoning);
            }
            for call in tool_calls {
                total += count_text(&call.name) + count_text(&call.arguments) + TOOL_CALL_OVERHEAD;
            }
            total
        }
        message::Message::Tool {
            tool_call_id,
            content,
        } => {
            let mut total = count_text(tool_call_id) + TOOL_MESSAGE_OVERHEAD;
            if let Some(content) = content {
                total += count_text(content);
            }
            total
        }
    }
}

/// 估算一批消息（未校准）。
pub fn count_messages(messages: &[message::Message]) -> u64 {
    messages.iter().map(count_message).sum()
}

/// 估算 tool schema 的固定开销（未校准）。
///
/// 每个工具的 JSON Schema 会随每次请求发送，属于"常驻开销"，不随对话增长，
/// 但计算切点预算时必须先把它扣掉，否则会高估可用于对话的窗口。
pub fn count_tools(tools: &[&crate::tool::ToolDefinition]) -> u64 {
    tools
        .iter()
        .map(|tool| {
            count_text(&tool.name)
                + count_text(&tool.description)
                + count_text(&tool.parameters.to_string())
                + ROLE_OVERHEAD
        })
        .sum()
}

/// 把原始估算按校准系数回乘。
///
/// `factor` 在**聚合时统一回乘**，而不是在 [`count_message`] 内部逐条乘。
/// 因为系数一致，累加序不变，切点结果稳定；也让 [`HeuristicCounter::calibrate`]
/// 能观测到未污染的原始估计。
pub fn scaled(raw: u64, factor: f64) -> u64 {
    (raw as f64 * factor).round() as u64
}

/// L3：精确 token 计数接口。
///
/// **本轮刻意不实现**（`docs/compaction.md` 第八节）。留这个 trait 是为了将来接
/// 供应商的 `/count_tokens` 或本地词表时，不必改动调用方。
/// 现在唯一的实现是启发式的 [`HeuristicCounter`]。
pub trait TokenCounter: Send + Sync {
    /// 单条消息的原始估算（未校准）。
    fn count_message(&self, message: &message::Message) -> u64;

    /// 一批消息的原始估算（未校准）。
    fn count_messages(&self, messages: &[message::Message]) -> u64 {
        messages.iter().map(|m| self.count_message(m)).sum()
    }
}

/// L1 启发式计数器 + L2 校准状态。
///
/// `factor` 用 EMA 平滑，初值 `1.0`：
///
/// ```text
/// ratio  = real / est
/// factor = 0.7 * factor + 0.3 * ratio
/// ```
///
/// 平滑是必要的：单次请求的 `input_tokens` 会受缓存、协议包装等噪声影响，
/// 直接替换 `factor` 会让切点在相邻两轮之间跳变。
#[derive(Debug)]
pub struct HeuristicCounter {
    /// 校准系数，按 1000 倍定点存储，避免为共享状态引入锁。
    factor_milli: AtomicU64,
}

impl Default for HeuristicCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicCounter {
    /// EMA 平滑权重：旧值占 0.7，新观测占 0.3。
    const EMA_OLD: f64 = 0.7;
    const EMA_NEW: f64 = 0.3;

    pub fn new() -> Self {
        Self {
            factor_milli: AtomicU64::new(1000),
        }
    }

    /// 当前校准系数。
    pub fn factor(&self) -> f64 {
        self.factor_milli.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// 用一次真实响应校准 `factor`。
    ///
    /// `estimated` 必须是**同一请求**的原始估计总量，且**不含** `factor`
    /// （即 [`scaled`] 之前的值）。这正是把 `factor` 排除在 [`count_message`]
    /// 之外的原因：否则 `ratio` 恒为 1，`factor` 永远无法收敛。
    ///
    /// 返回校准后的 `factor`。`estimated == 0` 或 `real == 0` 时视为无效观测，
    /// 保持不变——**不能把"测不到"当成"估算为 0"**（与 `Usage` 的 `Option` 语义一致）。
    pub fn calibrate(&self, real: u64, estimated: u64) -> f64 {
        if estimated == 0 || real == 0 {
            return self.factor();
        }
        let ratio = real as f64 / estimated as f64;
        let next = Self::EMA_OLD * self.factor() + Self::EMA_NEW * ratio;
        // 负值不可能，但定点转换会把负数变成 0，这里显式挡住非有限值。
        if !next.is_finite() || next <= 0.0 {
            return self.factor();
        }
        let milli = (next * 1000.0).round().max(1.0) as u64;
        self.factor_milli.store(milli, Ordering::Relaxed);
        milli as f64 / 1000.0
    }

    /// 单条消息的**校准后**估算。切点计算用这个。
    pub fn estimate_message(&self, message: &message::Message) -> u64 {
        scaled(count_message(message), self.factor())
    }

    /// 一批消息的**校准后**估算。
    ///
    /// 先累加原始值再回乘，而不是逐条回乘后相加：避免每条 `round()` 的
    /// 舍入误差累积，也让结果与累加序无关。
    pub fn estimate_messages(&self, messages: &[message::Message]) -> u64 {
        scaled(count_messages(messages), self.factor())
    }
}

impl TokenCounter for HeuristicCounter {
    fn count_message(&self, message: &message::Message) -> u64 {
        count_message(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, ToolCall};

    /// CJK 一个字算 1 token：这是"宁可高估"的安全侧假设。
    #[test]
    fn cjk_counts_one_per_char() {
        assert_eq!(count_text("你好世界"), 4);
        // 标点也落在 CJK 区段内。
        assert_eq!(count_text("你好，世界"), 5);
    }

    /// ASCII 按 0.25/字符，且**整体向上取整**：空串为 0，1 字符也要算 1。
    #[test]
    fn ascii_counts_quarter_per_char() {
        assert_eq!(count_text(""), 0);
        assert_eq!(count_text("abcd"), 1);
        // 5 个字符 → 1.25 → ceil → 2
        assert_eq!(count_text("abcde"), 2);
        assert_eq!(count_text("hello world"), 3); // 11 * 0.25 = 2.75 → 3
    }

    /// 其他 Unicode 取 0.5/字符（emoji 等 BPE 碎片化区段）。
    #[test]
    fn other_unicode_counts_half_per_char() {
        assert_eq!(count_text("🚀🚀"), 1); // 2 * 0.5 = 1.0
        assert_eq!(count_text("🚀🚀🚀"), 2); // 3 * 0.5 = 1.5 → 2
    }

    /// 混排按各类别累加，不按整体比例估算。
    #[test]
    fn mixed_script_accumulates_per_category() {
        // "hi" = 0.5, "中" = 1.0 → 1.5 → 2
        assert_eq!(count_text("hi中"), 2);
    }

    /// 助手消息必须计入 `reasoning_content`——它真的会被序列化进请求。
    /// 漏掉它会让含推理的消息被系统性低估。
    #[test]
    fn assistant_reasoning_is_counted() {
        let plain = Message::Assistant {
            content: Some("答案".into()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let with_reasoning = Message::Assistant {
            content: Some("答案".into()),
            reasoning_content: Some("我先想想".into()),
            tool_calls: vec![],
        };
        assert!(
            count_message(&with_reasoning) > count_message(&plain),
            "reasoning_content 必须计入估算"
        );
    }

    /// tool call 的 name + arguments 都要计入，且每个 call 带固定包装开销。
    #[test]
    fn tool_call_arguments_are_counted() {
        let no_call = Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![],
        };
        let one_call = Message::Assistant {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            }],
        };
        // 一个 call 的净增量必然超过它自身的固定包装开销。
        assert!(count_message(&one_call) >= count_message(&no_call) + TOOL_CALL_OVERHEAD);
    }

    /// 每条消息都有固定的 role 包装开销：短消息也不会估成 0。
    #[test]
    fn every_message_has_role_overhead() {
        let empty_user = Message::User {
            content: String::new(),
        };
        assert_eq!(count_message(&empty_user), ROLE_OVERHEAD);
    }

    /// `Tool` 消息的 `tool_call_id` 是真实开销，不能被忽略。
    #[test]
    fn tool_message_counts_its_id() {
        let tool = Message::Tool {
            tool_call_id: "call_abcdefgh".into(),
            content: Some("ok".into()),
        };
        assert!(count_message(&tool) > TOOL_MESSAGE_OVERHEAD);
    }

    /// 校准必须真的能推动 `factor`——这是"估算排除 factor"这一设计的回归测试。
    /// 如果哪天有人把 `factor` 乘回 `count_message`，`ratio` 会恒为 1，此断言失败。
    #[test]
    fn calibrate_moves_factor_toward_observation() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.factor(), 1.0);

        // 真实值是估算的 2 倍 → factor 应上升，但被 EMA 压到 1.3。
        let factor = counter.calibrate(2000, 1000);
        assert!(
            (factor - 1.3).abs() < 1e-9,
            "EMA 结果应为 1.3，实际 {factor}"
        );

        // 再来一次同样观测，继续向 2.0 靠拢但不过冲。
        let factor = counter.calibrate(2000, 1000);
        assert!(factor > 1.3 && factor < 2.0, "应继续上升且不过冲: {factor}");
    }

    /// 无效观测（估算为 0 / 真实为 0）不能污染 `factor`。
    #[test]
    fn calibrate_ignores_invalid_observations() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.calibrate(100, 0), 1.0);
        assert_eq!(counter.calibrate(0, 100), 1.0);
        assert_eq!(counter.factor(), 1.0);
    }

    /// 校准后的估算 = 原始估算 * factor。
    #[test]
    fn estimate_applies_factor() {
        let counter = HeuristicCounter::new();
        let messages = vec![Message::User {
            content: "你好世界".into(),
        }];
        let raw = count_messages(&messages);
        assert_eq!(counter.estimate_messages(&messages), raw);

        // 真实值翻倍后，同样的消息应估得更高。
        counter.calibrate(raw * 2, raw);
        assert!(counter.estimate_messages(&messages) > raw);
    }

    /// 先累加再回乘，避免逐条舍入误差：空消息集恒为 0。
    #[test]
    fn empty_message_set_is_zero() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.estimate_messages(&[]), 0);
    }
}
