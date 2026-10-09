// 一轮 assistant 回复按事件发生顺序切成的线性段落流（对标 TUI 的 items 数组）：
// 思考块 / 正文块 / 工具组块，谁先发生谁在前。desktop 原先把整轮压成「正文 + 一个
// 工具折叠区」，无法表达「思考 → 正文 → 工具 → 正文 → 思考 → 工具」这种交错。
export type AiSegment =
  | { kind: "content"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tools"; executions: AiToolExecution[] };

// 迁移自 shiwen 的 AiToolExecution，仅保留 Shirley 工具卡片实际消费的字段。
// shiwen 定制字段（entity / space / host_repository 等）已剥离。

export type AiToolExecution = {
  id: number | string;
  execution_id?: number;
  call_id?: string | null;
  content_offset?: number | null;
  tool_name: string;
  status: "pending" | "running" | "complete" | "error";
  approval_state?: "pending" | "approved" | "rejected" | null;
  summary: string;
  result_json?: string;
  error?: string | null;
  created_at?: string;
  completed_at?: string | null;
  input?: Record<string, unknown>;
};
