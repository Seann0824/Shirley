/**
 * Write plain text to the system clipboard.
 *
 * The async Clipboard API needs a secure context and a user gesture; the Tauri webview
 * normally satisfies both, but a plain-browser dev session (`npm run dev`) can still end up
 * without `navigator.clipboard`. The `execCommand` path is the fallback for exactly those
 * cases -- it is deprecated but it is the only thing that works there.
 *
 * Returns whether the text actually reached the clipboard, so the caller can report failure
 * instead of silently pretending it copied.
 */
export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;

  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Permission denied / not focused: fall through to the legacy path.
    }
  }

  if (typeof document === "undefined") return false;
  const previousFocus = document.activeElement;
  const textarea = document.createElement("textarea");
  textarea.value = text;
  // Keep it out of view and out of the accessibility tree while still selectable.
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.top = "0";
  textarea.style.left = "-9999px";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  try {
    textarea.select();
    textarea.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(textarea);
    // The legacy path steals focus; give it back so typing keeps working.
    if (previousFocus instanceof HTMLElement) previousFocus.focus();
  }
}
