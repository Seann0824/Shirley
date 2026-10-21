//! In-app browser: a real, embeddable web view rendered as a **child webview** of the
//! main window (Tauri `WebviewBuilder` + `Window::add_child`, requires the `unstable`
//! feature). The React side owns the chrome (tabs / address bar / nav buttons) and
//! reports the content rectangle; this module owns the native webviews and their
//! navigation history.
//!
//! Design notes (see `docs/desktop-interface.md`):
//! - One tab = one child webview labelled `browser-<tabId>`. Only the active tab is
//!   shown; the rest are hidden. Creating / closing / activating is driven by the UI.
//! - The embedded page is **remote content**: it is not served from the app origin,
//!   its label is not covered by any capability (`capabilities/default.json` scopes
//!   `windows: ["main"]`, no `remote`), so it has **no IPC access**. It can only
//!   browse. `window.open` is redirected back into the same tab.
//! - Navigation history lives here (not in the OS webview) because Tauri exposes no
//!   `can_go_back` / `go_back` API; `on_navigation` records entries and back/forward
//!   replay them with a one-shot `suppress` flag.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::webview::{NewWindowResponse, PageLoadEvent, PageLoadPayload, WebviewBuilder};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, Url, WebviewUrl};

/// The single channel the React chrome listens on for per-tab browser state.
const BROWSER_EVENT: &str = "browser://state";

/// A desktop Chrome UA so sites serve their normal (non-degraded) layout.
const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Default)]
struct TabState {
    /// Visited URLs, oldest first. `cursor` points at the current entry.
    history: Vec<String>,
    cursor: usize,
    /// Set right before a programmatic back/forward navigation so the resulting
    /// `on_navigation` callback does not record a duplicate entry.
    suppress: bool,
    url: String,
    title: String,
    loading: bool,
}

#[derive(Default)]
struct Registry {
    tabs: HashMap<String, TabState>,
}

/// Managed state shared with the webview callbacks (which run on the main thread and
/// therefore use a `std` mutex — never held across an `await`).
#[derive(Clone, Default)]
pub struct BrowserState {
    inner: Arc<Mutex<Registry>>,
}

#[derive(Serialize, Clone)]
struct BrowserStateWire {
    tab_id: String,
    url: String,
    title: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
}

fn wire(tab_id: &str, tab: &TabState) -> BrowserStateWire {
    BrowserStateWire {
        tab_id: tab_id.to_string(),
        url: tab.url.clone(),
        title: tab.title.clone(),
        loading: tab.loading,
        can_go_back: tab.cursor > 0,
        can_go_forward: tab.cursor + 1 < tab.history.len(),
    }
}

fn emit(app: &AppHandle, wire: BrowserStateWire) {
    // Only the main (local) webview listens; the remote child webviews have no IPC.
    let _ = app.emit_to("main", BROWSER_EVENT, wire);
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        position: LogicalPosition::new(x, y).into(),
        size: LogicalSize::new(width.max(1.0), height.max(1.0)).into(),
    }
}

/// Normalize an address-bar entry: bare hosts get `https://`, unknown schemes are
/// rejected. Returns a parsed `Url` ready for `navigate`.
fn normalize_url(input: &str) -> Result<Url, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty address".into());
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed: Url = candidate.parse().map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(format!("unsupported scheme: {other}")),
    }
}

fn record_navigation(app: &AppHandle, reg: &Arc<Mutex<Registry>>, tab_id: &str, url: &Url) {
    let payload = {
        let mut guard = reg.lock().unwrap();
        let Some(tab) = guard.tabs.get_mut(tab_id) else {
            return;
        };
        if tab.suppress {
            // Programmatic back/forward replay: keep the cursor, just sync the URL.
            tab.suppress = false;
            tab.url = url.to_string();
        } else {
            tab.history.truncate(tab.cursor + 1);
            tab.history.push(url.to_string());
            tab.cursor = tab.history.len() - 1;
            tab.url = url.to_string();
        }
        tab.loading = true;
        wire(tab_id, tab)
    };
    emit(app, payload);
}

fn set_title(app: &AppHandle, reg: &Arc<Mutex<Registry>>, tab_id: &str, title: String) {
    let payload = {
        let mut guard = reg.lock().unwrap();
        let Some(tab) = guard.tabs.get_mut(tab_id) else {
            return;
        };
        tab.title = title;
        wire(tab_id, tab)
    };
    emit(app, payload);
}

fn on_page_load(
    app: &AppHandle,
    reg: &Arc<Mutex<Registry>>,
    tab_id: &str,
    payload: &PageLoadPayload<'_>,
) {
    let event = payload.event();
    let url = payload.url().to_string();
    let wire = {
        let mut guard = reg.lock().unwrap();
        let Some(tab) = guard.tabs.get_mut(tab_id) else {
            return;
        };
        match event {
            PageLoadEvent::Started => {
                tab.loading = true;
                tab.url = url;
            }
            PageLoadEvent::Finished => {
                tab.loading = false;
                tab.url = url;
            }
        }
        wire(tab_id, tab)
    };
    emit(app, wire);
}

fn label_for(tab_id: &str) -> String {
    format!("browser-{tab_id}")
}

/// Hide every browser webview except `keep`.
fn hide_others(app: &AppHandle, reg: &Arc<Mutex<Registry>>, keep: &str) {
    let labels: Vec<String> = {
        let guard = reg.lock().unwrap();
        guard
            .tabs
            .keys()
            .filter(|id| id.as_str() != keep)
            .map(|id| label_for(id))
            .collect()
    };
    for label in labels {
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.hide();
        }
    }
}

#[tauri::command]
pub async fn agent_browser_create(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    tab_id: String,
    url: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let parsed = normalize_url(&url)?;
    let label = label_for(&tab_id);
    let window = app.get_window("main").ok_or("main window not found")?;

    // Reuse an existing webview for this tab (e.g. a remount): just reposition + load.
    if let Some(existing) = app.get_webview(&label) {
        existing
            .set_bounds(rect(x, y, width, height))
            .map_err(|e| e.to_string())?;
        existing.show().map_err(|e| e.to_string())?;
        existing.navigate(parsed).map_err(|e| e.to_string())?;
        hide_others(&app, &state.inner, &tab_id);
        return Ok(());
    }

    {
        let mut guard = state.inner.lock().unwrap();
        guard.tabs.insert(
            tab_id.clone(),
            TabState {
                history: vec![parsed.to_string()],
                cursor: 0,
                suppress: true, // the initial load must not add a duplicate entry
                url: parsed.to_string(),
                title: String::new(),
                loading: true,
            },
        );
    }

    let reg_nav = state.inner.clone();
    let tab_nav = tab_id.clone();
    let app_nav = app.clone();

    let reg_title = state.inner.clone();
    let tab_title = tab_id.clone();
    let app_title = app.clone();

    let reg_load = state.inner.clone();
    let tab_load = tab_id.clone();
    let app_load = app.clone();

    let app_new = app.clone();
    let label_new = label.clone();

    let builder = WebviewBuilder::new(label.clone(), WebviewUrl::External(parsed))
        .user_agent(CHROME_UA)
        .on_navigation(move |target| {
            record_navigation(&app_nav, &reg_nav, &tab_nav, target);
            true
        })
        .on_new_window(move |target, _features| {
            // `target=_blank` / `window.open`: open in the same tab instead of a new window.
            if let Some(webview) = app_new.get_webview(&label_new) {
                let _ = webview.navigate(target);
            }
            NewWindowResponse::Deny
        })
        .on_page_load(move |_webview, payload| {
            on_page_load(&app_load, &reg_load, &tab_load, &payload);
        })
        .on_document_title_changed(move |_webview, title| {
            set_title(&app_title, &reg_title, &tab_title, title);
        });

    window
        .add_child(
            builder,
            LogicalPosition::new(x, y),
            LogicalSize::new(width.max(1.0), height.max(1.0)),
        )
        .map_err(|e| e.to_string())?;

    hide_others(&app, &state.inner, &tab_id);
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_set_bounds(
    app: AppHandle,
    tab_id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        webview
            .set_bounds(rect(x, y, width, height))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_navigate(
    app: AppHandle,
    tab_id: String,
    url: String,
) -> Result<(), String> {
    let parsed = normalize_url(&url)?;
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        webview.navigate(parsed).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_back(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    tab_id: String,
) -> Result<(), String> {
    let target = {
        let mut guard = state.inner.lock().unwrap();
        let Some(tab) = guard.tabs.get_mut(&tab_id) else {
            return Ok(());
        };
        if tab.cursor == 0 {
            return Ok(());
        }
        tab.cursor -= 1;
        tab.suppress = true;
        tab.history[tab.cursor].clone()
    };
    if let (Ok(parsed), Some(webview)) =
        (target.parse::<Url>(), app.get_webview(&label_for(&tab_id)))
    {
        webview.navigate(parsed).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_forward(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    tab_id: String,
) -> Result<(), String> {
    let target = {
        let mut guard = state.inner.lock().unwrap();
        let Some(tab) = guard.tabs.get_mut(&tab_id) else {
            return Ok(());
        };
        if tab.cursor + 1 >= tab.history.len() {
            return Ok(());
        }
        tab.cursor += 1;
        tab.suppress = true;
        tab.history[tab.cursor].clone()
    };
    if let (Ok(parsed), Some(webview)) =
        (target.parse::<Url>(), app.get_webview(&label_for(&tab_id)))
    {
        webview.navigate(parsed).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_reload(app: AppHandle, tab_id: String) -> Result<(), String> {
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        webview.reload().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_activate(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    tab_id: String,
) -> Result<(), String> {
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        webview.show().map_err(|e| e.to_string())?;
    }
    hide_others(&app, &state.inner, &tab_id);
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_close(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
    tab_id: String,
) -> Result<(), String> {
    {
        let mut guard = state.inner.lock().unwrap();
        guard.tabs.remove(&tab_id);
    }
    if let Some(webview) = app.get_webview(&label_for(&tab_id)) {
        webview.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_hide_all(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
) -> Result<(), String> {
    let labels: Vec<String> = {
        let guard = state.inner.lock().unwrap();
        guard.tabs.keys().map(|id| label_for(id)).collect()
    };
    for label in labels {
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.hide();
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_browser_destroy_all(
    app: AppHandle,
    state: tauri::State<'_, BrowserState>,
) -> Result<(), String> {
    let labels: Vec<String> = {
        let mut guard = state.inner.lock().unwrap();
        let labels = guard.tabs.keys().map(|id| label_for(id)).collect();
        guard.tabs.clear();
        labels
    };
    for label in labels {
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.close();
        }
    }
    Ok(())
}
