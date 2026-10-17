//! Token accounting: the prerequisite for compaction cut-point budgets.
//!
//! Based on section 4 of `docs/compaction.md`. The core constraint is **good enough, not
//! absolutely precise**:
//!
//! - `input_tokens` is the true total for the whole request, including system, tool
//!   schemas, role wrappers, and protocol special tokens, and **cannot be decomposed
//!   precisely per message**. Precise attribution would require replicating the chat
//!   template, which is too costly. And compaction only needs cut points whose **relative
//!   size is accurate** — knowing "where to cut so it fits the budget" is enough.
//! - So here we only do L1 (a CJK-aware heuristic estimate) and calibrate it with L2 (the
//!   real `input_tokens`).
//! - L3 is [`TokenCounter`]: left for a future `/count_tokens` endpoint or a local
//!   vocabulary; **not implemented this round**.
//!
//! Key invariant: **estimation functions always return the "uncalibrated" raw value**;
//! `factor` is only multiplied back once during aggregation ([`scaled`]). Otherwise
//! calibration would cancel itself out — if `count_message` already multiplied by `factor`,
//! then `ratio = real / est` would always be `1` and the EMA could never move `factor` (see
//! [`HeuristicCounter::calibrate`]).

use crate::message;
use std::sync::atomic::{AtomicU64, Ordering};

/// The wrapper overhead of a single message (role field, protocol delimiters, etc.).
const ROLE_OVERHEAD: u64 = 4;

/// The wrapper overhead of each tool call (`id` / `type` / `function` fixed fields, etc.).
const TOOL_CALL_OVERHEAD: u64 = 8;

/// The wrapper overhead of a `Tool` message (fixed fields besides `tool_call_id`).
const TOOL_MESSAGE_OVERHEAD: u64 = 4;

/// Estimate how many tokens a piece of text occupies (uncalibrated).
///
/// Coefficients are tiered by character class (`docs/compaction.md` 4.3.1):
///
/// | class | coefficient | basis |
/// | --- | --- | --- |
/// | CJK | 1.0 | Han characters under byte-level BPE have fertility ≈ 1.00x (arXiv:2106.00400 / 2608.26449) |
/// | ASCII | 0.25 | English ≈ 1.2 tokens/word, average word length ≈ 4.7 chars (arXiv:2605.24718) |
/// | other | 0.5 | Failed BPE merges fragment into single-byte tokens (arXiv:2607.24276); conservative |
///
/// CJK at 1.0 is deliberately **biased high** (compaction triggers early, on the safe
/// side); the true value may be 0.5~1.0 and is pulled back by
/// [`HeuristicCounter::calibrate`].
pub fn count_text(text: &str) -> u64 {
    let mut total = 0.0_f64;
    for ch in text.chars() {
        total += char_weight(ch);
    }
    total.ceil() as u64
}

/// The token weight of a single character. Tiering rationale in [`count_text`].
pub(crate) fn char_weight(ch: char) -> f64 {
    let code = ch as u32;
    if is_cjk(code) {
        1.0
    } else if code <= 0x7F {
        0.25
    } else {
        0.5
    }
}

/// Whether a character is in a CJK range (Han, kana, Hangul, CJK punctuation).
///
/// Accounting policy: CJK punctuation is billed at 1.0 too — it really does occupy request
/// volume.
fn is_cjk(code: u32) -> bool {
    is_cjk_word_char(code) || matches!(code, 0x3000..=0x303F) // CJK punctuation
}

/// Whether a character is a CJK **ideographic** character (Han, kana, Hangul; no
/// punctuation).
///
/// `pub(crate)`: keeps the CJK single-char test available for counting policy, but it
/// treats punctuation as a separator — the classification must be defined in exactly one
/// place, with the two uses composing it, to avoid drift from "changed one and forgot the
/// other".
pub(crate) fn is_cjk_word_char(code: u32) -> bool {
    matches!(
        code,
        0x4E00..=0x9FFF     // CJK Unified Ideographs
        | 0x3040..=0x30FF   // Hiragana / Katakana
        | 0xAC00..=0xD7AF   // Hangul syllables
    )
}

/// Tokens billed for one image reference.
///
/// Images are billed by the provider by resolution/detail, not by characters. We
/// cannot know the exact number without decoding the image, so use a flat,
/// deliberately generous constant so the compaction budget does not under-count
/// an image-heavy turn. It is a heuristic, like everything else in this module.
const IMAGE_TOKENS: u64 = 384;

/// Estimate the tokens a [`message::Content`]'s parts occupy (uncalibrated), **without**
/// the role wrapper. Shared by `User` (adds [`ROLE_OVERHEAD`]) and `Tool` (adds
/// [`TOOL_MESSAGE_OVERHEAD`] instead), so a text result counts exactly as it did before.
pub fn count_content_parts(content: &message::Content) -> u64 {
    content
        .as_parts()
        .iter()
        .map(|part| match part {
            message::ContentPart::Text { text } => count_text(text),
            message::ContentPart::Image(_) => IMAGE_TOKENS,
        })
        .sum::<u64>()
}

/// Estimate the tokens a [`message::Content`] occupies (uncalibrated).
pub fn count_content(content: &message::Content) -> u64 {
    count_content_parts(content) + ROLE_OVERHEAD
}

/// Estimate how many tokens a single message occupies (uncalibrated).
///
/// Only counts fields that are **actually serialized into the request**: `reasoning_content`
/// is also written out in `encode_messages`, so it must be counted, otherwise assistant
/// messages with reasoning would be systematically underestimated.
pub fn count_message(message: &message::Message) -> u64 {
    match message {
        message::Message::System { content }
        | message::Message::ContextSummary { content } => count_text(content) + ROLE_OVERHEAD,
        message::Message::User { content } => count_content(content),
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
                total += count_content_parts(content);
            }
            total
        }
    }
}

/// Estimate a batch of messages (uncalibrated).
pub fn count_messages(messages: &[message::Message]) -> u64 {
    messages.iter().map(count_message).sum()
}

/// Estimate the fixed overhead of tool schemas (uncalibrated).
///
/// Each tool's JSON Schema is sent with every request, which is "standing overhead" that
/// doesn't grow with the conversation, but the cut-point budget must subtract it first,
/// otherwise the window available for conversation would be overestimated.
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

/// Multiply a raw estimate back by the calibration factor.
///
/// `factor` is multiplied back **once during aggregation**, not per-message inside
/// [`count_message`]. Because the factor is uniform, the accumulation order is unchanged
/// and cut-point results are stable; it also lets [`HeuristicCounter::calibrate`] observe
/// the uncontaminated raw estimate.
pub fn scaled(raw: u64, factor: f64) -> u64 {
    (raw as f64 * factor).round() as u64
}

/// L3: the precise token-counting interface.
///
/// **Deliberately not implemented this round** (`docs/compaction.md` section 8). This trait
/// exists so that hooking up a provider's `/count_tokens` or a local vocabulary later
/// requires no change to callers.
/// The only implementation today is the heuristic [`HeuristicCounter`].
pub trait TokenCounter: Send + Sync {
    /// The raw estimate of a single message (uncalibrated).
    fn count_message(&self, message: &message::Message) -> u64;

    /// The raw estimate of a batch of messages (uncalibrated).
    fn count_messages(&self, messages: &[message::Message]) -> u64 {
        messages.iter().map(|m| self.count_message(m)).sum()
    }
}

/// The L1 heuristic counter + L2 calibration state.
///
/// `factor` is smoothed with an EMA, starting at `1.0`:
///
/// ```text
/// ratio  = real / est
/// factor = 0.7 * factor + 0.3 * ratio
/// ```
///
/// Smoothing is necessary: a single request's `input_tokens` is noisy (cache, protocol
/// wrappers), and replacing `factor` outright would make cut points jump between adjacent
/// turns.
#[derive(Debug)]
pub struct HeuristicCounter {
    /// Calibration factor, stored fixed-point at 1000x, to avoid a lock for shared state.
    factor_milli: AtomicU64,
}

impl Default for HeuristicCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl HeuristicCounter {
    /// EMA smoothing weights: the old value weighs 0.7, the new observation 0.3.
    const EMA_OLD: f64 = 0.7;
    const EMA_NEW: f64 = 0.3;

    pub fn new() -> Self {
        Self {
            factor_milli: AtomicU64::new(1000),
        }
    }

    /// The current calibration factor.
    pub fn factor(&self) -> f64 {
        self.factor_milli.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// Calibrate `factor` with one real response.
    ///
    /// `estimated` must be the raw estimated total of the **same request**, and must
    /// **exclude** `factor` (i.e. the value before [`scaled`]). That is exactly why `factor`
    /// is excluded from [`count_message`]: otherwise `ratio` would always be 1 and `factor`
    /// could never converge.
    ///
    /// Returns the calibrated `factor`. When `estimated == 0` or `real == 0` it is an
    /// invalid observation and the factor stays put — **don't treat "can't measure" as
    /// "estimated 0"** (consistent with the `Option` semantics of `Usage`).
    pub fn calibrate(&self, real: u64, estimated: u64) -> f64 {
        if estimated == 0 || real == 0 {
            return self.factor();
        }
        let ratio = real as f64 / estimated as f64;
        let next = Self::EMA_OLD * self.factor() + Self::EMA_NEW * ratio;
        // Negative values are impossible, but the fixed-point conversion would turn a
        // negative into 0; explicitly block non-finite values here.
        if !next.is_finite() || next <= 0.0 {
            return self.factor();
        }
        let milli = (next * 1000.0).round().max(1.0) as u64;
        self.factor_milli.store(milli, Ordering::Relaxed);
        milli as f64 / 1000.0
    }

    /// The **calibrated** estimate of a single message. Cut-point computation uses this.
    pub fn estimate_message(&self, message: &message::Message) -> u64 {
        scaled(count_message(message), self.factor())
    }

    /// The **calibrated** estimate of a batch of messages.
    ///
    /// Sum the raw values first, then multiply back, rather than multiplying back
    /// per-message and summing: this avoids accumulating each `round()`'s error and makes
    /// the result independent of accumulation order.
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

    /// One CJK character counts as 1 token: the "bias high" safe-side assumption.
    #[test]
    fn cjk_counts_one_per_char() {
        assert_eq!(count_text("你好世界"), 4);
        // Punctuation also falls in the CJK range.
        assert_eq!(count_text("你好，世界"), 5);
    }

    /// ASCII counts 0.25/char, and the total is **rounded up**: empty is 0, one char is 1.
    #[test]
    fn ascii_counts_quarter_per_char() {
        assert_eq!(count_text(""), 0);
        assert_eq!(count_text("abcd"), 1);
        // 5 chars → 1.25 → ceil → 2
        assert_eq!(count_text("abcde"), 2);
        assert_eq!(count_text("hello world"), 3); // 11 * 0.25 = 2.75 → 3
    }

    /// Other Unicode counts 0.5/char (emoji and other BPE-fragmenting ranges).
    #[test]
    fn other_unicode_counts_half_per_char() {
        assert_eq!(count_text("🚀🚀"), 1); // 2 * 0.5 = 1.0
        assert_eq!(count_text("🚀🚀🚀"), 2); // 3 * 0.5 = 1.5 → 2
    }

    /// Mixed scripts accumulate per class, not by an overall ratio.
    #[test]
    fn mixed_script_accumulates_per_category() {
        // "hi" = 0.5, "中" = 1.0 → 1.5 → 2
        assert_eq!(count_text("hi中"), 2);
    }

    /// Assistant messages must count `reasoning_content` — it really is serialized into the
    /// request. Missing it would systematically underestimate messages with reasoning.
    #[test]
    fn assistant_reasoning_is_counted() {
        let plain = Message::Assistant {
            content: Some("answer".into()),
            reasoning_content: None,
            tool_calls: vec![],
        };
        let with_reasoning = Message::Assistant {
            content: Some("answer".into()),
            reasoning_content: Some("let me think".into()),
            tool_calls: vec![],
        };
        assert!(
            count_message(&with_reasoning) > count_message(&plain),
            "reasoning_content must be counted in the estimate"
        );
    }

    /// A tool call's name + arguments must both be counted, and each call has a fixed
    /// wrapper overhead.
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
        // The net increase of one call must exceed its own fixed wrapper overhead.
        assert!(count_message(&one_call) >= count_message(&no_call) + TOOL_CALL_OVERHEAD);
    }

    /// Every message has a fixed role wrapper overhead: a short message is never estimated
    /// as 0.
    #[test]
    fn every_message_has_role_overhead() {
        let empty_user = Message::User {
            content: String::new().into(),
        };
        assert_eq!(count_message(&empty_user), ROLE_OVERHEAD);
    }

    /// A `Tool` message's `tool_call_id` is real overhead and must not be ignored.
    #[test]
    fn tool_message_counts_its_id() {
        let tool = Message::Tool {
            tool_call_id: "call_abcdefgh".into(),
            content: Some("ok".into()),
        };
        assert!(count_message(&tool) > TOOL_MESSAGE_OVERHEAD);
    }

    /// Calibration must actually be able to move `factor` — this is the regression test for
    /// the "estimates exclude factor" design. If someone multiplies `factor` back into
    /// `count_message`, `ratio` would always be 1 and this assertion would fail.
    #[test]
    fn calibrate_moves_factor_toward_observation() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.factor(), 1.0);

        // The real value is 2x the estimate → factor should rise, but the EMA caps it at 1.3.
        let factor = counter.calibrate(2000, 1000);
        assert!(
            (factor - 1.3).abs() < 1e-9,
            "the EMA result should be 1.3, got {factor}"
        );

        // Repeat the same observation; it keeps approaching 2.0 without overshooting.
        let factor = counter.calibrate(2000, 1000);
        assert!(factor > 1.3 && factor < 2.0, "should keep rising without overshooting: {factor}");
    }

    /// Invalid observations (estimate 0 / real 0) must not pollute `factor`.
    #[test]
    fn calibrate_ignores_invalid_observations() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.calibrate(100, 0), 1.0);
        assert_eq!(counter.calibrate(0, 100), 1.0);
        assert_eq!(counter.factor(), 1.0);
    }

    /// The calibrated estimate = raw estimate * factor.
    #[test]
    fn estimate_applies_factor() {
        let counter = HeuristicCounter::new();
        let messages = vec![Message::User {
            content: "你好世界".into(),
        }];
        let raw = count_messages(&messages);
        assert_eq!(counter.estimate_messages(&messages), raw);

        // After the real value doubles, the same messages should estimate higher.
        counter.calibrate(raw * 2, raw);
        assert!(counter.estimate_messages(&messages) > raw);
    }

    /// Sum first, multiply back later, avoiding per-message rounding error: an empty message
    /// set is always 0.
    #[test]
    fn empty_message_set_is_zero() {
        let counter = HeuristicCounter::new();
        assert_eq!(counter.estimate_messages(&[]), 0);
    }
}
