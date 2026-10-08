import { AiMessageTimeline, type AiMessageTimelineItem } from "@/components/ai/AiMessageTimeline";
import { AiToolExecutionCard } from "@/components/ai/AiToolExecutionCard";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import type { AiToolExecution } from "@/types/ai";

// 迁移自 shiwen FreeChatAssistantMessage，剥离 A2UI surface / 实体投影。
// 审批（pending / approval_state === "pending"）第一期不展示：代码路径保留在
// AiToolExecutionCard 内，但这里不把审批态执行项渲染出来。
export function AssistantMessage({
  content,
  streaming,
  executions,
}: {
  content: string;
  streaming: boolean;
  executions: AiToolExecution[];
}) {
  const userFacing = executions.filter(
    (execution) => execution.status === "complete" || execution.status === "running",
  );
  const internal = executions.filter(
    (execution) => !userFacing.includes(execution) && execution.status === "error",
  );

  const items: AiMessageTimelineItem[] = [
    ...userFacing.map((execution) => ({
      key: `execution:${execution.id}`,
      contentOffset: execution.content_offset,
      content: (
        <AiToolExecutionCard
          execution={execution}
          deciding={false}
          onDecision={() => {}}
          className="my-0"
        />
      ),
    })),
    ...(internal.length > 0
      ? [
          {
            key: "tool-activity",
            contentOffset: internal[0]?.content_offset,
            content: (
              <AiToolActivityDisclosure
                executions={internal}
                decidingToolIds={new Set<number>()}
                onToolDecision={() => {}}
              />
            ),
          },
        ]
      : []),
  ];

  return <AiMessageTimeline content={content} streaming={streaming} items={items} />;
}
