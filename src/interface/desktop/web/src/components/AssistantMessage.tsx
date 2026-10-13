import { useEffect, useState } from "react";
import { Archive, LoaderCircle } from "lucide-react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import { TextDisclosure } from "@/components/ai/TextDisclosure";
import type { AiSegment } from "@/types/ai";
import type { StopReason } from "@/types/wire";

/** 一轮只有一个过程折叠；compact 是过程事件，不再拆分外层分组。 */
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
    // 结束时自动收起；完成后用户仍可手动展开，普通增量不重置其选择。
    setOpen(streaming);
  }, [streaming]);

  const timeline: AiSegment[] = segments.length
    ? segments
    : content ? [{ kind: "content", text: content }] : [];
  const last = timeline.at(-1);
  // 工具之后没有正文时，不把“让我检查一下”这种中间说明冒充最终答复。
  const hasResult = !streaming && !failed && stopReason !== "cancelled" && last?.kind === "content";
  const result = hasResult ? last : undefined;
  const process = hasResult ? timeline.slice(0, -1) : timeline;
  const hasProcess = streaming || process.length > 0 || failed || stopReason === "cancelled";
  if (!hasProcess && !result) return null;

  const state = streaming ? "正在处理" : failed ? "处理失败" : stopReason === "cancelled" ? "已停止" : "处理用时";
  const elapsed = formatDuration(durationMs);
  const label = streaming
    ? "正在处理…"
    : elapsed ? `${state} ${elapsed}` : state === "处理用时" ? "处理过程" : state;

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
    ? `${hours}小时 ${minutes}分 ${remainder}秒`
    : minutes ? `${minutes}分 ${remainder}秒` : `${seconds}秒`;
}

function SegmentView({ segment, streaming }: { segment: AiSegment; streaming: boolean }) {
  if (segment.kind === "compact") {
    return (
      <TextDisclosure
        label="已压缩上下文"
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
