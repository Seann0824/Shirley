import { useState } from "react";
import { AlertCircle, BookOpen, Globe, ListChecks, LoaderCircle, Terminal, Wrench } from "lucide-react";
import type { AiToolExecution } from "@/types/ai";
import { TextDisclosure } from "./TextDisclosure";
import { aggregateToolExecutions } from "./toolErrorUtils";

const TOOL_LABELS: Record<string, string> = {
  bash: "Run command",
  read: "Read file",
  // Legacy name remains displayable in persisted session history.
  read_file: "Read file",
  read_skill_file: "Read skill file",
  web_search: "Search the web",
  todo: "Update tasks",
  activate_skill: "Activate skill",
  recall: "Search memory",
};

/** After the tool group is expanded, show concise activity rows; arguments and output expand on demand. */
export function AiToolActivityDisclosure({
  executions,
}: {
  executions: AiToolExecution[];
}) {
  if (executions.length === 0) return null;
  const running = executions.some((execution) => execution.status === "running");
  const labels = [...new Set(executions.map((execution) => TOOL_LABELS[execution.tool_name] || execution.tool_name))];
  const summary = `${labels.slice(0, 3).join(", ")}${labels.length > 3 ? " and more" : ""}`;
  const failures = executions.filter((execution) => execution.status === "error").length;

  return (
    <TextDisclosure
      defaultOpen
      leading={running ? <LoaderCircle className="shrink-0 animate-spin" size={14} aria-hidden="true" /> : <ToolIcon execution={executions[0]} />}
      label={`${summary} · ${executions.length} items`}
      trailing={failures > 0 ? <span className="shrink-0 text-danger-ink">{failures} incomplete</span> : undefined}
      className="min-w-0"
      triggerClassName="max-w-full text-left"
      contentClassName="mt-2 flex min-w-0 flex-col gap-2"
    >
      {aggregateToolExecutions(executions).map((item, index) => {
        if (item.kind === "aggregated-error") {
          return (
            <TextDisclosure
              key={`${item.key}:${index}`}
              label={`${item.executions.every((execution) => execution.tool_name === item.executions[0].tool_name) ? TOOL_LABELS[item.executions[0].tool_name] || "Tool call" : "Tool call"} · ${item.executions.length} incomplete`}
              leading={<AlertCircle size={14} className="shrink-0 text-danger-ink" aria-hidden="true" />}
              className="min-w-0"
              triggerClassName="max-w-full text-left"
              contentClassName="mt-2 flex min-w-0 flex-col gap-2 pl-5"
            >
              {item.executions.map((execution, failureIndex) => <ToolActivityRow key={`${execution.id}:${failureIndex}`} execution={execution} />)}
            </TextDisclosure>
          );
        }
        const execution = item.execution;
        return <ToolActivityRow key={`${execution.id}:${index}`} execution={execution} />;
      })}
    </TextDisclosure>
  );
}

function ToolIcon({ execution }: { execution: AiToolExecution }) {
  const Icon = execution.status === "error" ? AlertCircle
    : execution.status === "running" ? LoaderCircle
    : execution.tool_name === "bash" ? Terminal
    : execution.tool_name === "web_search" ? Globe
    : execution.tool_name === "todo" ? ListChecks
    : ["read", "read_file", "read_skill_file"].includes(execution.tool_name) ? BookOpen
    : Wrench;
  return <Icon size={14} aria-hidden="true" className={`shrink-0${execution.status === "running" ? " animate-spin" : ""}${execution.status === "error" ? " text-danger-ink" : ""}`} />;
}

function activityLabel(execution: AiToolExecution): string {
  const input = execution.input;
  const path = input?.path ?? input?.file_path;
  if (["read", "read_file", "read_skill_file"].includes(execution.tool_name) && typeof path === "string") {
    return `Read ${path.replaceAll("\\", "/").split("/").filter(Boolean).at(-1) || path}`;
  }
  const detail = execution.tool_name === "bash" ? input?.command
    : execution.tool_name === "web_search" ? input?.query ?? input?.q
    : undefined;
  const label = TOOL_LABELS[execution.tool_name] || execution.tool_name;
  return typeof detail === "string" ? `${label}: ${detail.replace(/\s+/g, " ").slice(0, 180)}` : label;
}

function displayOutput(raw: string): string {
  try {
    const parsed: unknown = JSON.parse(raw);
    return typeof parsed === "string" ? parsed : JSON.stringify(parsed, null, 2);
  } catch {
    return raw;
  }
}

function ToolActivityRow({ execution }: { execution: AiToolExecution }) {
  const [open, setOpen] = useState(false);
  const output = execution.error || execution.result_json || execution.summary;
  return (
    <TextDisclosure
      label={activityLabel(execution)}
      open={open}
      onOpenChange={setOpen}
      leading={<ToolIcon execution={execution} />}
      trailing={execution.status === "error" ? <span className="shrink-0 text-danger-ink">Incomplete</span> : undefined}
      className="min-w-0"
      triggerClassName="max-w-full text-left"
      contentClassName="mt-2 ml-5 min-w-0 rounded-control bg-inset p-3 text-caption"
    >
      {open && <>
      {execution.input && (
        <div className="mb-3">
          <p className="mb-1 text-muted">Arguments</p>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words font-utility text-ink">{JSON.stringify(execution.input, null, 2)}</pre>
        </div>
      )}
      <p className="mb-1 text-muted">{execution.status === "error" ? "Error details" : "Output"}</p>
      <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words font-utility text-ink">{output ? displayOutput(output) : execution.status === "running" ? "Waiting for the tool to return..." : "No output saved for this record."}</pre>
      </>}
    </TextDisclosure>
  );
}
