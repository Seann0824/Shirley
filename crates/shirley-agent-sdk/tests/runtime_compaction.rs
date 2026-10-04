//! 上下文压缩切点与重建的契约测试。
//!
//! 覆盖 `docs/compaction.md` 4.5 / 5.2 的核心不变量：
//! 切点算法（预算、tool 配对、至少保留一条）、三段切分、
//! 以及 `CompactParts::rebuild` 的顺序与"恰好一条摘要"约束。
//!
//! 原先内联在 `runtime/mod.rs` 的 `mod tests`，拆模块后移到集成测试，
//! 只依赖 `shirley_agent_sdk` 的公开 API。

use shirley_agent_sdk::{HeuristicCounter, Message, ToolCall, plan_cut};


fn system(text: &str) -> Message {
    Message::System {
        content: text.into(),
    }
}
fn user(text: &str) -> Message {
    Message::User {
        content: text.into(),
    }
}
fn assistant_calls(ids: &[&str]) -> Message {
    Message::Assistant {
        content: None,
        reasoning_content: None,
        thinking_signature: None,
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                id: (*id).into(),
                name: "bash".into(),
                arguments: "{\"command\":\"ls\"}".into(),
            })
            .collect(),
    }
}
fn tool(id: &str, content: &str) -> Message {
    Message::Tool {
        tool_call_id: id.into(),
        content: Some(content.into()),
    }
}

/// 每条消息约 4 字符中文 = 4 token + 4 包装 = 8 token 左右。
/// 用足够小的 budget 逼出"只保留尾部几条"的行为。
fn counter() -> HeuristicCounter {
    HeuristicCounter::new()
}

/// 全是背景（只有 system）时没有可压内容。
#[test]
fn no_compaction_when_only_background() {
    let messages = vec![system("你是 Shirley")];
    assert!(plan_cut(&messages, 1000, &counter()).is_none());
}

/// 全部能装进预算 → 不压缩。
#[test]
fn no_compaction_when_everything_fits() {
    let messages = vec![system("s"), user("你好"), user("世界")];
    let plan = plan_cut(&messages, 10_000, &counter());
    assert!(plan.is_none(), "全部装得下时不该有切点: {plan:?}");
}

/// 背景前缀永不被压缩：`head` 覆盖开头 system，且切点不会落进它。
#[test]
fn background_prefix_is_never_compacted() {
    let mut messages = vec![system("你是 Shirley")];
    for i in 0..40 {
        messages.push(user(&format!("第 {i} 条很长的用户消息，用来把预算撑爆")));
    }
    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    assert_eq!(plan.head, 1, "开头 system 必须是背景");
    assert!(plan.cut > plan.head, "切点必须在背景之后");
}

/// 上一轮的 `ContextSummary` 属于背景：它会被新摘要替换，不占保留预算。
#[test]
fn prior_summary_is_background() {
    let mut messages = vec![
        system("你是 Shirley"),
        Message::ContextSummary {
            content: "上一轮摘要".into(),
        },
    ];
    for i in 0..40 {
        messages.push(user(&format!("第 {i} 条很长的用户消息，用来把预算撑爆")));
    }
    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    assert_eq!(plan.head, 2, "system + 上轮摘要都算背景");
    assert!(plan.cut >= 2, "不能压掉背景");
}

/// 至少保留一条：即使最后一条单独就超预算，也要留在保留段里。
#[test]
fn always_keeps_at_least_one_message() {
    let messages = vec![
        system("s"),
        user("第一条"),
        user("超长的一条消息".repeat(50).as_str()),
    ];
    let plan = plan_cut(&messages, 1, &counter()).expect("应产生切点");
    assert!(
        plan.cut < messages.len(),
        "至少要保留最后一条，cut 必须小于 len"
    );
    // 最后一条（超预算的那条）必须在保留段内。
    assert_eq!(plan.cut, messages.len() - 1);
}
/// 核心修正：保留段第一条不能是 `Tool`，否则孤儿 tool_call 会被 API 400。
#[test]
fn never_cuts_between_assistant_tool_calls_and_tool_results() {
    let mut messages = vec![system("s"), user("帮我看看")];
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("call_{i}")]));
        messages.push(tool(&format!("call_{i}"), "历史工具输出内容，用来占预算"));
    }
    messages.push(user("这是本轮任务"));
    for i in 0..10 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "本轮工具输出，很长很长很长很长很长",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("应产生切点");
    assert!(
        !matches!(messages.get(plan.cut), Some(Message::Tool { .. })),
        "切点落在了 Tool 结果上，会产生孤儿 tool_call: cut={}",
        plan.cut
    );
    assert!(
        matches!(messages.get(plan.cut), Some(Message::Assistant { .. })),
        "应回退到 Assistant{{tool_calls}}: cut={}",
        plan.cut
    );
}

/// 连续多条 Tool 结果（一次并行调用）必须被整体保留，配对完整。
#[test]
fn keeps_consecutive_tool_results_together() {
    let mut messages = vec![system("s"), user("并行调用")];
    messages.push(assistant_calls(&["a", "b", "c"]));
    messages.push(tool("a", "结果 A 很长很长很长很长很长很长很长很长很长"));
    messages.push(tool("b", "结果 B 很长很长很长很长很长很长很长很长很长"));
    messages.push(tool("c", "结果 C 很长很长很长很长很长很长很长很长很长"));

    let plan = plan_cut(&messages, 20, &counter()).expect("应产生切点");
    assert!(
        matches!(messages.get(plan.cut), Some(Message::Assistant { .. })),
        "并行 tool 结果应整体保留: cut={}",
        plan.cut
    );
    let tool_count = messages[plan.cut..]
        .iter()
        .filter(|m| matches!(m, Message::Tool { .. }))
        .count();
    assert_eq!(tool_count, 3, "三条 tool 结果都要保留");
}

/// 回归：一条用户消息触发一长串大 tool 输出时，压缩**必须**仍然生效。
///
/// 这是"切点卡在最后一条 User 之前"会退化成 `None` 的故障场景：
/// 那种写法下切点会被顶到 `head`，压缩静默失效，上下文继续膨胀直到 API 报错。
/// §4.5 的算法（不额外约束最后一条 User）在这种情况下仍能产出切点。
#[test]
fn compaction_still_fires_when_one_task_has_a_long_tool_chain() {
    let mut messages = vec![system("s"), user("帮我重构整个项目")];
    for i in 0..40 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "巨大的工具输出，非常长非常长非常长非常长非常长非常长非常长",
        ));
    }
    let plan = plan_cut(&messages, 60, &counter())
        .expect("一长串大 tool 输出必须能触发压缩，否则会撑爆上下文");
    assert!(plan.cut > plan.head, "必须有可压区间");
    // 任务本身仍被正确提取（供重建阶段决定如何保留，见 step 4）。
    match &plan.current_task {
        Some(Message::User { content }) => assert_eq!(content, "帮我重构整个项目"),
        other => panic!("current_task 应是最后一条 User，实际 {other:?}"),
    }
}

/// 提取用户最新提出的问题，且它一定落在保留段内。
#[test]
fn extracts_last_user_message_as_current_task() {
    let mut messages = vec![system("s"), user("第一个问题")];
    for i in 0..30 {
        messages.push(user(&format!("中间很长的消息 {i} 撑预算")));
    }
    messages.push(user("这是最新提出的问题"));
    let plan = plan_cut(&messages, 30, &counter()).expect("应产生切点");

    match &plan.current_task {
        Some(Message::User { content }) => assert_eq!(content, "这是最新提出的问题"),
        other => panic!("current_task 应是最后一条 User，实际 {other:?}"),
    }
    // 本布局下任务就是最后一条消息，无条件落在保留段内（原样保留，不靠摘要）。
    let task_index = messages
        .iter()
        .rposition(|m| matches!(m, Message::User { .. }))
        .unwrap();
    assert!(
        plan.cut <= task_index,
        "本轮任务必须在保留段 [{}..] 内",
        plan.cut
    );
}

/// `compacted_range` 就是 `head..cut`，与重建逻辑一致。
#[test]
fn compacted_range_matches_head_and_cut() {
    let mut messages = vec![system("s"), user("旧")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    let plan = plan_cut(&messages, 20, &counter()).expect("应产生切点");
    assert_eq!(plan.compacted_range(), plan.head..plan.cut);
}

/// 校准后的 factor 会改变切点：估算被拉高时，保留段应更短（切点更靠后）。
#[test]
fn calibration_shifts_the_cut_point() {
    let mut messages = vec![system("s")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    let counter = HeuristicCounter::new();
    let base = plan_cut(&messages, 60, &counter).expect("应产生切点");

    // 真实 token 是估算的 2 倍 → 同样的预算只能装下更少的消息。
    counter.calibrate(2000, 1000);
    let calibrated = plan_cut(&messages, 60, &counter).expect("应产生切点");

    assert!(
        calibrated.cut > base.cut,
        "估算被拉高后保留段应更短: base={} calibrated={}",
        base.cut,
        calibrated.cut
    );
}
/// 三段必须互不重叠，且合起来恰好覆盖 `messages[head..]` 的全部消息。
///
/// 这是重建正确性的基础：拼接时既不重复也不遗漏。
#[test]
fn parts_cover_everything_after_background_exactly_once() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "工具输出"));

    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);

    // 三段总条数 = head 之后的全部消息数。
    // 任务被摘出时才单独计数；否则它已包含在 `remain` 里，重复计数会多算一条。
    let extracted = usize::from(parts.task_extracted);
    let total = parts.to_compress.len() + parts.remain.len() + extracted;
    assert_eq!(
        total,
        messages.len() - plan.head,
        "三段必须恰好覆盖背景之后的全部消息"
    );

    // 内容层面：`to_compress + (摘出的 task) + remain` 应与原区间逐条相等。
    // `Message` 未实现 `PartialEq`，用 `Debug` 表示做逐条比较。
    let mut rebuilt: Vec<Message> = parts.to_compress.clone();
    if parts.task_extracted {
        rebuilt.extend(parts.current_task.clone());
    }
    rebuilt.extend(parts.remain.clone());
    let rebuilt: Vec<String> = rebuilt.iter().map(|m| format!("{m:?}")).collect();
    let original: Vec<String> = messages[plan.head..]
        .iter()
        .map(|m| format!("{m:?}"))
        .collect();
    assert_eq!(
        rebuilt, original,
        "重建顺序应为 to_compress + current_task + remain"
    );
}

/// `current_task` 不得同时出现在 `to_compress` 或 `remain` 里。
#[test]
fn current_task_is_not_duplicated() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(user("本轮任务"));
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "工具输出"));

    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);

    let is_task = |m: &Message| matches!(m, Message::User { content } if content == "本轮任务");
    assert!(parts.current_task.as_ref().is_some_and(is_task));
    assert!(
        !parts.to_compress.iter().any(is_task),
        "任务不应出现在 to_compress"
    );
    // 任务在末尾，落在保留段内 → 不摘出，位置不变（只在 remain 里出现一次）。
    assert!(!parts.task_extracted, "任务在保留段时不应被摘出");
    assert_eq!(
        parts.remain.iter().filter(|m| is_task(m)).count(),
        1,
        "任务应恰好出现在 remain 中一次"
    );
}

/// 任务落在待压缩区时，仍被单独摘出——这是"长 tool 链"场景下的关键保证。
#[test]
fn task_is_extracted_even_when_inside_the_compacted_region() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..20 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    let task_index = messages.len();
    messages.push(user("本轮任务"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "巨大的工具输出，很长很长很长很长",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("应产生切点");
    // 切点落在任务之后 → 任务本会被压进摘要。
    assert!(plan.cut > task_index, "构造前提：切点应落在任务之后");
    let parts = plan.split(&messages);
    assert!(parts.task_extracted, "任务落在待压缩区时必须被摘出");
    match &parts.current_task {
        Some(Message::User { content }) => assert_eq!(content, "本轮任务"),
        other => panic!("任务必须被摘出，实际 {other:?}"),
    }
    // 摘出后不得再留在待压缩段里（否则会重复注入）。
    assert!(
        !parts
            .to_compress
            .iter()
            .any(|m| matches!(m, Message::User { content } if content == "本轮任务")),
        "被摘出的任务不应留在 to_compress"
    );
}

/// 待压缩段不含背景前缀（system / 上一轮摘要）。
#[test]
fn to_compress_never_contains_background() {
    let mut messages = vec![
        system("你是 Shirley"),
        Message::ContextSummary {
            content: "上一轮摘要".into(),
        },
    ];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    let plan = plan_cut(&messages, 30, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);

    assert!(
        !parts.to_compress.iter().any(|m| matches!(
            m,
            Message::System { .. } | Message::ContextSummary { .. }
        )),
        "背景不能进入待压缩段"
    );
}
/// 回归：任务在保留段里时，重建**不得**打乱顺序。
///
/// 早期 `split` 无条件把 `current_task` 摘出并前置，导致 `U_task` 跑到
/// `U_prev` 之前、两个 user 连排。修好后只在任务会落进待压缩区时才摘出。
#[test]
fn rebuild_preserves_order_when_task_is_in_remain() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    // 任务在末尾，后面还有一条 assistant——任务必然落在保留段内。
    messages.push(user("本轮任务"));
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "工具输出"));

    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);
    assert!(!parts.task_extracted, "构造前提：任务应在保留段内");

    let rebuilt = parts.rebuild("摘要");

    // 重建后消息数 = 摘要 + 保留段（系统提示词不在其中）。
    assert_eq!(rebuilt.len(), 1 + parts.remain.len());
    assert!(matches!(
        rebuilt[0],
        Message::ContextSummary { .. }
    ));
    // 摘出的任务不该额外出现：重建里 user 的条数与保留段一致。
    let users_in_rebuilt = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::User { .. }))
        .count();
    assert_eq!(
        users_in_rebuilt,
        parts
            .remain
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count(),
        "任务未被摘出时，user 条数应与保留段一致（不得重复注入）"
    );
    // 关键：任务必须仍在其后一条 assistant 之前（顺序未被颠倒）。
    let task_pos = rebuilt
        .iter()
        .position(|m| matches!(m, Message::User { content } if content == "本轮任务"))
        .expect("任务应在重建结果里");
    assert!(
        matches!(
            rebuilt.get(task_pos + 1),
            Some(Message::Assistant { .. })
        ),
        "任务后面应紧跟它的 assistant，实际: {:?}",
        rebuilt.get(task_pos + 1)
    );
}

/// 任务被摘出时，`rebuild` 把它插在 `remain` 之前，且其后链保持完整。
#[test]
fn rebuild_reinjects_extracted_task_before_remain() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..20 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(user("本轮任务"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "巨大的工具输出，很长很长很长很长",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);
    assert!(parts.task_extracted, "构造前提：任务应被摘出");

    let rebuilt = parts.rebuild("摘要");

    // 结构：摘要 + 任务 + 保留段（系统提示词不在其中）。
    assert!(matches!(
        rebuilt[0],
        Message::ContextSummary { .. }
    ));
    let task_pos = 1;
    assert!(
        matches!(rebuilt[task_pos], Message::User { .. }),
        "摘出的任务应紧跟摘要之后"
    );
    assert_eq!(rebuilt.len(), 2 + parts.remain.len());
    // 任务只出现一次。
    let task_count = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::User { content } if content == "本轮任务"))
        .count();
    assert_eq!(task_count, 1, "任务必须恰好出现一次");
}

/// 重建结果不得出现孤儿 `Tool`：每条 `Tool` 前面都有产出它的 `Assistant`。
#[test]
fn rebuild_never_produces_orphan_tool_results() {
    let mut messages = vec![system("s"), user("旧问题")];
    for i in 0..20 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(user("本轮任务"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "巨大的工具输出，很长很长很长很长",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);
    let rebuilt = parts.rebuild("摘要");

    for (i, msg) in rebuilt.iter().enumerate() {
        if matches!(msg, Message::Tool { .. }) {
            assert!(
                i > 0
                    && matches!(
                        rebuilt[i - 1],
                        Message::Assistant { .. } | Message::Tool { .. }
                    ),
                "第 {i} 条 Tool 没有前置 Assistant，会产生孤儿 tool_call"
            );
        }
    }
}

/// 重建后必须**恰好一条** `ContextSummary`：禁止"摘要叠摘要"。
///
/// 这是 `docs/compaction.md` 5.2 的核心不变量。旧实现把新摘要追加在末尾，
/// 于是旧摘要与新摘要并存，同一段被反复压缩（论文 caveat 4：越压越差）。
#[test]
fn rebuild_keeps_exactly_one_summary() {
    let mut messages = vec![
        system("你是 Shirley"),
        Message::ContextSummary {
            content: "上一轮摘要".into(),
        },
    ];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(user("本轮任务"));

    let plan = plan_cut(&messages, 40, &counter()).expect("应产生切点");
    let parts = plan.split(&messages);
    let rebuilt = parts.rebuild("新摘要");

    let summaries: Vec<_> = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::ContextSummary { .. }))
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "重建后只能有一条摘要，实际 {}",
        summaries.len()
    );
    match summaries[0] {
        Message::ContextSummary { content } => assert_eq!(content, "新摘要"),
        _ => unreachable!(),
    }
    // 重建结果不含任何 System：系统提示词由 `Agent` 现取现置顶，不走 rebuild。
    assert!(
        !rebuilt
            .iter()
            .any(|m| matches!(m, Message::System { .. })),
        "rebuild 不应复制系统提示词"
    );
    // 摘要就是重建结果的开头。
    assert!(matches!(rebuilt[0], Message::ContextSummary { .. }));
}

/// 连续两次重建，摘要数量仍为 1——反复压缩不叠加。
#[test]
fn repeated_compaction_does_not_stack_summaries() {
    let mut messages = vec![system("你是 Shirley")];
    for i in 0..30 {
        messages.push(user(&format!("历史 {i} 撑预算")));
    }
    messages.push(user("本轮任务"));

    // 第一次压缩。
    let plan = plan_cut(&messages, 40, &counter()).expect("第一次应产生切点");
    let parts = plan.split(&messages);
    let mut current = parts.rebuild("摘要一");

    // 第二次压缩：基于第一次的结果继续追加历史。
    for i in 0..30 {
        current.push(user(&format!("新历史 {i} 撑预算")));
    }
    current.push(user("第二个任务"));

    let plan = plan_cut(&current, 40, &counter()).expect("第二次应产生切点");
    let parts = plan.split(&current);
    let rebuilt = parts.rebuild("摘要二");

    let count = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::ContextSummary { .. }))
        .count();
    assert_eq!(count, 1, "反复压缩后摘要仍应只有一条，实际 {count}");
}

