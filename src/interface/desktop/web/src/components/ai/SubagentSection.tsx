import { useEffect, useRef, useState } from "react";
import { Bot, ChevronDown, ChevronRight, Loader2, Square } from "lucide-react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";
import { agentBridge, type HistoryMessage, type SubagentSummary } from "@/lib/bridge";

/**
 * Subagents of the current session (`docs/subagent.md` decision 10, P2).
 *
 * Reads `index.jsonl` via `agent_list_subagents` and renders one row per dispatched
 * subagent. Clicking a row lazily loads that subagent's transcript (`<id>.jsonl`) and
 * expands it inline, reusing the same `HistoryMessage` shape as the parent transcript.
 * A running subagent can be cancelled from its row (`agent_cancel_subagent`).
 *
 * Self-contained: it holds no cross-session state, so a project/session switch just
 * remounts it (keyed by `project::session`).
 */
export function SubagentSection({
  project,
  session,
  className,
}: {
  /** Owning project id (routes the command; a session name is unique only within a project). */
  project: string;
  /** Owning session name. */
  session: string;
  className?: string;
}) {
  const [open, setOpen] = useState(true);
  const [items, setItems] = useState<SubagentSummary[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [transcripts, setTranscripts] = useState<Record<string, HistoryMessage[]>>({});
  const aliveRef = useRef(true);

  useEffect(() => {
    aliveRef.current = true;
    const load = async () => {
      try {
        const bridge = await agentBridge();
        const list = await bridge.listSubagents(project, session);
        if (aliveRef.current) setItems(list);
      } catch {
        // Silent poll failure: keep the previous list rather than clearing it.
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 1500);
    return () => {
      aliveRef.current = false;
      window.clearInterval(timer);
    };
  }, [project, session]);

  const toggle = async (id: string) => {
    if (expanded === id) {
      setExpanded(null);
      return;
    }
    setExpanded(id);
    // Load the transcript on demand; refetch every time so a still-running child's
    // appended messages show up.
    try {
      const bridge = await agentBridge();
      const messages = await bridge.loadSubagent(project, session, id);
      if (aliveRef.current) setTranscripts((prev) => ({ ...prev, [id]: messages }));
    } catch {
      // Leave whatever we had; the row stays expanded.
    }
  };

  const cancel = async (id: string) => {
    try {
      const bridge = await agentBridge();
      await bridge.cancelSubagent(project, session, id);
    } catch {
      // Idempotent on the Rust side; ignore.
    }
  };

  if (items.length === 0) return null;

  return (
    <section className={cn("mb-3", className)}>
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleTrigger className="flex h-8 w-full items-center gap-1 px-2 text-left">
          <span className="min-w-0 truncate text-[0.875rem] font-medium text-muted">Subagents</span>
          <span className="shrink-0 text-caption text-muted">{items.length}</span>
          {open ? (
            <ChevronDown className="size-3.5 shrink-0 text-muted" />
          ) : (
            <ChevronRight className="size-3.5 shrink-0 text-muted" />
          )}
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div role="listbox" aria-label="Subagents" className="flex flex-col gap-0.5">
            {items.map((item) => (
              <div key={item.id}>
                <div className="group flex items-start gap-1.5 rounded-control px-2 py-1.5 transition-colors hover:bg-quiet">
                  <button
                    type="button"
                    className="flex min-w-0 flex-1 items-start gap-1.5 text-left"
                    onClick={() => void toggle(item.id)}
                  >
                    {item.status === "running" ? (
                      <Loader2 className="mt-0.5 size-3.5 shrink-0 animate-spin text-muted" />
                    ) : (
                      <Bot className="mt-0.5 size-3.5 shrink-0 text-muted" />
                    )}
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate text-body-sm text-ink">
                          {item.task || item.id}
                        </span>
                        <span className="shrink-0 text-caption text-muted">{item.kind}</span>
                      </span>
                      <span className="block truncate text-caption text-muted">
                        {item.status}
                        {item.status === "running" ? "" : ` · ${formatMs(item.elapsed_ms)}`}
                      </span>
                    </span>
                  </button>
                  {item.status === "running" && (
                    <button
                      type="button"
                      aria-label={`Cancel ${item.id}`}
                      title="Cancel"
                      className="hidden size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors group-hover:flex hover:bg-selected hover:text-ink"
                      onClick={() => void cancel(item.id)}
                    >
                      <Square className="size-3" />
                    </button>
                  )}
                </div>
                {expanded === item.id && (
                  <div className="ml-4 border-l border-line py-1 pl-3">
                    {(transcripts[item.id] ?? []).length === 0 ? (
                      <p className="px-1 py-1 text-caption text-muted">No transcript yet.</p>
                    ) : (
                      (transcripts[item.id] ?? []).map((message, index) => (
                        <div key={index} className="mb-2">
                          <div className="text-caption font-medium text-muted">
                            {message.role === "user" ? "task" : "agent"}
                          </div>
                          <pre className="whitespace-pre-wrap break-words text-caption text-ink">
                            {message.content}
                          </pre>
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>
            ))}
          </div>
        </CollapsibleContent>
      </Collapsible>
    </section>
  );
}

/** Human-readable elapsed time (ms -> "1.2s" / "3m04s"). */
function formatMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.floor(seconds % 60);
  return `${minutes}m${String(rest).padStart(2, "0")}s`;
}
