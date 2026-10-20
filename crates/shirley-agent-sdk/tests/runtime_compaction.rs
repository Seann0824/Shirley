//! Contract tests for the context-compaction cut point and rebuild.
//!
//! Covers the core invariants of sections 4.5 / 5.2 of `docs/compaction.md`:
//! the cut-point algorithm (budget, tool pairing, keep at least one), the three-way split,
//! and the ordering and "exactly one summary" constraint of `CompactParts::rebuild`.
//!
//! Originally inlined in `mod tests` of `runtime/mod.rs`, moved to an integration test after the
//! module split; relies only on the public API of `shirley_agent_sdk`.

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

/// Each message is roughly 4 Chinese chars = 4 tokens + 4 wrapper = about 8 tokens.
/// Use a small enough budget to force the "keep only the last few" behavior.
fn counter() -> HeuristicCounter {
    HeuristicCounter::new()
}

/// With nothing but background (only system), there is nothing to compress.
#[test]
fn no_compaction_when_only_background() {
    let messages = vec![system("You are Shirley")];
    assert!(plan_cut(&messages, 1000, &counter()).is_none());
}

/// Everything fits in the budget -> no compression.
#[test]
fn no_compaction_when_everything_fits() {
    let messages = vec![system("s"), user("hello"), user("world")];
    let plan = plan_cut(&messages, 10_000, &counter());
    assert!(
        plan.is_none(),
        "there should be no cut point when everything fits: {plan:?}"
    );
}

/// The background prefix is never compacted: `head` covers the leading system, and the cut point never lands in it.
#[test]
fn background_prefix_is_never_compacted() {
    let mut messages = vec![system("You are Shirley")];
    for i in 0..40 {
        messages.push(user(&format!("long user message #{i} to blow the budget")));
    }
    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    assert_eq!(plan.head, 1, "the leading system must be background");
    assert!(
        plan.cut > plan.head,
        "the cut point must be after the background"
    );
}

/// The previous `ContextSummary` is background: it gets replaced by the new summary and does not consume the retain budget.
#[test]
fn prior_summary_is_background() {
    let mut messages = vec![
        system("You are Shirley"),
        Message::ContextSummary {
            content: "previous summary".into(),
        },
    ];
    for i in 0..40 {
        messages.push(user(&format!("long user message #{i} to blow the budget")));
    }
    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    assert_eq!(
        plan.head, 2,
        "system + previous summary both count as background"
    );
    assert!(plan.cut >= 2, "background must not be compacted");
}

/// Keep at least one message: even if the last one alone exceeds the budget, it stays in the retained segment.
#[test]
fn always_keeps_at_least_one_message() {
    let messages = vec![
        system("s"),
        user("first message"),
        user("an extremely long message".repeat(50).as_str()),
    ];
    let plan = plan_cut(&messages, 1, &counter()).expect("a cut point should be produced");
    assert!(
        plan.cut < messages.len(),
        "at least the last message must be kept, so cut must be less than len"
    );
    // The last message (the over-budget one) must be in the retained segment.
    assert_eq!(plan.cut, messages.len() - 1);
}
/// Core fix: the first message of the retained segment must not be a `Tool`, or the orphan tool_call gets a 400 from the API.
#[test]
fn never_cuts_between_assistant_tool_calls_and_tool_results() {
    let mut messages = vec![system("s"), user("take a look for me")];
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("call_{i}")]));
        messages.push(tool(
            &format!("call_{i}"),
            "historical tool output to consume budget",
        ));
    }
    messages.push(user("this is the current task"));
    for i in 0..10 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "current tool output, very very very long",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("a cut point should be produced");
    assert!(
        !matches!(messages.get(plan.cut), Some(Message::Tool { .. })),
        "the cut point landed on a Tool result, which produces an orphan tool_call: cut={}",
        plan.cut
    );
    assert!(
        matches!(messages.get(plan.cut), Some(Message::Assistant { .. })),
        "it should back off to Assistant{{tool_calls}}: cut={}",
        plan.cut
    );
}

/// Consecutive Tool results (one parallel call) must be retained as a whole, with complete pairing.
#[test]
fn keeps_consecutive_tool_results_together() {
    let mut messages = vec![system("s"), user("parallel calls")];
    messages.push(assistant_calls(&["a", "b", "c"]));
    messages.push(tool("a", "result A is very very very long"));
    messages.push(tool("b", "result B is very very very long"));
    messages.push(tool("c", "result C is very very very long"));

    let plan = plan_cut(&messages, 20, &counter()).expect("a cut point should be produced");
    assert!(
        matches!(messages.get(plan.cut), Some(Message::Assistant { .. })),
        "parallel tool results should be retained as a whole: cut={}",
        plan.cut
    );
    let tool_count = messages[plan.cut..]
        .iter()
        .filter(|m| matches!(m, Message::Tool { .. }))
        .count();
    assert_eq!(tool_count, 3, "all three tool results must be retained");
}

/// Regression: when one user message triggers a long chain of large tool output, compaction **must** still fire.
///
/// This is the failure scenario where "the cut point sticking just before the last User" degenerates into `None`:
/// in that form the cut point gets pushed up to `head`, compaction silently fails, and the context keeps growing until the API errors.
/// The section 4.5 algorithm (which adds no extra constraint on the last User) still produces a cut point here.
#[test]
fn compaction_still_fires_when_one_task_has_a_long_tool_chain() {
    let mut messages = vec![system("s"), user("help me refactor the whole project")];
    for i in 0..40 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "huge tool output, extremely extremely extremely long",
        ));
    }
    let plan = plan_cut(&messages, 60, &counter()).expect(
        "a long chain of large tool output must trigger compaction, or the context will blow up",
    );
    assert!(plan.cut > plan.head, "there must be a compactable region");
    // The task itself is still extracted correctly (so the rebuild stage can decide how to retain it; see step 4).
    match &plan.current_task {
        Some(Message::User { content }) => {
            assert_eq!(content, "help me refactor the whole project")
        }
        other => panic!("current_task should be the last User, got {other:?}"),
    }
}

/// Extract the user's most recent question, and it must land in the retained segment.
#[test]
fn extracts_last_user_message_as_current_task() {
    let mut messages = vec![system("s"), user("first question")];
    for i in 0..30 {
        messages.push(user(&format!(
            "long message {i} in the middle to consume budget"
        )));
    }
    messages.push(user("this is the most recent question"));
    let plan = plan_cut(&messages, 30, &counter()).expect("a cut point should be produced");

    match &plan.current_task {
        Some(Message::User { content }) => assert_eq!(content, "this is the most recent question"),
        other => panic!("current_task should be the last User, got {other:?}"),
    }
    // In this layout the task is the last message, unconditionally in the retained segment (kept as-is, not via the summary).
    let task_index = messages
        .iter()
        .rposition(|m| matches!(m, Message::User { .. }))
        .unwrap();
    assert!(
        plan.cut <= task_index,
        "the current task must be in the retained segment [{}..]",
        plan.cut
    );
}

/// `compacted_range` is exactly `head..cut`, consistent with the rebuild logic.
#[test]
fn compacted_range_matches_head_and_cut() {
    let mut messages = vec![system("s"), user("old")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    let plan = plan_cut(&messages, 20, &counter()).expect("a cut point should be produced");
    assert_eq!(plan.compacted_range(), plan.head..plan.cut);
}

/// A calibrated factor changes the cut point: when the estimate is pulled up, the retained segment should be shorter (the cut point further back).
#[test]
fn calibration_shifts_the_cut_point() {
    let mut messages = vec![system("s")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    let counter = HeuristicCounter::new();
    let base = plan_cut(&messages, 60, &counter).expect("a cut point should be produced");

    // The real token count is 2x the estimate -> the same budget fits fewer messages.
    counter.calibrate(2000, 1000);
    let calibrated = plan_cut(&messages, 60, &counter).expect("a cut point should be produced");

    assert!(
        calibrated.cut > base.cut,
        "the retained segment should be shorter after the estimate is raised: base={} calibrated={}",
        base.cut,
        calibrated.cut
    );
}
/// The three segments must not overlap and together cover exactly all messages in `messages[head..]`.
///
/// This is the basis of rebuild correctness: concatenation neither duplicates nor drops anything.
#[test]
fn parts_cover_everything_after_background_exactly_once() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "tool output"));

    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);

    // The total count of the three segments = all messages after head.
    // Count the task separately only when it is extracted; otherwise it is already in `remain`, and counting it again would overcount by one.
    let extracted = usize::from(parts.task_extracted);
    let total = parts.to_compress.len() + parts.remain.len() + extracted;
    assert_eq!(
        total,
        messages.len() - plan.head,
        "the three segments must cover exactly all messages after the background"
    );

    // At the content level: `to_compress + (extracted task) + remain` should equal the original region message by message.
    // `Message` does not implement `PartialEq`; compare message by message using the `Debug` representation.
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
        "the rebuild order should be to_compress + current_task + remain"
    );
}

/// `current_task` must not appear in both `to_compress` and `remain`.
#[test]
fn current_task_is_not_duplicated() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(user("current task"));
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "tool output"));

    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);

    let is_task = |m: &Message| matches!(m, Message::User { content } if content == "current task");
    assert!(parts.current_task.as_ref().is_some_and(is_task));
    assert!(
        !parts.to_compress.iter().any(is_task),
        "the task should not appear in to_compress"
    );
    // The task is at the end, in the retained segment -> not extracted, position unchanged (appears once in remain).
    assert!(
        !parts.task_extracted,
        "the task should not be extracted when it is in the retained segment"
    );
    assert_eq!(
        parts.remain.iter().filter(|m| is_task(m)).count(),
        1,
        "the task should appear exactly once in remain"
    );
}

/// When the task lands in the to-compress region, it is still extracted separately — the key guarantee in the "long tool chain" scenario.
#[test]
fn task_is_extracted_even_when_inside_the_compacted_region() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..20 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    let task_index = messages.len();
    messages.push(user("current task"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "huge tool output, very very long",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("a cut point should be produced");
    // The cut point lands after the task -> the task would otherwise be compressed into the summary.
    assert!(
        plan.cut > task_index,
        "setup precondition: the cut point should land after the task"
    );
    let parts = plan.split(&messages);
    assert!(
        parts.task_extracted,
        "the task must be extracted when it lands in the to-compress region"
    );
    match &parts.current_task {
        Some(Message::User { content }) => assert_eq!(content, "current task"),
        other => panic!("the task must be extracted, got {other:?}"),
    }
    // Once extracted it must not remain in the to-compress segment (or it would be injected twice).
    assert!(
        !parts
            .to_compress
            .iter()
            .any(|m| matches!(m, Message::User { content } if content == "current task")),
        "the extracted task should not remain in to_compress"
    );
}

/// The to-compress segment contains no background prefix (system / previous summary).
#[test]
fn to_compress_never_contains_background() {
    let mut messages = vec![
        system("You are Shirley"),
        Message::ContextSummary {
            content: "previous summary".into(),
        },
    ];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    let plan = plan_cut(&messages, 30, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);

    assert!(
        !parts
            .to_compress
            .iter()
            .any(|m| matches!(m, Message::System { .. } | Message::ContextSummary { .. })),
        "background must not enter the to-compress segment"
    );
}
/// Regression: when the task is in the retained segment, rebuild **must not** scramble the order.
///
/// An early `split` unconditionally extracted `current_task` and moved it to the front, so `U_task` ran
/// before `U_prev`, putting two users back to back. After the fix it is extracted only when the task would land in the to-compress region.
#[test]
fn rebuild_preserves_order_when_task_is_in_remain() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    // The task is at the end, with an assistant after it — the task is necessarily in the retained segment.
    messages.push(user("current task"));
    messages.push(assistant_calls(&["x"]));
    messages.push(tool("x", "tool output"));

    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);
    assert!(
        !parts.task_extracted,
        "setup precondition: the task should be in the retained segment"
    );

    let rebuilt = parts.rebuild("summary");

    // Message count after rebuild = summary + retained segment (the system prompt is not among them).
    assert_eq!(rebuilt.len(), 1 + parts.remain.len());
    assert!(matches!(rebuilt[0], Message::ContextSummary { .. }));
    // The extracted task should not appear extra: the number of users in the rebuild matches the retained segment.
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
        "when the task is not extracted, the user count should match the retained segment (no double injection)"
    );
    // Key: the task must still precede its following assistant (the order was not reversed).
    let task_pos = rebuilt
        .iter()
        .position(|m| matches!(m, Message::User { content } if content == "current task"))
        .expect("the task should be in the rebuild result");
    assert!(
        matches!(rebuilt.get(task_pos + 1), Some(Message::Assistant { .. })),
        "the task should be immediately followed by its assistant, got: {:?}",
        rebuilt.get(task_pos + 1)
    );
}

/// When the task is extracted, `rebuild` inserts it before `remain`, and its following chain stays intact.
#[test]
fn rebuild_reinjects_extracted_task_before_remain() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..20 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(user("current task"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "huge tool output, very very long",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);
    assert!(
        parts.task_extracted,
        "setup precondition: the task should be extracted"
    );

    let rebuilt = parts.rebuild("summary");

    // Structure: summary + task + retained segment (the system prompt is not among them).
    assert!(matches!(rebuilt[0], Message::ContextSummary { .. }));
    let task_pos = 1;
    assert!(
        matches!(rebuilt[task_pos], Message::User { .. }),
        "the extracted task should immediately follow the summary"
    );
    assert_eq!(rebuilt.len(), 2 + parts.remain.len());
    // The task appears only once.
    let task_count = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::User { content } if content == "current task"))
        .count();
    assert_eq!(task_count, 1, "the task must appear exactly once");
}

/// The rebuild result must not produce an orphan `Tool`: every `Tool` is preceded by the `Assistant` that produced it.
#[test]
fn rebuild_never_produces_orphan_tool_results() {
    let mut messages = vec![system("s"), user("old question")];
    for i in 0..20 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(user("current task"));
    for i in 0..20 {
        messages.push(assistant_calls(&[&format!("live_{i}")]));
        messages.push(tool(
            &format!("live_{i}"),
            "huge tool output, very very long",
        ));
    }

    let plan = plan_cut(&messages, 60, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);
    let rebuilt = parts.rebuild("summary");

    for (i, msg) in rebuilt.iter().enumerate() {
        if matches!(msg, Message::Tool { .. }) {
            assert!(
                i > 0
                    && matches!(
                        rebuilt[i - 1],
                        Message::Assistant { .. } | Message::Tool { .. }
                    ),
                "Tool #{i} has no preceding Assistant, which produces an orphan tool_call"
            );
        }
    }
}

/// After rebuild there must be **exactly one** `ContextSummary`: "summary stacking on summary" is forbidden.
///
/// This is the core invariant of section 5.2 of `docs/compaction.md`. The old implementation appended the new summary at the end,
/// so the old and new summaries coexisted and the same span was compressed repeatedly (paper caveat 4: the more you compress, the worse it gets).
#[test]
fn rebuild_keeps_exactly_one_summary() {
    let mut messages = vec![
        system("You are Shirley"),
        Message::ContextSummary {
            content: "previous summary".into(),
        },
    ];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(user("current task"));

    let plan = plan_cut(&messages, 40, &counter()).expect("a cut point should be produced");
    let parts = plan.split(&messages);
    let rebuilt = parts.rebuild("new summary");

    let summaries: Vec<_> = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::ContextSummary { .. }))
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "after rebuild there can be only one summary, got {}",
        summaries.len()
    );
    match summaries[0] {
        Message::ContextSummary { content } => assert_eq!(content, "new summary"),
        _ => unreachable!(),
    }
    // The rebuild result contains no System: the system prompt is fetched fresh and pinned to the top by `Agent`, not via rebuild.
    assert!(
        !rebuilt.iter().any(|m| matches!(m, Message::System { .. })),
        "rebuild should not copy the system prompt"
    );
    // The summary is the head of the rebuild result.
    assert!(matches!(rebuilt[0], Message::ContextSummary { .. }));
}

/// Two rebuilds in a row still yield one summary — repeated compaction does not stack.
#[test]
fn repeated_compaction_does_not_stack_summaries() {
    let mut messages = vec![system("You are Shirley")];
    for i in 0..30 {
        messages.push(user(&format!("history {i} to consume budget")));
    }
    messages.push(user("current task"));

    // First compaction.
    let plan = plan_cut(&messages, 40, &counter()).expect("the first should produce a cut point");
    let parts = plan.split(&messages);
    let mut current = parts.rebuild("summary one");

    // Second compaction: keep appending history on top of the first result.
    for i in 0..30 {
        current.push(user(&format!("new history {i} to consume budget")));
    }
    current.push(user("second task"));

    let plan = plan_cut(&current, 40, &counter()).expect("the second should produce a cut point");
    let parts = plan.split(&current);
    let rebuilt = parts.rebuild("summary two");

    let count = rebuilt
        .iter()
        .filter(|m| matches!(m, Message::ContextSummary { .. }))
        .count();
    assert_eq!(
        count, 1,
        "after repeated compaction there should still be only one summary, got {count}"
    );
}
