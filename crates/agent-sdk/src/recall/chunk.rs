//! 分块策略（`docs/recall.md` 第二节）。
//!
//! **不能以单条消息为文档单位**："reason → act" 是同一条 `Assistant` 的两个字段
//! （`reasoning_content` / `tool_calls`），"observation" 是独立的 `Tool` 消息，
//! 靠 `tool_call_id` 关联。单条消息为块会丢上下文，孤立 `Tool` 是孤儿 tool_call，
//! 注入回上下文时 API 会 400。
//!
//! 因此：
//! - `UserChunk`：一条 User 消息，独立成块；
//! - `StepChunk`：一条 `Assistant{tool_calls}` + 其配对全部 `Tool` 结果（原子组）。
//!
//! `reasoning_content` **不入索引视图**：过程性思维（"我需要/让我看看"）低信息高重复，
//! 会污染 IDF。`ContextSummary` 是压缩产物，也不入库。

use crate::message::Message;

/// 索引视图对单个字段的截断上限（`docs/recall.md` 3.3）。
///
/// 长输出（build 日志几万 token）会垄断 BM25 长度归一化。
/// **索引视图 ≠ 注入内容**：这里截断只为打分；召回返回的是原文。
const INDEX_TRUNCATE: usize = 2000;

/// 一个召回块。
#[derive(Debug, Clone)]
pub enum Chunk {
    /// 用户消息：任务锚点。
    User { content: String },
    /// 一个完整 ReAct 步：assistant（含 tool_calls）+ 配对 observation。
    Step {
        /// Assistant 的正式回复（可无）
        content: Option<String>,
        /// 工具调用骨架：name + arguments
        calls: Vec<(String, String)>,
        /// 配对的工具结果（按 tool_call_id 匹配）
        observations: Vec<Option<String>>,
    },
}

impl Chunk {
    /// chunk 的可检索投影（`docs/recall.md` 2.3）。
    ///
    /// **不含 `reasoning_content`**。StepChunk 的骨架（工具名 + 参数）入索引——
    /// 它们是"做了什么"的锚点，符号名是最高 IDF 的信号。
    pub fn index_text(&self) -> String {
        let mut text = String::new();
        match self {
            Chunk::User { content } => {
                text.push_str(truncate(content).as_ref());
            }
            Chunk::Step {
                content,
                calls,
                observations,
            } => {
                if let Some(content) = content {
                    text.push_str(truncate(content).as_ref());
                    text.push('\n');
                }
                for (name, arguments) in calls {
                    text.push_str(name);
                    text.push(' ');
                    text.push_str(truncate(arguments).as_ref());
                    text.push('\n');
                }
                for observation in observations.iter().flatten() {
                    text.push_str(truncate(observation).as_ref());
                    text.push('\n');
                }
            }
        }
        text
    }
}

fn truncate(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().count() <= INDEX_TRUNCATE {
        return std::borrow::Cow::Borrowed(text);
    }
    let head: String = text.chars().take(INDEX_TRUNCATE).collect();
    std::borrow::Cow::Owned(format!("{head}…"))
}

/// 把消息序列切块（`docs/recall.md` 2.2）。
///
/// 规则：
/// - `User` → 独立 `UserChunk`；
/// - `Assistant{tool_calls}` + 紧随其后的全部 `Tool`（按 id 配对）→ 一个 `StepChunk`；
/// - `Assistant` 无 tool_calls → 文本块（只取 content，reasoning 不入库）；
/// - `System` / `ContextSummary` / 孤立 `Tool` → 不产出 chunk
///   （孤立 Tool 在正常消息流里不存在，压缩切点已保证配对）。
pub fn chunk_messages(messages: &[Message]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        match &messages[i] {
            Message::User { content } => {
                chunks.push(Chunk::User {
                    content: content.clone(),
                });
                i += 1;
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if tool_calls.is_empty() {
                    // 无工具调用的回复：只保留正文（决策 / 结论是可召回的对话内容）。
                    if content.as_deref().is_some_and(|text| !text.trim().is_empty()) {
                        chunks.push(Chunk::Step {
                            content: content.clone(),
                            calls: Vec::new(),
                            observations: Vec::new(),
                        });
                    }
                    i += 1;
                } else {
                    // 收集紧随其后的全部 Tool 消息，按 id 配对。
                    let calls: Vec<(String, String)> = tool_calls
                        .iter()
                        .map(|call| (call.name.clone(), call.arguments.clone()))
                        .collect();
                    let ids: Vec<String> =
                        tool_calls.iter().map(|call| call.id.clone()).collect();
                    let mut observations: Vec<Option<String>> = ids.iter().map(|_| None).collect();
                    let mut j = i + 1;
                    while j < messages.len() {
                        if let Message::Tool {
                            tool_call_id,
                            content,
                        } = &messages[j]
                        {
                            let Some(pos) = ids.iter().position(|id| id == tool_call_id) else {
                                // 不是本步的 tool_call_id：安全起见也吞掉（防孤儿），
                                // 但不配对到任何槽位。
                                break;
                            };
                            {
                                observations[pos] = content.clone();
                                j += 1;
                                continue;
                            }
                            // 不是本步的 tool_call_id：安全起见也吞掉（防孤儿），
                            // 但不配对到任何槽位。
                        }
                        break;
                    }
                    chunks.push(Chunk::Step {
                        content: content.clone(),
                        calls,
                        observations,
                    });
                    i = j;
                }
            }
            // 背景 / 压缩产物 / 其他：跳过。
            _ => i += 1,
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::chunk_messages;
    use crate::message::{Message, ToolCall};

    fn user(text: &str) -> Message {
        Message::User { content: text.into() }
    }

    fn assistant(text: Option<&str>, calls: &[(&str, &str)]) -> Message {
        Message::Assistant {
            content: text.map(String::from),
            reasoning_content: Some("我需要看看文件".into()),
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
    fn step_groups_parallel_tools() {
        // 一条 assistant 并发两个工具 → 两条 Tool 聚为一个块
        let messages = vec![
            assistant(None, &[("a", "bash"), ("b", "bash")]),
            tool("a", "output A"),
            tool("b", "output B"),
        ];
        let chunks = chunk_messages(&messages);
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            super::Chunk::Step { observations, .. } => {
                assert_eq!(observations, &vec![Some("output A".into()), Some("output B".into())]);
            }
            other => panic!("期望 Step，得到 {other:?}"),
        }
    }

    #[test]
    fn step_atomic_injection_pairing() {
        // 两条独立 assistant 各带一个 tool → 两个块，配对不串
        let messages = vec![
            assistant(None, &[("a", "bash")]),
            tool("a", "A"),
            assistant(None, &[("b", "bash")]),
            tool("b", "B"),
        ];
        let chunks = chunk_messages(&messages);
        assert_eq!(chunks.len(), 2);
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
            Message::System { content: "sys".into() },
            Message::ContextSummary { content: "sum".into() },
            user("任务"),
        ];
        assert_eq!(chunk_messages(&messages).len(), 1);
    }
}
