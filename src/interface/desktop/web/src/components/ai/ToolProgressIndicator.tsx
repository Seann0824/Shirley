import { LoaderCircle } from "lucide-react";
import type { AiToolExecution } from "@/types/ai";

const HIGH_TOOL_COUNT_THRESHOLD = 10;

export function ToolProgressIndicator({
  visible,
  executions,
}: {
  visible: boolean;
  executions: AiToolExecution[];
}) {
  if (!visible) return null;
  const total = executions.length;
  if (total === 0) return null;
  const running = executions.some(
    (execution) => execution.status === "running" || execution.status === "pending",
  );
  const highCount = total > HIGH_TOOL_COUNT_THRESHOLD;

  return (
    <div className="flex items-center gap-2 px-2 pt-1.5 text-caption text-muted">
      {running && <LoaderCircle className="size-3.5 animate-spin" aria-hidden="true" />}
      <span>
        {running ? "正在调用工具" : "工具调用"} · 已调用 {total} 个
      </span>
      {highCount && <span className="hidden sm:inline">· 调用次数较多，可能需要更长时间…</span>}
    </div>
  );
}
