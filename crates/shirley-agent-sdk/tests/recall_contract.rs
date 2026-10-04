//! 召回契约测试（`docs/recall.md` 第七节验收）。
//!
//! 覆盖：召回无损（原文逐字返回）、工具输出统一清空为占位标记、
//! chunk 化的并发多工具配对、recall 工具的参数与返回。

use shirley_agent_sdk::chunk_messages;
use shirley_agent_sdk::{Tool, ToolContext, ToolError};
use shirley_agent_sdk::{Message, RecallStore, RecallTool, ToolCall};
use std::sync::Arc;

fn user(text: &str) -> Message {
    Message::User {
        content: text.into(),
    }
}

fn assistant_with_calls(ids: &[&str]) -> Message {
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

fn tool_msg(id: &str, content: &str) -> Message {
    Message::Tool {
        tool_call_id: id.into(),
        content: Some(content.into()),
    }
}

/// 验收 2：召回返回的 chunk 内容与压缩前原文逐字一致（无损，不二次摘要）。
#[test]
fn retrieve_returns_verbatim_content() {
    let store = RecallStore::new();
    let messages = vec![
        user("我叫夏莉，住在上海，最喜欢的历史人物是朱雀"),
        assistant_with_calls(&["a"]),
        tool_msg("a", "src/main.rs\nsrc/lib.rs"),
    ];
    let chunks = chunk_messages(&messages);
    store.index(chunks);

    let hits = store.retrieve("夏莉 住在 哪里", 5);
    assert!(!hits.is_empty());
    let top = &hits[0].1;
    let text = top.index_text();
    // 原文无损（该 chunk 短，不触截断）
    assert!(text.contains("我叫夏莉，住在上海，最喜欢的历史人物是朱雀"));
}

/// 验收 5 / 决策 1：带工具调用的步与其 Tool 结果都不入召回库。
///
/// 工具输出是"世界可再生的"，走重建路径；入库会让召回被代码 / 日志淹没。
#[test]
fn tool_step_excluded_from_recall() {
    let messages = vec![
        assistant_with_calls(&["a", "b", "c"]),
        tool_msg("a", "output A"),
        tool_msg("b", "output B"),
        tool_msg("c", "output C"),
    ];
    assert!(chunk_messages(&messages).is_empty(), "工具类消息不该入库");
}

/// 验收 7（防递归）：recall 自己产生的步（Assistant{recall} + Tool 召回文本）不入库，
/// 否则召回文本会被再次索引、雪球放大。
#[test]
fn recall_step_not_reindexed() {
    let messages = vec![
        Message::Assistant {
            content: Some("让我回忆一下".into()),
            reasoning_content: None,
            thinking_signature: None,
            tool_calls: vec![ToolCall {
                id: "r".into(),
                name: "recall".into(),
                arguments: "{\"query\":\"名字\"}".into(),
            }],
        },
        tool_msg("r", "召回到 5 条历史内容：……"),
    ];
    assert!(chunk_messages(&messages).is_empty(), "recall 步不该入库");
}

/// 验收 3 的先决：压缩区里的 tool 消息不入库，天然无孤儿。
#[test]
fn orphan_tool_dropped_not_mispaired() {
    let messages = vec![tool_msg("ghost", "没人认领的结果")];
    assert!(chunk_messages(&messages).is_empty(), "孤立 Tool 不该产出 chunk");
}

/// recall 工具：合法参数 → 返回渲染文本；k 边界钳制。
#[tokio::test]
async fn recall_tool_invocation() -> Result<(), ToolError> {
    let store = Arc::new(RecallStore::new());
    let messages = vec![user("用户名叫夏莉，代号 Shirley")];
    store.index(chunk_messages(&messages));
    let tool = RecallTool::new(store);

    let output = tool
        .invoke(serde_json::json!({ "query": "用户 名字 代号", "k": 3 }), ToolContext::new())
        .await?;
    let text = output.as_str().expect("返回应是字符串");
    assert!(text.contains("用户名叫夏莉"), "recall 结果应含原文");

    // 缺 query → 参数错误
    let err = tool.invoke(serde_json::json!({ "k": 1 }), ToolContext::new()).await;
    assert!(err.is_err());

    // 无命中 → 友好提示（不报错）
    let output = tool
        .invoke(serde_json::json!({ "query": "zzz 不存在的词" }), ToolContext::new())
        .await?;
    assert!(output.as_str().unwrap().contains("no relevant history"));
    Ok(())
}
