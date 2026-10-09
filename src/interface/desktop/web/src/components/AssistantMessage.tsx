import { Brain, ChevronDown } from "lucide-react";
import { useState } from "react";
import { AiMessageTimeline, type AiMessageTimelineItem } from "@/components/ai/AiMessageTimeline";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";
import type { AiToolExecution } from "@/types/ai";

// 迁移自 shiwen FreeChatAssistantMessage，剥离 A2UI surface / 实体投影。
// 审批（pending / approval_state === "pending"）第一期不展示：代码路径保留在
// AiToolExecutionCard 内，但这里不把审批态执行项渲染出来。
//
// 对标 TUI 的线性渲染：TUI 把一轮回复渲染成「思考 → 正文 → 工具」的顺序流
// （`app.rs::add_message`）。这里用同一套：思考经 ReasoningDelta 累积成文本，
// 渲染成正文**上方**的一个折叠区；工具执行按 `content_offset` 落到正文的对应
// 位置，由 `AiMessageTimeline` 交错切分——而不是全部堆在正文顶部。
export function AssistantMessage({
  content,
  streaming,
  reasoning,
  executions,
}: {
  content: string;
  streaming: boolean;
  reasoning: string;
  executions: AiToolExecution[];
}) {
  const items: AiMessageTimelineItem[] = [];

  // 思考：正文上方的折叠区（对标 TUI 的 🧠 思考 块）。
  if (reasoning.trim()) {
    items.push({
      key: "reasoning",
      contentOffset: 0,
      content: <ReasoningDisclosure reasoning={reasoning} streaming={streaming} />,
    });
  }

  // 工具：按 content_offset 分组，落到正文对应位置。
  // 用码点计长与 AiMessageTimeline 的口径一致（Array.from）。
  const contentLength = Array.from(content).length;
  const groups = new Map<number, AiToolExecution[]>();
  for (const execution of executions) {
    const raw = execution.content_offset;
    const offset =
      typeof raw === "number" && Number.isFinite(raw)
        ? Math.min(Math.max(Math.trunc(raw), 0), contentLength)
        : 0;
    groups.set(offset, [...(groups.get(offset) ?? []), execution]);
  }
  for (const [offset, group] of [...groups.entries()].sort((a, b) => a[0] - b[0])) {
    items.push({
      key: `tools:${offset}`,
      contentOffset: offset,
      content: (
        <AiToolActivityDisclosure
          executions={group}
          decidingToolIds={new Set<number>()}
          onToolDecision={() => {}}
        />
      ),
    });
  }

  return <AiMessageTimeline content={content} streaming={streaming} items={items} />;
}

function ReasoningDisclosure({
  reasoning,
  streaming,
}: {
  reasoning: string;
  streaming: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <Button type="button" variant="ghost" size="sm" className="text-muted">
          <Brain data-icon="inline-start" />
          <span>{streaming ? "正在思考" : "思考过程"}</span>
          <ChevronDown
            className={cn("transition-transform duration-150", open && "rotate-180")}
            data-icon="inline-end"
          />
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-2 border-l-2 border-line-strong pl-3 text-body-sm whitespace-pre-wrap text-muted">
        {reasoning}
      </CollapsibleContent>
    </Collapsible>
  );
}
