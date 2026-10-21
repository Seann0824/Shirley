//! Desktop interface (Tauri 2 + React) — see `docs/desktop-interface.md`.
//!
//! A **sibling interface** to the TUI: both share the same
//! [`crate::bootstrap::AgentFactory`] (same model config / tools / session
//! directory), differing only in how they render. The desktop side drives each
//! session's own `Agent::run_stream` via Tauri commands and translates
//! `AgentEvent` through [`wire`] into webview events pushed to the frontend.
//!
//! Build gate: the Tauri shell only compiles when the `desktop` feature is on;
//! a default (TUI-only) build never touches the webview toolchain. The frontend
//! bundle is built from `web/` (Vite) into `web/dist`.

pub mod wire;

#[cfg(feature = "desktop")]
pub mod browser;

use crate::bootstrap::AgentFactory;

#[cfg(feature = "desktop")]
mod shell;

/// Start the desktop interface (blocking: enters the Tauri event loop, must be
/// called on the main thread).
#[cfg(feature = "desktop")]
pub fn run(factory: AgentFactory) -> std::io::Result<()> {
    shell::run(factory)
}

/// Fallback when the `desktop` feature is not enabled: report the error
/// honestly, never degrade silently.
#[cfg(not(feature = "desktop"))]
pub fn run(_factory: AgentFactory) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "desktop interface not compiled; build with `cargo run --features desktop -- --desktop` (see docs/desktop-interface.md)",
    ))
}
