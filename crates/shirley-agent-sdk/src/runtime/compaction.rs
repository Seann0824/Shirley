use crate::message;
use crate::token;

/// The budget basis for the retained tail: 20% of the context window (`docs/compaction.md` 4.1).
pub(super) const RETAIN_RATIO: f64 = 0.20;

/// The threshold that triggers compaction: when this turn's (input + output) reaches
/// `context_window * 0.80`, compact before the next request (`docs/compaction.md` 4.1).
pub(super) const DEFAULT_TRIGGER_RATIO: f64 = 0.80;

/// The compaction-policy config: turns "when to compact, how much to keep, what the summary
/// looks like" from hardcoded into tunable.
///
/// The defaults are the SDK's current built-in policy ([`CompressionConfig::default`]). The
/// application layer can override as needed — e.g. lower the trigger threshold (compact
/// earlier, save tokens) or replace the summary template (change language / focus).
/// `compression_instruction` (the domain-relevant trade-offs) is still passed separately on
/// `Agent`; this config only covers **policy parameters**, not business trade-offs.
#[derive(Debug, Clone, PartialEq)]
pub struct CompressionConfig {
    /// Trigger threshold (fraction of the context window, 0 < r <= 1). Default `0.80`.
    ///
    /// When this turn's usage `(input_tokens + output_tokens)` reaches
    /// `context_window * trigger_ratio`, compact before the next request.
    pub trigger_ratio: f64,
    /// The budget basis for the retained tail (fraction of the context window, 0 < r < 1).
    /// Default `0.20`.
    ///
    /// The tail kept after compaction (the recent conversation) is estimated against a token
    /// budget of `context_window * retain_ratio`.
    pub retain_ratio: f64,
    /// The summary output template, **appended** after the caller's `compression_instruction`.
    /// Default [`COMPACTION_TEMPLATE`] (structured XML + no speculation about "next steps").
    pub template: String,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            trigger_ratio: DEFAULT_TRIGGER_RATIO,
            retain_ratio: RETAIN_RATIO,
            template: COMPACTION_TEMPLATE.to_string(),
        }
    }
}

/// The output template and hard rules for the compaction summary (`docs/compaction.md` 5.3).
///
/// When [`super::agent::Agent::compress_context`] builds the compaction request, it **appends**
/// this after the caller-supplied `compression_instruction`, guaranteeing the summary is always
/// a structured XML block that doesn't fabricate content out of bounds.
///
/// The reason for putting "structure" in the SDK rather than the caller: which tags exist and
/// what is forbidden are the compaction protocol's contract and shouldn't be re-written by every
/// caller; the caller only owns the domain-relevant trade-offs (e.g. a coding agent cares which
/// files / commands are worth keeping). The template **deliberately does not ask for a "next
/// step"** — that would tempt the model to invent a plan the user never issued
/// (`docs/compaction.md` 2.2 root cause B).
pub(super) const COMPACTION_TEMPLATE: &str = include_str!("../../prompts/compaction_template.md");

/// The three segments produced by a compaction cut.
///
/// The three segments **do not overlap** and together cover exactly `messages[head..]` (minus
/// the background prefix). Use [`CompactParts::rebuild`] to reconstruct — don't concatenate
/// yourself, the order matters.
///
/// **On extracting `current_task`**: only when the task would fall into the to-compress region
/// (`task_extracted`) is it pulled out of `to_compress` and re-injected at rebuild. When the task
/// is already in the retained segment, **leave it alone**, otherwise moving it before `remain`
/// would scramble the order (`U_task` jumping before `U_prev`, two users back to back).
#[derive(Debug, Clone)]
pub struct CompactParts {
    /// Fixed background-prefix length (`messages[..head]`). The system prompt is **not copied
    /// from here** — it is held by `Agent` and regenerated on rebuild, see [`CompactParts::rebuild`].
    pub head: usize,

    /// The to-compress segment: handed to the summarizer, compacted into one `ContextSummary`.
    pub to_compress: Vec<message::Message>,

    /// The retained segment: kept in the request as-is. If the task is in it, its position is unchanged.
    pub remain: Vec<message::Message>,

    /// The user's latest question (always provided, for the summarizer as `<current_goal>`).
    pub current_task: Option<message::Message>,

    /// Whether the task falls in the to-compress region and needs extracting then re-injecting.
    pub task_extracted: bool,
}

impl CompactParts {
    /// Rebuild the message list in the correct order, **directly returning a `ContextSummary`**
    /// as the head.
    ///
    /// ```text
    /// [ContextSummary] + (task_extracted ? current_task : []) + remain
    /// ```
    ///
    /// **Does not keep the leading `System`**: the system prompt is not a compaction product; it
    /// is held by `Agent` and regenerated and prepended on every rebuild (see
    /// `Agent::system_message`). So this neither receives nor copies any `System` from the
    /// original messages.
    ///
    /// The output keeps only the new summary; the caller must first hand the old summary to the
    /// summarizer to inherit prior conclusions, and must not implement "keep only one summary"
    /// as "don't read the old summary".
    ///
    /// - When the task isn't extracted, the order is just `[summary] + remain`, matching the
    ///   original order;
    /// - When the task is extracted it is inserted before `remain`. `plan_cut` already guarantees
    ///   `remain` doesn't start with an orphan `Tool`, so "the task + its following
    ///   assistant/tool chain" stays a complete pair.
    pub fn rebuild(&self, summary: impl Into<String>) -> Vec<message::Message> {
        // The summary is constructed here directly as a `ContextSummary`; the system prompt is
        // no longer copied from the original messages.
        let mut out = Vec::with_capacity(self.remain.len() + 2);
        out.push(message::Message::ContextSummary {
            content: summary.into(),
        });
        if let (true, Some(task)) = (self.task_extracted, &self.current_task) {
            out.push(task.clone());
        }
        out.extend(self.remain.iter().cloned());
        out
    }
}

/// A compaction cut plan.
///
/// Computed by [`plan_cut`], it describes "which segment to keep, which to compress, and what
/// this turn's task is".
#[derive(Debug, Clone)]
pub struct CutPlan {
    /// Fixed background-prefix length: `messages[..head]` is never compressed.
    ///
    /// Includes the leading consecutive `System` (the system prompt) and the `ContextSummary`
    /// immediately after it (the previous turn's summary). The latter is **replaced** at rebuild
    /// rather than stacked, so it is likewise not counted against the budget.
    pub head: usize,

    /// The retained segment's start: `messages[cut..]` is kept as-is, `messages[head..cut]` is compressed.
    pub cut: usize,

    /// The user's latest question (the last `User` in `messages`).
    ///
    /// Under the normal layout it is the first message of the retained segment (the cut sits right
    /// before it), so it stays in the request as-is and is also explicitly fed to the summarizer
    /// as `<current_goal>` — exactly the fix for the "AI forgets the user's task after compaction"
    /// failure: the task must not exist only inside the summary.
    pub current_task: Option<message::Message>,
}

impl CutPlan {
    /// The range that needs compressing (replaced with a summary).
    #[must_use]
    pub fn compacted_range(&self) -> std::ops::Range<usize> {
        self.head..self.cut
    }

    /// Split the messages into three segments: `to_compress` / `remain` / `current_task`.
    ///
    /// **Extraction rule**: extract only when the task would fall into the to-compress region
    /// (`cut > task_index`); then it is removed from `to_compress` and re-injected at rebuild,
    /// avoiding being summarized away. When the task is already in the retained segment, leave it
    /// alone — otherwise moving it before `remain` would scramble the order.
    #[must_use]
    pub fn split(&self, messages: &[message::Message]) -> CompactParts {
        let task_index = messages
            .iter()
            .rposition(|msg| matches!(msg, message::Message::User { .. }));

        // Only a task inside the to-compress region [head, cut) needs extracting.
        let task_extracted = task_index.is_some_and(|i| i >= self.head && i < self.cut);

        let collect = |range: std::ops::Range<usize>| {
            range
                .filter(|&i| !(task_extracted && Some(i) == task_index))
                .map(|i| messages[i].clone())
                .collect::<Vec<_>>()
        };

        CompactParts {
            head: self.head,
            to_compress: collect(self.compacted_range()),
            remain: collect(self.cut..messages.len()),
            current_task: self.current_task.clone(),
            task_extracted,
        }
    }
}

/// The fixed background-prefix length.
///
/// After the leading consecutive `System`, if a `ContextSummary` immediately follows, it counts as
/// background too: it is "the previous round's compaction result" and is replaced by the new
/// summary at rebuild, so it shouldn't consume the 20% budget a second time.
fn background_len(messages: &[message::Message]) -> usize {
    let mut len = messages
        .iter()
        .take_while(|msg| matches!(msg, message::Message::System { .. }))
        .count();
    if matches!(
        messages.get(len),
        Some(message::Message::ContextSummary { .. })
    ) {
        len += 1;
    }
    len
}

/// Compute the compaction cut point (`docs/compaction.md` 4.5).
///
/// Accumulate estimated tokens from the tail backwards until adding one more would exceed
/// `budget`, giving the retained segment's start. Two corrections, **correctness over budget**:
///
/// 1. **Keep at least one**: the last message unconditionally enters the retained segment, even if
///    it alone exceeds the budget.
/// 2. **tool pairing**: the first message of the retained segment must not be a `Tool` result, or
///    an orphan `tool_call` appears and the API rejects the request with a 400. Roll back to the
///    `Assistant{tool_calls}` that produced it; consecutive `Tool` results get swallowed into the
///    retained segment together, so the pairing stays complete.
///
/// **No extra constraint of "the cut must not cross the last `User`".** The real layout is
/// `system, [history], USER(this turn's task), assistant(tool_calls), tool, tool, ...`, and the cut
/// lands **before** this turn's task — the task itself belongs to the retained segment (see
/// [`CutPlan::current_task`]), and the tool chain after it is what's compressible. Forcing the cut
/// before the last `User` would push it back to `head` when a single user message triggers a long
/// chain of large tool outputs, making compaction useless — and that is exactly the case that most
/// needs compaction.
pub fn plan_cut(
    messages: &[message::Message],
    budget: u64,
    counter: &token::HeuristicCounter,
) -> Option<CutPlan> {
    let head = background_len(messages);
    if head >= messages.len() {
        return None;
    }

    let mut acc = 0u64;
    let mut cut = messages.len();
    for i in (head..messages.len()).rev() {
        let cost = counter.estimate_message(&messages[i]);
        // `acc > 0` keeps the first message unconditionally, implementing correction 1.
        if acc > 0 && acc.saturating_add(cost) > budget {
            break;
        }
        acc = acc.saturating_add(cost);
        cut = i;
    }

    // Correction 2: the retained segment must not start with a Tool result.
    while cut > head && matches!(messages.get(cut), Some(message::Message::Tool { .. })) {
        cut -= 1;
    }

    if cut <= head {
        return None;
    }

    let current_task = messages
        .iter()
        .rev()
        .find(|msg| matches!(msg, message::Message::User { .. }))
        .cloned();

    Some(CutPlan {
        head,
        cut,
        current_task,
    })
}

#[cfg(test)]
mod template_tests {
    use super::COMPACTION_TEMPLATE;

    /// `docs/compaction.md` 5.3: the summary must be these XML blocks, none missing.
    #[test]
    fn template_has_all_required_blocks() {
        for tag in [
            "current_goal",
            "hard_constraints",
            "decisions",
            "progress",
            "open_questions",
            "compacted_range",
        ] {
            assert!(
                COMPACTION_TEMPLATE.contains(&format!("<{tag}>"))
                    && COMPACTION_TEMPLATE.contains(&format!("</{tag}>")),
                "missing tag {tag}",
            );
        }
    }

    /// Root cause B: the template must never **ask** for "next steps" (there is no `next_step`
    /// block); it can only explicitly forbid inventing them.
    #[test]
    fn template_never_asks_for_next_steps() {
        assert!(!COMPACTION_TEMPLATE.contains("<next_step"));
        assert!(COMPACTION_TEMPLATE.contains("do not speculate"));
        assert!(COMPACTION_TEMPLATE.contains("do not invent"));
    }

    /// `<compacted_range>` is a recall hook and must note "the exact details are not in the summary".
    #[test]
    fn template_marks_compacted_range_as_lossy() {
        assert!(COMPACTION_TEMPLATE.contains("exact details"));
        assert!(COMPACTION_TEMPLATE.contains("re-read"));
    }
}
