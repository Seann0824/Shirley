// Tab strip rendered **inside the top header bar** (not a separate row).
//
// The strip follows the **session** (Codex desktop model): each session owns its own
// independent tab space. The first tab is always the session's chat surface (labelled
// with the session title, not closable); after it come the web pages opened *within
// this session*; the trailing `+` opens a new session. Switching sessions (from the
// sidebar) swaps the entire strip — tabs never leak across sessions.
//
// Exactly one surface is foreground: either the session's chat tab or one of its web
// tabs. The strip therefore only marks the chat tab active when no web tab is
// foreground.
//
// The strip is intentionally compact (h-7 pills) so it sits within the 48px header
// alongside the window controls and the right-side action buttons — it does not add a
// second bar.

import { Globe, LoaderCircle, MessageSquare, Plus, X } from "lucide-react";
import { cn } from "@/ui/utils";
import type { WebTab } from "@/components/ai/WorkspaceTabs";

const tabBase =
  "group flex h-7 max-w-[200px] min-w-[110px] shrink-0 items-center gap-1.5 rounded-control px-2.5 text-caption cursor-pointer";
const tabActive = "bg-canvas text-ink shadow-sm";
const tabIdle = "text-muted hover:bg-selected";

export function HeaderTabs({
  sessionTitle,
  chatActive,
  onSelectChat,
  webTabs,
  activeWebTabId,
  onSelectWebTab,
  onCloseWebTab,
  onNewSession,
  canNewSession,
}: {
  /** Title of the current session (its chat tab). Empty => "New session". */
  sessionTitle: string;
  /** Whether the session's chat tab is the foreground surface (no web tab active). */
  chatActive: boolean;
  onSelectChat: () => void;
  /** Web tabs opened within the current session only. */
  webTabs: WebTab[];
  activeWebTabId: string | null;
  onSelectWebTab: (id: string) => void;
  onCloseWebTab: (id: string) => void;
  onNewSession: () => void;
  /** Whether the "+" (new session) button is shown (hidden when there is nowhere to create). */
  canNewSession: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
      <div
        role="tab"
        aria-selected={chatActive}
        title={sessionTitle || "New session"}
        onClick={onSelectChat}
        className={cn(tabBase, chatActive ? tabActive : tabIdle)}
      >
        <MessageSquare className="size-3.5 shrink-0 opacity-60" />
        <span className="min-w-0 flex-1 truncate">{sessionTitle || "New session"}</span>
      </div>

      {webTabs.map((tab) => {
        const active = tab.id === activeWebTabId;
        const label = tab.meta?.title || (tab.initialUrl ? tab.initialUrl : "New tab");
        return (
          <div
            key={`w:${tab.id}`}
            role="tab"
            aria-selected={active}
            title={label}
            onClick={() => onSelectWebTab(tab.id)}
            className={cn(tabBase, active ? tabActive : tabIdle)}
          >
            {tab.meta?.loading ? (
              <LoaderCircle className="size-3.5 shrink-0 animate-spin" />
            ) : (
              <Globe className="size-3.5 shrink-0 opacity-60" />
            )}
            <span className="min-w-0 flex-1 truncate">{label}</span>
            <button
              type="button"
              aria-label="Close tab"
              onClick={(event) => {
                event.stopPropagation();
                onCloseWebTab(tab.id);
              }}
              className="flex size-4 shrink-0 items-center justify-center rounded-full text-muted opacity-0 transition-opacity hover:bg-selected hover:text-ink group-hover:opacity-100"
            >
              <X className="size-3" />
            </button>
          </div>
        );
      })}

      {canNewSession && (
        <button
          type="button"
          aria-label="New session"
          title="New session"
          onClick={onNewSession}
          className="flex size-7 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
        >
          <Plus className="size-4" />
        </button>
      )}
    </div>
  );
}
