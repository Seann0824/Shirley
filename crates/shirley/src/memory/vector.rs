//! Sidecar persistence of semantic vectors (`docs/memory.md` 10 / V2.5).
//!
//! **No vector database**: one `vectors.json` per memory root, storing `id -> vector` + the identifier of the
//! embedding model that generated it. The rationale matches the whole memory system -- the directory is small, zero external services, pure files.
//! At retrieval time the vectors are read into memory and brute-force cosine is computed (the entry count is on the order of hundreds, so a linear scan suffices; when ANN is
//! truly needed, revisit -- do not pay complexity upfront for a possible future).
//!
//! **Model binding**: vectors are strongly tied to the model that generated them. If the currently configured embedding model differs from
//! the one recorded in the sidecar, the entire old vector set is discarded (`load_merged` skips it),
//! avoiding silent errors like "querying B's query vector with A's vectors".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// sidecar file name (stored under each memory root).
pub const VECTORS_FILE: &str = "vectors.json";

/// The in-memory form of one sidecar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    /// Identifier of the embedding model that generated these vectors (changing the model invalidates the whole set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `id -> vector`. Uses a `BTreeMap` to keep the on-disk order stable (diff-friendly).
    #[serde(default)]
    pub vectors: BTreeMap<String, Vec<f32>>,
}

impl VectorStore {
    /// Empty store (bound to the given model).
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            vectors: BTreeMap::new(),
        }
    }

    /// Read and merge from multiple roots: a later root overrides an earlier one (workspace overrides global).
    ///
    /// Only sidecars matching `expected_model` are accepted -- a whole set with a mismatched model is discarded (returns an empty store).
    /// Any corrupt file is skipped (do not let one bad file bring down the whole store).
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
                continue; // model mismatch -> discard this vector set
            }
            for (id, vector) in store.vectors {
                merged.vectors.insert(id, vector);
            }
        }
        merged
    }

    /// Number of entries.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Whether it is empty.
    #[allow(
        dead_code,
        reason = "kept for tests, diagnostics, or a future scenario"
    )]
    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    /// Get one vector.
    pub fn get(&self, id: &str) -> Option<&[f32]> {
        self.vectors.get(id).map(Vec::as_slice)
    }

    /// Write / overwrite one vector.
    pub fn upsert(&mut self, id: impl Into<String>, vector: Vec<f32>) {
        self.vectors.insert(id.into(), vector);
    }

    /// Keep only the ids in `keep` (clean up residual vectors of deleted entries). Returns the number removed.
    pub fn retain_ids<'a>(&mut self, keep: impl IntoIterator<Item = &'a str>) -> usize {
        let keep: std::collections::HashSet<&str> = keep.into_iter().collect();
        let before = self.vectors.len();
        self.vectors.retain(|id, _| keep.contains(id.as_str()));
        before - self.vectors.len()
    }

    /// Persist to `root/vectors.json` (creates the directory automatically).
    pub fn save(&self, root: &Path) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(root)?;
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        super::store::atomic_write(&root.join(VECTORS_FILE), text.as_bytes())
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

/// Called with the memory root's write lock held, before changing/removing an entry.
pub(super) fn invalidate(root: &Path, id: &str) -> Result<(), std::io::Error> {
    let path = root.join(VECTORS_FILE);
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let Ok(mut store) = serde_json::from_str::<VectorStore>(&text) else {
        return Ok(());
    };
    if store.vectors.remove(id).is_some() {
        store.save(root)?;
    }
    Ok(())
}

/// Cosine similarity. Returns `0.0` when either vector is a zero vector / dimensions differ (treated as dissimilar, no panic).
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
    #[expect(
        clippy::float_cmp,
        reason = "exact 0.0 is the documented sentinel for dimension-mismatch / zero-vector"
    )]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0, "dimension mismatch -> 0");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "zero vector -> 0");
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

        // changed the embedding model -> the old vectors are discarded entirely.
        let loaded =
            VectorStore::load_merged(std::slice::from_ref(&root), "text-embedding-3-small");
        assert!(
            loaded.is_empty(),
            "a model mismatch should discard all old vectors"
        );
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
        assert_eq!(
            merged.get("a"),
            Some([9.0].as_slice()),
            "workspace overrides global"
        );
        assert_eq!(merged.len(), 2);

        let mut pruned = merged.clone();
        let removed = pruned.retain_ids(["a"]);
        assert_eq!(removed, 1);
        assert_eq!(pruned.len(), 1);
    }
}
