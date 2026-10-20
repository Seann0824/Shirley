import type { AiToolExecution } from "@/types/ai";

/// Mirrors the SDK's `ErrorKind` (see `types/wire.ts`), plus `generic` for the
/// "no structured kind available" fallback (e.g. a cancellation synthesized by the UI).
export type ToolErrorKind =
  | "transport"
  | "rate_limited"
  | "server_error"
  | "bad_request"
  | "unsupported"
  | "tool_failure"
  | "internal"
  | "generic";

/**
 * Classify a failed execution by the stable `error_kind` the SDK reported on the
 * `tool_finished` event. We deliberately do **not** match on the error text: the text
 * is display-only and may change (or be localized), while the kind is a stable contract.
 */
export function classifyToolError(execution: AiToolExecution): ToolErrorKind {
  return execution.error_kind ?? "generic";
}

export function friendlyToolError(kind: ToolErrorKind): string {
  switch (kind) {
    case "transport":
      return "The tool couldn't reach the network and was skipped automatically";
    case "rate_limited":
      return "The tool was rate limited and was skipped automatically";
    case "server_error":
      return "The tool's service returned an error and it was skipped automatically";
    case "bad_request":
      return "The tool call was malformed and was skipped automatically";
    case "unsupported":
      return "The tool isn't available here and was skipped automatically";
    case "tool_failure":
      return "The tool ran into a problem and was skipped automatically";
    case "internal":
      return "The tool hit an internal error and was skipped automatically";
    case "generic":
      return "The tool ran into a problem and was skipped automatically";
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
 * Group consecutive failed tool executions into one aggregated item. Pending, running,
 * and complete executions always stay individual.
 */
export function aggregateToolExecutions(executions: readonly AiToolExecution[]): ToolRenderItem[] {
  const items: ToolRenderItem[] = [];
  let index = 0;
  while (index < executions.length) {
    const execution = executions[index];
    if (execution.status !== "error") {
      items.push({ kind: "single", execution });
      index += 1;
      continue;
    }
    const errorKind = classifyToolError(execution);
    const group: AiToolExecution[] = [execution];
    let next = index + 1;
    while (next < executions.length) {
      const candidate = executions[next];
      if (candidate.status !== "error") break;
      group.push(candidate);
      next += 1;
    }
    if (group.length > 1) {
      items.push({
        kind: "aggregated-error",
        key: `aggregated:${group[0].id}:${group.length}`,
        executions: group,
        errorKind,
      });
    } else {
      items.push({ kind: "single", execution: group[0] });
    }
    index = next;
  }
  return items;
}
