//! 召回契约测试（`docs/recall.md` 第七节验收）。
//!
//! 覆盖：召回无损（原文逐字返回）、工具输出统一清空为占位标记、
//! chunk 化的并发多工具配对、recall 工具的参数与返回。

use agent_sdk::chunk_messages;
use agent_sdk::{Tool, ToolError};
use agent_sdk::{Message, RecallStore, RecallTool, ToolCall};
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

/// 验收 5：一条 Assistant 并发 N 个工具 → N 条 Tool 聚为一个块，配对不串。
#[test]
fn parallel_tools_group_into_one_chunk() {
    let messages = vec![
        assistant_with_calls(&["a", "b", "c"]),
        tool_msg("a", "output A"),
        tool_msg("b", "output B"),
        tool_msg("c", "output C"),
    ];
    let chunks = chunk_messages(&messages);
    assert_eq!(chunks.len(), 1);
    let text = chunks[0].index_text();
    assert!(text.contains("output A") && text.contains("output C"));
}

/// 验收 3 的先决：压缩区里的 tool 消息 chunk 化后配对完整（无孤儿）。
#[test]
fn orphan_tool_dropped_not_mispaired() {
    // 第二条 Tool 的 id 不属于前面的 assistant → 不产出 chunk（吞掉防孤儿）
    let messages = vec![tool_msg("ghost", "没人认领的结果")];
    let chunks = chunk_messages(&messages);
    assert!(chunks.is_empty(), "孤立 Tool 不该产出 chunk");
}

/// recall 工具：合法参数 → 返回渲染文本；k 边界钳制。
#[tokio::test]
async fn recall_tool_invocation() -> Result<(), ToolError> {
    let store = Arc::new(RecallStore::new());
    let messages = vec![user("用户名叫夏莉，代号 Shirley")];
    store.index(chunk_messages(&messages));
    let tool = RecallTool::new(store);

    let output = tool
        .invoke(serde_json::json!({ "query": "用户 名字 代号", "k": 3 }))
        .await?;
    let text = output.as_str().expect("返回应是字符串");
    assert!(text.contains("用户名叫夏莉"), "recall 结果应含原文");

    // 缺 query → 参数错误
    let err = tool.invoke(serde_json::json!({ "k": 1 })).await;
    assert!(err.is_err());

    // 无命中 → 友好提示（不报错）
    let output = tool.invoke(serde_json::json!({ "query": "zzz 不存在的词" })).await?;
    assert!(output.as_str().unwrap().contains("没有检索到"));
    Ok(())
}
