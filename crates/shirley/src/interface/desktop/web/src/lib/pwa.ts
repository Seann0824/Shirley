/**
 * PWA bootstrap for the cloud web build.
 *
 * Registers the service worker that makes Shirley installable ("Add to Home Screen" on
 * iOS/Android) and keeps the app shell available offline. Only runs in a real browser
 * (skips the Tauri desktop webview, where there is no PWA install flow and no server
 * origin to cache against).
 */
export function registerServiceWorker(): void {
  if (typeof window === "undefined") return;
  if (!("serviceWorker" in navigator)) return;
  // Tauri desktop webview: skip. There is no install-to-home-screen story there.
  if ("__TAURI_INTERNALS__" in window) return;
  // Only in production builds; the Vite dev server's HMR does not play well with an SW.
  if (import.meta.env.DEV) return;

  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      // Registration is best-effort: the app still works without offline support.
    });
  });
}
