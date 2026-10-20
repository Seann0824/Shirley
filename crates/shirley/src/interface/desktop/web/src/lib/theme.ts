/**
 * Theme preference shared by the unified Settings dialog and the app shell.
 *
 * Persisted as `shirley.desktop.theme`; "system" is represented by the absence of a
 * stored value so the inline boot script in `index.html` can keep following the OS.
 * Changes are broadcast through `shirley-theme-change` so components that render
 * theme-dependent visuals (e.g. the flight map) can refresh without a reload.
 */
export type ThemeMode = "system" | "light" | "dark";

const THEME_KEY = "shirley.desktop.theme";
export const THEME_CHANGE_EVENT = "shirley-theme-change";

export function getThemeMode(): ThemeMode {
  try {
    const value = window.localStorage.getItem(THEME_KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

export function resolveTheme(mode: ThemeMode): "light" | "dark" {
  if (mode === "light" || mode === "dark") return mode;
  try {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  } catch {
    return "light";
  }
}

export function applyTheme(mode: ThemeMode) {
  document.documentElement.dataset.theme = resolveTheme(mode);
}

export function setThemeMode(mode: ThemeMode) {
  try {
    if (mode === "system") window.localStorage.removeItem(THEME_KEY);
    else window.localStorage.setItem(THEME_KEY, mode);
  } catch {
    // Private mode / disabled storage: the change still applies for this session.
  }
  applyTheme(mode);
  window.dispatchEvent(new CustomEvent<ThemeMode>(THEME_CHANGE_EVENT, { detail: mode }));
}
