//! macOS backend: `AXUIElement` for observation and actions, `CGEvent` for keystrokes.
//!
//! Two non-obvious facts drive most of this file:
//!
//! 1. **Electron / Chromium apps expose an empty tree until asked.** Setting
//!    `AXManualAccessibility = true` on the *application* element makes them build the web subtree.
//!    Without it a snapshot of VS Code returns one node.
//! 2. **An accessibility call can block for the full IPC timeout when the target app is hung.**
//!    `AXUIElementSetMessagingTimeout` keeps one stuck app from freezing the whole agent.

mod cf;
mod input;

use accessibility_sys as ax;
use cf::{CfOwned, CfStr};
use core_foundation_sys::{
    base::{CFRelease, CFTypeRef},
    dictionary::{
        CFDictionaryCreate, CFDictionaryGetValue, CFDictionaryRef, kCFTypeDictionaryKeyCallBacks,
        kCFTypeDictionaryValueCallBacks,
    },
    number::kCFBooleanTrue,
    string::CFStringRef,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;

use super::{ActOutcome, Action, AppInfo, ComputerError, Limits, Locator, Node, Snapshot};

/// How long any single accessibility call may take before it gives up on a stuck app.
const MESSAGING_TIMEOUT_SECONDS: f32 = 1.5;

/// Set once so a missing permission produces one system dialog, not one per tool call.
static TRUST_PROMPTED: AtomicBool = AtomicBool::new(false);

// CoreGraphics window list: enough to enumerate apps without asking the accessibility API about
// every process on the machine (which would be one IPC round-trip per app).
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFTypeRef;
    static kCGWindowOwnerPID: CFStringRef;
    static kCGWindowOwnerName: CFStringRef;
    static kCGWindowLayer: CFStringRef;
}

const WINDOW_LIST_ON_SCREEN_ONLY: u32 = 1 << 0;
const WINDOW_LIST_EXCLUDE_DESKTOP: u32 = 1 << 4;

/// An owned `AXUIElement` (or any CF value used as one). Released on drop.
struct Element(CfOwned);

impl Element {
    fn from_owned(raw: CFTypeRef) -> Option<Self> {
        Some(Self(owned(raw)?))
    }

    fn as_ref(&self) -> ax::AXUIElementRef {
        self.0.as_ref().cast_mut().cast()
    }
}

fn owned(raw: CFTypeRef) -> Option<CfOwned> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: callers pass the `+1` reference returned by a CoreFoundation copy / create call.
    Some(unsafe { CfOwned::from_owned(raw) })
}

pub(super) fn is_trusted(prompt: bool) -> bool {
    // SAFETY: no arguments, no borrowed state.
    if unsafe { ax::AXIsProcessTrusted() } {
        return true;
    }
    if !prompt || TRUST_PROMPTED.swap(true, Ordering::Relaxed) {
        return false;
    }
    // Ask the system to show the "grant Accessibility" dialog. The flag is a CFDictionary of
    // { kAXTrustedCheckOptionPrompt: true }; the call still returns the *current* state, which is
    // why the tool reports the permission error on the first attempt either way.
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a CoreFoundation string constant and
    // `kCFBooleanTrue` a CF boolean, both owned by the framework; the callbacks come from the
    // framework as well. The dictionary is released before returning.
    unsafe {
        let keys = [ax::kAXTrustedCheckOptionPrompt.cast::<std::ffi::c_void>()];
        let values = [kCFBooleanTrue.cast::<std::ffi::c_void>()];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        );
        if options.is_null() {
            return false;
        }
        let trusted = ax::AXIsProcessTrustedWithOptions(options);
        CFRelease(options.cast());
        trusted
    }
}

/// Worded for the model: it must be able to tell the user exactly what to switch on. TCC grants
/// the permission to the *responsible* process, so the wording names the launching app rather than
/// Shirley itself.
pub(super) const NOT_TRUSTED_HELP: &str = "accessibility permission is not granted. macOS only \
lets a process drive other apps' user interfaces after the user enables the app that launched \
Shirley (Terminal / iTerm / the Shirley app itself) in System Settings > Privacy & Security > \
Accessibility. The permission prompt was triggered; after granting it, call this tool again.";

fn ensure_trusted() -> Result<(), ComputerError> {
    if is_trusted(true) {
        Ok(())
    } else {
        Err(ComputerError::NotTrusted(NOT_TRUSTED_HELP.to_owned()))
    }
}

// ---------------------------------------------------------------------------------------------
// attribute plumbing
// ---------------------------------------------------------------------------------------------

fn copy_attr(element: ax::AXUIElementRef, attribute: &str) -> Option<CfOwned> {
    let name = CfStr::new(attribute)?;
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: `element` is a live AXUIElement and `name` a live CFString; the API writes a `+1`
    // value into `value` on success, whose ownership we take.
    let status =
        unsafe { ax::AXUIElementCopyAttributeValue(element, name.as_ref(), &raw mut value) };
    if status != ax::kAXErrorSuccess {
        return None;
    }
    owned(value)
}

fn string_attr(element: ax::AXUIElementRef, attribute: &str) -> Option<String> {
    let value = copy_attr(element, attribute)?;
    cf::to_string(value.as_ref())
}

fn bool_attr(element: ax::AXUIElementRef, attribute: &str) -> Option<bool> {
    let value = copy_attr(element, attribute)?;
    cf::to_bool(value.as_ref())
}

fn set_bool_attr(
    element: ax::AXUIElementRef,
    attribute: &str,
    value: bool,
) -> Result<(), ComputerError> {
    let name = CfStr::new(attribute)
        .ok_or_else(|| ComputerError::failed(format!("bad attribute name '{attribute}'")))?;
    let flag = if value {
        // SAFETY: framework-owned CF boolean constant, only borrowed for the call.
        unsafe { kCFBooleanTrue.cast::<std::ffi::c_void>() }
    } else {
        // SAFETY: framework-owned CF boolean constant, only borrowed for the call.
        unsafe { core_foundation_sys::number::kCFBooleanFalse.cast::<std::ffi::c_void>() }
    };
    // SAFETY: `element` is live, `name` a live CFString, and `flag` a valid CF boolean value.
    let status = unsafe { ax::AXUIElementSetAttributeValue(element, name.as_ref(), flag) };
    if status == ax::kAXErrorSuccess {
        Ok(())
    } else {
        Err(ComputerError::failed(format!(
            "cannot set {attribute}: {}",
            describe(status)
        )))
    }
}

fn is_settable(element: ax::AXUIElementRef, attribute: &str) -> bool {
    let Some(name) = CfStr::new(attribute) else {
        return false;
    };
    let mut settable: u8 = 0;
    // SAFETY: `element` / `name` are live; `settable` is a valid out-parameter.
    let status =
        unsafe { ax::AXUIElementIsAttributeSettable(element, name.as_ref(), &raw mut settable) };
    status == ax::kAXErrorSuccess && settable != 0
}

fn element_list(element: ax::AXUIElementRef, attribute: &str) -> Vec<Element> {
    let Some(value) = copy_attr(element, attribute) else {
        return Vec::new();
    };
    cf::array_items(value.as_ref())
        .into_iter()
        .filter_map(|item| {
            // SAFETY: `item` is a valid element inside `value`, which outlives this iteration; we
            // retain it so it survives `value` being dropped.
            let child = unsafe { cf::retain(item) };
            Element::from_owned(child.as_ref())
        })
        .collect()
}

fn action_names(element: ax::AXUIElementRef) -> Vec<String> {
    let mut names: core_foundation_sys::array::CFArrayRef = std::ptr::null();
    // SAFETY: `element` is live and `names` a valid out-parameter receiving a `+1` array.
    let status = unsafe { ax::AXUIElementCopyActionNames(element, &raw mut names) };
    if status != ax::kAXErrorSuccess {
        return Vec::new();
    }
    let Some(array) = owned(names.cast()) else {
        return Vec::new();
    };
    cf::array_items(array.as_ref())
        .into_iter()
        .filter_map(cf::to_string)
        .collect()
}

/// Read the element's value as display text, translating the booleans that checkboxes report as
/// numbers into something readable.
fn value_of(element: ax::AXUIElementRef, role: &str) -> Option<String> {
    let value = copy_attr(element, ax::kAXValueAttribute)?;
    let raw = value.as_ref();
    if let Some(text) = cf::to_string(raw) {
        return (!text.trim().is_empty()).then_some(text);
    }
    if let Some(flag) = cf::to_bool(raw) {
        return Some(flag.to_string());
    }
    let number = cf::to_i64(raw)?;
    let checkbox = matches!(role, ax::kAXCheckBoxRole | ax::kAXRadioButtonRole);
    if checkbox {
        return Some(if number == 0 { "unchecked" } else { "checked" }.to_owned());
    }
    Some(number.to_string())
}

fn describe(status: ax::AXError) -> String {
    let name = match status {
        ax::kAXErrorSuccess => "success",
        ax::kAXErrorFailure => "generic failure",
        ax::kAXErrorIllegalArgument => "illegal argument",
        ax::kAXErrorInvalidUIElement => "element is gone",
        ax::kAXErrorCannotComplete => "the app did not respond",
        ax::kAXErrorAttributeUnsupported => "attribute not supported",
        ax::kAXErrorActionUnsupported => "action not supported",
        ax::kAXErrorNotImplemented => "not implemented by the app",
        ax::kAXErrorAPIDisabled => "accessibility API disabled",
        ax::kAXErrorNoValue => "no value",
        _ => "unknown error",
    };
    format!("{name} ({status})")
}

// ---------------------------------------------------------------------------------------------
// apps
// ---------------------------------------------------------------------------------------------

fn dict_value(dict: CFDictionaryRef, key: CFStringRef) -> CFTypeRef {
    // SAFETY: `dict` / `key` are live CoreFoundation values; the returned borrow is only read
    // while `dict` is alive.
    unsafe { CFDictionaryGetValue(dict, key.cast()) }
}

pub(super) fn apps() -> Result<Vec<AppInfo>, ComputerError> {
    // SAFETY: option flags and a null "relative to" window are the documented usage.
    let raw = unsafe {
        CGWindowListCopyWindowInfo(WINDOW_LIST_ON_SCREEN_ONLY | WINDOW_LIST_EXCLUDE_DESKTOP, 0)
    };
    let Some(list) = owned(raw) else {
        return Err(ComputerError::failed(
            "could not read the window list from CoreGraphics (is there a window server session?)",
        ));
    };
    let frontmost = focused_pid();
    let mut found: Vec<AppInfo> = Vec::new();
    for item in cf::array_items(list.as_ref()) {
        let dict: CFDictionaryRef = item.cast();
        // SAFETY: the statics are framework-owned CFString constants.
        let pid = cf::to_i64(dict_value(dict, unsafe { kCGWindowOwnerPID }));
        // SAFETY: framework-owned CFString constant.
        let layer_key = unsafe { kCGWindowLayer };
        let layer = cf::to_i64(dict_value(dict, layer_key)).unwrap_or(0);
        // Layer 0 is "normal window"; everything else is the menu bar, dock, tooltips, wallpaper.
        let (Some(pid), 0) = (pid, layer) else {
            continue;
        };
        let pid = i32::try_from(pid).unwrap_or(0);
        if pid <= 0 {
            continue;
        }
        // SAFETY: framework-owned CFString constant.
        let name = cf::to_string(dict_value(dict, unsafe { kCGWindowOwnerName }))
            .unwrap_or_else(|| format!("pid {pid}"));
        match found.iter_mut().find(|app| app.pid == pid) {
            Some(app) => app.windows += 1,
            None => found.push(AppInfo {
                pid,
                name,
                windows: 1,
                frontmost: frontmost == Some(pid),
            }),
        }
    }
    found.sort_by(|left, right| {
        right
            .frontmost
            .cmp(&left.frontmost)
            .then(right.windows.cmp(&left.windows))
            .then(left.name.cmp(&right.name))
    });
    Ok(found)
}

fn focused_pid() -> Option<i32> {
    // SAFETY: creates a system-wide element (`+1`), released by `Element`'s drop.
    let system = Element::from_owned(unsafe { ax::AXUIElementCreateSystemWide().cast() })?;
    let focused = copy_attr(system.as_ref(), ax::kAXFocusedApplicationAttribute)?;
    let mut pid: ax::pid_t = 0;
    // SAFETY: `focused` is a live application element; `pid` is a valid out-parameter.
    let status = unsafe { ax::AXUIElementGetPid(focused.as_ref().cast_mut().cast(), &raw mut pid) };
    (status == ax::kAXErrorSuccess && pid > 0).then_some(pid)
}

fn app_name(pid: i32) -> String {
    apps()
        .ok()
        .and_then(|apps| apps.into_iter().find(|app| app.pid == pid))
        .map_or_else(|| format!("pid {pid}"), |app| app.name)
}

fn resolve_pid(app: Option<&str>) -> Result<i32, ComputerError> {
    let Some(raw) = app.map(str::trim).filter(|value| !value.is_empty()) else {
        return focused_pid().ok_or_else(|| {
            ComputerError::failed(
                "no app is frontmost; pass an app name (computer_apps lists them)",
            )
        });
    };
    if let Ok(pid) = raw.parse::<i32>() {
        return Ok(pid);
    }
    let needle = raw.to_lowercase();
    let candidates = apps()?;
    if let Some(app) = candidates
        .iter()
        .find(|app| app.name.to_lowercase() == needle)
    {
        return Ok(app.pid);
    }
    let mut matches: Vec<&AppInfo> = candidates
        .iter()
        .filter(|app| app.name.to_lowercase().contains(&needle))
        .collect();
    match matches.len() {
        1 => Ok(matches[0].pid),
        0 => Err(ComputerError::failed(format!(
            "no app matched '{raw}'; computer_apps lists the apps that own on-screen windows"
        ))),
        _ => {
            matches.sort_by_key(|app| app.pid);
            let names: Vec<String> = matches
                .iter()
                .map(|app| format!("{} (pid {})", app.name, app.pid))
                .collect();
            Err(ComputerError::failed(format!(
                "'{raw}' matches several apps: {}; pass one of those pids instead",
                names.join(", ")
            )))
        }
    }
}

fn create_app(pid: i32) -> Result<Element, ComputerError> {
    // SAFETY: creates an application element (`+1`), owned by the returned `Element`.
    let element = Element::from_owned(unsafe { ax::AXUIElementCreateApplication(pid).cast() })
        .ok_or_else(|| {
            ComputerError::failed(format!(
                "cannot attach to pid {pid}; the app may have quit -- run computer_apps again"
            ))
        })?;
    // SAFETY: `element` is live; the timeout is a plain value.
    unsafe { ax::AXUIElementSetMessagingTimeout(element.as_ref(), MESSAGING_TIMEOUT_SECONDS) };
    // Electron / Chromium: the web subtree does not exist until the app is told we want it.
    let _ = set_bool_attr(element.as_ref(), "AXManualAccessibility", true);
    Ok(element)
}

// ---------------------------------------------------------------------------------------------
// snapshot
// ---------------------------------------------------------------------------------------------

pub(super) fn snapshot(
    app: Option<&str>,
    window: Option<&str>,
    limits: Limits,
) -> Result<Snapshot, ComputerError> {
    ensure_trusted()?;
    let pid = resolve_pid(app)?;
    let name = app_name(pid);
    let application = create_app(pid)?;
    let windows = element_list(application.as_ref(), ax::kAXWindowsAttribute);
    if windows.is_empty() {
        return Err(ComputerError::failed(format!(
            "{name} exposes no window: it may be minimized, still launching, or running without a \
visible window. Activate it first (computer_apps lists pids), then snapshot again."
        )));
    }
    let window_index = pick_window(&windows, window)?;
    let window_element = &windows[window_index];
    let window_title = string_attr(window_element.as_ref(), ax::kAXTitleAttribute)
        .unwrap_or_else(|| format!("window {window_index}"));

    let mut nodes = Vec::new();
    let mut path = Vec::new();
    let mut partial = false;
    walk(
        window_element.as_ref(),
        0,
        &mut path,
        &mut nodes,
        &limits,
        &mut partial,
    );

    Ok(Snapshot {
        app: name,
        pid,
        window_title,
        window_index,
        nodes,
        partial,
    })
}

fn pick_window(windows: &[Element], wanted: Option<&str>) -> Result<usize, ComputerError> {
    let Some(wanted) = wanted.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(0);
    };
    if let Ok(index) = wanted.parse::<usize>() {
        if index < windows.len() {
            return Ok(index);
        }
        return Err(ComputerError::failed(format!(
            "window index {index} is out of range; the app has {} window(s)",
            windows.len()
        )));
    }
    let needle = wanted.to_lowercase();
    let titles: Vec<(usize, String)> = windows
        .iter()
        .enumerate()
        .map(|(index, element)| {
            (
                index,
                string_attr(element.as_ref(), ax::kAXTitleAttribute).unwrap_or_default(),
            )
        })
        .collect();
    if let Some((index, _)) = titles
        .iter()
        .find(|(_, title)| title.to_lowercase().contains(&needle))
    {
        return Ok(*index);
    }
    let available: Vec<String> = titles
        .iter()
        .map(|(index, title)| format!("{index}: \"{title}\""))
        .collect();
    Err(ComputerError::failed(format!(
        "no window title matched '{wanted}'; available windows: {}",
        available.join(", ")
    )))
}

fn walk(
    element: ax::AXUIElementRef,
    depth: usize,
    path: &mut Vec<u32>,
    nodes: &mut Vec<Node>,
    limits: &Limits,
    partial: &mut bool,
) {
    if nodes.len() >= limits.max_nodes {
        *partial = true;
        return;
    }
    let index = nodes.len();
    nodes.push(read_node(element, depth, path));
    if depth >= limits.max_depth {
        *partial = true;
        return;
    }
    for (child_index, child) in element_list(element, ax::kAXChildrenAttribute)
        .iter()
        .enumerate()
    {
        path.push(u32::try_from(child_index).unwrap_or(u32::MAX));
        let child_node = nodes.len();
        walk(child.as_ref(), depth + 1, path, nodes, limits, partial);
        path.pop();
        if child_node < nodes.len() {
            nodes[index].children.push(child_node);
        }
        if nodes.len() >= limits.max_nodes {
            *partial = true;
            return;
        }
    }
}

fn read_node(element: ax::AXUIElementRef, depth: usize, path: &[u32]) -> Node {
    let role = string_attr(element, ax::kAXRoleAttribute).unwrap_or_default();
    let title = string_attr(element, ax::kAXTitleAttribute)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| string_attr(element, ax::kAXDescriptionAttribute));
    Node {
        value: value_of(element, &role),
        actions: action_names(element),
        settable: is_settable(element, ax::kAXValueAttribute)
            || is_settable(element, ax::kAXSelectedAttribute),
        enabled: bool_attr(element, ax::kAXEnabledAttribute).unwrap_or(true),
        children: Vec::new(),
        title,
        role,
        path: path.to_vec(),
        depth,
    }
}

// ---------------------------------------------------------------------------------------------
// actions
// ---------------------------------------------------------------------------------------------

fn resolve(locator: &Locator) -> Result<Element, ComputerError> {
    let application = create_app(locator.pid)?;
    let windows = element_list(application.as_ref(), ax::kAXWindowsAttribute);
    let mut current = windows.into_iter().nth(locator.window).ok_or_else(|| {
        ComputerError::stale(format!(
            "window {} of pid {} is gone (the app closed or reordered its windows); take a new \
snapshot",
            locator.window, locator.pid
        ))
    })?;
    for step in &locator.path {
        let index = usize::try_from(*step).unwrap_or(usize::MAX);
        current = element_list(current.as_ref(), ax::kAXChildrenAttribute)
            .into_iter()
            .nth(index)
            .ok_or_else(|| {
                ComputerError::stale(format!(
                    "the interface changed under the ref (child {step} disappeared); take a new \
snapshot and use the fresh refs"
                ))
            })?;
    }
    let role = string_attr(current.as_ref(), ax::kAXRoleAttribute).unwrap_or_default();
    if !locator.role.is_empty() && role != locator.role {
        return Err(ComputerError::stale(format!(
            "the element at that ref is now a {role} (the snapshot said {}); take a new snapshot",
            locator.role
        )));
    }
    Ok(current)
}

pub(super) fn perform(
    locator: &Locator,
    action: Action,
    value: Option<&str>,
) -> Result<ActOutcome, ComputerError> {
    ensure_trusted()?;
    let element = resolve(locator)?;
    let element_ref = element.as_ref();
    let detail = match action {
        Action::Press => press(element_ref)?,
        Action::SetValue => set_value(element_ref, value.unwrap_or_default())?,
        Action::Type => type_into(element_ref, value.unwrap_or_default())?,
        Action::Focus => {
            set_bool_attr(element_ref, ax::kAXFocusedAttribute, true)?;
            ", now focused".to_owned()
        }
        Action::Select => select(element_ref)?,
        Action::ShowMenu => perform_action(element_ref, ax::kAXShowMenuAction)?,
        Action::Increment => perform_action(element_ref, ax::kAXIncrementAction)?,
        Action::Decrement => perform_action(element_ref, ax::kAXDecrementAction)?,
    };
    Ok(ActOutcome { detail })
}

/// Activate the element. `AXPress` is the direct route; the fallbacks cover rows and menu items
/// that only advertise confirm / raise.
fn press(element: ax::AXUIElementRef) -> Result<String, ComputerError> {
    for candidate in [ax::kAXPressAction, ax::kAXConfirmAction, ax::kAXPickAction] {
        if let Ok(detail) = perform_action(element, candidate) {
            return Ok(detail);
        }
    }
    // Last resort: focus it; some views react to focus by opening (e.g. a collapsed disclosure).
    set_bool_attr(element, ax::kAXFocusedAttribute, true)?;
    Ok(", focus only (the element does not support any press action)".to_owned())
}

fn perform_action(element: ax::AXUIElementRef, action: &str) -> Result<String, ComputerError> {
    let name = CfStr::new(action)
        .ok_or_else(|| ComputerError::failed(format!("bad action name '{action}'")))?;
    // SAFETY: `element` and `name` are live for the duration of the call.
    let status = unsafe { ax::AXUIElementPerformAction(element, name.as_ref()) };
    if status != ax::kAXErrorSuccess {
        return Err(ComputerError::failed(format!(
            "the element does not support {action}: {}",
            describe(status)
        )));
    }
    Ok(format!(" via {action}"))
}

fn set_value(element: ax::AXUIElementRef, value: &str) -> Result<String, ComputerError> {
    if !is_settable(element, ax::kAXValueAttribute) {
        return Err(ComputerError::failed(
            "this element does not accept a value; use action 'type' instead (it focuses the \
element and sends keystrokes)",
        ));
    }
    let name = CfStr::new(ax::kAXValueAttribute)
        .ok_or_else(|| ComputerError::failed("bad attribute name"))?;
    let text = CfStr::new(value).ok_or_else(|| {
        ComputerError::failed("the value contains a NUL byte and cannot be written")
    })?;
    // SAFETY: element / attribute / value are all live CF values.
    let status =
        unsafe { ax::AXUIElementSetAttributeValue(element, name.as_ref(), text.as_ref().cast()) };
    if status != ax::kAXErrorSuccess {
        return Err(ComputerError::failed(format!(
            "writing the value failed: {}",
            describe(status)
        )));
    }
    let read_back = value_of(element, "").unwrap_or_default();
    let short: String = read_back.chars().take(80).collect();
    Ok(format!(", value is now \"{short}\""))
}

fn select(element: ax::AXUIElementRef) -> Result<String, ComputerError> {
    if is_settable(element, ax::kAXSelectedAttribute) {
        set_bool_attr(element, ax::kAXSelectedAttribute, true)?;
        return Ok(", selected".to_owned());
    }
    press(element)
}

/// Prefer writing the value: it bypasses the keyboard layout (so Chinese input methods cannot
/// mangle the text) and it is atomic. Fall back to synthesized keystrokes only when the element
/// refuses a value.
fn type_into(element: ax::AXUIElementRef, value: &str) -> Result<String, ComputerError> {
    if is_settable(element, ax::kAXValueAttribute)
        && let Ok(detail) = set_value(element, value)
    {
        return Ok(detail);
    }
    set_bool_attr(element, ax::kAXFocusedAttribute, true)?;
    sleep(Duration::from_millis(40));
    input::type_text(value);
    Ok(", typed into the focused element".to_owned())
}

pub(super) fn key(keys: &str, app: Option<&str>) -> Result<String, ComputerError> {
    ensure_trusted()?;
    let mut activated = String::new();
    if let Some(app) = app.map(str::trim).filter(|value| !value.is_empty()) {
        let pid = resolve_pid(Some(app))?;
        let element = create_app(pid)?;
        // Bring it forward: key events go to whatever app owns keyboard focus.
        set_bool_attr(element.as_ref(), ax::kAXFrontmostAttribute, true)?;
        sleep(Duration::from_millis(150));
        activated = format!(" to {}", app_name(pid));
    }
    let trimmed = keys.trim();
    if trimmed.is_empty() {
        return Err(ComputerError::failed("no keys given"));
    }
    if trimmed.contains('+') {
        let (modifiers, code) = parse_chord(trimmed).ok_or_else(|| {
            ComputerError::failed(format!(
                "unrecognized key chord '{trimmed}'; examples: cmd+s, cmd+shift+t, ctrl+a, return"
            ))
        })?;
        input::press_key(code, modifiers);
    } else if let Some((modifiers, code)) = single_key(trimmed) {
        input::press_key(code, modifiers);
    } else {
        input::type_text(trimmed);
    }
    Ok(activated)
}

fn single_key(text: &str) -> Option<(input::Modifiers, u16)> {
    let code = input::key_code(&text.to_ascii_lowercase())?;
    let mut modifiers = input::Modifiers::default();
    if text.chars().count() == 1 && text.chars().next().is_some_and(char::is_uppercase) {
        modifiers.add("shift");
    }
    Some((modifiers, code))
}

fn parse_chord(keys: &str) -> Option<(input::Modifiers, u16)> {
    let parts: Vec<&str> = keys
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let (last, modifiers_parts) = parts.split_last()?;
    let mut modifiers = input::Modifiers::default();
    for part in modifiers_parts {
        if !modifiers.add(&part.to_ascii_lowercase()) {
            return None;
        }
    }
    Some((modifiers, input::key_code(&last.to_ascii_lowercase())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-machine smoke test: lists the apps that own windows and snapshots the first one.
    ///
    /// Needs Accessibility permission for the test runner plus a live window server session, so it
    /// is ignored by default. Run it deliberately:
    /// `cargo test -p shirley --features server --lib computer -- --ignored --nocapture`
    /// Window enumeration does **not** need Accessibility (only Screen Recording, and that only
    /// affects window *titles*), so this one proves the CoreGraphics/CF plumbing on a real machine.
    #[test]
    #[ignore = "needs a live window server session"]
    fn lists_real_apps() {
        let apps = apps().expect("window list");
        println!("{} apps with on-screen windows:", apps.len());
        for app in apps.iter().take(15) {
            println!(
                "  {}{} (pid {}, {} windows)",
                if app.frontmost { "* " } else { "  " },
                app.name,
                app.pid,
                app.windows
            );
        }
        assert!(
            !apps.is_empty(),
            "a logged-in session has at least one window"
        );
    }

    #[test]
    #[ignore = "needs Accessibility permission and a live window server session"]
    fn snapshot_a_real_app() {
        // `false` = do not prompt: a test must not pop a system dialog on someone's screen.
        if !is_trusted(false) {
            println!("Accessibility permission is not granted to this process; nothing to verify");
            return;
        }
        let apps = apps().expect("window list");
        println!("{} apps with windows", apps.len());
        for app in apps.iter().take(10) {
            println!("  {} (pid {}, {} windows)", app.name, app.pid, app.windows);
        }
        let Some(app) = apps.first() else {
            return;
        };
        // Go through `Engine` so the printed text is exactly what the model would see.
        let engine = crate::computer::Engine::new();
        let rendered = engine
            .snapshot(
                Some(&app.name),
                None,
                crate::computer::Limits::default(),
                4000,
            )
            .expect("snapshot");
        println!("{rendered}");
    }
}
