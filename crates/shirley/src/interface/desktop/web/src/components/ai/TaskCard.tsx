import { useState } from "react";
import { CheckCircle2, ChevronDown, Circle, ListChecks, LoaderCircle, X } from "lucide-react";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";
import type { TaskViewWire } from "@/types/wire";

// Task-ledger card: puts "the task progress the model tracks itself" directly in front of
// the user.
//
// Why it is needed: the todo ledger was previously injected to the model only via
// ContextProvider, so the user could not see "the task is moving forward" — the most direct
// signal. This consumes the structured `todo_updated` snapshot the backend emits when the
// `todo` tool completes (it does not parse the injected XML).
//
// Collapse strategy: the card itself is **not resident** — the user opens it from the
// composer's task toggle, and it closes on demand (`onClose`). Within the card, goal +
// progress stay visible while the step list is collapsed by default — steps are the
// "process", and the user mainly cares "what is the goal, how far has it advanced".
export function TaskCard({ task, onClose }: { task: TaskViewWire; onClose?: () => void }) {
  const [open, setOpen] = useState(false);
  const done = task.total > 0 && task.completed === task.total;

  return (
    <div className="mx-3 mb-2 rounded-card border border-line bg-surface px-3 py-2 text-body-sm">
      <Collapsible open={open} onOpenChange={setOpen}>
        <div className="flex items-center gap-2">
          <ListChecks className="size-3.5 shrink-0 text-muted" aria-hidden="true" />
          <span className="min-w-0 flex-1 truncate text-ink">
            {task.goal ?? "Task"}
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
                <span className="sr-only">Expand task steps</span>
                <ChevronDown
                  className={cn("transition-transform duration-150", open && "rotate-180")}
                  aria-hidden="true"
                />
              </Button>
            </CollapsibleTrigger>
          )}
          {onClose && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="text-muted"
              aria-label="Hide task list"
              onClick={onClose}
            >
              <X aria-hidden="true" />
            </Button>
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
