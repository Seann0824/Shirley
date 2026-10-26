import type { AiToolExecution } from "@/types/ai";
import type { RenderPartWire } from "@/types/wire";
import { RenderParts } from "./RenderParts";

/**
 * Draw a turn's tool products directly in the conversation stream.
 *
 * A run log is something you fold away; a login QR code awaiting a scan is something you look
 * at. Leaving images inside the "process" disclosure means two clicks before the human can
 * even see that there is something to scan, so the non-text parts are lifted out here and the
 * disclosure keeps only the log (`docs/render-parts.md`).
 */
export function ToolArtifacts({ executions }: { executions: AiToolExecution[] }) {
  const parts: RenderPartWire[] = executions
    .flatMap((execution) => execution.parts ?? [])
    .filter((part) => part.kind !== "text");
  if (parts.length === 0) return null;
  return <RenderParts parts={parts} className="min-w-0 space-y-2" />;
}
