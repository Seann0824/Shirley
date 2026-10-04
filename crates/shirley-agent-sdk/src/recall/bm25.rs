//! BM25 检索器（`docs/recall.md` 第三节）。
//!
//! 把"消息召回"重述为标准检索问题：query = AI 生成的检索词，
//! documents = chunk 集合，输出 = top-k。BM25 的 IDF 恰好放大
//! 符号名 / 报错码 / 文件路径这类高区分度信号，是 coding agent
//! 历史检索的合适起点。
//!
//! 已知短板（`docs/recall.md` 3.2）：只做词面匹配（同义改写召回不了，
//! 等 embedding 补）；chunk 长度差异极端导致长度归一化失真（`b` 取 0.5 缓解）。

use super::tokenize::tokenize;
use std::collections::HashMap;

/// 命中的 chunk：分数 + 在召回库中的序号。
///
/// 分数只是词面重合度的代理，**不是相关性的真理**；
/// 最终筛选交给调用 recall 的 AI（它能看到原文自己判断）。
#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub index: usize,
    pub score: f64,
}

/// BM25 参数（`docs/recall.md` 3.3）。
///
/// `b` 低于经典 0.75：chunk 长度差异极端（5 token 的 User vs 截断后的长输出），
/// 减弱长度惩罚，避免真正重要的长内容被压制。
#[derive(Debug, Clone, Copy)]
pub struct Bm25Params {
    /// TF 饱和度。经典默认 1.2。
    pub k1: f64,
    /// 长度归一化强度。默认 0.5（低于经典 0.75，理由见上）。
    pub b: f64,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.5 }
    }
}

/// 一篇"文档"在索引里的形态：词频表 + 长度。
struct IndexedDoc {
    /// 词元总数（重复计数），用于长度归一化
    len: usize,
}

/// BM25 检索器：内存倒排索引，索引时算好词频，查询时打分。
pub struct Bm25Index {
    params: Bm25Params,
    docs: Vec<IndexedDoc>,
    /// 词元 -> [(文档序号, 词频)]。打分只碰 query 命中的文档，不全表扫描。
    postings: HashMap<String, Vec<(usize, u32)>>,
    /// 语料总词元数（含重复），算平均文档长度用。
    total_len: usize,
}

impl Bm25Index {
    pub fn new(params: Bm25Params) -> Self {
        Self {
            params,
            docs: Vec::new(),
            postings: HashMap::new(),
            total_len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// 索引一篇文档，返回它的序号。
    ///
    /// 追加语义；调用方保证"同一 chunk 不重复入库"。
    pub fn add(&mut self, text: &str) -> usize {
        let tokens = tokenize(text);
        let mut tf: HashMap<String, u32> = HashMap::new();
        for token in &tokens {
            *tf.entry(token.clone()).or_insert(0) += 1;
        }

        let index = self.docs.len();
        let len = tokens.len();
        self.total_len += len;
        for (token, count) in &tf {
            self.postings
                .entry(token.clone())
                .or_default()
                .push((index, *count));
        }
        self.docs.push(IndexedDoc { len });
        index
    }

    /// 检索 top-k。
    ///
    /// 经典 BM25 打分：
    /// ```text
    /// score(q, d) = Σ_t∈q  idf(t) * tf(t,d) * (k1+1) / (tf(t,d) + k1*(1 - b + b*|d|/avgdl))
    /// ```
    /// query 词去重（重复的 query 词不放大权重，那是 query 改写的事）。
    pub fn search(&self, query: &str, k: usize) -> Vec<ScoredChunk> {
        if self.docs.is_empty() || k == 0 {
            return Vec::new();
        }
        let avgdl = self.total_len as f64 / self.docs.len() as f64;
        if avgdl <= 0.0 {
            return Vec::new();
        }

        let mut query_tokens = tokenize(query);
        query_tokens.sort();
        query_tokens.dedup();

        // doc -> 累计分数。
        let mut scores: HashMap<usize, f64> = HashMap::new();
        for token in &query_tokens {
            let Some(hits) = self.postings.get(token) else {
                continue;
            };
            let idf = self.idf(hits.len());
            for (doc, tf) in hits {
                let tf = *tf as f64;
                let doc_len = self.docs[*doc].len as f64;
                let norm =
                    self.params.k1 * (1.0 - self.params.b + self.params.b * doc_len / avgdl);
                let score = idf * tf * (self.params.k1 + 1.0) / (tf + norm);
                *scores.entry(*doc).or_insert(0.0) += score;
            }
        }

        let mut ranked: Vec<ScoredChunk> = scores
            .into_iter()
            .map(|(index, score)| ScoredChunk { index, score })
            .collect();
        // 稳定排序：同分时序号小（更早）的在前，行为可预测。
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.index.cmp(&b.index))
        });
        ranked.truncate(k);
        ranked
    }

    /// BM25 idf：`ln(1 + (N - df + 0.5) / (df + 0.5))`。
    ///
    /// 取非负形式，避免极高频词（df ≈ N）出现负权重互相抵消的病态行为。
    fn idf(&self, df: usize) -> f64 {
        let n = self.docs.len() as f64;
        let df = df as f64;
        (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_index_returns_nothing() {
        let index = Bm25Index::new(Bm25Params::default());
        assert!(index.search("任何", 5).is_empty());
    }

    #[test]
    fn top1_hit_for_distinctive_token() {
        let mut index = Bm25Index::new(Bm25Params::default());
        index.add("error error error file file common");
        index.add("夏莉 上海 居住");
        index.add("cargo build release");
        let hits = index.search("夏莉", 3);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].index, 1);
    }

    #[test]
    fn idf_favors_rare_tokens() {
        // 同一 query 词在两篇文档词频相同，但出现范围更小的词应更易胜出
        let mut index = Bm25Index::new(Bm25Params::default());
        index.add("alpha rare");
        index.add("alpha alpha beta");
        index.add("alpha alpha gamma");
        // rare 只在 doc0，alpha 在全部 → "rare" 应把 doc0 顶到第一
        let hits = index.search("rare alpha", 3);
        assert_eq!(hits[0].index, 0);
    }

    #[test]
    fn k_truncates() {
        let mut index = Bm25Index::new(Bm25Params::default());
        index.add("a");
        index.add("a");
        index.add("a");
        let hits = index.search("a", 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn zero_k_returns_empty() {
        let mut index = Bm25Index::new(Bm25Params::default());
        index.add("a");
        assert!(index.search("a", 0).is_empty());
    }
}
