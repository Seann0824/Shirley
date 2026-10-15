import { useEffect, useState } from "react";
import { Archive, LoaderCircle } from "lucide-react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import { TextDisclosure } from "@/components/ai/TextDisclosure";
import type { AiSegment } from "@/types/ai";
import type { StopReason } from "@/types/wire";

/** A turn has only one process disclosure; compact is a process event and is no longer split into an outer group. */
export function AssistantMessage({
  content,
  streaming,
  segments,
  durationMs,
  failed = false,
  stopReason,
}: {
  content: string;
  streaming: boolean;
  segments: AiSegment[];
  durationMs?: number;
  failed?: boolean;
  stopReason?: StopReason;
}) {
  const [open, setOpen] = useState(streaming);
  useEffect(() => {
    // Auto-collapse when finished; after completion the user can still expand manually, and ordinary deltas do not reset the choice.
    setOpen(streaming);
  }, [streaming]);

  const timeline: AiSegment[] = segments.length
    ? segments
    : content ? [{ kind: "content", text: content }] : [];
  const last = timeline.at(-1);
  // When there is no body text after the tools, do not pass off intermediate notes like "let me check" as the final answer.
  const hasResult = !streaming && !failed && stopReason !== "cancelled" && last?.kind === "content";
  const result = hasResult ? last : undefined;
  const process = hasResult ? timeline.slice(0, -1) : timeline;
  const hasProcess = streaming || process.length > 0 || failed || stopReason === "cancelled";
  if (!hasProcess && !result) return null;

  const state = streaming ? "Processing" : failed ? "Processing failed" : stopReason === "cancelled" ? "Stopped" : "Took";
  const elapsed = formatDuration(durationMs);
  const label = streaming
    ? "Processing..."
    : elapsed ? `${state} ${elapsed}` : state === "Took" ? "Process" : state;

  return (
    <div className="flex min-w-0 flex-col gap-4" data-ai-message-timeline>
      {hasProcess && (
        <TextDisclosure
          label={label}
          open={open}
          onOpenChange={setOpen}
          leading={streaming ? <LoaderCircle size={14} className="shrink-0 animate-spin" aria-hidden="true" /> : undefined}
          className="min-w-0"
          triggerClassName="w-full max-w-full rounded-none border-b border-line pt-1 pb-3 text-left"
          contentClassName="flex min-w-0 flex-col gap-4 pt-4 pb-1"
        >
          {process.map((segment, index) => (
            <SegmentView key={index} segment={segment} streaming={streaming && index === process.length - 1} />
          ))}
        </TextDisclosure>
      )}
      {result?.kind === "content" && (
        <div data-ai-result>
          <AiMarkdown content={result.text} streaming={false} />
        </div>
      )}
    </div>
  );
}

function formatDuration(durationMs?: number): string | null {
  if (durationMs === undefined || !Number.isFinite(durationMs) || durationMs < 0) return null;
  const seconds = Math.max(1, Math.round(durationMs / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor(seconds % 3600 / 60);
  const remainder = seconds % 60;
  return hours
    ? `${hours}h ${minutes}m ${remainder}s`
    : minutes ? `${minutes}m ${remainder}s` : `${seconds}s`;
}

function SegmentView({ segment, streaming }: { segment: AiSegment; streaming: boolean }) {
  if (segment.kind === "compact") {
    return (
      <TextDisclosure
        label="Context compacted"
        leading={<Archive size={14} className="shrink-0" aria-hidden="true" />}
        className="min-w-0"
        contentClassName="mt-2 max-h-80 overflow-auto rounded-control bg-inset p-3"
      >
        <AiMarkdown content={segment.text} streaming={false} />
      </TextDisclosure>
    );
  }
  if (segment.kind === "tools") {
    return <AiToolActivityDisclosure executions={segment.executions} decidingToolIds={new Set<number>()} onToolDecision={() => {}} />;
  }
  return <AiMarkdown content={segment.text} streaming={streaming} />;
}
