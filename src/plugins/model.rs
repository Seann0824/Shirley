//! Normalized plugin model: one shape for every external ecosystem.
//!
//! `docs/plugin-marketplace.md` decision 1: **only the normalized model is
//! recognized**. Claude Code marketplaces, the dsh registry, and the MCP registry
//! all look different on the wire; each is translated into [`PluginEntry`] at the
//! boundary, so the UI (and, later, the importer) never sees a per-ecosystem shape.

use serde::Serialize;

/// Which external ecosystem a source belongs to. This is *provenance*, not a
/// capability kind — capability kinds (skill / mcp / command / ...) are resolved
/// later by the importer, which is out of scope for this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
    /// Anthropic's Claude Code plugin marketplace (`marketplace.json`).
    ClaudeMarketplace,
    /// The dsh (DeepSeek Harness) community plugin registry.
    DshRegistry,
    /// The official Model Context Protocol server registry.
    McpRegistry,
}

/// The wire format a source's index file uses, which selects the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    /// Claude Code `marketplace.json`: `{ "plugins": [ { name, description, source, ... } ] }`.
    ClaudeMarketplaceJson,
    /// dsh registry: `{ "schemaVersion", "categories", "plugins": [ { id, name, owner, url, description{zh,en}, category, ... } ] }`.
    DshRegistryJson,
    /// MCP registry: `{ "servers": [ { "server": { name, description, title, version, remotes[] } } ] }`.
    McpRegistryJson,
}

/// A downloadable plugin source (a marketplace index). This is the *catalog of
/// catalogs*: the UI lists these, and fetching one expands it into
/// [`PluginEntry`]s. It records where the index lives and how to read it; the
/// actual plugin download / install is a later phase (`docs/plugin-marketplace-design.md` P3).
#[derive(Debug, Clone, Serialize)]
pub struct PluginSource {
    /// Stable id used by the frontend to request this source.
    pub id: String,
    /// Human-facing name.
    pub name: String,
    /// One-line description of what the source offers.
    pub description: String,
    /// The ecosystem this source belongs to.
    pub kind: PluginSourceKind,
    /// The URL of the index file (fetched as JSON).
    pub url: String,
    /// The homepage the user can visit for more context (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// The index wire format (selects the parser).
    pub format: SourceFormat,
    /// The index is large (the full dsh registry is ~23 MB); the UI may warn or
    /// require an explicit action before fetching.
    pub large: bool,
}

/// One normalized plugin entry, as shown in the UI.
///
/// Deliberately minimal: it carries what is needed to *display and identify* a
/// plugin, not what is needed to install it (that is the importer's job, a later
/// phase). `source` keeps the raw pointer the entry came from (a git URL, a
/// subdirectory, a `./` path) so a future installer can resolve it without a
/// re-fetch.
#[derive(Debug, Clone, Serialize)]
pub struct PluginEntry {
    /// The id of the [`PluginSource`] this entry came from.
    pub source_id: String,
    /// Plugin name (unique within a source, in practice).
    pub name: String,
    /// One-line description (may be empty when the upstream omits it).
    pub description: String,
    /// Author / owner, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Category / topic, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The upstream location the plugin can be fetched from (git URL, subdir, or
    /// a `./`-relative path within the source repo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The homepage / repository URL, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

impl PluginEntry {
    /// Convenience constructor used by the per-format parsers.
    pub(crate) fn new(source_id: &str, name: String) -> Self {
        Self {
            source_id: source_id.to_string(),
            name,
            description: String::new(),
            author: None,
            category: None,
            source: None,
            homepage: None,
        }
    }
}
