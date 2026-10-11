import { cn } from "@/ui/utils";

/**
 * 会话历史加载骨架屏。
 *
 * 切换 / 打开会话时，历史快照要走一次 `agent_subscribe` 往返；这段时间里与其让
 * 界面「卡在旧会话」或「空白一闪」，不如先渲染一屏占位骨架——用户立刻看到
 * 「我点的会话已经在加载了」的反馈（先响应交互，再补内容）。
 *
 * 形态刻意贴近真实对话：一条右对齐的用户气泡 + 两条左对齐的助手回复（头像 + 若干
 * 文本行），避免加载完成后布局跳动过大。
 */
export function AiConversationSkeleton({ className }: { className?: string }) {
  const lineWidths = ["78%", "92%", "60%", "84%"];
  return (
    <div
      className={cn("flex flex-col gap-6 px-1 py-6 md:px-3", className)}
      aria-busy="true"
      aria-label="正在加载消息记录"
    >
      {/* 用户消息（右对齐气泡） */}
      <div className="flex justify-end">
        <div className="h-9 w-2/5 animate-pulse-soft rounded-2xl bg-shimmer" />
      </div>
      {/* 助手回复（左对齐：头像 + 多行文本） */}
      {[0, 1].map((row) => (
        <div key={row} className="flex flex-col gap-3">
          <div className="size-7 animate-pulse-soft rounded-full bg-shimmer" />
          <div className="flex flex-col gap-2.5 pl-9">
            {lineWidths.map((width, index) => (
              <div
                key={index}
                className="h-3.5 animate-pulse-soft rounded-full bg-shimmer"
                style={{ width }}
              />
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}
