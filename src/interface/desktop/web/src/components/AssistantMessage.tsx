import { Brain, ChevronDown } from "lucide-react";
import { useState } from "react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";
import type { AiSegment } from "@/types/ai";

// 迁移自 shiwen FreeChatAssistantMessage，剥离 A2UI surface / 实体投影。
// 审批（pending / approval_state === "pending"）第一期不展示：代码路径保留在
// AiToolExecutionCard 内，但这里不把审批态执行项渲染出来。
//
// 对标 TUI 的线性渲染：TUI 把一轮回复渲染成有序的 `items`（思考块 → 正文 →
// 工具组 → 正文 → 思考 → 工具 …），按事件发生顺序排列。这里用同一套 `segments`：
// App 按 `AgentEvent` 到达顺序把一轮切成 content / reasoning / tools 三种段落，
// 本组件**按数组顺序**渲染——谁先发生谁在前，而不是把所有工具按内容偏移归并到
// 一处。思考段渲染成折叠区（对标 TUI 的 🧠 思考 块）。
export function AssistantMessage({
  content,
  streaming,
  segments,
}: {
  content: string;
  streaming: boolean;
  segments: AiSegment[];
}) {
  // 还没有任何段落、但正在流式：给个占位提示（与旧行为一致）。
  if (segments.length === 0) {
    return streaming ? <span className="shimmer">正在思考并操作…</span> : null;
  }

  return (
    <div className="flex min-w-0 flex-col gap-3" data-ai-message-timeline>
      {segments.map((segment, index) => {
        if (segment.kind === "reasoning") {
          return (
            <ReasoningDisclosure
              key={`reasoning:${index}`}
              reasoning={segment.text}
              streaming={streaming && index === segments.length - 1}
            />
          );
        }
        if (segment.kind === "tools") {
          return (
            <AiToolActivityDisclosure
              key={`tools:${index}`}
              executions={segment.executions}
              decidingToolIds={new Set<number>()}
              onToolDecision={() => {}}
            />
          );
        }
        return (
          <AiMarkdown
            key={`content:${index}`}
            content={segment.text}
            streaming={streaming && index === segments.length - 1}
          />
        );
      })}
    </div>
  );
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
