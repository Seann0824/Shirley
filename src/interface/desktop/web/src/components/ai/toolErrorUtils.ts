import type { AiToolExecution } from "@/types/ai";

export type ToolErrorKind = "timeout" | "approval" | "missing_context" | "generic";

export function classifyToolError(execution: AiToolExecution): ToolErrorKind {
  const text = `${execution.error ?? ""} ${execution.summary ?? ""}`.toUpperCase();
  if (text.includes("APPROVAL_REQUIRED")) return "approval";
  if (text.includes("TOOL_TIMEOUT") || text.includes("超时")) return "timeout";
  if (text.includes("MISSING_USER_CONTEXT") || text.includes("缺少") || text.includes("上下文")) {
    return "missing_context";
  }
  return "generic";
}

export function friendlyToolError(kind: ToolErrorKind): string {
  switch (kind) {
    case "timeout":
      return "这个工具响应较慢，已自动跳过";
    case "approval":
      return "需要你确认后才能继续";
    case "missing_context":
      return "缺少必要的上下文信息";
    case "generic":
      return "工具执行遇到问题，已自动跳过";
  }
}

export function rawToolErrorText(execution: AiToolExecution): string {
  return execution.error || execution.summary || "";
}

/**
 * A failed execution is "resolved" when a later execution of the same tool
 * completed successfully, so the earlier failure no longer needs attention.
 */
export function isResolvedFailure(
  execution: AiToolExecution,
  allExecutions: readonly AiToolExecution[],
): boolean {
  if (execution.status !== "error") return false;
  const index = allExecutions.findIndex((item) => item.id === execution.id);
  if (index === -1) return false;
  return allExecutions
    .slice(index + 1)
    .some((later) => later.tool_name === execution.tool_name && later.status === "complete");
}

export type AggregatedToolErrorGroup = {
  kind: "aggregated-error";
  key: string;
  executions: AiToolExecution[];
  errorKind: ToolErrorKind;
};

export type ToolRenderItem =
  { kind: "single"; execution: AiToolExecution } | AggregatedToolErrorGroup;

/**
 * Group consecutive, non-approval failed tool executions into one aggregated
 * item. Approval-required failures stay individual because they need user
 * action. Pending, running, and complete executions always stay individual.
 */
export function aggregateToolExecutions(executions: readonly AiToolExecution[]): ToolRenderItem[] {
  const items: ToolRenderItem[] = [];
  let index = 0;
  while (index < executions.length) {
    const execution = executions[index];
    const isFailed = execution.status === "error";
    const errorKind = isFailed ? classifyToolError(execution) : null;
    const canAggregate = isFailed && errorKind !== "approval";
    if (!canAggregate) {
      items.push({ kind: "single", execution });
      index += 1;
      continue;
    }
    const group: AiToolExecution[] = [execution];
    let next = index + 1;
    while (next < executions.length) {
      const candidate = executions[next];
      if (candidate.status !== "error") break;
      const candidateKind = classifyToolError(candidate);
      if (candidateKind === "approval") break;
      group.push(candidate);
      next += 1;
    }
    if (group.length > 1) {
      items.push({
        kind: "aggregated-error",
        key: `aggregated:${group[0].id}:${group.length}`,
        executions: group,
        errorKind: errorKind!,
      });
    } else {
      items.push({ kind: "single", execution: group[0] });
    }
    index = next;
  }
  return items;
}
