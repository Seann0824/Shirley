//! Plugin source registry + real fetch/parse pipeline.
//!
//! This is the "download-source integration" half of the task: it lists the
//! known marketplaces and, on demand, fetches one index over HTTP and normalizes
//! it into [`PluginEntry`]s. It does **not** download plugins, resolve
//! dependencies, or check compatibility — that is a later phase
//! (`docs/plugin-marketplace-design.md` P3+).
//!
//! Design choices:
//! - **The registry is static** (no backend, `docs/plugin-marketplace.md`
//!   decision 4). A source is a URL + a format; adding one is a one-line change.
//! - **Fetch is bounded**: a per-request timeout and a hard byte cap
//!   ([`MAX_RESPONSE_BYTES`]), so a runaway/huge index cannot exhaust memory.
//! - **Parsing is per-format and defensive**: unknown fields are ignored, missing
//!   fields degrade to `None`, and an empty name is skipped. One bad entry never
//!   fails the whole source.
//! - **Real network is used**: there is no mock. Tests that hit the network are
//!   `#[ignore]`d and run explicitly with `--ignored`.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::model::{PluginEntry, PluginSource, PluginSourceKind, SourceFormat};

/// Default cap on entries returned per source (the UI does not need 18k rows at
/// once; the full dsh registry has ~18k plugins).
pub const DEFAULT_LIMIT: usize = 500;

/// Hard cap on a single fetched index body (bytes). The largest known source
/// (the full dsh registry) is ~23 MB, so this leaves headroom while still
/// bounding memory.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Per-request timeout for fetching an index.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// The static registry of known plugin sources.
///
/// Order is the display order. All URLs were verified reachable (HTTP 200) with
/// their current index shapes; see the module docs of `plugins` for provenance.
pub fn registry() -> Vec<PluginSource> {
    vec![
        PluginSource {
            id: "claude-official".into(),
            name: "Claude Code Plugins (official)".into(),
            description: "Anthropic's official Claude Code plugin marketplace.".into(),
            kind: PluginSourceKind::ClaudeMarketplace,
            url: "https://cdn.jsdelivr.net/gh/anthropics/claude-plugins-official@main/.claude-plugin/marketplace.json".into(),
            homepage: Some("https://github.com/anthropics/claude-plugins-official".into()),
            format: SourceFormat::ClaudeMarketplaceJson,
            large: false,
        },
        PluginSource {
            id: "claude-skills".into(),
            name: "Anthropic Agent Skills".into(),
            description: "Anthropic's example skill packs (document, design, API, ...).".into(),
            kind: PluginSourceKind::ClaudeMarketplace,
            url: "https://cdn.jsdelivr.net/gh/anthropics/skills@main/.claude-plugin/marketplace.json".into(),
            homepage: Some("https://github.com/anthropics/skills".into()),
            format: SourceFormat::ClaudeMarketplaceJson,
            large: false,
        },
        PluginSource {
            id: "dsh-community".into(),
            name: "dsh Community Registry".into(),
            description: "Community-scanned index of DeepSeek Harness (dsh) plugins.".into(),
            kind: PluginSourceKind::DshRegistry,
            url: "https://cdn.jsdelivr.net/gh/vbarter/dsh-plugin-registry@main/data/plugins-lite.json".into(),
            homepage: Some("https://dsplugin.app/".into()),
            format: SourceFormat::DshRegistryJson,
            large: true,
        },
        PluginSource {
            id: "mcp-registry".into(),
            name: "MCP Registry (official)".into(),
            description: "The official Model Context Protocol server registry.".into(),
            kind: PluginSourceKind::McpRegistry,
            url: "https://registry.modelcontextprotocol.io/v0/servers?limit=100".into(),
            homepage: Some("https://registry.modelcontextprotocol.io/".into()),
            format: SourceFormat::McpRegistryJson,
            large: false,
        },
    ]
}

/// Fetch one source and return its normalized, capped plugin list.
///
/// `limit` caps the number of entries (0 or `None` means [`DEFAULT_LIMIT`]).
/// Unknown source ids return an error listing the known ids.
pub async fn fetch_source(
    source_id: &str,
    limit: Option<usize>,
) -> Result<Vec<PluginEntry>, String> {
    let source = registry()
        .into_iter()
        .find(|s| s.id == source_id)
        .ok_or_else(|| format!("unknown plugin source: {source_id}"))?;
    let limit = match limit {
        Some(0) | None => DEFAULT_LIMIT,
        Some(n) => n,
    };
    fetch_and_parse(&source, limit).await
}

/// Fetch a single source's index and parse it. Exposed for tests and future
/// reuse (e.g. a per-source refresh command).
pub async fn fetch_and_parse(
    source: &PluginSource,
    limit: usize,
) -> Result<Vec<PluginEntry>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("shirley/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("failed to build http client: {e}"))?;

    let response = tokio::time::timeout(FETCH_TIMEOUT, client.get(&source.url).send())
        .await
        .map_err(|_| format!("fetching {} timed out", source.id))?
        .map_err(|e| format!("fetching {} failed: {e}", source.id))?;

    let status = response.status();
    let bytes = read_limited(response).await?;
    if !status.is_success() {
        return Err(format!("source {} returned {status}", source.id));
    }

    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("source {} returned invalid JSON: {e}", source.id))?;
    Ok(parse_entries(source, &value, limit))
}

/// Read a response body with a hard byte cap (streaming accumulation).
async fn read_limited(response: reqwest::Response) -> Result<Vec<u8>, String> {
    use futures::StreamExt;
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err("index exceeds the size cap".into());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read failed: {e}"))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err("index exceeds the size cap".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Dispatch to the parser for the source's format, then cap the result.
pub fn parse_entries(source: &PluginSource, value: &Value, limit: usize) -> Vec<PluginEntry> {
    let mut entries = match source.format {
        SourceFormat::ClaudeMarketplaceJson => parse_claude_marketplace(&source.id, value),
        SourceFormat::DshRegistryJson => parse_dsh_registry(&source.id, value),
        SourceFormat::McpRegistryJson => parse_mcp_registry(&source.id, value),
    };
    entries.truncate(limit);
    entries
}

// ---------------------------------------------------------------------------
// Claude Code marketplace.json
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ClaudeMarketplace {
    #[serde(default)]
    plugins: Vec<ClaudePlugin>,
}

#[derive(Deserialize)]
struct ClaudePlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: Option<ClaudeAuthor>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    /// Either a string (`"./plugins/x"`) or an object
    /// (`{source:"git-subdir", url, path, ref, sha}`).
    #[serde(default)]
    source: Option<Value>,
}

#[derive(Deserialize)]
struct ClaudeAuthor {
    #[serde(default)]
    name: Option<String>,
}

fn parse_claude_marketplace(source_id: &str, value: &Value) -> Vec<PluginEntry> {
    let Ok(marketplace) = serde_json::from_value::<ClaudeMarketplace>(value.clone()) else {
        return Vec::new();
    };
    marketplace
        .plugins
        .into_iter()
        .filter(|p| !p.name.trim().is_empty())
        .map(|p| {
            let mut entry = PluginEntry::new(source_id, p.name);
            entry.description = p.description;
            entry.author = p.author.and_then(|a| a.name);
            entry.category = p.category;
            entry.homepage = p.homepage;
            entry.source = p.source.map(source_to_string);
            entry
        })
        .collect()
}

/// Flatten Claude's `source` (string or object) into one display string.
fn source_to_string(value: Value) -> String {
    match value {
        Value::String(s) => s,
        Value::Object(map) => {
            // Prefer a URL + path (git-subdir), else the first string field.
            let url = map.get("url").and_then(Value::as_str);
            let path = map.get("path").and_then(Value::as_str);
            match (url, path) {
                (Some(url), Some(path)) => format!("{url}#{path}"),
                (Some(url), None) => url.to_string(),
                _ => map
                    .values()
                    .find_map(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// dsh community registry
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DshRegistry {
    #[serde(default)]
    plugins: Vec<DshPlugin>,
}

#[derive(Deserialize)]
struct DshPlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    description: Option<DshText>,
    #[serde(default)]
    category: Option<String>,
}

/// dsh descriptions are either a plain string or `{zh, en}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum DshText {
    Plain(String),
    Localized { zh: Option<String>, en: Option<String> },
}

impl DshText {
    fn display(&self) -> String {
        match self {
            DshText::Plain(s) => s.clone(),
            DshText::Localized { zh, en } => zh
                .clone()
                .or_else(|| en.clone())
                .unwrap_or_default(),
        }
    }
}

fn parse_dsh_registry(source_id: &str, value: &Value) -> Vec<PluginEntry> {
    let Ok(registry) = serde_json::from_value::<DshRegistry>(value.clone()) else {
        return Vec::new();
    };
    registry
        .plugins
        .into_iter()
        .filter(|p| !p.name.trim().is_empty())
        .map(|p| {
            let mut entry = PluginEntry::new(source_id, p.name);
            entry.description = p.description.map(|d| d.display()).unwrap_or_default();
            entry.author = p.owner;
            entry.category = p.category;
            entry.homepage = p.url.clone();
            entry.source = p.url;
            entry
        })
        .collect()
}

// ---------------------------------------------------------------------------
// MCP registry
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct McpRegistry {
    #[serde(default)]
    servers: Vec<McpServerWrapper>,
}

#[derive(Deserialize)]
struct McpServerWrapper {
    server: McpServer,
}

#[derive(Deserialize)]
struct McpServer {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    remotes: Vec<McpRemote>,
}

#[derive(Deserialize)]
struct McpRemote {
    #[serde(default)]
    url: Option<String>,
}

fn parse_mcp_registry(source_id: &str, value: &Value) -> Vec<PluginEntry> {
    let Ok(registry) = serde_json::from_value::<McpRegistry>(value.clone()) else {
        return Vec::new();
    };
    registry
        .servers
        .into_iter()
        .filter(|w| !w.server.name.trim().is_empty())
        .map(|w| {
            let server = w.server;
            let mut entry = PluginEntry::new(source_id, server.name);
            entry.description = server.description.unwrap_or_default();
            entry.category = Some("mcp".into());
            entry.homepage = server.remotes.first().and_then(|r| r.url.clone());
            entry.source = server.title;
            entry
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn claude_source() -> PluginSource {
        registry()
            .into_iter()
            .find(|s| s.id == "claude-official")
            .unwrap()
    }

    #[test]
    fn registry_has_the_expected_sources() {
        let ids: Vec<_> = registry().into_iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec!["claude-official", "claude-skills", "dsh-community", "mcp-registry"]
        );
    }

    #[test]
    fn parses_claude_marketplace_with_string_and_object_sources() {
        let value = json!({
            "name": "x",
            "plugins": [
                {
                    "name": "agent-sdk-dev",
                    "description": "dev kit",
                    "author": {"name": "Anthropic"},
                    "category": "development",
                    "homepage": "https://example.com",
                    "source": "./plugins/agent-sdk-dev"
                },
                {
                    "name": "adobe",
                    "source": {"source": "git-subdir", "url": "https://github.com/adobe/skills.git", "path": "plugins/creative-cloud"}
                },
                {"name": "  "}
            ]
        });
        let entries = parse_entries(&claude_source(), &value, 10);
        assert_eq!(entries.len(), 2, "empty names are skipped");
        assert_eq!(entries[0].name, "agent-sdk-dev");
        assert_eq!(entries[0].author.as_deref(), Some("Anthropic"));
        assert_eq!(entries[0].source.as_deref(), Some("./plugins/agent-sdk-dev"));
        assert_eq!(
            entries[1].source.as_deref(),
            Some("https://github.com/adobe/skills.git#plugins/creative-cloud")
        );
    }

    #[test]
    fn parses_dsh_registry_with_localized_description() {
        let value = json!({
            "schemaVersion": 2,
            "plugins": [
                {
                    "id": "a/b#dsh",
                    "name": "b#dsh",
                    "owner": "a",
                    "url": "https://github.com/a/b",
                    "description": {"zh": "中文描述", "en": "english"},
                    "category": "ui"
                },
                {"name": "plain", "description": "just a string"}
            ]
        });
        let source = registry().into_iter().find(|s| s.id == "dsh-community").unwrap();
        let entries = parse_entries(&source, &value, 10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].description, "中文描述");
        assert_eq!(entries[0].author.as_deref(), Some("a"));
        assert_eq!(entries[1].description, "just a string");
    }

    #[test]
    fn parses_mcp_registry_and_uses_first_remote_url() {
        let value = json!({
            "servers": [
                {
                    "server": {
                        "name": "ac.inference.sh/mcp",
                        "description": "run AI apps",
                        "title": "inference.sh",
                        "remotes": [
                            {"type": "streamable-http", "url": "https://sh.inference.ac"},
                            {"type": "streamable-http", "url": "https://api.inference.sh/mcp"}
                        ]
                    }
                }
            ]
        });
        let source = registry().into_iter().find(|s| s.id == "mcp-registry").unwrap();
        let entries = parse_entries(&source, &value, 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ac.inference.sh/mcp");
        assert_eq!(entries[0].homepage.as_deref(), Some("https://sh.inference.ac"));
        assert_eq!(entries[0].category.as_deref(), Some("mcp"));
    }

    #[test]
    fn limit_is_enforced() {
        let value = json!({
            "plugins": (0..10).map(|i| json!({"name": format!("p{i}")})).collect::<Vec<_>>()
        });
        let entries = parse_entries(&claude_source(), &value, 3);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn malformed_payload_yields_empty_not_panic() {
        let source = claude_source();
        assert!(parse_entries(&source, &json!({"plugins": "not-an-array"}), 10).is_empty());
        assert!(parse_entries(&source, &json!({}), 10).is_empty());
    }

    /// Real network test: fetches every registered source and checks it parses
    /// into at least one entry. Run with `cargo test -p Shirley plugins -- --ignored`.
    #[tokio::test]
    #[ignore = "hits the real network; run explicitly with --ignored"]
    async fn real_sources_are_reachable_and_parse() {
        for source in registry() {
            let entries = fetch_and_parse(&source, 5)
                .await
                .unwrap_or_else(|e| panic!("source {} failed: {e}", source.id));
            assert!(
                !entries.is_empty(),
                "source {} returned no entries",
                source.id
            );
            println!(
                "source {} -> {} entries (sample: {})",
                source.id,
                entries.len(),
                entries[0].name
            );
        }
    }
}
