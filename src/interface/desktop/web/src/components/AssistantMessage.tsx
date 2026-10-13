import { LoaderCircle } from "lucide-react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import { TextDisclosure } from "@/components/ai/TextDisclosure";
import type { AiSegment } from "@/types/ai";

// 迁移自 shiwen FreeChatAssistantMessage，剥离 A2UI surface / 实体投影。
// 审批（pending / approval_state === "pending"）第一期不展示：代码路径保留在
// AiToolExecutionCard 内，但这里不把审批态执行项渲染出来。
//
// 渲染取向（对标 Codex 的「Worked for 20s」）：
// - **思考不展示**：用户只关心「AI 在做什么」，不关心它怎么想。思考过程程序内部
//   消化——reasoning 增量在 App 层就不进段落流（见 types/ai.ts），这里拿到的
//   segments 已经只剩「行为」。界面只保留一个「还在做事」的信号，避免用户把
//   「正在思考」误判成「挂了」。
// - **完成后**：整轮「过程」（工具调用 / 中间叙述）收进一个文字折叠，标题是
//   「处理用时 Ns」；该轮的**最终总结**（最后一段正文）直接渲染在折叠下方。
// - **流式中**：可见内容按发生顺序内联渲染（工具 / 正文），末尾给「正在操作」信号。
// - 时间顺序始终保留：段落严格按发生顺序排列，仅**相邻同类**合并（由上游保证）。
export function AssistantMessage({
  content,
  streaming,
  segments,
  durationMs,
}: {
  content: string;
  streaming: boolean;
  segments: AiSegment[];
  durationMs?: number;
}) {
  // segments 已只含「行为」（reasoning 在 App 层被丢弃）。
  const visible = segments;

  // 流式中：内联按时间顺序渲染可见内容，末尾常驻「还在做事」信号。
  if (streaming) {
    return (
      <div className="flex min-w-0 flex-col gap-3" data-ai-message-timeline>
        {visible.map((segment, index) => (
          <SegmentView
            key={index}
            segment={segment}
            streaming={index === visible.length - 1}
          />
        ))}
        <WorkingLine />
      </div>
    );
  }

  // 没有可见内容（空轮）：不占位。
  if (visible.length === 0) return null;

  // 完成后：最终总结 = 最后一段正文；其余可见段 = 过程。
  const lastContentIndex = visible.reduce(
    (acc, segment, index) => (segment.kind === "content" ? index : acc),
    -1,
  );
  // 压缩标签作为时间线边界，避免被过程折叠吞掉或挪到其他内容前后。
  const groups: { process: boolean; segments: AiSegment[] }[] = [];
  visible.forEach((segment, index) => {
    if (segment.kind === "compact" || index === lastContentIndex) {
      groups.push({ process: false, segments: [segment] });
    } else {
      const last = groups.at(-1);
      if (last?.process) last.segments.push(segment);
      else groups.push({ process: true, segments: [segment] });
    }
  });

  return (
    <div className="flex min-w-0 flex-col gap-3" data-ai-message-timeline>
      {groups.map((group, groupIndex) => group.process ? (
        <TextDisclosure key={groupIndex} label={processLabel(durationMs)} contentClassName="mt-2 flex flex-col gap-3">
          {group.segments.map((segment, index) => (
            <SegmentView key={index} segment={segment} streaming={false} />
          ))}
        </TextDisclosure>
      ) : (
        <div key={groupIndex} data-ai-result={group.segments[0].kind === "content" || undefined}>
          <SegmentView segment={group.segments[0]} streaming={false} />
        </div>
      ))}
    </div>
  );
}

/** 过程折叠标题：有耗时则「处理用时 Ns」，否则退化成「处理过程」。 */
function processLabel(durationMs?: number): string {
  if (durationMs && durationMs > 0) {
    const seconds = Math.max(1, Math.round(durationMs / 1000));
    return `处理用时 ${seconds}s`;
  }
  return "处理过程";
}

/** 「还在做事」信号：思考期间也能让用户看到 AI 没挂，只是不暴露它在想什么。 */
function WorkingLine() {
  return (
    <div className="flex items-center gap-2 text-body-sm text-muted" data-ai-working>
      <LoaderCircle className="shrink-0 animate-spin" size={14} aria-hidden="true" />
      <span className="shimmer">正在操作…</span>
    </div>
  );
}

/** 单个可见段落：工具组折叠 / 正文直接渲染（思考段已在上面被过滤）。 */
function SegmentView({ segment, streaming }: { segment: AiSegment; streaming: boolean }) {
  if (segment.kind === "compact") {
    return (
      <TextDisclosure label="已压缩上下文" contentClassName="mt-2">
        <AiMarkdown content={segment.text} streaming={false} />
      </TextDisclosure>
    );
  }
  if (segment.kind === "tools") {
    return (
      <AiToolActivityDisclosure
        executions={segment.executions}
        decidingToolIds={new Set<number>()}
        onToolDecision={() => {}}
      />
    );
  }
  return <AiMarkdown content={segment.text} streaming={streaming} />;
}
