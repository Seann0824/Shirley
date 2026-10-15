import { useState } from "react";
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  LoaderCircle,
  ShieldQuestion,
  XCircle,
} from "lucide-react";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import {
  Card,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/ui/card";
import { Separator } from "@/ui/separator";
import type { AiToolExecution } from "@/types/ai";
import { cn } from "@/ui/utils";
import {
  classifyToolError,
  friendlyToolError,
  rawToolErrorText,
  type ToolErrorKind,
} from "./toolErrorUtils";

const TOOL_LABELS: Record<string, string> = {
  bash: "Run command",
  read_file: "Read file",
  web_search: "Search the web",
  todo: "Update tasks",
};

type ToolStatus = "pending" | "running" | "complete" | "error" | "rejected";

const STATUS_LABELS: Record<ToolStatus, string> = {
  pending: "Awaiting approval",
  running: "In progress",
  complete: "Completed",
  error: "Incomplete",
  rejected: "Rejected",
};

function StatusIcon({ status }: { status: ToolStatus }) {
  if (status === "pending") return <ShieldQuestion aria-hidden="true" />;
  if (status === "running") return <LoaderCircle className="animate-spin" aria-hidden="true" />;
  if (status === "error") return <AlertCircle className="text-danger-ink" aria-hidden="true" />;
  if (status === "rejected") return <XCircle aria-hidden="true" />;
  return <CheckCircle2 aria-hidden="true" />;
}

function ToolErrorDetail({
  execution,
  errorKind,
}: {
  execution: AiToolExecution;
  errorKind: ToolErrorKind;
}) {
  const [expanded, setExpanded] = useState(false);
  const raw = rawToolErrorText(execution);
  if (!raw) return null;
  return (
    <div className="px-4 pb-3 md:px-5">
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="h-7 gap-1 px-1.5 text-caption text-muted"
        onClick={() => setExpanded((value) => !value)}
        aria-expanded={expanded}
      >
        {expanded ? "Hide details" : "Details"}
        <ChevronDown
          className={cn("transition-transform duration-150", expanded && "rotate-180")}
          size={14}
          aria-hidden="true"
        />
      </Button>
      {expanded && (
        <p className="mt-1 break-words rounded-control bg-inset px-3 py-2 font-utility text-caption text-muted">
          {raw}
        </p>
      )}
    </div>
  );
}

export function AiToolExecutionCard({
  execution,
  deciding,
  onDecision,
  resolved = false,
  className,
}: {
  execution: AiToolExecution;
  deciding: boolean;
  onDecision: (execution: AiToolExecution, decision: "approve" | "reject") => void;
  resolved?: boolean;
  className?: string;
}) {
  const pending = execution.status === "pending" || execution.approval_state === "pending";
  const running = execution.status === "running" && !pending;
  const rejected = execution.approval_state === "rejected";
  const failed = execution.status === "error";
  const status: ToolStatus = pending
    ? "pending"
    : running
      ? "running"
      : rejected
        ? "rejected"
        : failed
          ? "error"
          : "complete";
  const errorKind = failed ? classifyToolError(execution) : null;
  const title = TOOL_LABELS[execution.tool_name] || execution.tool_name;
  const summary = failed
    ? friendlyToolError(errorKind ?? "generic")
    : execution.summary ||
      (pending ? "Shirley will run it as soon as you approve." : running ? "Shirley is working..." : "Action completed");

  const header = (
    <CardHeader
      className={cn(
        "grid grid-cols-[2rem_minmax(0,1fr)] gap-x-3 gap-y-0 px-4 py-4 md:px-5",
        resolved && "opacity-50",
      )}
    >
      <span className="flex size-8 items-center justify-center rounded-full bg-subtle text-ink">
        <StatusIcon status={status} />
      </span>
      <div className="min-w-0 self-center">
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle className="text-body-sm">{title}</CardTitle>
          <Badge variant={status === "error" ? "destructive" : "secondary"}>
            {STATUS_LABELS[status]}
          </Badge>
          {resolved && <Badge variant="secondary">Resolved</Badge>}
        </div>
        <CardDescription className="mt-1 leading-relaxed">
          {summary}
        </CardDescription>
      </div>
    </CardHeader>
  );

  return (
    <Card
      className={cn("my-3 gap-0 overflow-hidden py-0 shadow-none", className)}
      data-ai-tool={execution.tool_name}
      aria-label={`${TOOL_LABELS[execution.tool_name] || execution.tool_name} result`}
    >
      {header}
      {failed && errorKind && <ToolErrorDetail execution={execution} errorKind={errorKind} />}
      {pending && (
        <>
          <Separator />
          <CardFooter className="flex-wrap justify-end gap-2 px-4 py-3 md:px-5">
            <Button
              type="button"
              size="sm"
              disabled={deciding}
              onClick={() => onDecision(execution, "approve")}
            >
              Allow
            </Button>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              disabled={deciding}
              onClick={() => onDecision(execution, "reject")}
            >
              Reject
            </Button>
          </CardFooter>
        </>
      )}
    </Card>
  );
}
