// Single in-app browser tab (one native child webview per tab).
//
// The page itself is a **native child webview** owned by Rust (`agent_browser_*`
// commands); this component only paints the chrome (address bar / nav buttons) and
// reports the content rectangle so the native webview can float on top of it.
//
// One `BrowserView` == one tab == one native webview (label `browser-<tabId>`).
// All tabs stay mounted (the parent keeps them alive); only the `active` one is
// shown. When `active` flips on we (re)position and `activate` the webview; when the
// parent switches to another tab it either `activate`s that one (which hides us) or
// `hideAll`s (chat tab).
//
// State (url / title / loading / history flags) is pushed from Rust via
// `browser://state`; we filter by `tabId` and mirror it up to the parent so the tab
// strip can render the live title + loading spinner.

import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, ArrowRight, LoaderCircle, RotateCw, Search, X } from "lucide-react";
import { browserBridge, type BrowserTabState } from "@/lib/browser-bridge";
import { cn } from "@/ui/utils";

/** Live per-tab metadata mirrored up to the tab strip. */
export type BrowserTabMeta = {
  tabId: string;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
};

export function BrowserView({
  tabId,
  active,
  initialUrl,
  onMeta,
  onClose,
}: {
  tabId: string;
  /** Whether this tab is the foreground tab. Inactive tabs are CSS-hidden and their
   *  native webview is hidden by the backend (`activate` on another tab / `hideAll`). */
  active: boolean;
  /** Optional URL to load on first activation (Globe button may pass one). */
  initialUrl?: string;
  onMeta: (meta: BrowserTabMeta) => void;
  onClose: () => void;
}) {
  const [url, setUrl] = useState(initialUrl ?? "");
  const [title, setTitle] = useState("");
  const [loading, setLoading] = useState(false);
  const [canGoBack, setCanGoBack] = useState(false);
  const [canGoForward, setCanGoForward] = useState(false);
  // Whether the native webview has been created for this tab.
  const [created, setCreated] = useState(false);
  // Address-bar draft while editing; falls back to the committed URL otherwise.
  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState(false);

  const contentRef = useRef<HTMLDivElement>(null);
  const createdRef = useRef(created);
  createdRef.current = created;

  // Mirror live state up to the tab strip.
  useEffect(() => {
    onMeta({ tabId, url, title, loading, canGoBack, canGoForward });
  }, [tabId, url, title, loading, canGoBack, canGoForward, onMeta]);

  // Per-tab state pushed from Rust (filtered by tab id).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void browserBridge
      .onState((state: BrowserTabState) => {
        if (state.tabId !== tabId) return;
        setUrl((prev) => state.url || prev);
        setTitle((prev) => state.title || prev);
        setLoading(state.loading);
        setCanGoBack(state.canGoBack);
        setCanGoForward(state.canGoForward);
      })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [tabId]);

  /** Measure the content rectangle (CSS px = Tauri logical px) and push it down. */
  const syncBounds = useCallback(() => {
    const el = contentRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    if (rect.width < 2 || rect.height < 2) return;
    if (createdRef.current) {
      void browserBridge.setBounds(tabId, {
        x: rect.left,
        y: rect.top,
        width: rect.width,
        height: rect.height,
      });
    }
  }, [tabId]);

  // Keep the active webview positioned + shown as the panel resizes / tab activates.
  useEffect(() => {
    if (!active) return;
    const el = contentRef.current;
    if (!el) return;
    const run = () => {
      syncBounds();
      if (createdRef.current) void browserBridge.activate(tabId);
    };
    run();
    const observer = new ResizeObserver(() => run());
    observer.observe(el);
    window.addEventListener("resize", run);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", run);
    };
  }, [active, created, syncBounds, tabId]);

  const commitNavigation = useCallback(
    async (rawUrl: string) => {
      const value = rawUrl.trim();
      if (!value) return;
      // Optimistically reflect the typed URL; Rust normalizes it (adds https://).
      setUrl(value);
      const rect = contentRef.current?.getBoundingClientRect();
      const bounds = rect
        ? { x: rect.left, y: rect.top, width: rect.width, height: rect.height }
        : { x: 0, y: 0, width: 1, height: 1 };
      if (!createdRef.current) {
        await browserBridge.create(tabId, value, bounds);
        setCreated(true);
        createdRef.current = true;
        void browserBridge.activate(tabId);
      } else {
        await browserBridge.navigate(tabId, value);
      }
      setEditing(false);
    },
    [tabId],
  );

  // Load the initial URL once on first activation.
  const initialDone = useRef(false);
  useEffect(() => {
    if (!active || initialDone.current) return;
    initialDone.current = true;
    if (initialUrl) void commitNavigation(initialUrl);
  }, [active, initialUrl, commitNavigation]);

  const addressValue = editing ? draft : url;

  return (
    <div
      className={cn("min-h-0 min-w-0 flex-1 flex-col", active ? "flex" : "hidden")}
      aria-hidden={!active}
    >
      {/* Address bar */}
      <div className="flex h-11 shrink-0 items-center gap-1 border-b border-line px-2">
        <button
          type="button"
          aria-label="Back"
          disabled={!canGoBack}
          onClick={() => void browserBridge.back(tabId)}
          className="flex size-8 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink disabled:pointer-events-none disabled:opacity-30"
        >
          <ArrowLeft className="size-4" />
        </button>
        <button
          type="button"
          aria-label="Forward"
          disabled={!canGoForward}
          onClick={() => void browserBridge.forward(tabId)}
          className="flex size-8 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink disabled:pointer-events-none disabled:opacity-30"
        >
          <ArrowRight className="size-4" />
        </button>
        <button
          type="button"
          aria-label="Reload"
          onClick={() => void browserBridge.reload(tabId)}
          className="flex size-8 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
        >
          {loading ? <LoaderCircle className="size-4 animate-spin" /> : <RotateCw className="size-4" />}
        </button>
        <form
          className="flex min-w-0 flex-1 items-center"
          onSubmit={(event) => {
            event.preventDefault();
            void commitNavigation(draft);
          }}
        >
          <div className="flex h-8 min-w-0 flex-1 items-center gap-1.5 rounded-control bg-inset px-2.5">
            <Search className="size-3.5 shrink-0 text-muted" />
            <input
              value={addressValue}
              spellCheck={false}
              onFocus={() => {
                setDraft(url);
                setEditing(true);
              }}
              onChange={(event) => {
                setEditing(true);
                setDraft(event.target.value);
              }}
              onBlur={() => setEditing(false)}
              placeholder="Enter a URL"
              className="min-w-0 flex-1 bg-transparent text-caption text-ink outline-none placeholder:text-muted"
            />
          </div>
        </form>
        <button
          type="button"
          aria-label="Close tab"
          onClick={onClose}
          className="flex size-8 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
        >
          <X className="size-4" />
        </button>
      </div>

      {/* Content area: the native child webview floats exactly over this rectangle. */}
      <div ref={contentRef} className="relative min-h-0 flex-1 overflow-hidden bg-canvas">
        {!created && (
          <div className="flex h-full flex-col items-center justify-center gap-2 px-8 text-center text-muted">
            <Search className="size-8 opacity-40" />
            <p className="text-body-sm">Type an address above to browse in-app.</p>
            <p className="text-caption opacity-70">Pages open right here, as their own tab.</p>
          </div>
        )}
      </div>
    </div>
  );
}
