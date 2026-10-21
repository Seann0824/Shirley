// In-app browser bridge (Tauri-only).
//
// The browser panel embeds a **real** web view: a native child webview owned by the
// Rust side (`interface/desktop/browser.rs`). The React layer only paints the chrome
// (tabs / address bar / nav buttons) and reports the content rectangle; the native
// webview floats on top of that rectangle. So these commands are pure desktop
// transport — there is no cloud / mock equivalent (the browser is local-only, like
// the native folder picker).

export type BrowserTabState = {
  tabId: string;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
};

const isTauri =
  typeof window !== "undefined" &&
  "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke: nativeInvoke } = await import("@tauri-apps/api/core");
  return nativeInvoke<T>(command, args);
}

export const browserBridge = {
  /** Whether the in-app browser is available (only inside the Tauri desktop shell). */
  available: isTauri,

  async create(tabId: string, url: string, bounds: DOMRectRect): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_create", { tabId, url, ...bounds });
  },

  async setBounds(tabId: string, bounds: DOMRectRect): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_set_bounds", { tabId, ...bounds });
  },

  async navigate(tabId: string, url: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_navigate", { tabId, url });
  },

  async back(tabId: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_back", { tabId });
  },

  async forward(tabId: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_forward", { tabId });
  },

  async reload(tabId: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_reload", { tabId });
  },

  async activate(tabId: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_activate", { tabId });
  },

  async close(tabId: string): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_close", { tabId });
  },

  async hideAll(): Promise<void> {
    if (!isTauri) return;
    await invoke("agent_browser_hide_all");
  },

  /** Subscribe to `browser://state` (per-tab url/title/loading/history flags). */
  async onState(listener: (state: BrowserTabState) => void): Promise<() => void> {
    if (!isTauri) return () => {};
    const { listen } = await import("@tauri-apps/api/event");
    return listen<BrowserTabState>("browser://state", (event) => listener(event.payload));
  },
};

/** The logical rectangle (CSS pixels) the native webview should occupy. */
export type DOMRectRect = { x: number; y: number; width: number; height: number };
