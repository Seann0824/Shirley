//! 分块策略（`docs/recall.md` 第二节）。
//!
//! **入召回库的只有"重建不出来"的对话性内容**（`docs/recall.md` 决策 1）：
//! - `User` 消息 → 任务锚点，主要检索对象；
//! - 无 `tool_calls` 的 `Assistant` 文本 → 结论 / 决策 / 约定。
//!
//! **工具类消息（带 `tool_calls` 的 `Assistant` 与全部 `Tool` 结果）不入库**：
//! 文件内容、命令输出是"世界可再生的"，压缩时统一清空为占位标记，AI 走重建路径
//! （重新读取 / 重新执行）。把它们塞进召回库既违背设计，也会让召回结果被几万 token
//! 的代码 / 日志淹没——这正是"召回内容太多"的根因。
//!
//! 不入库工具类消息顺带解决了**防递归**（`docs/recall.md` 4.3）：recall 工具产生的是
//! `Assistant{tool_calls: recall}` + `Tool`（召回文本），两者都不入库，召回文本不会被
//! 再次索引、不会雪球式放大。
//!
//! `reasoning_content` **不入索引视图**：过程性思维（"我需要/让我看看"）低信息高重复，
//! 会污染 IDF。`ContextSummary` 是压缩产物，也不入库。

use crate::message::Message;

/// 索引视图对单个字段的截断上限（`docs/recall.md` 3.3）。
///
/// 长输出（build 日志几万 token）会垄断 BM25 长度归一化。
/// **索引视图 ≠ 注入内容**：这里截断只为打分；召回返回的是原文。
const INDEX_TRUNCATE: usize = 2000;

/// 一个召回块：只承载对话性内容。
#[derive(Debug, Clone)]
pub enum Chunk {
    /// 用户消息：任务锚点。
    User { content: String },
    /// 无工具调用的 assistant 文本：结论 / 决策 / 约定。
    Assistant { content: String },
}

impl Chunk {
    /// chunk 的可检索投影（`docs/recall.md` 2.3）。
    ///
    /// **不含 `reasoning_content`，也不含任何工具调用 / 工具输出**——后者压根不入库。
    pub fn index_text(&self) -> String {
        match self {
            Chunk::User { content } | Chunk::Assistant { content } => {
                truncate(content).into_owned()
            }
        }
    }
}

fn truncate(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().count() <= INDEX_TRUNCATE {
        return std::borrow::Cow::Borrowed(text);
    }
    let head: String = text.chars().take(INDEX_TRUNCATE).collect();
    std::borrow::Cow::Owned(format!("{head}…"))
}

/// 把消息序列切块（`docs/recall.md` 决策 1）。
///
/// 规则：
/// - `User` → `UserChunk`；
/// - 无 `tool_calls` 且正文非空的 `Assistant` → `AssistantChunk`；
/// - 其余（`System` / `ContextSummary` / `Tool` / 带 `tool_calls` 的 `Assistant`）→ 跳过。
///
/// 因为工具类消息一律不入库，这里**不需要**再为 `Assistant{tool_calls}` 与其
/// `Tool` 结果做配对——`Tool` 消息在 `_` 分支被逐个跳过，天然无孤儿风险。
pub fn chunk_messages(messages: &[Message]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    for message in messages {
        match message {
            Message::User { content } => {
                chunks.push(Chunk::User {
                    content: content.clone(),
                });
            }
            Message::Assistant {
                content: Some(content),
                tool_calls,
                ..
            } if tool_calls.is_empty() => {
                if !content.trim().is_empty() {
                    chunks.push(Chunk::Assistant {
                        content: content.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::chunk_messages;
    use crate::message::{Message, ToolCall};

    fn user(text: &str) -> Message {
        Message::User {
            content: text.into(),
        }
    }

    fn assistant(text: Option<&str>, calls: &[(&str, &str)]) -> Message {
        Message::Assistant {
            content: text.map(String::from),
            reasoning_content: Some("我需要看看文件".into()),
            thinking_signature: None,
            tool_calls: calls
                .iter()
                .map(|(id, name)| ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
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

    #[test]
    fn user_independent_chunk() {
        let chunks = chunk_messages(&[user("我叫夏莉")]);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], super::Chunk::User { content } if content == "我叫夏莉"));
    }

    #[test]
    fn assistant_text_chunked() {
        let chunks = chunk_messages(&[assistant(Some("结论：用 BM25"), &[])]);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], super::Chunk::Assistant { content } if content == "结论：用 BM25"));
    }

    #[test]
    fn tool_step_excluded_from_recall() {
        // 带 tool_calls 的 assistant 与其 Tool 结果都不入库（决策 1 + 防递归）。
        let messages = vec![
            assistant(None, &[("a", "bash"), ("b", "bash")]),
            tool("a", "output A"),
            tool("b", "output B"),
        ];
        assert!(chunk_messages(&messages).is_empty());
    }

    #[test]
    fn recall_step_not_reindexed() {
        // recall 工具调用步（Assistant{tool_calls: recall} + Tool 召回文本）不入库，
        // 否则召回文本会被再次索引、雪球放大（docs/recall.md 4.3 防递归）。
        let messages = vec![
            assistant(Some("让我回忆一下"), &[("r", "recall")]),
            tool("r", "召回到 5 条历史内容：……（很长）"),
        ];
        assert!(chunk_messages(&messages).is_empty());
    }

    #[test]
    fn reasoning_excluded_from_index() {
        let chunks = chunk_messages(&[assistant(Some("结论：用 BM25"), &[])]);
        let indexed = chunks[0].index_text();
        assert!(indexed.contains("BM25"));
        assert!(!indexed.contains("我需要看看文件"), "reasoning 不该入索引视图");
    }

    #[test]
    fn system_and_summary_skipped() {
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::ContextSummary {
                content: "sum".into(),
            },
            user("任务"),
        ];
        assert_eq!(chunk_messages(&messages).len(), 1);
    }
}
