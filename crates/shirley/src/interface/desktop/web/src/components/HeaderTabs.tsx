// Tab strip rendered **inside the top header bar** (not a separate row).
//
// Two kinds of tabs share one strip:
//   - session tabs: one per open session (`project` + `session`); clicking focuses that
//     session (the chat surface), which is the primary content of the app.
//   - web tabs: in-app browser pages (desktop only); clicking focuses that page.
//
// Exactly one surface is foreground: either a session (chat) or a web tab. The strip
// therefore only marks a session tab active when no web tab is foreground.
//
// The strip is intentionally compact (h-7 pills) so it sits within the 48px header
// alongside the window controls and the right-side action buttons — it does not add a
// second bar.

import { Globe, LoaderCircle, MessageSquare, Plus, X } from "lucide-react";
import { cn } from "@/ui/utils";
import type { WebTab } from "@/components/ai/WorkspaceTabs";

export type SessionTab = { project: string; session: string };

const tabBase =
  "group flex h-7 max-w-[200px] min-w-[110px] shrink-0 items-center gap-1.5 rounded-control px-2.5 text-caption cursor-pointer";
const tabActive = "bg-canvas text-ink shadow-sm";
const tabIdle = "text-muted hover:bg-selected";

export function HeaderTabs({
  sessionTabs,
  activeProject,
  sessionName,
  sessionTitles,
  onSelectSession,
  onCloseSession,
  onNewSession,
  webTabs,
  activeWebTabId,
  onSelectWebTab,
  onCloseWebTab,
  canNewSession,
}: {
  sessionTabs: SessionTab[];
  activeProject: string | null;
  sessionName: string | null;
  /** Session titles keyed by `project::session`. */
  sessionTitles: Record<string, string>;
  onSelectSession: (project: string, session: string) => void;
  onCloseSession: (project: string, session: string) => void;
  onNewSession: () => void;
  webTabs: WebTab[];
  activeWebTabId: string | null;
  onSelectWebTab: (id: string) => void;
  onCloseWebTab: (id: string) => void;
  /** Whether the "+" (new session) button is shown (hidden when there is nowhere to create). */
  canNewSession: boolean;
}) {
  const chatForeground = activeWebTabId === null;
  return (
    <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
      {sessionTabs.map((tab) => {
        const key = `${tab.project}::${tab.session}`;
        const active = chatForeground && tab.project === activeProject && tab.session === sessionName;
        const label = sessionTitles[key] || "New session";
        return (
          <div
            key={`s:${key}`}
            role="tab"
            aria-selected={active}
            title={label}
            onClick={() => onSelectSession(tab.project, tab.session)}
            className={cn(tabBase, active ? tabActive : tabIdle)}
          >
            <MessageSquare className="size-3.5 shrink-0 opacity-60" />
            <span className="min-w-0 flex-1 truncate">{label}</span>
            <button
              type="button"
              aria-label="Close tab"
              onClick={(event) => {
                event.stopPropagation();
                onCloseSession(tab.project, tab.session);
              }}
              className="flex size-4 shrink-0 items-center justify-center rounded-full text-muted opacity-0 transition-opacity hover:bg-selected hover:text-ink group-hover:opacity-100"
            >
              <X className="size-3" />
            </button>
          </div>
        );
      })}

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
