//! Application domain state and types: sessions, projects, the status
//! projection, the model catalog, the task ledger, and workspace search.
//!
//! These describe *what the application is about* (a session, a project, a
//! turn's health, a model, a task) without rendering it. Interfaces render
//! these; `bootstrap` wires them together.

pub mod model_providers;
pub mod models;
pub mod permission;
pub mod project;
pub mod session;
pub mod sidecar;
pub mod status;
pub mod todo;
pub mod workspace_search;
