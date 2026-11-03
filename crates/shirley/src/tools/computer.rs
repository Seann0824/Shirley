//! Computer-use tools: let the agent drive a GUI app the way a user would.
//!
//! Four tools, not forty. The surface is deliberately small because every tool definition sits in
//! the prompt prefix and is paid for on every request; a fat command list also gives the model
//! more ways to pick the wrong one. `apps -> snapshot -> act -> snapshot` covers the loop.
//!
//! Why the accessibility tree instead of screenshots:
//! - a screenshot step costs tens of thousands of tokens and needs a vision model; the tree costs
//!   ~1.5k tokens and works with any text model;
//! - coordinates go stale the moment the window moves, the display changes resolution, or the app
//!   is on a Retina screen; a ref names the control itself;
//! - accessibility actions do not move the pointer or steal focus, so the human can keep working
//!   while the agent drives a background window.

use std::sync::Arc;

use shirley_agent_sdk::{ToolContext, ToolError, tool};

use crate::computer::{
    Action, ComputerError, DEFAULT_MAX_CHARS, DEFAULT_MAX_NODES, Engine, Limits,
};

/// Upper bound on `max_nodes`, so one call cannot walk a pathological tree for minutes.
const MAX_MAX_NODES: usize = 4000;

/// Shared computer-use state, injected at registration time by [`computer_on_register`].
#[derive(Debug)]
pub(crate) struct ComputerState {
    engine: Arc<Engine>,
}

/// Inject the engine when the tool is registered: registration and dependency injection are one
/// step, so they cannot drift apart.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the tool lifecycle hook signature is fixed: fn(&mut ToolContext) -> Result<(), ToolError>"
)]
pub(crate) fn computer_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    // Four tools share one engine. A later registration must not replace the one the first
    // installed, or the ref table a snapshot just filled would vanish before the next action.
    if ctx.get::<ComputerState>().is_some() {
        return Ok(());
    }
    let engine = Arc::new(Engine::new());
    // Ask for accessibility permission once, at startup, rather than in the middle of a task: the
    // system dialog is easier to act on when the agent is not holding a half-finished step.
    let _ = engine.trust(true);
    ctx.insert(ComputerState { engine });
    Ok(())
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the tool lifecycle hook signature is fixed: fn(&mut ToolContext) -> Result<(), ToolError>"
)]
pub(crate) fn computer_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<ComputerState>();
    Ok(())
}

fn engine(ctx: &ToolContext) -> Result<Arc<Engine>, ToolError> {
    ctx.get::<ComputerState>()
        .map(|state| Arc::clone(&state.engine))
        .ok_or_else(|| {
            ToolError::ExecutionError(
                "computer use is not initialised for this session (registration hook did not run)"
                    .to_owned(),
            )
        })
}

fn tool_error(error: ComputerError) -> ToolError {
    ToolError::ExecutionError(error.to_string())
}

/// Run one accessibility call off the async worker: every call is a cross-process IPC that can
/// block for the whole messaging timeout when the target app is busy.
async fn blocking<T, F>(work: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, ComputerError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result.map_err(tool_error),
        Err(error) => Err(ToolError::ExecutionError(format!(
            "the computer-use worker failed: {error}"
        ))),
    }
}

/// List apps that own on-screen windows.
#[tool(
    description = "List the apps that currently own on-screen windows (name, pid, window count, \
which one is frontmost). Call this first for any task that involves a GUI app: it is how you learn \
which apps are running and get the exact app name or pid to pass to computer_snapshot. Also use it \
to (re)trigger the macOS accessibility permission prompt when computer_snapshot reports that \
permission is missing.",
    on_register = computer_on_register,
    on_unregister = computer_on_unregister
)]
pub async fn computer_apps(ctx: &ToolContext) -> Result<String, ToolError> {
    let engine = engine(ctx)?;
    blocking(move || engine.apps()).await
}

/// Read one window's accessibility tree.
#[tool(
    description = "Read one window's accessibility tree as indented text. Every node you can act on \
carries a ref such as @e12 -- pass that exact string to computer_act. A line looks like \
`@e7 text field \"Search\" = \"rust\" [writable] {click}`: role, title, current value, whether the \
value can be written, and the supported actions. This is how you SEE an app: no screenshot needed. \
Call it at the start of a desktop task, after any action that changes the interface, and whenever a \
ref is reported stale. Omit `app` only when the app you want is already frontmost.",
    on_register = computer_on_register,
    on_unregister = computer_on_unregister
)]
pub async fn computer_snapshot(
    ctx: &ToolContext,
    #[param(
        description = "App name or pid, e.g. \"Notion\" or \"1234\". A unique fragment of the name is enough. Omit for the frontmost app."
    )]
    app: Option<String>,
    #[param(
        description = "Which window: an index like \"0\", or a fragment of the window title. Omit for the app's first window."
    )]
    window: Option<String>,
    #[param(
        description = "Maximum number of nodes to walk (default 600, max 4000). Lower it for huge trees; raise it when the part you need is not in the output."
    )]
    max_nodes: Option<u32>,
) -> Result<String, ToolError> {
    let engine = engine(ctx)?;
    let limits = Limits {
        max_nodes: max_nodes
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .map_or(DEFAULT_MAX_NODES, |value| value.min(MAX_MAX_NODES)),
        ..Limits::default()
    };
    blocking(move || engine.snapshot(app.as_deref(), window.as_deref(), limits, DEFAULT_MAX_CHARS))
        .await
}

/// Perform one action on a ref.
#[tool(
    description = "Act on one element from the latest computer_snapshot, by its ref. Actions: \
click (press the control), set_value (write a text field's value directly -- this is the reliable \
way to enter text, no keyboard layout or input method involved), type (focus the element and \
synthesize keystrokes; only when set_value is refused), focus, select (rows, tabs, checkboxes), \
show_menu (context menu), increment / decrement (steppers, sliders). Actions go through the \
accessibility API: the pointer does not move and focus is not stolen. `value` is required for \
set_value and type. Verify with a new computer_snapshot: do not assume the click did what you \
hoped.",
    on_register = computer_on_register,
    on_unregister = computer_on_unregister
)]
pub async fn computer_act(
    ctx: &ToolContext,
    #[param(description = "The ref from the latest snapshot, e.g. \"@e12\".")] ref_id: String,
    #[param(
        description = "One of: click, set_value, type, focus, select, show_menu, increment, decrement."
    )]
    action: String,
    #[param(description = "The text to write, for set_value / type. Required for those actions.")]
    value: Option<String>,
) -> Result<String, ToolError> {
    let Some(action) = Action::parse(&action) else {
        return Err(ToolError::ArgumentsError(format!(
            "'{action}' is not an action; use click, set_value, type, focus, select, show_menu, \
increment or decrement"
        )));
    };
    if ref_id.trim().is_empty() {
        return Err(ToolError::ArgumentsError(
            "ref_id is empty; pass a ref such as @e12 from the latest computer_snapshot".to_owned(),
        ));
    }
    if matches!(action, Action::SetValue | Action::Type) && value.is_none() {
        return Err(ToolError::ArgumentsError(format!(
            "action '{}' needs the value argument",
            action.name()
        )));
    }
    let engine = engine(ctx)?;
    blocking(move || engine.act(&ref_id, action, value.as_deref())).await
}

/// Send keystrokes or literal text.
#[tool(
    description = "Send a keystroke chord or literal text. Use it for keyboard shortcuts the app does \
not expose as accessibility actions ('cmd+s', 'cmd+shift+t', 'cmd+z', 'ctrl+a'), for navigation keys \
('return', 'escape', 'tab', 'space', 'page_down', 'up', 'down'), or to type plain text. Plain text \
is delivered as Unicode, so CJK input works and the keyboard layout is irrelevant -- but prefer \
computer_act with set_value for filling a field, because that is atomic and verifiable. Pass `app` \
to activate it first: key events go to whichever app has keyboard focus.",
    on_register = computer_on_register,
    on_unregister = computer_on_unregister
)]
pub async fn computer_key(
    ctx: &ToolContext,
    #[param(
        description = "A chord like \"cmd+s\", a key name like \"return\" / \"page_down\", or literal text to type."
    )]
    keys: String,
    #[param(
        description = "App name or pid to activate first. Omit to send to the currently frontmost app."
    )]
    app: Option<String>,
) -> Result<String, ToolError> {
    if keys.trim().is_empty() {
        return Err(ToolError::ArgumentsError(
            "keys is empty; give a chord like cmd+s, a key name, or the text to type".to_owned(),
        ));
    }
    let engine = engine(ctx)?;
    blocking(move || engine.key(&keys, app.as_deref())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use shirley_agent_sdk::Tool;

    #[test]
    fn computer_tools_expose_stable_names_and_schemas() {
        let expected = [
            ("computer_apps", 0),
            ("computer_snapshot", 3),
            ("computer_act", 3),
            ("computer_key", 2),
        ];
        // Bind the tools first: `definition()` borrows `self`, so a temporary would not live long
        // enough for the array.
        let apps = computer_apps::tool();
        let snapshot = computer_snapshot::tool();
        let act = computer_act::tool();
        let key = computer_key::tool();
        let definitions = [
            apps.definition(),
            snapshot.definition(),
            act.definition(),
            key.definition(),
        ];
        for (definition, (name, params)) in definitions.iter().zip(expected) {
            assert_eq!(definition.name, name, "tool name drifted");
            let properties = definition
                .parameters
                .get("properties")
                .and_then(|v| v.as_object());
            assert_eq!(
                properties.map_or(0, serde_json::Map::len),
                params,
                "{name} should expose {params} parameters"
            );
        }
        // The prompt tells the model to pass `ref_id`; the schema must agree.
        let properties = act.definition().parameters["properties"]
            .as_object()
            .expect("properties")
            .clone();
        assert!(
            properties.contains_key("ref_id"),
            "ref_id missing from schema"
        );
        assert!(
            properties.contains_key("action"),
            "action missing from schema"
        );
        assert!(
            properties.contains_key("value"),
            "value missing from schema"
        );
    }

    #[test]
    fn every_documented_action_parses() {
        // Keep the enum and the copy in sync: a name the prompt advertises must parse.
        for raw in [
            "click",
            "set_value",
            "type",
            "focus",
            "select",
            "show_menu",
            "increment",
            "decrement",
        ] {
            let action = Action::parse(raw).unwrap_or_else(|| panic!("{raw} should parse"));
            assert_eq!(action.name(), raw, "{raw} should round-trip");
        }
        assert!(Action::parse("double_click").is_none());
    }
}
