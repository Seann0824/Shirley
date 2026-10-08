import { AiMessageTimeline, type AiMessageTimelineItem } from "@/components/ai/AiMessageTimeline";
import { AiToolActivityDisclosure } from "@/components/ai/AiToolActivityDisclosure";
import type { AiToolExecution } from "@/types/ai";

// 迁移自 shiwen FreeChatAssistantMessage，剥离 A2UI surface / 实体投影。
// 审批（pending / approval_state === "pending"）第一期不展示：代码路径保留在
// AiToolExecutionCard 内，但这里不把审批态执行项渲染出来。
//
// 「连续工具调用收集到一起」（对齐 shiwen）：shiwen 只把「用户可见」的执行项
// （pending / 审批中 / 带 entity_url 的完成项）单独成卡，其余全部塞进一个
// AiToolActivityDisclosure 折叠区。Shirley 没有 entity / 审批渲染，所以**所有**
// 执行项都归入折叠区——连续的工具调用被收成一行「查看处理过程 · N 项」，
// 而不是每个调用铺一张卡。
export function AssistantMessage({
  content,
  streaming,
  executions,
}: {
  content: string;
  streaming: boolean;
  executions: AiToolExecution[];
}) {
  const items: AiMessageTimelineItem[] = executions.length
    ? [
        {
          key: "tool-activity",
          contentOffset: executions[0]?.content_offset,
          content: (
            <AiToolActivityDisclosure
              executions={executions}
              decidingToolIds={new Set<number>()}
              onToolDecision={() => {}}
            />
          ),
        },
      ]
    : [];

  return <AiMessageTimeline content={content} streaming={streaming} items={items} />;
}
