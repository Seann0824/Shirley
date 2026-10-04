use crate::message;
use crate::token;

/// 保留尾部的预算口径：上下文窗口的 20%（`docs/compaction.md` 4.1）。
pub(super) const RETAIN_RATIO: f64 = 0.20;

/// 压缩摘要的输出模板与硬性规则（`docs/compaction.md` 5.3）。
///
/// [`super::agent::Agent::compress_context`] 在构造压缩请求时，会把它**追加**到调用方
/// 传入的 `compression_instruction` 之后，保证摘要始终是结构化的 XML 块，且不越界编造内容。
///
/// 之所以把"结构"放进 SDK 而不是调用方：有哪些标签、禁止什么，是压缩协议的契约，
/// 不该由每个调用方各写一遍；调用方只负责领域相关的取舍（例如 coding agent 关心
/// 哪些文件 / 命令值得保留）。模板里**刻意不要求"下一步"**——那会诱导模型编造
/// 用户从未下达的计划（`docs/compaction.md` 2.2 根因 B）。
pub(super) const COMPACTION_TEMPLATE: &str = r#"严格按下面的格式输出，不要输出 XML 之外的任何解释。

只提炼对话中已经发生的内容：不得推演、不得补充用户未提出的计划、不得编造"下一步"。
忠实复述用户明确下达的指令与约束，逐字保留关键措辞，不要改写用户意图。

<current_goal>忠实复述用户当前真正下达的任务，不得添加、不得删改</current_goal>
<hard_constraints>用户明确提出的约束（禁止的操作、必须遵守的约定、边界条件）</hard_constraints>
<decisions>已经做出的关键决策，以及做出该决策的原因</decisions>
<progress>已完成的工作与当前状态</progress>
<open_questions>尚未解决的问题或悬而未决的疑问</open_questions>
<compacted_range>此前对话已被压缩，精确细节不在本摘要中。需要时：文件内容、命令结果可重新读取或重新执行；用户曾说过的话、约定与决策请调用 recall 工具检索</compacted_range>"#;

/// 压缩切点算出的三段内容。
///
/// 三段**互不重叠**，且合起来恰好覆盖 `messages[head..]` 的全部消息（背景前缀除外）。
/// 重建请用 [`CompactParts::rebuild`]，不要自己拼接——顺序有讲究。
///
/// **关于 `current_task` 的摘出规则**：只有当任务会落进待压缩区时（`task_extracted`），
/// 才把它从 `to_compress` 摘出、在重建时重新注入。任务本来就在保留段里时**不动它**，
/// 否则把它提到 `remain` 前面会打乱顺序（`U_task` 跑到 `U_prev` 之前，两个 user 连排）。
#[derive(Debug, Clone)]
pub struct CompactParts {
    /// 固定背景前缀长度（`messages[..head]`）。系统提示词**不从这里复制**——
    /// 它由 `Agent` 自己持有、重建时重新生成，见 [`CompactParts::rebuild`]。
    pub head: usize,

    /// 待压缩段：交给摘要器，压缩成一条 `ContextSummary`。
    pub to_compress: Vec<message::Message>,

    /// 保留段：原样留在请求里。任务若在其中，位置不变。
    pub remain: Vec<message::Message>,

    /// 用户最新提出的问题（始终提供，供摘要器作为 `<current_goal>`）。
    pub current_task: Option<message::Message>,

    /// 任务是否落在待压缩区、需要摘出后重新注入。
    pub task_extracted: bool,
}

impl CompactParts {
    /// 按正确顺序重建消息列表，**直接返回一条 `ContextSummary`** 作为开头。
    ///
    /// ```text
    /// [ContextSummary] + (task_extracted ? current_task : []) + remain
    /// ```
    ///
    /// **不保留开头的 `System`**：系统提示词不属于压缩产物，由 `Agent` 自己持有，
    /// 每次重建时重新生成一条再置顶（见 `Agent::system_message`）。
    /// 所以这里不接收、也不复制原消息里的任何 `System`。
    ///
    /// **丢弃上一轮的 `ContextSummary`**：它不会出现在输出里，避免"摘要叠摘要"
    /// —— `docs/compaction.md` 5.2 明令禁止，论文 caveat 4 也指出多次压缩只会更差。
    ///
    /// - 任务未被摘出时，顺序即 `[summary] + remain`，与原顺序一致；
    /// - 任务被摘出时插在 `remain` 之前。`plan_cut` 已保证 `remain`
    ///   不以孤儿 `Tool` 开头，因此"任务 + 其后的 assistant/tool 链"配对完整。
    pub fn rebuild(&self, summary: impl Into<String>) -> Vec<message::Message> {
        // 摘要直接由本方法构造为 `ContextSummary`，不再从原消息里复制系统提示词。
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

/// 压缩切点计划。
///
/// 由 [`plan_cut`] 计算，描述"保留哪一段、压哪一段、本轮任务是什么"。
#[derive(Debug, Clone)]
pub struct CutPlan {
    /// 固定背景前缀长度：`messages[..head]` 永不压缩。
    ///
    /// 包含开头连续的 `System`（系统提示词）与紧邻其后的 `ContextSummary`
    /// （上一轮的摘要）。后者在重建时会被**替换**而不是叠加，所以同样不计入预算。
    pub head: usize,

    /// 保留段起点：`messages[cut..]` 原样保留，`messages[head..cut]` 被压缩。
    pub cut: usize,

    /// 用户最新提出的问题（`messages` 中最后一条 `User`）。
    ///
    /// 正常布局下它就是保留段的第一条（切点紧邻其前），因此会原样留在请求里，
    /// 同时显式喂给摘要器作为 `<current_goal>`——这正是"压缩后 AI 忘记用户任务"
    /// 那个故障的修法：任务不能只存在于摘要里。
    pub current_task: Option<message::Message>,
}

impl CutPlan {
    /// 需要被压缩（换成摘要）的区间。
    pub fn compacted_range(&self) -> std::ops::Range<usize> {
        self.head..self.cut
    }

    /// 把消息切成三段：`to_compress` / `remain` / `current_task`。
    ///
    /// **摘出规则**：仅当任务会落进待压缩区（`cut > task_index`）时才摘出，
    /// 此时它会从 `to_compress` 中移除，重建时重新注入，避免被摘要掉。
    /// 任务本就在保留段时不动它——否则把它提到 `remain` 前面会打乱顺序。
    pub fn split(&self, messages: &[message::Message]) -> CompactParts {
        let task_index = messages
            .iter()
            .rposition(|msg| matches!(msg, message::Message::User { .. }));

        // 只有落在待压缩区 [head, cut) 内的任务才需要摘出。
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

/// 固定背景前缀长度。
///
/// 开头连续的 `System` 之后，若紧跟一条 `ContextSummary`，它也算背景：
/// 它是"上一轮的压缩结果"，重建时会被新摘要替换，不该再吃一遍 20% 预算。
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

/// 计算压缩切点（`docs/compaction.md` 4.5）。
///
/// 从尾部往前累加估算 token，直到再加就超出 `budget`，得到保留段起点。
/// 两处修正，**正确性优先于预算**：
///
/// 1. **至少保留一条**：最后一条消息无条件进入保留段，即使它单独就超预算。
/// 2. **tool 配对**：保留段的第一条不能是 `Tool` 结果，否则产生孤儿 `tool_call`，
///    请求会被 API 以 400 拒绝。回退到产出它的 `Assistant{tool_calls}`，
///    连续多条 `Tool` 结果会被一起吞进保留段，配对自然完整。
///
/// **不额外约束"切点不越过最后一条 `User`"。** 真实布局是
/// `system, [历史], USER(本轮任务), assistant(tool_calls), tool, tool, ...`，
/// 切点就落在本轮任务**之前**——任务本身属于保留段（见 [`CutPlan::current_task`]），
/// 它后面的 tool 链才是可压对象。若强行把切点卡在最后一条 `User` 之前，
/// 一条用户消息触发一长串大 tool 输出时切点会退到 `head`，压缩直接失效，
/// 而这恰恰是最需要压缩的情形。
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
        // `acc > 0` 让第一条无条件保留，实现修正 1。
        if acc > 0 && acc.saturating_add(cost) > budget {
            break;
        }
        acc = acc.saturating_add(cost);
        cut = i;
    }

    // 修正 2：保留段第一条不能是 Tool 结果。
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

    /// `docs/compaction.md` 5.3：摘要必须是这些 XML 块，缺一不可。
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
                "缺少标签 {tag}",
            );
        }
    }

    /// 根因 B：模板绝不能**要求**"下一步"（没有 next_step 块），
    /// 只能明确禁止编造它。
    #[test]
    fn template_never_asks_for_next_steps() {
        assert!(!COMPACTION_TEMPLATE.contains("<next_step"));
        assert!(COMPACTION_TEMPLATE.contains("不得推演"));
        assert!(COMPACTION_TEMPLATE.contains("不得编造"));
    }

    /// `<compacted_range>` 是召回钩子，必须提示"精确细节不在摘要里"。
    #[test]
    fn template_marks_compacted_range_as_lossy() {
        assert!(COMPACTION_TEMPLATE.contains("精确细节"));
        assert!(COMPACTION_TEMPLATE.contains("重新读取"));
    }
}
