import { useState } from "react";
import { AlertCircle, BookOpen, Globe, ListChecks, LoaderCircle, Terminal, Wrench } from "lucide-react";
import type { AiToolExecution } from "@/types/ai";
import { TextDisclosure } from "./TextDisclosure";
import { AiToolExecutionCard } from "./AiToolExecutionCard";
import { aggregateToolExecutions } from "./toolErrorUtils";

const TOOL_LABELS: Record<string, string> = {
  bash: "执行命令",
  read_file: "读取文件",
  read_skill_file: "读取技能文件",
  web_search: "搜索网页",
  todo: "更新任务",
  activate_skill: "启用技能",
  recall: "检索记忆",
};

/** 工具组展开后显示简洁活动行；参数和输出再按需展开。 */
export function AiToolActivityDisclosure({
  executions,
  decidingToolIds,
  onToolDecision,
}: {
  executions: AiToolExecution[];
  decidingToolIds: Set<number>;
  onToolDecision: (execution: AiToolExecution, decision: "approve" | "reject") => void;
}) {
  if (executions.length === 0) return null;
  const running = executions.some((execution) => execution.status === "running");
  const pending = executions.some((execution) => execution.status === "pending" || execution.approval_state === "pending");
  const labels = [...new Set(executions.map((execution) => TOOL_LABELS[execution.tool_name] || execution.tool_name))];
  const summary = `${labels.slice(0, 3).join("、")}${labels.length > 3 ? "等操作" : ""}`;
  const failures = executions.filter((execution) => execution.status === "error").length;

  return (
    <TextDisclosure
      defaultOpen
      leading={running ? <LoaderCircle className="shrink-0 animate-spin" size={14} aria-hidden="true" /> : <ToolIcon execution={executions[0]} />}
      label={`${summary} · ${executions.length} 项${pending ? " · 待确认" : ""}`}
      trailing={failures > 0 ? <span className="shrink-0 text-danger-ink">{failures} 项未完成</span> : undefined}
      className="min-w-0"
      triggerClassName="max-w-full text-left"
      contentClassName="mt-2 flex min-w-0 flex-col gap-2"
    >
      {aggregateToolExecutions(executions).map((item, index) => {
        if (item.kind === "aggregated-error") {
          return (
            <TextDisclosure
              key={`${item.key}:${index}`}
              label={`${item.executions.every((execution) => execution.tool_name === item.executions[0].tool_name) ? TOOL_LABELS[item.executions[0].tool_name] || "工具调用" : "工具调用"} · ${item.executions.length} 项未完成`}
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
        // 需要用户决策的执行项保留既有卡片和回调，普通活动不铺大卡片。
        if (execution.status === "pending" || execution.approval_state === "pending") {
          return <AiToolExecutionCard
            key={`${execution.id}:${index}`}
            execution={execution}
            deciding={decidingToolIds.has(execution.execution_id ?? (typeof execution.id === "number" ? execution.id : Number.NaN))}
            onDecision={onToolDecision}
            className="my-0"
          />;
        }
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
    : execution.tool_name === "read_file" || execution.tool_name === "read_skill_file" ? BookOpen
    : Wrench;
  return <Icon size={14} aria-hidden="true" className={`shrink-0${execution.status === "running" ? " animate-spin" : ""}${execution.status === "error" ? " text-danger-ink" : ""}`} />;
}

function activityLabel(execution: AiToolExecution): string {
  const input = execution.input;
  const path = input?.path ?? input?.file_path;
  if ((execution.tool_name === "read_file" || execution.tool_name === "read_skill_file") && typeof path === "string") {
    return `读取 ${path.replaceAll("\\", "/").split("/").filter(Boolean).at(-1) || path}`;
  }
  const detail = execution.tool_name === "bash" ? input?.command
    : execution.tool_name === "web_search" ? input?.query ?? input?.q
    : undefined;
  const label = TOOL_LABELS[execution.tool_name] || execution.tool_name;
  return typeof detail === "string" ? `${label}：${detail.replace(/\s+/g, " ").slice(0, 180)}` : label;
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
      trailing={execution.status === "error" ? <span className="shrink-0 text-danger-ink">未完成</span> : undefined}
      className="min-w-0"
      triggerClassName="max-w-full text-left"
      contentClassName="mt-2 ml-5 min-w-0 rounded-control bg-inset p-3 text-caption"
    >
      {open && <>
      {execution.input && (
        <div className="mb-3">
          <p className="mb-1 text-muted">参数</p>
          <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words font-utility text-ink">{JSON.stringify(execution.input, null, 2)}</pre>
        </div>
      )}
      <p className="mb-1 text-muted">{execution.status === "error" ? "错误详情" : "输出"}</p>
      <pre className="max-h-80 overflow-auto whitespace-pre-wrap break-words font-utility text-ink">{output ? displayOutput(output) : execution.status === "running" ? "等待工具返回…" : "历史记录未保存输出。"}</pre>
      </>}
    </TextDisclosure>
  );
}
