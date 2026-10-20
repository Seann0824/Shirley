//! Personal-memory synchronization. Only explicitly personal Markdown entries cross the wire.
//!
//! The cloud is the single source of truth and the only place memory is *authored*. A local
//! directory is a disposable mirror: curator output is uploaded immediately ([`push_written`])
//! and the cloud's full record set is then installed over the local copy ([`install_cloud`]).
//! Periodic sync is pull-only -- the remote always overwrites local, including deletions, and
//! any local edit made while offline is discarded on the next pull. Nothing is ever forked into
//! a separate "conflict" copy.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::format::{Domain, Entry, MemoryError};
use super::store::{ENTRY_DIRS, MemoryStore, atomic_write, lock_root, validate_id};
use serde::{Deserialize, Serialize};

type Records = BTreeMap<String, Record>;
const LEDGER: &str = ".sync-ledger.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub revision: String,
    /// None is a tombstone, retained to prevent offline clients resurrecting deletions.
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Change {
    pub id: String,
    pub expected: Option<String>,
    pub content: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncRequest {
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub local: Option<String>,
    pub remote: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct SyncResponse {
    pub records: Records,
    pub conflicts: Vec<Conflict>,
    pub excluded: usize,
}

#[derive(Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub uploaded: usize,
    pub downloaded: usize,
    pub conflicts: usize,
    pub excluded: usize,
}

#[derive(Serialize, Deserialize)]
pub struct MemoryReview {
    pub id: String,
    pub revision: String,
    pub subject: String,
    pub body: String,
}

#[derive(Serialize, Deserialize)]
pub struct MemoryApproval {
    pub id: String,
    pub revision: String,
}

fn legacy_entries(root: &Path) -> Result<Vec<(std::path::PathBuf, Entry)>, MemoryError> {
    // Run the strict scan first: do not silently ignore corrupt files or symlinks during review.
    scan(root)?;
    MemoryStore::new(root)
        .list_entries_in(root)?
        .into_iter()
        .filter_map(|(path, entry)| {
            if entry.domain != Domain::Personal {
                return None;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) if !super::format::has_explicit_domain(&text) => Some(Ok((path, entry))),
                Ok(_) => None,
                Err(error) => Some(Err(error.into())),
            }
        })
        .collect()
}

pub fn review_legacy(root: &Path) -> Result<Vec<MemoryReview>, MemoryError> {
    reject_symlink(root)?;
    let _lock = lock_root(root)?;
    Ok(legacy_entries(root)?
        .into_iter()
        .map(|(_, entry)| MemoryReview {
            revision: revision(Some(&entry.render())),
            id: entry.id,
            subject: entry.subject,
            body: entry.body,
        })
        .collect())
}

pub fn confirm_personal(root: &Path, approvals: &[MemoryApproval]) -> Result<(), MemoryError> {
    reject_symlink(root)?;
    let _lock = lock_root(root)?;
    let entries = legacy_entries(root)?;
    if approvals.iter().any(|approval| {
        !entries.iter().any(|(_, entry)| {
            entry.id == approval.id && revision(Some(&entry.render())) == approval.revision
        })
    }) {
        return Err(io_error(
            "memory changed or is no longer unclassified; refresh the review before confirming",
        ));
    }
    for (path, entry) in entries {
        if approvals.iter().any(|approval| approval.id == entry.id) {
            atomic_write(&path, entry.render().as_bytes())?;
        }
    }
    rebuild(root)
}

fn io_error(e: impl std::fmt::Display) -> MemoryError {
    MemoryError::Parse(e.to_string())
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, MemoryError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io_error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.into()),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), MemoryError> {
    atomic_write(path, &serde_json::to_vec_pretty(value).map_err(io_error)?)
}

fn revision(content: Option<&str>) -> String {
    let bytes = match content {
        Some(s) => format!("entry:{s}"),
        None => "deleted".into(),
    };
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, bytes.as_bytes()).to_string()
}

fn canonical(id: &str, content: &str) -> Result<String, MemoryError> {
    validate_id(id)?;
    if content.len() > 128 * 1024 {
        return Err(io_error("memory entry exceeds 128 KiB"));
    }
    // Old records default to Personal in the parser. That is insufficient consent to upload.
    let front = content
        .strip_prefix('\u{feff}')
        .unwrap_or(content)
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---").map(|(front, _)| front))
        .ok_or_else(|| io_error("missing memory frontmatter"))?;
    if !front.lines().any(|line| line.trim() == "domain: personal") {
        return Err(io_error("sync requires explicit domain: personal"));
    }
    let mut entry = Entry::parse(content)?;
    if entry.id != id || entry.domain != Domain::Personal {
        return Err(io_error("memory identity/domain mismatch"));
    }
    // Usage statistics belong to each device, not the shared user profile.
    entry.usage_count = None;
    entry.utility = None;
    // Shared references retain their text but must never resolve against an unrelated
    // same-named session on another device/project. Source transcripts are not uploaded.
    for source in &mut entry.source {
        if !source.starts_with("shared-evidence:") {
            *source = format!("shared-evidence:{source}");
        }
    }
    Ok(entry.render())
}

fn reject_symlink(path: &Path) -> Result<(), MemoryError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(io_error("memory sync does not follow symlinks"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Strict scan: corrupt files abort synchronization instead of being mistaken for deletions.
/// Project or unclassified legacy entries are excluded, even if they sit in the global directory.
fn scan(root: &Path) -> Result<(BTreeMap<String, String>, BTreeSet<String>), MemoryError> {
    reject_symlink(root)?;
    let mut entries = BTreeMap::new();
    let mut excluded = BTreeSet::new();
    for dir in ENTRY_DIRS {
        let parent = root.join(dir);
        reject_symlink(&parent)?;
        let files = match std::fs::read_dir(parent) {
            Ok(files) => files,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for file in files {
            let path = file?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            reject_symlink(&path)?;
            let text = std::fs::read_to_string(&path)?;
            let entry = Entry::parse(&text)?;
            validate_id(&entry.id)?;
            if path.file_stem().and_then(|s| s.to_str()) != Some(entry.id.as_str()) {
                return Err(io_error("memory file name and id differ"));
            }
            match canonical(&entry.id, &text) {
                Ok(content) => {
                    if entries.insert(entry.id.clone(), content).is_some() {
                        return Err(io_error("duplicate personal memory id"));
                    }
                }
                Err(_) => {
                    excluded.insert(entry.id);
                }
            }
        }
    }
    if excluded.iter().any(|id| entries.contains_key(id)) {
        return Err(io_error("personal and excluded memories share an id"));
    }
    Ok((entries, excluded))
}

fn snapshot(root: &Path) -> Result<(Records, BTreeSet<String>), MemoryError> {
    let known: Records = read_json(&root.join(LEDGER))?;
    let (entries, excluded) = scan(root)?;
    if known.keys().any(|id| excluded.contains(id)) {
        return Err(io_error(
            "a previously shared memory is now unclassified/project; resolve its domain before syncing",
        ));
    }
    let mut records = Records::new();
    for id in known.keys().chain(entries.keys()) {
        let content = entries.get(id).cloned();
        records.insert(
            id.clone(),
            Record {
                revision: revision(content.as_deref()),
                content,
            },
        );
    }
    Ok((records, excluded))
}

/// Called with the root's OS write lock held. Never writes project directories.
fn apply(root: &Path, id: &str, content: Option<&str>) -> Result<(), MemoryError> {
    validate_id(id)?;
    let store = MemoryStore::new(root);
    let parsed = content
        .map(|text| canonical(id, text).and_then(|s| Entry::parse(&s)))
        .transpose()?;
    let target = parsed.as_ref().map(|entry| store.entry_path(entry));
    if parsed.is_none() {
        super::vector::invalidate(root, id)?;
    }
    if let Some(mut entry) = parsed {
        // Keep local usage metrics when installing a shared revision.
        if let Some((_, old)) = store.list_entries()?.into_iter().find(|(_, e)| e.id == id) {
            entry.usage_count = old.usage_count;
            entry.utility = old.utility;
        }
        store.write_entry_unlocked(&entry)?;
    }
    for dir in ENTRY_DIRS {
        let path = root.join(dir).join(format!("{id}.md"));
        if target.as_ref() == Some(&path) {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn rebuild(root: &Path) -> Result<(), MemoryError> {
    let store = MemoryStore::new(root);
    super::curator::rebuild_index_unlocked(&store)?;
    super::curator::rebuild_core_unlocked(&store)
}

/// Install the cloud's full record set as the local mirror. The cloud is authoritative: every
/// record overwrites the local copy, and any local personal entry absent from `records` is removed
/// (a local edit made offline is simply dropped). Project/legacy entries are never touched.
/// Called with the root's OS write lock held. Returns `(changed, excluded)`.
fn install_cloud(root: &Path, records: &Records) -> Result<(usize, usize), MemoryError> {
    let (current, excluded) = snapshot(root)?;
    // Validate the whole batch before touching disk.
    for (id, record) in records {
        validate_id(id)?;
        if excluded.contains(id) {
            return Err(io_error(format!(
                "remote memory {id} collides with an excluded local entry"
            )));
        }
        if let Some(content) = &record.content {
            canonical(id, content)?;
        }
        if record.revision != revision(record.content.as_deref()) {
            return Err(io_error("invalid remote memory revision"));
        }
    }
    let mut changed = 0;
    for (id, record) in records {
        if current.get(id) != Some(record) {
            apply(root, id, record.content.as_deref())?;
            changed += 1;
        }
    }
    // Cloud is the single source of truth: drop local entries the cloud no longer has.
    for id in current.keys() {
        if !records.contains_key(id) && !excluded.contains(id) {
            apply(root, id, None)?;
            changed += 1;
        }
    }
    write_json(&root.join(LEDGER), records)?;
    rebuild(root)?;
    Ok((changed, excluded.len()))
}

/// Cloud transaction: validate the entire batch before touching disk, then compare-and-swap.
/// Each file is atomic; ledger state can be reconstructed after an interrupted response.
pub fn exchange(root: &Path, request: SyncRequest) -> Result<SyncResponse, MemoryError> {
    if request.changes.len() > 2000 {
        return Err(io_error("too many sync changes"));
    }
    reject_symlink(root)?;
    let _lock = lock_root(root)?;
    let (mut records, excluded) = snapshot(root)?;
    let mut seen = BTreeSet::new();
    for change in &request.changes {
        validate_id(&change.id)?;
        if !seen.insert(&change.id) || excluded.contains(&change.id) {
            return Err(io_error("duplicate or excluded memory id in sync batch"));
        }
        if let Some(content) = &change.content {
            canonical(&change.id, content)?;
        }
    }
    // Persist known ids before mutation, so an interrupted deletion still has a tombstone.
    write_json(&root.join(LEDGER), &records)?;
    // The cloud is authoritative: apply every incoming change as last-write-wins. There is no
    // compare-and-swap and no fork into a separate conflict entry -- whichever write lands last wins.
    for change in request.changes {
        let content = change
            .content
            .as_deref()
            .map(|s| canonical(&change.id, s))
            .transpose()?;
        apply(root, &change.id, content.as_deref())?;
        records.insert(
            change.id,
            Record {
                revision: revision(content.as_deref()),
                content,
            },
        );
    }
    write_json(&root.join(LEDGER), &records)?;
    rebuild(root)?;
    Ok(SyncResponse {
        records,
        conflicts: Vec::new(),
        excluded: excluded.len(),
    })
}

/// Serializes syncs in this process; the OS lock additionally coordinates disk writes across processes.
static CLIENT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Upload freshly written entries, then install the cloud's authoritative record set back over the
/// local mirror. This is the *only* write path to the cloud: memory is authored on the cloud, and
/// the local directory is just the cache the model reads from. Entries outside `ids` are not sent;
/// they are still overwritten/removed by the returned cloud snapshot.
pub async fn push_written(
    global: &Path,
    remote: &crate::remote::RemoteConfig,
    ids: &[String],
) -> Result<SyncReport, String> {
    if ids.is_empty() {
        return Ok(SyncReport::default());
    }
    let _client = CLIENT_LOCK.lock().await;
    let root = global.join("memory");
    let changes = {
        let _lock = lock_root(&root).map_err(|e| e.to_string())?;
        let (current, excluded) = snapshot(&root).map_err(|e| e.to_string())?;
        let mut seen = BTreeSet::new();
        let mut changes = Vec::new();
        for id in ids {
            if excluded.contains(id) || !seen.insert(id.clone()) {
                continue;
            }
            if let Some(record) = current.get(id) {
                changes.push(Change {
                    id: id.clone(),
                    expected: None,
                    content: record.content.clone(),
                });
            }
        }
        changes
    };
    if changes.is_empty() {
        return Ok(SyncReport::default());
    }
    let uploaded = changes.len();
    let response = remote
        .post("api/memory/sync", &SyncRequest { changes })
        .await?;
    let response: SyncResponse = serde_json::from_value(response).map_err(|e| e.to_string())?;
    let (downloaded, excluded) = {
        let _lock = lock_root(&root).map_err(|e| e.to_string())?;
        install_cloud(&root, &response.records).map_err(|e| e.to_string())?
    };
    Ok(SyncReport {
        uploaded,
        downloaded,
        conflicts: 0,
        excluded,
    })
}

/// Periodic pull. The cloud is the single source of truth, so this never uploads: it fetches the
/// cloud's full record set and installs it over the local mirror, deletions included. Offline local
/// edits are discarded by design.
pub async fn synchronize(
    global: &Path,
    remote: &crate::remote::RemoteConfig,
) -> Result<SyncReport, String> {
    let _client = CLIENT_LOCK.lock().await;
    let root = global.join("memory");
    let response = remote
        .post(
            "api/memory/sync",
            &SyncRequest {
                changes: Vec::new(),
            },
        )
        .await?;
    let response: SyncResponse = serde_json::from_value(response).map_err(|e| e.to_string())?;
    let (downloaded, excluded) = {
        let _lock = lock_root(&root).map_err(|e| e.to_string())?;
        install_cloud(&root, &response.records).map_err(|e| e.to_string())?
    };
    Ok(SyncReport {
        uploaded: 0,
        downloaded,
        conflicts: 0,
        excluded,
    })
}

#[cfg(test)]
mod tests {
    use super::super::format::{Confidence, EntryStatus, EntryType};
    use super::*;
    use std::path::PathBuf;

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "shirley-sync-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(id: &str, body: &str) -> Entry {
        Entry {
            id: id.into(),
            entry_type: EntryType::Preference,
            domain: Domain::Personal,
            subject: "Preference".into(),
            created_at: "2026-10-19".into(),
            valid_from: None,
            supersedes: None,
            status: EntryStatus::Active,
            confidence: Confidence::High,
            scope: None,
            utility: None,
            usage_count: None,
            source: vec!["session.jsonl#turn:1".into()],
            body: body.into(),
        }
    }
    fn change(entry: &Entry, expected: Option<String>) -> Change {
        Change {
            id: entry.id.clone(),
            expected,
            content: Some(entry.render()),
        }
    }
    fn request(changes: Vec<Change>) -> SyncRequest {
        SyncRequest { changes }
    }

    #[test]
    fn shares_only_explicit_personal_entries_not_projects_legacy_or_sidecars() {
        let root = Temp::new();
        let store =
            MemoryStore::with_roots(vec![root.0.join("memory"), root.0.join("project-memory")]);
        store
            .write_entry(&entry("personal", "Use Chinese"))
            .unwrap();
        let mut project = entry("project", "Secret project design");
        project.domain = Domain::Project;
        store.write_entry(&project).unwrap();
        let legacy = entry("legacy", "Unclassified old memory");
        std::fs::write(
            store.entry_path(&legacy),
            legacy.render().replace("domain: personal\n", ""),
        )
        .unwrap();
        // Even a misplaced project-domain file in the global root cannot be uploaded.
        std::fs::write(
            store.primary_root().join("facts/misplaced.md"),
            project.render().replace("id: project", "id: misplaced"),
        )
        .unwrap();
        std::fs::write(store.primary_root().join("vectors.json"), "secret vector").unwrap();
        let result = exchange(store.primary_root(), request(vec![])).unwrap();
        assert_eq!(
            result.records.keys().cloned().collect::<Vec<_>>(),
            vec!["personal"]
        );
        assert_eq!(result.excluded, 2);
        assert!(store.entry_path(&project).exists());
    }

    #[test]
    fn concurrent_updates_are_last_write_wins_without_conflict_copies() {
        let root = Temp::new();
        let original = entry("language", "Chinese");
        let first = exchange(&root.0, request(vec![change(&original, None)])).unwrap();
        let base = first.records["language"].revision.clone();
        let cloud_edit = entry("language", "English");
        exchange(
            &root.0,
            request(vec![change(&cloud_edit, Some(base.clone()))]),
        )
        .unwrap();
        // A stale client writes with an outdated `expected`: the cloud is authoritative and
        // last-write-wins, so the newer value simply replaces the old one -- no conflict copy.
        let offline = entry("language", "Japanese");
        let result = exchange(&root.0, request(vec![change(&offline, Some(base))])).unwrap();
        assert!(result.conflicts.is_empty());
        assert_eq!(
            Entry::parse(result.records["language"].content.as_ref().unwrap())
                .unwrap()
                .body,
            "Japanese"
        );
        // Exactly one entry remains for the id; no forked `conflict-*` record is created.
        assert_eq!(result.records.len(), 1);
        assert!(!result.records.keys().any(|id| id.starts_with("conflict-")));
    }

    #[test]
    fn stale_offline_edit_wins_over_earlier_deletion_last_write_wins() {
        let root = Temp::new();
        let original = entry("language", "Chinese");
        let first = exchange(&root.0, request(vec![change(&original, None)])).unwrap();
        let base = first.records["language"].revision.clone();
        let deleted = exchange(
            &root.0,
            request(vec![Change {
                id: "language".into(),
                expected: Some(base.clone()),
                content: None,
            }]),
        )
        .unwrap();
        assert_eq!(deleted.records["language"].content, None);
        // Last-write-wins: the later offline edit is applied even though it was based on a
        // now-deleted revision -- no tombstone veto and no conflict copy.
        let offline = entry("language", "English");
        let result = exchange(&root.0, request(vec![change(&offline, Some(base))])).unwrap();
        assert!(result.conflicts.is_empty());
        assert_eq!(
            Entry::parse(result.records["language"].content.as_ref().unwrap())
                .unwrap()
                .body,
            "English"
        );
        assert!(!result.records.keys().any(|id| id.starts_with("conflict-")));
        let again = exchange(&root.0, request(vec![])).unwrap();
        assert!(again.records["language"].content.is_some());
    }

    #[test]
    fn rejected_batch_cannot_partially_write_or_escape_personal_root() {
        let root = Temp::new();
        let good = entry("good", "Chinese");
        let mut bad = entry("bad", "Project secret");
        bad.domain = Domain::Project;
        assert!(
            exchange(
                &root.0,
                request(vec![change(&good, None), change(&bad, None)])
            )
            .is_err()
        );
        assert!(MemoryStore::new(&root.0).list_entries().unwrap().is_empty());
        bad.domain = Domain::Personal;
        bad.id = "../../escaped".into();
        assert!(exchange(&root.0, request(vec![change(&bad, None)])).is_err());
    }

    #[test]
    fn corrupted_memory_aborts_instead_of_becoming_a_tombstone() {
        let root = Temp::new();
        let original = entry("language", "Chinese");
        exchange(&root.0, request(vec![change(&original, None)])).unwrap();
        std::fs::write(
            MemoryStore::new(&root.0).entry_path(&original),
            "broken entry",
        )
        .unwrap();
        assert!(exchange(&root.0, request(vec![])).is_err());
    }

    #[test]
    fn usage_statistics_do_not_change_shared_revision() {
        let mut original = entry("language", "Chinese");
        let before = canonical(&original.id, &original.render()).unwrap();
        original.usage_count = Some(40);
        original.utility = Some(0.9);
        assert_eq!(canonical(&original.id, &original.render()).unwrap(), before);
    }

    #[test]
    fn legacy_rewrites_stay_local_until_user_confirms_reviewed_content() {
        let root = Temp::new();
        let store = MemoryStore::new(&root.0);
        let mut original = entry("legacy", "An old preference");
        let path = store.write_entry(&original).unwrap();
        std::fs::write(&path, original.render().replace("domain: personal\n", "")).unwrap();
        let review = review_legacy(&root.0).unwrap();
        assert_eq!(review.len(), 1);
        original.usage_count = Some(4);
        store.write_entry(&original).unwrap();
        assert!(!super::super::format::has_explicit_domain(
            &std::fs::read_to_string(&path).unwrap()
        ));
        assert!(
            exchange(&root.0, request(vec![]))
                .unwrap()
                .records
                .is_empty()
        );
        // The user's earlier review cannot approve newly changed content.
        assert!(
            confirm_personal(
                &root.0,
                &[MemoryApproval {
                    id: review[0].id.clone(),
                    revision: review[0].revision.clone()
                }]
            )
            .is_err()
        );
        let review = review_legacy(&root.0).unwrap();
        confirm_personal(
            &root.0,
            &[MemoryApproval {
                id: review[0].id.clone(),
                revision: review[0].revision.clone(),
            }],
        )
        .unwrap();
        assert!(review_legacy(&root.0).unwrap().is_empty());
        assert!(
            exchange(&root.0, request(vec![]))
                .unwrap()
                .records
                .contains_key("legacy")
        );
    }

    #[test]
    fn competing_cloud_writers_are_serialized_by_os_lock() {
        let root = Temp::new();
        let original = entry("language", "Chinese");
        let first = exchange(&root.0, request(vec![change(&original, None)])).unwrap();
        let base = first.records["language"].revision.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads = ["English", "Japanese"]
            .into_iter()
            .map(|body| {
                let path = root.0.clone();
                let base = base.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    exchange(
                        &path,
                        request(vec![change(&entry("language", body), Some(base))]),
                    )
                    .unwrap()
                    .conflicts
                    .len()
                })
            })
            .collect::<Vec<_>>();
        // Last-write-wins under the OS lock: no conflicts, exactly one surviving revision.
        assert_eq!(
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .sum::<usize>(),
            0
        );
        assert_eq!(exchange(&root.0, request(vec![])).unwrap().records.len(), 1);
    }

    #[test]
    fn synced_edits_invalidate_only_their_local_vectors() {
        use super::super::vector::VectorStore;
        let root = Temp::new();
        let personal = root.0.join("memory");
        let project = root.0.join("project-memory");
        let original = entry("language", "Chinese");
        let first = exchange(&personal, request(vec![change(&original, None)])).unwrap();
        let mut vectors = VectorStore::new("model");
        vectors.upsert("language", vec![1.0, 0.0]);
        vectors.upsert("other", vec![0.0, 1.0]);
        vectors.save(&personal).unwrap();
        vectors.save(&project).unwrap();
        let edited = exchange(
            &personal,
            request(vec![change(
                &entry("language", "English"),
                Some(first.records["language"].revision.clone()),
            )]),
        )
        .unwrap();
        let current = VectorStore::load_merged(std::slice::from_ref(&personal), "model");
        assert!(current.get("language").is_none());
        assert!(current.get("other").is_some());
        assert!(
            VectorStore::load_merged(&[project], "model")
                .get("language")
                .is_some()
        );
        let mut vectors = current;
        vectors.upsert("language", vec![0.5, 0.5]);
        vectors.save(&personal).unwrap();
        exchange(
            &personal,
            request(vec![Change {
                id: "language".into(),
                expected: Some(edited.records["language"].revision.clone()),
                content: None,
            }]),
        )
        .unwrap();
        assert!(
            VectorStore::load_merged(&[personal], "model")
                .get("language")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn sync_rejects_symlinked_memory_files() {
        let root = Temp::new();
        let outside = root.0.join("outside.md");
        std::fs::write(&outside, entry("language", "secret").render()).unwrap();
        MemoryStore::ensure_layout_at(&root.0.join("memory")).unwrap();
        std::os::unix::fs::symlink(outside, root.0.join("memory/preferences/language.md")).unwrap();
        assert!(exchange(&root.0.join("memory"), request(vec![])).is_err());
    }

    #[cfg(feature = "server")]
    async fn cloud(root: PathBuf) -> (crate::remote::RemoteConfig, tokio::task::JoinHandle<()>) {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/api/memory/sync",
            post(move |Json(input): Json<SyncRequest>| {
                let root = root.clone();
                async move { Json(exchange(&root, input).unwrap()) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            crate::remote::RemoteConfig {
                url,
                token: String::new(),
                sync_personal_memory: true,
            },
            task,
        )
    }

    /// The only write path: upload the named entries, then the cloud's record set is installed back.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn push_written_uploads_then_mirrors_the_cloud() {
        let server = Temp::new();
        let client = Temp::new();
        let (remote, task) = cloud(server.0.join("memory")).await;
        let store = MemoryStore::new(client.0.join("memory"));
        store.write_entry(&entry("language", "Chinese")).unwrap();
        let report = push_written(&client.0, &remote, &["language".to_string()])
            .await
            .unwrap();
        assert_eq!(report.uploaded, 1);
        // The cloud now owns the entry and it was installed back locally.
        assert!(
            MemoryStore::new(server.0.join("memory"))
                .list_entries()
                .unwrap()
                .iter()
                .any(|(_, e)| e.id == "language" && e.body == "Chinese")
        );
        assert_eq!(
            Entry::parse(
                &std::fs::read_to_string(store.entry_path(&entry("language", ""))).unwrap()
            )
            .unwrap()
            .body,
            "Chinese"
        );
        task.abort();
    }

    /// Pull is authoritative: a local-only entry the cloud does not have is removed, and an offline
    /// local edit is overwritten by the cloud's value.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn pull_overwrites_local_and_drops_offline_edits() {
        let server = Temp::new();
        let client = Temp::new();
        let server_store = MemoryStore::new(server.0.join("memory"));
        server_store
            .write_entry(&entry("language", "Cloud English"))
            .unwrap();
        let (remote, task) = cloud(server.0.join("memory")).await;
        let store = MemoryStore::new(client.0.join("memory"));
        // Local-only entry: the cloud has never heard of it.
        store
            .write_entry(&entry("local-only", "Should be dropped"))
            .unwrap();
        // Offline edit of an entry the cloud owns.
        store
            .write_entry(&entry("language", "Offline Japanese"))
            .unwrap();
        let report = synchronize(&client.0, &remote).await.unwrap();
        assert_eq!(report.uploaded, 0);
        let listed = store.list_entries().unwrap();
        assert_eq!(listed.len(), 1, "local-only entry must be dropped");
        assert_eq!(listed[0].1.id, "language");
        assert_eq!(listed[0].1.body, "Cloud English", "remote overwrites local");
        task.abort();
    }

    /// A failed pull leaves the local mirror readable (it is only a cache); the next pull reconciles.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn offline_pull_keeps_local_mirror_readable() {
        let server = Temp::new();
        let client = Temp::new();
        let (remote, task) = cloud(server.0.join("memory")).await;
        let store = MemoryStore::new(client.0.join("memory"));
        store.write_entry(&entry("language", "Chinese")).unwrap();
        task.abort();
        assert!(synchronize(&client.0, &remote).await.is_err());
        assert!(store.entry_path(&entry("language", "")).exists());
    }

    /// Two devices converge because writes go through the cloud, not through local uploads.
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn two_devices_converge_through_cloud_writes() {
        let server = Temp::new();
        let a = Temp::new();
        let b = Temp::new();
        let (remote, task) = cloud(server.0.join("memory")).await;
        let a_store = MemoryStore::with_roots(vec![a.0.join("memory"), a.0.join("project/memory")]);
        let mut project = entry("project", "Local secret");
        project.domain = Domain::Project;
        let project_path = a_store.write_entry(&project).unwrap();
        let project_before = std::fs::read(&project_path).unwrap();

        a_store.write_entry(&entry("language", "Chinese")).unwrap();
        push_written(&a.0, &remote, &["language".to_string()])
            .await
            .unwrap();
        // Device B pulls the cloud and sees the entry.
        synchronize(&b.0, &remote).await.unwrap();
        let b_store = MemoryStore::new(b.0.join("memory"));
        assert_eq!(b_store.list_entries().unwrap().len(), 1);

        // B edits and pushes; A's next pull reflects it.
        b_store.write_entry(&entry("language", "English")).unwrap();
        push_written(&b.0, &remote, &["language".to_string()])
            .await
            .unwrap();
        synchronize(&a.0, &remote).await.unwrap();
        assert_eq!(
            a_store
                .list_entries_in(a_store.primary_root())
                .unwrap()
                .iter()
                .find(|(_, e)| e.id == "language")
                .unwrap()
                .1
                .body,
            "English"
        );
        // Project-domain memory never leaves the device.
        assert_eq!(std::fs::read(project_path).unwrap(), project_before);
        task.abort();
    }
}
