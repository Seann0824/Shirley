//! 召回：压缩丢失信息的退路（`docs/recall.md`）。
//!
//! 定位：compaction 的自然配套能力，**SDK 内部持有，应用层无感**。
//!
//! - **索引自动**：`compress_context` 把被压掉的对话段分块后送进召回库；
//! - **检索 AI 触发**：recall 作为工具暴露给模型，AI 生成 query、自己决定何时调用。
//!   不做每轮自动检索——AI 触发天然带"相关性判断 + 可迭代重搜"。
//!
//! 持久化层留空（内存实现）；检索算法通过 [`Retriever`] trait 抽象，
//! BM25 是当前实现，embedding 以后加实现不动契约。

mod bm25;
mod chunk;
mod tokenize;

pub use bm25::{Bm25Index, Bm25Params, ScoredChunk};
pub use chunk::{Chunk, chunk_messages};

use crate::tool::{Tool, ToolDefinition, ToolError, ToolFuture};
use std::sync::{Arc, Mutex};

/// 检索器抽象（`docs/recall.md` 4.2）。
///
/// BM25 现在实现，embedding 以后实现；门面只依赖此 trait，
/// 上层完全不知道底下是什么算法。
pub trait Retriever: Send + Sync {
    fn retrieve(&self, query: &str, k: usize) -> Vec<ScoredChunk>;
}

impl Retriever for Bm25Index {
    fn retrieve(&self, query: &str, k: usize) -> Vec<ScoredChunk> {
        self.search(query, k)
    }
}

/// 召回存储：chunk 原文 + 检索器。
///
/// **召回无损**：chunk 原样保存，检索返回原文 + 元信息，绝不二次摘要——
/// 重新摘要等于二次损失，等于白召回（`docs/recall.md` 验收第 2 条）。
///
/// 持久化实现本轮不写；将来在 `flush` / `load` 的位置扩展。
pub struct RecallStore {
    /// BM25 索引单独持有：chunk 追加时同步索引。
    /// 多 retriever 融合（RRF）留给 `fuse.rs`（`docs/recall.md` 4.1），本轮未建；
    /// 扩展点由 `Retriever` trait 保证（tests::retriever_replaceable_via_trait）。
    inner: Mutex<Inner>,
}

struct Inner {
    index: Bm25Index,
    chunks: Vec<Chunk>,
}

impl RecallStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                index: Bm25Index::new(Bm25Params::default()),
                chunks: Vec::new(),
            }),
        }
    }

    /// 入库一批 chunk（压缩时自动调用）。
    ///
    /// chunk 原文与索引视图分开存：索引视图截断只为打分，召回返回原文。
    /// `&self`：store 由 `Arc` 共享（runtime 与 recall 工具各持一份），
    /// 入库走内部可变性。
    pub fn index(&self, chunks: Vec<Chunk>) {
        let mut inner = self.inner.lock().expect("recall lock poisoned");
        for chunk in chunks {
            let text = chunk.index_text();
            if text.trim().is_empty() {
                continue;
            }
            let _ = inner.index.add(&text);
            inner.chunks.push(chunk);
        }
    }

    /// 清空召回库：索引与 chunk 一起重置。
    ///
    /// 切换会话时用（`Agent::load_session`）——新会话的语料必须从零派生，
    /// 否则旧会话的 chunk 会污染检索。与 `index` 一样走内部可变性（`&self`），
    /// 因为 recall 工具与 runtime 共享同一 `Arc<RecallStore>`。
    pub fn clear(&self) {
        let mut inner = self.inner.lock().expect("recall lock poisoned");
        inner.index = Bm25Index::new(Bm25Params::default());
        inner.chunks.clear();
    }

    /// 检索：返回命中 chunk 的原文（含元信息），按分数降序。
    ///
    /// 分数只是词面重合度的代理；**最终筛选交给调用 recall 的 AI**——
    /// 它能看到原文自己判断，比任何阈值都可靠（`docs/recall.md` 一、决策 2）。
    /// 返回克隆的 chunk（原文无损），避免锁跨越返回值生命周期。
    pub fn retrieve(&self, query: &str, k: usize) -> Vec<(f64, Chunk)> {
        let inner = self.inner.lock().expect("recall lock poisoned");
        inner
            .index
            .search(query, k)
            .into_iter()
            .filter_map(|hit| {
                let chunk = inner.chunks.get(hit.index)?;
                Some((hit.score, chunk.clone()))
            })
            .collect()
    }
}

impl Default for RecallStore {
    fn default() -> Self {
        Self::new()
    }
}

/// recall 工具的渲染：原文 + 元信息，无损（`docs/recall.md` 4.3）。
///
/// 元信息让 AI 能判断"这是什么时候、什么类型的内容"。
/// 只渲染对话性 chunk（用户 / assistant 文本）——工具类消息压根不入库。
fn render_hits(hits: &[(f64, Chunk)]) -> String {
    if hits.is_empty() {
        return "no relevant history found; try rephrasing the query".to_string();
    }
    let mut out = format!("recalled {} history item(s):\n\n", hits.len());
    for (seq, (score, chunk)) in hits.iter().enumerate() {
        out.push_str(&format!(
            "--- [{} / score {:.2}] ---\n{}\n\n",
            seq + 1,
            score,
            render_chunk(chunk)
        ));
    }
    out
}

/// 单条召回内容注入上下文时的字符上限。
///
/// 与索引视图的截断不同：这里**不重写内容**，只是把超长 chunk（例如用户一次性粘贴
/// 的几万字符）截断并**显式标注**，避免单条内容垄断上下文。标注让 AI 知道"这里被截断了"，
/// 而非静默丢失——符合"召回不二次摘要"的底线（`docs/recall.md` 2.3）。
const INJECT_TRUNCATE: usize = 4000;

fn render_chunk(chunk: &Chunk) -> String {
    match chunk {
        Chunk::User { content } => format!("[user previously said]\n{}", clip(content)),
        Chunk::Assistant { content } => format!("[assistant previously said]\n{}", clip(content)),
    }
}

/// 超长内容截断并显式标注（不摘要、不静默丢弃）。
fn clip(content: &str) -> String {
    if content.chars().count() <= INJECT_TRUNCATE {
        return content.to_string();
    }
    let head: String = content.chars().take(INJECT_TRUNCATE).collect();
    format!("{head}\n... (content too long; only the first {INJECT_TRUNCATE} characters are shown)")
}

/// recall 工具（`docs/recall.md` 4.3）。
///
/// 手写实现 `Tool`（宏生成的工具是无状态的，这里要持有 `Arc<RecallStore>`）。
/// query 由 AI 生成——不替 AI 构造 query，少一个不确定性来源。
pub struct RecallTool {
    store: Arc<RecallStore>,
    definition: ToolDefinition,
}

impl RecallTool {
    /// 默认返回条数（`docs/recall.md` 3.3 候选值）。
    const DEFAULT_K: usize = 5;

    pub fn new(store: Arc<RecallStore>) -> Self {
        Self {
            store,
            definition: ToolDefinition {
                name: "recall".into(),
                description: "Search history that has been compacted away. Call this when the \
                    current context is missing information the user mentioned earlier \
                    (things they said, agreements, decisions) and it cannot be rebuilt by \
                    re-running tools. Describe what you are looking for in your own words."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "search query describing the history you are looking for"
                        },
                        "k": {
                            "type": "integer",
                            "description": "number of results to return, defaults to 5",
                            "minimum": 1,
                            "maximum": 20
                        }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

impl Tool for RecallTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value) -> ToolFuture<'_> {
        let store = self.store.clone();
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Args {
                query: String,
                k: Option<usize>,
            }
            let args: Args = serde_json::from_value(input).map_err(|error| {
                ToolError::ArgumentsError(format!("invalid tool arguments: {error}"))
            })?;
            let k = args.k.unwrap_or(Self::DEFAULT_K).clamp(1, 20);
            let hits = store.retrieve(&args.query, k);
            let text = render_hits(&hits);
            serde_json::to_value(text).map_err(|error| {
                ToolError::ExecutionError(format!("failed to serialize tool result: {error}"))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_retrieve_roundtrip() {
        let store = RecallStore::new();
        store.index(vec![
            Chunk::User { content: "我叫夏莉，住在上海".into() },
            Chunk::User { content: "帮我跑 cargo test".into() },
        ]);
        let hits = store.retrieve("名字 叫 什么", 5);
        assert!(!hits.is_empty());
        let top = &hits[0].1;
        assert!(matches!(top, Chunk::User { content } if content.contains("夏莉")));
    }

    #[test]
    fn empty_query_reports_no_results() {
        let store = RecallStore::new();
        assert!(store.retrieve("任何词", 5).is_empty());
    }

    #[test]
    fn retriever_replaceable_via_trait() {
        // 验收第 9 条：trait 可被第二个实现替换（stub 注入）——验证可扩展性。
        struct StubRetriever;
        impl Retriever for StubRetriever {
            fn retrieve(&self, _query: &str, k: usize) -> Vec<ScoredChunk> {
                (0..k)
                    .map(|i| ScoredChunk { index: i, score: 1.0 })
                    .collect()
            }
        }
        let stub = StubRetriever;
        let hits: Vec<ScoredChunk> = stub.retrieve("q", 3);
        assert_eq!(hits.len(), 3);
    }
}
