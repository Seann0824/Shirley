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
