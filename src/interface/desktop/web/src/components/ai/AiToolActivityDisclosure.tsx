import { LoaderCircle, Wrench } from "lucide-react";
import type { AiToolExecution } from "@/types/ai";
import { TextDisclosure } from "./TextDisclosure";
import { AggregatedToolErrorCard } from "./AggregatedToolErrorCard";
import { AiToolExecutionCard } from "./AiToolExecutionCard";
import { aggregateToolExecutions, isResolvedFailure } from "./toolErrorUtils";

/**
 * 一轮里的一个「工具调用组」：用**文字折叠**（非按钮）收起，展开后是按时序排列的
 * 工具卡片。对标 Codex——过程里的工具列表也是一层文字折叠。
 */
export function AiToolActivityDisclosure({
  executions,
  decidingToolIds,
  onToolDecision,
}: {
  executions: AiToolExecution[];
  decidingToolIds: Set<number>;
  onToolDecision: (execution: AiToolExecution, decision: "approve" | "reject") => void;
}) {
  const running = executions.some(
    (execution) => execution.status === "running" || execution.status === "pending",
  );
  const items = aggregateToolExecutions(executions);

  return (
    <TextDisclosure
      leading={
        running ? (
          <LoaderCircle className="shrink-0 animate-spin" size={14} aria-hidden="true" />
        ) : (
          <Wrench className="shrink-0" size={14} aria-hidden="true" />
        )
      }
      label={running ? "正在操作…" : `调用工具 · ${executions.length} 项`}
      contentClassName="mt-2 flex flex-col gap-2"
    >
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
    </TextDisclosure>
  );
}
