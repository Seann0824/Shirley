// Top-level tab architecture for the right-hand workspace area.
//
// The chat is **one tab**; every in-app web page is its own tab. The workspace shows
// exactly one tab at a time, full width — there is no default left/right split. A
// future "split right" is an extra capability of an individual tab, not the base layout.
//
// Only the web tabs are owned here; the chat tab is always present and its content is
// passed in as `children` (the whole conversation surface from App). The chat tab is
// `display:none`-hidden while a web tab is active; the native webview of an inactive
// web tab is hidden by the backend (`activate` on the foreground tab / `hideAll`).
//
// `openWebTab()` / `showChat()` are exposed imperatively so the chrome (Globe button)
// and session switching can drive the tabs without lifting all tab state into App.

import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Globe, LoaderCircle, Plus, X } from "lucide-react";
import { BrowserView, type BrowserTabMeta } from "@/components/ai/BrowserView";
import { browserBridge } from "@/lib/browser-bridge";
import { cn } from "@/ui/utils";

type WebTab = {
  id: string;
  /** Optional URL to load on first activation (from the Globe button). */
  initialUrl?: string;
  /** Live metadata mirrored from the BrowserView (title / loading). */
  meta?: BrowserTabMeta;
};

export type WorkspaceTabsHandle = {
  /** Open a new web tab (optionally loading `url`), and focus it. */
  openWebTab: (url?: string) => void;
  /** Bring the chat tab to the foreground. */
  showChat: () => void;
};

const CHAT_TAB_ID = "chat";

let webSeq = 0;
function newWebTab(url?: string): WebTab {
  webSeq += 1;
  return { id: `w${Date.now().toString(36)}-${webSeq}`, initialUrl: url };
}

function webTabLabel(tab: WebTab): string {
  if (tab.meta?.title) return tab.meta.title;
  if (tab.meta?.url || tab.initialUrl) {
    try {
      return new URL((tab.meta?.url || tab.initialUrl) as string).host;
    } catch {
      return tab.meta?.url || tab.initialUrl || "New tab";
    }
  }
  return "New tab";
}

export const WorkspaceTabs = forwardRef<
  WorkspaceTabsHandle,
  {
    /** Chat surface (the whole conversation area). Rendered as the permanent chat tab. */
    children: ReactNode;
    /** Label for the chat tab (session title). */
    chatTitle: string;
    /** Whether the in-app browser is available (Tauri desktop). When false, no tab
     *  strip is rendered at all and the chat simply fills the area. */
    browserAvailable: boolean;
  }
>(function WorkspaceTabs({ children, chatTitle, browserAvailable }, ref) {
  const [webTabs, setWebTabs] = useState<WebTab[]>([]);
  const [activeId, setActiveId] = useState<string>(CHAT_TAB_ID);
  const activeIdRef = useRef(activeId);
  activeIdRef.current = activeId;
  const webTabsRef = useRef(webTabs);
  webTabsRef.current = webTabs;

  const openWebTab = useCallback((url?: string) => {
    const tab = newWebTab(url);
    setWebTabs((prev) => [...prev, tab]);
    setActiveId(tab.id);
  }, []);

  const showChat = useCallback(() => {
    setActiveId(CHAT_TAB_ID);
    // Hide every native webview while the chat tab is foreground.
    void browserBridge.hideAll();
  }, []);

  useImperativeHandle(ref, () => ({ openWebTab, showChat }), [openWebTab, showChat]);

  const closeWebTab = useCallback((id: string) => {
    void browserBridge.close(id);
    setWebTabs((prev) => {
      const next = prev.filter((tab) => tab.id !== id);
      if (id === activeIdRef.current) {
        if (next.length === 0) {
          showChat();
        } else {
          const idx = prev.findIndex((tab) => tab.id === id);
          setActiveId(next[Math.max(0, idx - 1)].id);
        }
      }
      return next;
    });
  }, [showChat]);

  const onMeta = useCallback((meta: BrowserTabMeta) => {
    setWebTabs((prev) =>
      prev.map((tab) => (tab.id === meta.tabId ? { ...tab, meta } : tab)),
    );
  }, []);

  const chatActive = activeId === CHAT_TAB_ID;

  // Browser unavailable (cloud / mock): no tab chrome, chat fills the workspace.
  if (!browserAvailable) {
    return <div className="flex min-h-0 min-w-0 flex-1 flex-col">{children}</div>;
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col bg-canvas">
      {/* Tab strip */}
      <div className="flex h-10 shrink-0 items-center gap-1 border-b border-line bg-frame px-2">
        <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
          {/* Chat tab (permanent) */}
          <div
            role="tab"
            aria-selected={chatActive}
            onClick={showChat}
            className={cn(
              "group flex h-7 max-w-[220px] min-w-[120px] shrink-0 cursor-pointer items-center gap-1.5 rounded-control px-2.5 text-caption",
              chatActive ? "bg-canvas text-ink shadow-sm" : "text-muted hover:bg-selected",
            )}
          >
            <Globe className="size-3.5 shrink-0 opacity-60" />
            <span className="min-w-0 flex-1 truncate">{chatTitle}</span>
          </div>
          {/* Web tabs */}
          {webTabs.map((tab) => {
            const active = tab.id === activeId;
            return (
              <div
                key={tab.id}
                role="tab"
                aria-selected={active}
                onClick={() => setActiveId(tab.id)}
                className={cn(
                  "group flex h-7 max-w-[220px] min-w-[120px] shrink-0 cursor-pointer items-center gap-1.5 rounded-control px-2.5 text-caption",
                  active ? "bg-canvas text-ink shadow-sm" : "text-muted hover:bg-selected",
                )}
              >
                {tab.meta?.loading ? (
                  <LoaderCircle className="size-3.5 shrink-0 animate-spin" />
                ) : (
                  <span className="size-3.5 shrink-0 rounded-full bg-line" aria-hidden />
                )}
                <span className="min-w-0 flex-1 truncate">{webTabLabel(tab)}</span>
                <button
                  type="button"
                  aria-label="Close tab"
                  onClick={(event) => {
                    event.stopPropagation();
                    closeWebTab(tab.id);
                  }}
                  className="flex size-4 shrink-0 items-center justify-center rounded-full text-muted opacity-0 transition-opacity hover:bg-selected hover:text-ink group-hover:opacity-100"
                >
                  <X className="size-3" />
                </button>
              </div>
            );
          })}
        </div>
        <button
          type="button"
          aria-label="New browser tab"
          onClick={() => openWebTab()}
          className="flex size-7 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
        >
          <Plus className="size-4" />
        </button>
      </div>

      {/* Content: chat tab + one BrowserView per web tab, only one visible. */}
      <div className="relative flex min-h-0 flex-1">
        <div
          className={cn("min-h-0 min-w-0 flex-1 flex-col", chatActive ? "flex" : "hidden")}
          aria-hidden={!chatActive}
        >
          {children}
        </div>
        {webTabs.map((tab) => (
          <BrowserView
            key={tab.id}
            tabId={tab.id}
            active={tab.id === activeId}
            initialUrl={tab.initialUrl}
            onMeta={onMeta}
            onClose={() => closeWebTab(tab.id)}
          />
        ))}
      </div>
    </div>
  );
});
