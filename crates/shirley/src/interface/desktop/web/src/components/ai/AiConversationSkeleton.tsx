import { cn } from "@/ui/utils";

/**
 * Conversation-history loading skeleton.
 *
 * When switching / opening a session, the history snapshot needs one `agent_subscribe`
 * round-trip; rather than letting the UI "stick on the old session" or "flash blank"
 * during that time, render a screen of placeholder skeleton first — the user immediately
 * sees the feedback that "the session I clicked is already loading" (respond to the
 * interaction first, fill in content after).
 *
 * The shape deliberately mirrors a real conversation: one right-aligned user bubble + two
 * left-aligned assistant replies (avatar + several text lines), to avoid a big layout jump
 * once loading completes.
 */
export function AiConversationSkeleton({ className }: { className?: string }) {
  const lineWidths = ["78%", "92%", "60%", "84%"];
  return (
    <div
      className={cn("flex flex-col gap-6 px-1 py-6 md:px-3", className)}
      aria-busy="true"
      aria-label="Loading message history"
    >
      {/* User message (right-aligned bubble) */}
      <div className="flex justify-end">
        <div className="h-9 w-2/5 animate-pulse-soft rounded-2xl bg-shimmer" />
      </div>
      {/* Assistant reply (left-aligned: avatar + multi-line text) */}
      {[0, 1].map((row) => (
        <div key={row} className="flex flex-col gap-3">
          <div className="size-7 animate-pulse-soft rounded-full bg-shimmer" />
          <div className="flex flex-col gap-2.5 pl-9">
            {lineWidths.map((width, index) => (
              <div
                key={index}
                className="h-3.5 animate-pulse-soft rounded-full bg-shimmer"
                style={{ width }}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
