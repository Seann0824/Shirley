//! Computer use: let the agent observe a desktop app and act on it.
//!
//! The design is a native re-implementation of the "accessibility tree + element refs" model
//! proven by `vercel-labs/agent-desktop`, minus its multi-platform surface:
//!
//! - **Observe** (`snapshot`): walk the OS accessibility tree and render it as a compact text
//!   tree. Every node the agent can act on gets a short ref (`@e12`).
//! - **Act** (`act`): perform an accessibility action on a ref. Actions go through the
//!   accessibility API, so the pointer does not move and focus is not stolen -- the user can keep
//!   typing while the agent works. This is why the capability is text-based rather than
//!   screenshot-based: a screenshot loop costs tens of thousands of tokens per step and a
//!   vision-less model cannot read it at all.
//! - **Refs are snapshot-scoped**: a ref resolves to a *path* (pid + window ordinal + child
//!   indexes) that is re-walked on every action. A ref from an older snapshot therefore fails
//!   loudly (`STALE_REF`) instead of silently clicking the wrong control.
//!
//! Layering: this module is platform-neutral (types, ref table, rendering, `Engine`); the OS
//! specific part lives in [`macos`] (`AXUIElement` + `CGEvent`). Every other target compiles against
//! [`stub`], which reports the capability as unsupported so the tools degrade loudly rather than
//! lying about what they did.

use std::collections::HashMap;
use std::sync::Mutex;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
use stub as platform;

/// How many accessibility nodes one snapshot may return before it stops walking.
pub(crate) const DEFAULT_MAX_NODES: usize = 600;
/// How deep the walk goes. Web/Electron stacks wrap content in anonymous containers, so this is
/// generous on purpose; the node budget is the real guard.
pub(crate) const DEFAULT_MAX_DEPTH: usize = 30;
/// Character budget for the rendered tree (roughly 1.5~2k tokens).
pub(crate) const DEFAULT_MAX_CHARS: usize = 6000;

/// Failure of a computer-use request. The message is written for the model to read, so it says
/// what to do next, not just what went wrong.
///
/// `NotTrusted` carries the platform's own wording: "whose permission is it, and where do I click
/// to grant it" is exactly the part that is OS specific (macOS: Accessibility in System Settings;
/// Windows has no comparable grant, only UIPI elevation). Keeping that copy in the backend is what
/// lets a second backend exist without rewording the neutral error.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ComputerError {
    #[error("{0}")]
    NotTrusted(String),
    #[error("{0}")]
    Failed(String),
    #[error("{0}")]
    Stale(String),
    #[error("{0}")]
    Unsupported(String),
}

impl ComputerError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub(crate) fn stale(message: impl Into<String>) -> Self {
        Self::Stale(message.into())
    }
}

/// One app that owns at least one on-screen window.
#[derive(Debug, Clone)]
pub(crate) struct AppInfo {
    pub(crate) pid: i32,
    pub(crate) name: String,
    pub(crate) windows: usize,
    pub(crate) frontmost: bool,
}

/// One node of an accessibility tree.
///
/// `path` is the child-index path from the window root; it is what a ref resolves to, so it must
/// survive the snapshot (it does -- it is plain data, not a native pointer).
#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    pub(crate) role: String,
    pub(crate) title: Option<String>,
    pub(crate) value: Option<String>,
    pub(crate) actions: Vec<String>,
    pub(crate) settable: bool,
    pub(crate) enabled: bool,
    pub(crate) children: Vec<usize>,
    pub(crate) path: Vec<u32>,
    pub(crate) depth: usize,
}

/// A snapshot of one window's accessibility tree.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub(crate) app: String,
    pub(crate) pid: i32,
    pub(crate) window_title: String,
    pub(crate) window_index: usize,
    pub(crate) nodes: Vec<Node>,
    /// True when the walk stopped because it ran out of budget or time.
    pub(crate) partial: bool,
}

/// Budget for one snapshot walk.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) max_nodes: usize,
    pub(crate) max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: DEFAULT_MAX_NODES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// What a ref points at. Deliberately *not* a native element handle: a pointer would dangle as
/// soon as the app rebuilt its view, and re-walking a path turns that into a clean `STALE_REF`.
#[derive(Debug, Clone)]
pub(crate) struct Locator {
    pub(crate) pid: i32,
    pub(crate) window: usize,
    pub(crate) path: Vec<u32>,
    pub(crate) role: String,
    pub(crate) label: String,
}

/// An action the agent can request on a ref.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Activate the element (AXPress, falling back to confirm/show-menu style actions).
    Press,
    /// Replace the element's value (text fields, sliders, checkboxes).
    SetValue,
    /// Give the element keyboard focus, then type into it.
    Type,
    /// Move keyboard focus to the element.
    Focus,
    /// Select the element (rows, tabs, radio buttons).
    Select,
    /// Open the element's context menu.
    ShowMenu,
    /// Increase / decrease a stepper or slider.
    Increment,
    Decrement,
}

impl Action {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "click" | "press" | "tap" => Some(Self::Press),
            "set_value" | "setvalue" | "set" => Some(Self::SetValue),
            "type" | "enter_text" | "input" => Some(Self::Type),
            "focus" => Some(Self::Focus),
            "select" | "check" | "toggle" => Some(Self::Select),
            "show_menu" | "context_menu" | "right_click" => Some(Self::ShowMenu),
            "increment" => Some(Self::Increment),
            "decrement" => Some(Self::Decrement),
            _ => None,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Press => "click",
            Self::SetValue => "set_value",
            Self::Type => "type",
            Self::Focus => "focus",
            Self::Select => "select",
            Self::ShowMenu => "show_menu",
            Self::Increment => "increment",
            Self::Decrement => "decrement",
        }
    }
}

/// Result of performing an action, rendered for the model.
#[derive(Debug, Clone)]
pub(crate) struct ActOutcome {
    /// What actually changed, verified by reading the element back where possible.
    pub(crate) detail: String,
}

#[derive(Debug, Default)]
struct RefState {
    refs: HashMap<String, Locator>,
    counter: u64,
}

/// The computer-use engine: one per agent, shared by the computer tools.
///
/// Holds only the ref table (plain data). The platform layer is stateless, so `Engine` is
/// `Send + Sync` without unsafe.
#[derive(Debug)]
pub(crate) struct Engine {
    state: Mutex<RefState>,
}

impl Engine {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(RefState::default()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, RefState>, ComputerError> {
        self.state
            .lock()
            .map_err(|error| ComputerError::failed(format!("ref table lock is poisoned: {error}")))
    }

    /// Whether the process may drive other apps, and what to do about it if not.
    pub(crate) fn trust(&self, prompt: bool) -> bool {
        platform::is_trusted(prompt)
    }

    /// List apps that currently own on-screen windows, frontmost first.
    pub(crate) fn apps(&self) -> Result<String, ComputerError> {
        let apps = platform::apps()?;
        if apps.is_empty() {
            return Ok("no app currently owns an on-screen window".to_owned());
        }
        let mut out = String::from("apps with on-screen windows (frontmost first):\n");
        for app in &apps {
            out.push_str(&format!(
                "- {} (pid {}) windows={}{}\n",
                app.name,
                app.pid,
                app.windows,
                if app.frontmost { " [frontmost]" } else { "" }
            ));
        }
        out.push_str(
            "\nPass the app name or pid to computer_snapshot. Names are matched case-insensitively \
and may be a unique fragment.",
        );
        Ok(out)
    }

    /// Walk a window's accessibility tree and allocate refs for the actionable nodes.
    pub(crate) fn snapshot(
        &self,
        app: Option<&str>,
        window: Option<&str>,
        limits: Limits,
        max_chars: usize,
    ) -> Result<String, ComputerError> {
        let snapshot = platform::snapshot(app, window, limits)?;
        let mut state = self.lock()?;
        state.refs.clear();
        state.counter = 0;

        // Allocate refs first so rendering can look them up by node index.
        let mut refs: HashMap<usize, String> = HashMap::new();
        for (index, node) in snapshot.nodes.iter().enumerate() {
            if !is_actionable(node) {
                continue;
            }
            state.counter += 1;
            let ref_id = format!("@e{}", state.counter);
            state.refs.insert(
                ref_id.clone(),
                Locator {
                    pid: snapshot.pid,
                    window: snapshot.window_index,
                    path: node.path.clone(),
                    role: node.role.clone(),
                    label: node_label(node),
                },
            );
            refs.insert(index, ref_id);
        }

        Ok(render(&snapshot, &refs, max_chars))
    }

    /// Perform an action on a ref from the most recent snapshot.
    pub(crate) fn act(
        &self,
        ref_id: &str,
        action: Action,
        value: Option<&str>,
    ) -> Result<String, ComputerError> {
        let locator = {
            let state = self.lock()?;
            state.refs.get(ref_id.trim()).cloned().ok_or_else(|| {
                ComputerError::stale(format!(
                    "{} is not a ref from the current snapshot (refs are snapshot-scoped: take a \
new computer_snapshot first)",
                    ref_id.trim()
                ))
            })?
        };
        if matches!(action, Action::SetValue | Action::Type) && value.is_none() {
            return Err(ComputerError::failed(format!(
                "{} needs a value",
                action.name()
            )));
        }
        let outcome = platform::perform(&locator, action, value)?;
        Ok(format!(
            "ok: {} on {} ({} \"{}\"){}. Take a new computer_snapshot to confirm the effect.",
            action.name(),
            ref_id.trim(),
            normalize_role(&locator.role),
            locator.label,
            outcome.detail
        ))
    }

    /// Send keystrokes to an app (activating it first when named).
    pub(crate) fn key(&self, keys: &str, app: Option<&str>) -> Result<String, ComputerError> {
        let sent = platform::key(keys, app)?;
        Ok(format!(
            "ok: sent '{keys}'{sent}. Take a new computer_snapshot to confirm the effect."
        ))
    }
}

/// Characters a node needs a ref for the agent to act on it.
fn is_actionable(node: &Node) -> bool {
    !node.actions.is_empty() || node.settable || is_interactive_role(&node.role)
}

fn is_interactive_role(role: &str) -> bool {
    matches!(
        normalize_role(role).as_str(),
        "button"
            | "check box"
            | "radio button"
            | "text field"
            | "text area"
            | "secure text field"
            | "search field"
            | "combo box"
            | "pop up button"
            | "menu button"
            | "menu item"
            | "tab"
            | "slider"
            | "link"
            | "row"
            | "cell"
            | "outline row"
            | "list item"
            | "disclosure triangle"
            | "incrementor"
            | "toolbar button"
            | "color well"
    )
}

/// `AXTextField` -> `text field`. Shorter and cheaper in tokens than the raw AX role.
fn normalize_role(role: &str) -> String {
    let stripped = role.strip_prefix("AX").unwrap_or(role);
    let mut out = String::with_capacity(stripped.len() + 4);
    let mut previous_was_lower = false;
    for ch in stripped.chars() {
        if ch.is_uppercase() && previous_was_lower {
            out.push(' ');
        }
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
        previous_was_lower = ch.is_lowercase() || ch.is_ascii_digit();
    }
    out
}

fn node_label(node: &Node) -> String {
    let present = |text: &&str| !text.trim().is_empty();
    if let Some(title) = node.title.as_deref().filter(present) {
        return clamp(title, 60);
    }
    if let Some(value) = node.value.as_deref().filter(present) {
        return clamp(value, 60);
    }
    normalize_role(&node.role)
}

/// Truncate at a character boundary, marking the cut.
fn clamp(text: &str, max_chars: usize) -> String {
    let flat = text.replace(['\n', '\r', '\t'], " ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut out: String = flat.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Render the tree as indented text, cheapest-first: indentation follows *ref-bearing* ancestors,
/// so anonymous wrapper containers do not inflate the depth the model has to read.
fn render(snapshot: &Snapshot, refs: &HashMap<usize, String>, max_chars: usize) -> String {
    let mut header = format!(
        "# {} -- window \"{}\" (pid {}, {} nodes{})\n",
        snapshot.app,
        snapshot.window_title,
        snapshot.pid,
        snapshot.nodes.len(),
        if snapshot.partial {
            ", walk stopped at the budget"
        } else {
            ""
        }
    );
    if snapshot.nodes.is_empty() {
        header.push_str(
            "(no accessibility nodes: this app does not expose its interface, or the window is \
still loading)\n",
        );
        return header;
    }

    let mut out = header;
    // (node depth, rendered indent of that node)
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut truncated = false;
    for (index, node) in snapshot.nodes.iter().enumerate() {
        let Some(ref_id) = refs.get(&index) else {
            continue;
        };
        while matches!(stack.last(), Some(&(depth, _)) if depth >= node.depth) {
            stack.pop();
        }
        let indent = stack.len();
        let role = normalize_role(&node.role);
        let mut line = String::with_capacity(64);
        line.push_str(&"  ".repeat(indent));
        line.push_str(ref_id);
        line.push(' ');
        line.push_str(&role);
        if let Some(title) = node.title.as_deref().filter(|t| !t.trim().is_empty()) {
            line.push_str(&format!(" \"{}\"", clamp(title, 80)));
        }
        if let Some(value) = node.value.as_deref().filter(|v| !v.trim().is_empty()) {
            line.push_str(&format!(" = \"{}\"", clamp(value, 120)));
        }
        if !node.enabled {
            line.push_str(" disabled");
        }
        if node.settable {
            line.push_str(" [writable]");
        }
        if !node.actions.is_empty() {
            let names: Vec<String> = node.actions.iter().map(|action| verb_for(action)).collect();
            line.push_str(&format!(" {{{}}}", names.join(",")));
        }
        line.push('\n');
        if out.len() + line.len() > max_chars {
            truncated = true;
            break;
        }
        out.push_str(&line);
        stack.push((node.depth, indent));
    }

    if truncated || snapshot.partial {
        out.push_str(
            "(snapshot cut short; narrow it down with the window argument, or scroll the app and \
snapshot again)\n",
        );
    }
    out.push_str(
        "(refs are valid only for this snapshot; act on them, then snapshot again to verify)\n",
    );
    out
}

/// Map AX actions to the short verb the model should read.
fn verb_for(action: &str) -> String {
    match action {
        "AXPress" => "click",
        "AXConfirm" => "confirm",
        "AXCancel" => "cancel",
        "AXShowMenu" => "menu",
        "AXIncrement" => "+",
        "AXDecrement" => "-",
        "AXRaise" => "raise",
        "AXPick" => "pick",
        other => {
            return other
                .strip_prefix("AX")
                .unwrap_or(other)
                .to_ascii_lowercase();
        }
    }
    .to_owned()
}
