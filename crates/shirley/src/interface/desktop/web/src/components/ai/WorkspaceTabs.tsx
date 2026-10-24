// Content surface for the right-hand workspace area.
//
// Tabs themselves live in the top header bar (see `HeaderTabs`), not in a separate
// strip: the workspace is a single full-width surface that switches between the chat
// (the foreground session) and one in-app web page per web tab. This component only
// paints that content and keeps every web tab's `BrowserView` mounted (the active one
// visible). The tab *state* is owned by `App` so the header can render it.

import type { ReactNode } from "react";
import { BrowserView, type BrowserTabMeta } from "@/components/ai/BrowserView";
import { cn } from "@/ui/utils";

/** One in-app web tab (native child webview per tab). Owned by `App`. */
export type WebTab = {
  id: string;
  /** Optional URL to load on first activation (from the Globe button). */
  initialUrl?: string;
  /** Live metadata mirrored from the BrowserView (title / loading). */
  meta?: BrowserTabMeta;
};

export function WorkspaceTabs({
  children,
  webTabs,
  activeWebTabId,
  onMeta,
  onCloseWebTab,
}: {
  /** Chat surface (the whole conversation area). Shown when no web tab is foreground. */
  children: ReactNode;
  webTabs: WebTab[];
  /** Foreground web tab id; `null` = the chat is foreground. */
  activeWebTabId: string | null;
  onMeta: (meta: BrowserTabMeta) => void;
  onCloseWebTab: (id: string) => void;
}) {
  const chatActive = activeWebTabId === null;

  return (
    <div className="relative flex min-h-0 min-w-0 flex-1 flex-col bg-canvas">
      {/* Content: chat surface + one BrowserView per web tab, only one visible. */}
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
            active={tab.id === activeWebTabId}
            initialUrl={tab.initialUrl}
            onMeta={onMeta}
            onClose={() => onCloseWebTab(tab.id)}
          />
        ))}
      </div>
    </div>
  );
}
