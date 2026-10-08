import { useState } from "react";
import { ChevronDown, LoaderCircle, Wrench } from "lucide-react";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import type { AiToolExecution } from "@/types/ai";
import { cn } from "@/ui/utils";
import { AggregatedToolErrorCard } from "./AggregatedToolErrorCard";
import { AiToolExecutionCard } from "./AiToolExecutionCard";
import { aggregateToolExecutions, isResolvedFailure } from "./toolErrorUtils";

export function AiToolActivityDisclosure({
  executions,
  decidingToolIds,
  onToolDecision,
}: {
  executions: AiToolExecution[];
  decidingToolIds: Set<number>;
  onToolDecision: (execution: AiToolExecution, decision: "approve" | "reject") => void;
}) {
  const [open, setOpen] = useState(false);
  const running = executions.some(
    (execution) => execution.status === "running" || execution.status === "pending",
  );
  const items = aggregateToolExecutions(executions);

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <Button type="button" variant="ghost" size="sm" className="text-muted">
          {running ? (
            <LoaderCircle className="animate-spin" data-icon="inline-start" />
          ) : (
            <Wrench data-icon="inline-start" />
          )}
          <span>{running ? "正在整理回答" : `查看处理过程 · ${executions.length} 项`}</span>
          <ChevronDown
            className={cn("transition-transform duration-150", open && "rotate-180")}
            data-icon="inline-end"
          />
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-2 flex flex-col gap-2">
        {items.map((item) => {
          if (item.kind === "aggregated-error") {
            return (
              <AggregatedToolErrorCard
                key={item.key}
                executions={item.executions}
                errorKind={item.errorKind}
                className="my-0"
              />
            );
          }
          const execution = item.execution;
          return (
            <AiToolExecutionCard
              key={`execution:${execution.id}`}
              execution={execution}
              deciding={decidingToolIds.has(
                execution.execution_id ??
                  (typeof execution.id === "number" ? execution.id : Number.NaN),
              )}
              onDecision={onToolDecision}
              resolved={isResolvedFailure(execution, executions)}
              className="my-0"
            />
          );
        })}
      </CollapsibleContent>
    </Collapsible>
  );
}
