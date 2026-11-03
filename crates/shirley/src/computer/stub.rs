//! Fallback backend for targets without an accessibility-observer implementation.
//!
//! It exists so the crate builds everywhere and the tools fail *honestly*: the model gets
//! "this platform is not supported" instead of a silent no-op that looks like success.

use super::{ActOutcome, Action, AppInfo, ComputerError, Limits, Locator, Snapshot};

const UNSUPPORTED: &str = "computer use is currently implemented for macOS only (the \
accessibility API backend). On this platform the agent cannot observe or drive other apps' \
windows; use bash or the file tools instead.";

pub(super) fn is_trusted(_prompt: bool) -> bool {
    false
}

pub(super) fn apps() -> Result<Vec<AppInfo>, ComputerError> {
    Err(ComputerError::Unsupported(UNSUPPORTED.to_owned()))
}

pub(super) fn snapshot(
    _app: Option<&str>,
    _window: Option<&str>,
    _limits: Limits,
) -> Result<Snapshot, ComputerError> {
    Err(ComputerError::Unsupported(UNSUPPORTED.to_owned()))
}

pub(super) fn perform(
    _locator: &Locator,
    _action: Action,
    _value: Option<&str>,
) -> Result<ActOutcome, ComputerError> {
    Err(ComputerError::Unsupported(UNSUPPORTED.to_owned()))
}

pub(super) fn key(_keys: &str, _app: Option<&str>) -> Result<String, ComputerError> {
    Err(ComputerError::Unsupported(UNSUPPORTED.to_owned()))
}
