//! Content-addressed artifact store for tool output resources.
//!
//! Tools emit [`shirley_agent_sdk::ResourceRef`]s pointing at artifacts stored here by their
//! `sha256` digest, never by a filesystem path: a path can expire (temp file deleted, workspace
//! moved) but a hash cannot. The store is global (`~/.shirley/artifacts`), so the same bytes are
//! stored once no matter how many sessions or projects reference them.
//!
//! Layout:
//!
//! ```text
//! <root>/blobs/<sha[0:2]>/<sha256>        the bytes (deduplicated)
//! <root>/blobs/<sha[0:2]>/<sha256>.mime   the IANA media type (sidecar)
//! ```
//!
//! Phase 1 scope: ingest + read + validated resolution for the artifact HTTP endpoint. Session
//! reference counting / per-session quotas / LRU are a later phase (`docs/...` design doc); the
//! directory layout already leaves room for a `refs/<session>/<sha>` tree.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use shirley_agent_sdk::{ArtifactId, ModelAccess, ResourceMeta, ResourceRef};

/// Handle to the on-disk artifact store. Cheap to clone; only carries the root path.
#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The default global store under the unified state root (`~/.shirley/artifacts`).
    #[must_use]
    pub fn global() -> Self {
        Self::new(crate::storage::global_root().join("artifacts"))
    }

    #[must_use]
    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// Absolute path of a blob (no existence check).
    #[must_use]
    pub fn blob_path(&self, id: &ArtifactId) -> PathBuf {
        let hex = id.hex();
        self.root.join("blobs").join(&hex[..2]).join(hex)
    }

    /// Resolve a bare 64-char lowercase hex digest to an existing blob path.
    ///
    /// This is the **only** way the HTTP layer turns an untrusted `{sha256}` route parameter into
    /// a path: it rejects anything that is not exactly 64 hex chars (so no `/`, `..`, etc. can
    /// appear) and returns `None` when the blob is absent.
    #[must_use]
    pub fn resolve(&self, sha_hex: &str) -> Option<PathBuf> {
        if sha_hex.len() != 64 || !sha_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let hex = sha_hex.to_ascii_lowercase();
        let path = self.root.join("blobs").join(&hex[..2]).join(&hex);
        path.exists().then_some(path)
    }

    /// The stored MIME type for a resolved blob, if a sidecar was written at ingest time.
    #[must_use]
    pub fn mime_of(&self, sha_hex: &str) -> Option<String> {
        let path = self.resolve(sha_hex)?;
        let sidecar = path.with_extension("mime");
        std::fs::read_to_string(sidecar)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    /// Store `bytes` and return a [`ResourceRef`] for them. Idempotent: an existing blob is reused.
    pub async fn ingest(
        &self,
        bytes: &[u8],
        mime: &str,
        alt: Option<String>,
        meta: ResourceMeta,
    ) -> std::io::Result<ResourceRef> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hex::encode(hasher.finalize());
        let id = ArtifactId::from_sha256_hex(digest);
        let path = self.blob_path(&id);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            // Write to a temp name first, then rename, so a reader never sees a partial blob.
            let tmp = path.with_extension("tmp");
            tokio::fs::write(&tmp, bytes).await?;
            tokio::fs::rename(&tmp, &path).await?;
            // Sidecar MIME (best-effort; a missing sidecar falls back to octet-stream).
            let _ = tokio::fs::write(path.with_extension("mime"), mime).await;
        }

        Ok(ResourceRef {
            artifact: id,
            mime: mime.to_owned(),
            bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            alt,
            meta,
            model: ModelAccess::Denied,
            render: None,
        })
    }
}

/// Map a file extension to a raster-image MIME type, if it is one we treat as an inline image.
///
/// SVG is intentionally excluded from the inline-image path (it is script-capable; the design
/// doc's security section defers it to download / sandboxed iframe).
#[must_use]
pub fn image_mime_for_extension(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "ico" => Some("image/x-icon"),
        "tif" | "tiff" => Some("image/tiff"),
        "avif" => Some("image/avif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_id_rejects_path_components() {
        assert!(ArtifactId::parse("sha256:../etc/passwd").is_none());
        assert!(ArtifactId::parse("sha256:abc").is_none());
        assert!(ArtifactId::parse(&format!("sha256:{}", "a".repeat(64))).is_some());
        assert!(ArtifactId::parse(&format!("md5:{}", "a".repeat(64))).is_none());
    }

    #[test]
    fn resolve_rejects_non_hex() {
        let store = ArtifactStore::new(std::env::temp_dir().join("shirley_artifact_test"));
        assert!(store.resolve("../../etc/passwd").is_none());
        assert!(store.resolve(&"a".repeat(64)).is_none()); // well-formed but absent
    }

    #[tokio::test]
    async fn ingest_is_content_addressed_and_deduplicated() {
        let root = std::env::temp_dir().join(format!(
            "shirley_artifact_ingest_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let store = ArtifactStore::new(root.clone());
        let a = store
            .ingest(b"hello", "text/plain", None, ResourceMeta::Other)
            .await
            .unwrap();
        let b = store
            .ingest(b"hello", "text/plain", None, ResourceMeta::Other)
            .await
            .unwrap();
        assert_eq!(a.artifact, b.artifact, "same bytes -> same artifact id");
        assert_eq!(a.bytes, 5);
        assert!(store.resolve(a.artifact.hex()).is_some());
        assert_eq!(
            store.mime_of(a.artifact.hex()).as_deref(),
            Some("text/plain")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
