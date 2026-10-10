//! 语义向量的 sidecar 持久化（`docs/memory.md` §10 / V2.5）。
//!
//! **不引向量库**：每个记忆根下一个 `vectors.json`，存 `id → 向量` + 生成它用的
//! embedding 模型标识。理由与整个记忆系统一致——目录小、零外部服务、纯文件。
//! 检索时把向量读进内存做暴力余弦（条目数在几百量级，线性扫描足够；真到需要 ANN
//! 再说，不为可能的将来先付复杂度）。
//!
//! **模型绑定**：向量与生成它的模型强相关。若当前配置的 embedding 模型与 sidecar
//! 里记的不一致，则整份旧向量作废（`load_merged` 直接跳过），
//! 避免"用 A 模型的向量去查 B 模型的 query 向量"这种静默错误。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// sidecar 文件名（落在各记忆根下）。
pub const VECTORS_FILE: &str = "vectors.json";

/// 一份 sidecar 的内存形态。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    /// 生成这些向量的 embedding 模型标识（换模型即整份作废）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `id → 向量`。用 `BTreeMap` 让落盘顺序稳定（diff 友好）。
    #[serde(default)]
    pub vectors: BTreeMap<String, Vec<f32>>,
}

impl VectorStore {
    /// 空库（绑定给定模型）。
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            vectors: BTreeMap::new(),
        }
    }

    /// 从多根读取并合并：后出现的根覆盖先出现的（工作区覆盖全局）。
    ///
    /// 只认与 `expected_model` 一致的 sidecar——模型不匹配的整份丢弃（返回空库）。
    /// 任一文件损坏都跳过（不让一条坏文件拖垮整库）。
    pub fn load_merged(roots: &[PathBuf], expected_model: &str) -> Self {
        let mut merged = Self::new(expected_model);
        for root in roots {
            let path = root.join(VECTORS_FILE);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(store) = serde_json::from_str::<VectorStore>(&text) else {
                continue;
            };
            if store.model.as_deref() != Some(expected_model) {
                continue; // 模型不匹配 → 这份向量作废
            }
            for (id, vector) in store.vectors {
                merged.vectors.insert(id, vector);
            }
        }
        merged
    }

    /// 条目数。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// 是否为空。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// 取一条向量。
    pub fn get(&self, id: &str) -> Option<&[f32]> {
        self.vectors.get(id).map(Vec::as_slice)
    }

    /// 写入 / 覆盖一条向量。
    pub fn upsert(&mut self, id: impl Into<String>, vector: Vec<f32>) {
        self.vectors.insert(id.into(), vector);
    }

    /// 只保留 `keep` 里的 id（清理已删除条目的残留向量）。返回删除数。
    pub fn retain_ids<'a>(&mut self, keep: impl IntoIterator<Item = &'a str>) -> usize {
        let keep: std::collections::HashSet<&str> = keep.into_iter().collect();
        let before = self.vectors.len();
        self.vectors.retain(|id, _| keep.contains(id.as_str()));
        before - self.vectors.len()
    }

    /// 落盘到 `root/vectors.json`（自动建目录）。
    pub fn save(&self, root: &Path) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(root)?;
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(root.join(VECTORS_FILE), text)
    }
}

/// 余弦相似度。任一向量为零向量 / 维度不一致时返回 `0.0`（视为不相似，不 panic）。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_mem_vec_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0, "维度不一致 → 0");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "零向量 → 0");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let root = tmp("roundtrip");
        let mut store = VectorStore::new("bge-m3");
        store.upsert("a", vec![1.0, 2.0, 3.0]);
        store.upsert("b", vec![4.0, 5.0, 6.0]);
        store.save(&root).unwrap();

        let loaded = VectorStore::load_merged(std::slice::from_ref(&root), "bge-m3");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("a"), Some([1.0, 2.0, 3.0].as_slice()));
    }

    #[test]
    fn model_mismatch_discards_vectors() {
        let root = tmp("mismatch");
        let mut store = VectorStore::new("bge-m3");
        store.upsert("a", vec![1.0, 2.0]);
        store.save(&root).unwrap();

        // 换了 embedding 模型 → 旧向量整份作废。
        let loaded = VectorStore::load_merged(std::slice::from_ref(&root), "text-embedding-3-small");
        assert!(loaded.is_empty(), "模型不匹配应丢弃全部旧向量");
    }

    #[test]
    fn merge_later_root_overrides_and_retain_prunes() {
        let global = tmp("merge_global");
        let workspace = tmp("merge_workspace");
        let mut g = VectorStore::new("m");
        g.upsert("a", vec![1.0]);
        g.upsert("b", vec![2.0]);
        g.save(&global).unwrap();
        let mut w = VectorStore::new("m");
        w.upsert("a", vec![9.0]);
        w.save(&workspace).unwrap();

        let merged = VectorStore::load_merged(&[global, workspace], "m");
        assert_eq!(merged.get("a"), Some([9.0].as_slice()), "工作区覆盖全局");
        assert_eq!(merged.len(), 2);

        let mut pruned = merged.clone();
        let removed = pruned.retain_ids(["a"]);
        assert_eq!(removed, 1);
        assert_eq!(pruned.len(), 1);
    }
}
