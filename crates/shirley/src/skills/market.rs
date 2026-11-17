//! The skill market: a curated list of skills from public repositories, installed into one bot.
//!
//! - **What is listed**: `market/skills.json`, compiled in, so the market works with no index
//!   host to run. `[market] index_url` (or `SHIRLEY_MARKET_INDEX`) points at another index of
//!   the same shape instead.
//! - **Where a skill goes**: `bots/<bot>/skills/<id>/`, next to the bot's spec. Only that bot sees
//!   it ([`installed_root`] is one of the bot's skill roots, see `AgentFactory::for_bot`). A skill
//!   a bot wrote for itself lives in the same directory but carries no market marker; it is never
//!   offered back to the market.
//! - **How it is fetched**: file by file, no git or archive tools on the server. The file list
//!   comes from the GitHub API, else from jsDelivr's listing; each file from GitHub's raw host,
//!   else the jsDelivr CDN (reachable where GitHub is slow or blocked).
//! - **Safety**: paths are checked to stay inside the skill directory, the size and file count
//!   are capped, and a download lands in a hidden temp directory that only becomes the skill
//!   after every file arrived and `SKILL.md` parses.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};

use super::parse::parse_skill;

const BUILTIN_INDEX: &str = include_str!("../../market/skills.json");
/// Records where an installed skill came from; its presence is what makes it a market skill.
const MARKER_FILE: &str = ".market.json";
const SKILL_FILE: &str = "SKILL.md";
const MAX_FILES: usize = 400;
const MAX_BYTES: u64 = 40 * 1024 * 1024;
const PARALLEL_DOWNLOADS: usize = 6;
const LIST_TIMEOUT: Duration = Duration::from_secs(12);
const FILE_TIMEOUT: Duration = Duration::from_secs(30);

/// One entry of the market index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MarketSkill {
    /// The directory name it installs as, which is also the skill's name.
    pub id: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub category: String,
    /// `owner/name` on GitHub.
    pub repo: String,
    #[serde(rename = "ref", default = "default_ref")]
    pub git_ref: String,
    /// The skill directory inside the repository.
    pub path: String,
    #[serde(default)]
    pub license: String,
}

fn default_ref() -> String {
    "main".to_owned()
}

#[derive(Deserialize)]
struct Index {
    skills: Vec<MarketSkill>,
}

/// A skill in a bot's own directory.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledSkill {
    /// Directory name (= skill name).
    pub id: String,
    pub description: String,
    /// The market entry it was installed from; `None` for a skill the bot or the user wrote.
    pub market: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    id: String,
    repo: String,
    #[serde(rename = "ref")]
    git_ref: String,
    path: String,
    installed_at: String,
}

/// The directory a bot's own skills live in.
pub fn installed_root(bot_root: &Path) -> PathBuf {
    bot_root.join("skills")
}

/// The market listing: the configured index if it loads, else the built-in one.
pub async fn index(index_url: Option<&str>) -> Vec<MarketSkill> {
    if let Some(url) = index_url.filter(|url| !url.trim().is_empty()) {
        let fetched = async {
            let text = client()?
                .get(url)
                .timeout(LIST_TIMEOUT)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| error.to_string())?
                .text()
                .await
                .map_err(|error| error.to_string())?;
            parse_index(&text)
        }
        .await;
        match fetched {
            Ok(skills) => return skills,
            Err(error) => eprintln!("[market] index {url} unavailable, using the built-in list: {error}"),
        }
    }
    parse_index(BUILTIN_INDEX).unwrap_or_default()
}

fn parse_index(text: &str) -> Result<Vec<MarketSkill>, String> {
    let index: Index = serde_json::from_str(text).map_err(|error| error.to_string())?;
    Ok(index
        .skills
        .into_iter()
        .filter(|skill| valid_id(&skill.id) && valid_repo(&skill.repo) && safe_relative(&skill.path))
        .collect())
}

/// What is in the bot's skill directory, market or not.
pub fn installed(bot_root: &Path) -> Vec<InstalledSkill> {
    let root = installed_root(bot_root);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut skills: Vec<InstalledSkill> = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let id = dir.file_name()?.to_str()?.to_owned();
            if id.starts_with('.') || !dir.is_dir() {
                return None;
            }
            let text = std::fs::read_to_string(dir.join(SKILL_FILE)).ok()?;
            let meta = parse_skill(&text, &id, dir.clone()).ok()?;
            let market = std::fs::read_to_string(dir.join(MARKER_FILE))
                .ok()
                .and_then(|text| serde_json::from_str::<Marker>(&text).ok())
                .map(|marker| marker.id);
            Some(InstalledSkill {
                id,
                description: meta.description,
                market,
            })
        })
        .collect();
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    skills
}

/// Download `skill` into the bot's skill directory, replacing an earlier install of it.
pub async fn install(bot_root: &Path, skill: &MarketSkill) -> Result<(), String> {
    let client = client()?;
    let files = list_files(&client, skill).await?;
    if !files.iter().any(|(path, _)| path == SKILL_FILE) {
        return Err(format!("「{}」的来源里没有 SKILL.md", skill.title));
    }
    if files.len() > MAX_FILES || files.iter().map(|(_, size)| size).sum::<u64>() > MAX_BYTES {
        return Err(format!("「{}」太大了，没有安装", skill.title));
    }

    let root = installed_root(bot_root);
    let staging = root.join(format!(".installing-{}-{}", skill.id, uuid::Uuid::new_v4()));
    let result = async {
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|error| error.to_string())?;
        // Owned values per download: the future must not borrow across the spawn boundary.
        let jobs: Vec<(reqwest::Client, MarketSkill, String, PathBuf)> = files
            .iter()
            .map(|(path, _)| (client.clone(), skill.clone(), path.clone(), staging.clone()))
            .collect();
        let downloads = stream::iter(jobs.into_iter().map(|(client, skill, path, staging)| {
            async move {
                let bytes = fetch_file(&client, &skill, &path).await?;
                let target = staging.join(&path);
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                tokio::fs::write(&target, bytes)
                    .await
                    .map_err(|error| error.to_string())
            }
        }))
        .buffer_unordered(PARALLEL_DOWNLOADS)
        .collect::<Vec<_>>()
        .await;
        downloads.into_iter().collect::<Result<Vec<()>, String>>()?;

        let text = tokio::fs::read_to_string(staging.join(SKILL_FILE))
            .await
            .map_err(|error| error.to_string())?;
        parse_skill(&text, &skill.id, staging.clone())
            .map_err(|error| format!("SKILL.md 无法解析：{error:?}"))?;
        let marker = Marker {
            id: skill.id.clone(),
            repo: skill.repo.clone(),
            git_ref: skill.git_ref.clone(),
            path: skill.path.clone(),
            installed_at: chrono::Utc::now().to_rfc3339(),
        };
        let marker = serde_json::to_vec_pretty(&marker).map_err(|error| error.to_string())?;
        tokio::fs::write(staging.join(MARKER_FILE), marker)
            .await
            .map_err(|error| error.to_string())?;

        let target = root.join(&skill.id);
        if tokio::fs::try_exists(&target).await.unwrap_or(false) {
            tokio::fs::remove_dir_all(&target)
                .await
                .map_err(|error| error.to_string())?;
        }
        tokio::fs::rename(&staging, &target)
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    result
}

/// Remove one of the bot's skills (market or its own).
pub fn uninstall(bot_root: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("invalid skill id".to_owned());
    }
    let dir = installed_root(bot_root).join(id);
    if !dir.join(SKILL_FILE).is_file() {
        return Err(format!("没有安装技能 {id}"));
    }
    std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("shirley/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())
}

/// Every file of the skill as `(path relative to the skill directory, size)`.
async fn list_files(client: &reqwest::Client, skill: &MarketSkill) -> Result<Vec<(String, u64)>, String> {
    match list_from_github(client, skill).await {
        Ok(files) if !files.is_empty() => Ok(files),
        github => {
            let jsdelivr = list_from_jsdelivr(client, skill).await;
            jsdelivr.map_err(|error| {
                let github = github.err().unwrap_or_else(|| "empty listing".to_owned());
                eprintln!("[market] listing {} failed: github: {github}; jsdelivr: {error}", skill.id);
                "连不上技能来源（GitHub 和 jsDelivr 都失败了），请稍后再试".to_owned()
            })
        }
    }
}

async fn list_from_github(client: &reqwest::Client, skill: &MarketSkill) -> Result<Vec<(String, u64)>, String> {
    #[derive(Deserialize)]
    struct Tree {
        tree: Vec<Entry>,
        #[serde(default)]
        truncated: bool,
    }
    #[derive(Deserialize)]
    struct Entry {
        path: String,
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        size: u64,
    }
    let url = format!(
        "https://api.github.com/repos/{}/git/trees/{}?recursive=1",
        skill.repo, skill.git_ref
    );
    let tree: Tree = get_json(client, &url).await?;
    if tree.truncated {
        return Err("tree listing truncated".to_owned());
    }
    Ok(within(
        &skill.path,
        tree.tree
            .into_iter()
            .filter(|entry| entry.kind == "blob")
            .map(|entry| (entry.path, entry.size)),
    ))
}

async fn list_from_jsdelivr(client: &reqwest::Client, skill: &MarketSkill) -> Result<Vec<(String, u64)>, String> {
    #[derive(Deserialize)]
    struct Listing {
        files: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        name: String,
        #[serde(default)]
        size: u64,
    }
    let url = format!(
        "https://data.jsdelivr.com/v1/packages/gh/{}@{}?structure=flat",
        skill.repo, skill.git_ref
    );
    let listing: Listing = get_json(client, &url).await?;
    Ok(within(
        &skill.path,
        listing
            .files
            .into_iter()
            .map(|entry| (entry.name.trim_start_matches('/').to_owned(), entry.size)),
    ))
}

/// Keep the files under `dir`, relative to it, and only paths that stay inside it.
fn within(dir: &str, files: impl Iterator<Item = (String, u64)>) -> Vec<(String, u64)> {
    let prefix = format!("{}/", dir.trim_matches('/'));
    let mut kept: Vec<(String, u64)> = files
        .filter_map(|(path, size)| {
            let relative = path.strip_prefix(&prefix)?.to_owned();
            (safe_relative(&relative) && !is_marker(&relative)).then_some((relative, size))
        })
        .collect();
    kept.sort();
    kept
}

fn is_marker(path: &str) -> bool {
    path == MARKER_FILE
}

async fn fetch_file(client: &reqwest::Client, skill: &MarketSkill, path: &str) -> Result<Vec<u8>, String> {
    let sources = [
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            skill.repo, skill.git_ref, skill.path, path
        ),
        format!(
            "https://cdn.jsdelivr.net/gh/{}@{}/{}/{}",
            skill.repo, skill.git_ref, skill.path, path
        ),
    ];
    let mut last = String::new();
    for url in &sources {
        match get_bytes(client, url).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last = error,
        }
    }
    eprintln!("[market] {}/{path}: {last}", skill.id);
    Err(format!("下载 {path} 失败，请稍后再试"))
}

async fn get_json<T: for<'de> Deserialize<'de>>(client: &reqwest::Client, url: &str) -> Result<T, String> {
    client
        .get(url)
        .timeout(LIST_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())
}

async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let bytes = client
        .get(url)
        .timeout(FILE_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?
        .bytes()
        .await
        .map_err(|error| error.to_string())?;
    Ok(bytes.to_vec())
}

/// A skill id is a directory name and a skill name: lowercase letters, digits, `-`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let ok = |part: Option<&str>| {
        part.is_some_and(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// A relative path that cannot leave its directory.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_index_parses_and_every_entry_is_installable() {
        let skills = parse_index(BUILTIN_INDEX).unwrap();
        assert!(skills.len() >= 10);
        let raw: serde_json::Value = serde_json::from_str(BUILTIN_INDEX).unwrap();
        assert_eq!(skills.len(), raw["skills"].as_array().unwrap().len(), "an entry was rejected");
        for skill in &skills {
            assert!(!skill.title.is_empty() && !skill.summary.is_empty(), "{}", skill.id);
            assert!(skill.path.ends_with(&skill.id), "{}: the directory name is the skill name", skill.id);
        }
    }

    #[test]
    fn listings_keep_only_files_inside_the_skill() {
        let files = within(
            "skills/pdf",
            [
                ("skills/pdf/SKILL.md".to_owned(), 10),
                ("skills/pdf/scripts/fill.py".to_owned(), 20),
                ("skills/pdfx/SKILL.md".to_owned(), 1),
                ("skills/pdf/../../etc/passwd".to_owned(), 1),
                ("skills/pdf/.market.json".to_owned(), 1),
                ("README.md".to_owned(), 1),
            ]
            .into_iter(),
        );
        assert_eq!(
            files,
            [("SKILL.md".to_owned(), 10), ("scripts/fill.py".to_owned(), 20)]
        );
    }

    #[test]
    fn hostile_index_entries_are_dropped() {
        let index = r#"{"skills":[
            {"id":"ok","title":"t","summary":"s","repo":"a/b","path":"skills/ok"},
            {"id":"../x","title":"t","summary":"s","repo":"a/b","path":"skills/x"},
            {"id":"y","title":"t","summary":"s","repo":"a/b/c","path":"skills/y"},
            {"id":"z","title":"t","summary":"s","repo":"a/b","path":"../z"}
        ]}"#;
        let ids: Vec<String> = parse_index(index).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["ok"]);
    }

    #[test]
    fn installed_lists_market_and_own_skills_and_uninstall_removes_one() {
        let bot = std::env::temp_dir().join(format!("shirley_market_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bot);
        let root = installed_root(&bot);
        for (id, marker) in [("pdf", true), ("mine", false)] {
            let dir = root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(SKILL_FILE), format!("---\nname: {id}\ndescription: does {id}\n---\nbody")).unwrap();
            if marker {
                std::fs::write(
                    dir.join(MARKER_FILE),
                    r#"{"id":"pdf","repo":"a/b","ref":"main","path":"skills/pdf","installed_at":""}"#,
                )
                .unwrap();
            }
        }
        std::fs::create_dir_all(root.join(".installing-x")).unwrap();
        let listed = installed(&bot);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "mine");
        assert_eq!(listed[0].market, None);
        assert_eq!(listed[1].market.as_deref(), Some("pdf"));
        assert_eq!(listed[1].description, "does pdf");

        assert!(uninstall(&bot, "../pdf").is_err());
        uninstall(&bot, "pdf").unwrap();
        assert_eq!(installed(&bot).len(), 1);
        assert!(uninstall(&bot, "pdf").is_err());
        let _ = std::fs::remove_dir_all(&bot);
    }
}
