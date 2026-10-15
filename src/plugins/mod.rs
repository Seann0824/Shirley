//! Plugins: the entry point + **download-source integration** for the plugin
//! ecosystem (**an application-layer capability**).
//!
//! Scope of this module (per the current task): surface the available plugin
//! *sources* (Claude Code marketplaces, the dsh community registry, the MCP
//! registry, ...) and fetch their catalogs into one **normalized** list the UI can
//! render. The actual download / compatibility logic (importer, MCP runtime,
//! git-source clone, lockfile) is deliberately **not** implemented here yet — see
//! `docs/plugin-marketplace.md` and `docs/plugin-marketplace-design.md` for the
//! phased plan (P0..P4).
//!
//! It lives in the app layer for the same reason as `skills/` / `todo.rs` /
//! `memory/`: *which sources to trust, how to normalize, what to show* is a
//! trade-off of "the coding-agent product", not a generic SDK fundamental. This
//! module adds **no new SDK public types**.
//!
//! Layering:
//! - [`model`] — the normalized [`model::PluginEntry`] + the [`model::PluginSource`]
//!   descriptor (id / name / url / wire format).
//! - [`sources`] — the static source registry + the real fetch/parse pipeline
//!   (`reqwest` GET -> per-format parser -> normalized entries).

pub mod model;
pub mod sources;

pub use model::{PluginEntry, PluginSource, PluginSourceKind, SourceFormat};
pub use sources::{fetch_source, registry};
