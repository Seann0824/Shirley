import { useState } from "react";
import { AlertCircle, ChevronDown } from "lucide-react";
import { Button } from "@/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@/ui/card";
import type { AiToolExecution } from "@/types/ai";
import { cn } from "@/ui/utils";
import { friendlyToolError, rawToolErrorText, type ToolErrorKind } from "./toolErrorUtils";

const TOOL_LABELS: Record<string, string> = {
  bash: "执行命令",
  read_file: "读取文件",
  web_search: "搜索互联网",
  recall: "召回历史",
};

function toolLabel(name: string) {
  return TOOL_LABELS[name] || name;
}

export function AggregatedToolErrorCard({
  executions,
  errorKind,
  className,
}: {
  executions: AiToolExecution[];
  errorKind: ToolErrorKind;
  className?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const count = executions.length;
  const message = friendlyToolError(errorKind);

  return (
    <Card
      className={cn("my-3 gap-0 overflow-hidden py-0 shadow-none", className)}
      data-ai-tool="aggregated-error"
      aria-label={`${count} 个工具执行遇到问题`}
    >
      <CardHeader className="grid grid-cols-[2rem_minmax(0,1fr)] gap-x-3 gap-y-0 px-4 py-4 md:px-5">
        <span className="flex size-8 items-center justify-center rounded-full bg-subtle text-ink">
          <AlertCircle className="text-danger-ink" aria-hidden="true" />
        </span>
        <div className="min-w-0 self-center">
          <div className="flex flex-wrap items-center gap-2">
            <CardTitle className="text-body-sm">{count} 个工具执行遇到问题</CardTitle>
          </div>
          <CardDescription className="mt-1 leading-relaxed">{message}</CardDescription>
        </div>
      </CardHeader>
      <div className="px-4 pb-3 md:px-5">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 gap-1 px-1.5 text-caption text-muted"
          onClick={() => setExpanded((value) => !value)}
          aria-expanded={expanded}
        >
          {expanded ? "收起" : "查看详情"}
          <ChevronDown
            className={cn("transition-transform duration-150", expanded && "rotate-180")}
            size={14}
            aria-hidden="true"
          />
        </Button>
        {expanded && (
          <ul className="mt-2 space-y-2">
            {executions.map((execution) => {
              const raw = rawToolErrorText(execution);
              return (
                <li key={execution.id} className="rounded-control bg-inset px-3 py-2">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-body-sm font-medium">
                      {toolLabel(execution.tool_name)}
                    </span>
                    <span className="shrink-0 font-utility text-caption text-muted">未完成</span>
                  </div>
                  {raw && (
                    <p className="mt-1 break-words font-utility text-caption text-muted">{raw}</p>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Card>
  );
}
