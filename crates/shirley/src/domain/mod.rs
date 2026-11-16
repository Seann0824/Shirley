//! Application domain state and types: sessions, the status projection,
//! the model catalog, and the task ledger.
//!
//! These describe *what the application is about* without rendering it.
//! Interfaces render these; `bootstrap` wires them together.

pub mod bot;
pub mod model_providers;
pub mod models;
pub mod session;
pub mod status;
pub mod todo;
