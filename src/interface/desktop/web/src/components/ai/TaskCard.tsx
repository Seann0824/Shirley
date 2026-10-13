import { useState } from "react";
import { CheckCircle2, ChevronDown, Circle, ListChecks, LoaderCircle } from "lucide-react";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";
import type { TaskViewWire } from "@/types/wire";

// 任务账本卡片：把「模型自己记的任务进度」直接摆给用户看。
//
// 为什么需要它：todo 账本此前只经 ContextProvider 注入给模型，用户完全看不到
// 「任务在推进」这个最直接的信号。这里消费后端在 `todo` 工具完成时发的
// `todo_updated` 结构化快照（不解析注入 XML）。
//
// 折叠策略：目标 + 进度常驻（一眼可见），步骤清单默认收起——步骤是「过程」，
// 用户主要关心「目标是什么、推进到哪了」；想看细节再展开。
export function TaskCard({ task }: { task: TaskViewWire }) {
  const [open, setOpen] = useState(false);
  const done = task.total > 0 && task.completed === task.total;

  return (
    <div className="mx-3 mb-2 rounded-card border border-line bg-surface px-3 py-2 text-body-sm">
      <Collapsible open={open} onOpenChange={setOpen}>
        <div className="flex items-center gap-2">
          <ListChecks className="size-3.5 shrink-0 text-muted" aria-hidden="true" />
          <span className="min-w-0 flex-1 truncate text-ink">
            {task.goal ?? "任务"}
          </span>
          {task.total > 0 && (
            <span
              className={cn(
                "shrink-0 font-utility text-caption",
                done ? "text-ink" : "text-muted",
              )}
            >
              {task.completed}/{task.total}
            </span>
          )}
          {task.steps.length > 0 && (
            <CollapsibleTrigger asChild>
              <Button type="button" variant="ghost" size="sm" className="text-muted">
                <span className="sr-only">展开任务步骤</span>
                <ChevronDown
                  className={cn("transition-transform duration-150", open && "rotate-180")}
                  aria-hidden="true"
                />
              </Button>
            </CollapsibleTrigger>
          )}
        </div>
        {task.steps.length > 0 && (
          <CollapsibleContent className="mt-2 flex flex-col gap-1 border-l-2 border-line-strong pl-3">
            {task.steps.map((step, index) => (
              <div key={index} className="flex items-start gap-2 text-body-sm">
                <StepIcon status={step.status} />
                <span
                  className={cn(
                    "min-w-0",
                    step.status === "completed" ? "text-muted line-through" : "text-ink",
                  )}
                >
                  {step.text}
                </span>
              </div>
            ))}
          </CollapsibleContent>
        )}
      </Collapsible>
    </div>
  );
}

function StepIcon({ status }: { status: TaskViewWire["steps"][number]["status"] }) {
  if (status === "completed") {
    return <CheckCircle2 className="mt-0.5 size-3.5 shrink-0 text-muted" aria-hidden="true" />;
  }
  if (status === "in_progress") {
    return <LoaderCircle className="mt-0.5 size-3.5 shrink-0 animate-spin text-muted" aria-hidden="true" />;
  }
  return <Circle className="mt-0.5 size-3.5 shrink-0 text-muted" aria-hidden="true" />;
}
