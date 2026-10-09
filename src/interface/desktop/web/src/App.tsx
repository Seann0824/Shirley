import { useCallback, useEffect, useRef, useState } from "react";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { codePointLength } from "@/components/ai/AiMessageTimeline";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { FileChips } from "@/lib/file-mentions/FileChips";
import { FileMentionPopover } from "@/lib/file-mentions/FileMentionPopover";
import { useFileMentions } from "@/lib/file-mentions/useFileMentions";
import type { FileReference } from "@/lib/file-mentions/types";
import { agentBridge, type StreamHandle } from "@/lib/bridge";
import type { AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

export function App() {
  const [messages, setMessages] = useState<AiTranscriptMessage[]>([]);
  // 每个 assistant 轮次的「思考文本 + 工具执行」按 assistant 消息 id 分组；
  // 放 state 才能触发重渲染（直接改 ref 数组不会重渲染——曾踩过）。
  const [turnData, setTurnData] = useState<Record<string, AssistantTurnData>>({});
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [model, setModel] = useState("");
  const handleRef = useRef<StreamHandle | null>(null);

  const mentions = useFileMentions(input, setInput);

  useEffect(() => {
    void agentBridge()
      .then((bridge) => bridge.modelName().then(setModel).catch(() => {}));
  }, []);

  const send = useCallback(async () => {
    // 浮层打开时，回车先确认当前高亮的引用，而不是发送。
    if (mentions.popoverOpen && mentions.confirmSelection()) return;
    const text = input.trim();
    if (!text || busy) return;
    const references = mentions.references;
    setInput("");
    mentions.clearReferences();
    setError("");
    setBusy(true);

    const assistantId = nextId();
    setMessages((prev) => [
      ...prev,
      { id: nextId(), role: "user", content: text, status: "complete", references },
      { id: assistantId, role: "assistant", content: "", status: "streaming" },
    ]);
    setTurnData((prev) => ({ ...prev, [assistantId]: { reasoning: "", executions: [] } }));

    const patchMessage = (patch: (m: AiTranscriptMessage) => AiTranscriptMessage) => {
      setMessages((prev) => prev.map((m) => (m.id === assistantId ? patch(m) : m)));
    };
    const patchTurn = (patch: (turn: AssistantTurnData) => AssistantTurnData) => {
      setTurnData((prev) => ({
        ...prev,
        [assistantId]: patch(prev[assistantId] ?? { reasoning: "", executions: [] }),
      }));
    };
    // 本轮正文的同步累加器：工具事件要靠它算 content_offset（流式 offset =
    // 该工具调用发生时正文已累积的码点数），而 setMessages 是异步的、读不到即时值。
    let assistantContent = "";

    const bridge = await agentBridge();
    handleRef.current = await bridge.send(
      text,
      references.map((reference) => reference.path),
      (event) => {
        switch (event.type) {
          case "content_delta":
            assistantContent += event.text;
            patchMessage((m) => ({ ...m, content: m.content + event.text }));
            break;
          case "reasoning_delta":
            patchTurn((turn) => ({ ...turn, reasoning: turn.reasoning + event.text }));
            break;
          case "tool_started":
            patchTurn((turn) => ({
              ...turn,
              executions: [
                ...turn.executions,
                {
                  id: event.call_id,
                  call_id: event.call_id,
                  tool_name: event.name,
                  status: "running",
                  summary: "",
                  // 记录调用发生点：与正文交错渲染（对标 TUI 的线性消息流）。
                  content_offset: codePointLength(assistantContent),
                  input: safeParseArgs(event.arguments),
                },
              ],
            }));
            break;
          case "tool_finished":
            patchTurn((turn) => ({
              ...turn,
              executions: turn.executions.map((tool) =>
                tool.call_id === event.call_id
                  ? {
                      ...tool,
                      status: event.ok ? "complete" : "error",
                      summary: event.output.slice(0, 200),
                      error: event.ok ? null : event.output,
                    }
                  : tool,
              ),
            }));
            break;
          case "error":
            setError(event.message);
            patchMessage((m) => ({ ...m, status: "error" }));
            setBusy(false);
            handleRef.current = null;
            break;
          case "finished":
            patchMessage((m) => ({ ...m, status: "complete" }));
            setBusy(false);
            handleRef.current = null;
            break;
          default:
            break;
        }
      },
    );
  }, [busy, input, mentions]);

  const stop = useCallback(() => {
    handleRef.current?.cancel();
    handleRef.current = null;
    setBusy(false);
  }, []);

  const switchModel = useCallback(async (value: string) => {
    try {
      const bridge = await agentBridge();
      await bridge.setModel(value);
      setModel(value);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  return (
    <div className="flex h-screen w-full min-h-0 flex-col bg-canvas">
      <header className="flex h-12 shrink-0 items-center border-b border-line px-4">
        <span className="font-display text-body-sm text-ink">Shirley</span>
      </header>
      {/* 固定宽度居中（对齐 shiwen AiConversationSurface 的 max-w-190 = 760px）：
          聊天区与输入框都不随窗口拉宽，长文本按阅读舒适宽度换行。 */}
      <section className="mx-auto flex w-full max-w-190 min-h-0 flex-1 flex-col">
        <AiConversationTranscript
          messages={messages}
          conversationKey="main"
          extraContentKey={busy ? "busy" : ""}
          emptyContent={
            <p className="px-1 py-10 text-center text-body-sm text-muted">
              开始和 Shirley 对话吧。输入 <kbd className="font-utility">@</kbd> 可引用工作区文件。
            </p>
          }
          renderMessageContent={(message) =>
            message.role === "assistant" ? (
              <AssistantMessage
                content={message.content}
                streaming={message.status === "streaming"}
                reasoning={turnData[String(message.id)]?.reasoning ?? ""}
                executions={turnData[String(message.id)]?.executions ?? []}
              />
            ) : (
              <div className="flex min-w-0 flex-col gap-1.5">
                <FileChips references={message.references ?? []} />
                {message.content && <span>{message.content}</span>}
              </div>
            )
          }
        />
        {error && (
          <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
            {error}
          </p>
        )}
        <div className="relative shrink-0 px-3 pb-3">
          {mentions.popoverOpen && (
            <div className="absolute inset-x-3 bottom-full z-30 mb-1">
              <FileMentionPopover
                open={mentions.popoverOpen}
                query={mentions.query}
                results={mentions.results}
                loading={mentions.loading}
                error={mentions.error}
                selectedIndex={mentions.selectedIndex}
                onSelect={mentions.selectResult}
                onQueryChange={mentions.setQuery}
                onKeyDown={mentions.handleKeyDown}
                onRetry={() => mentions.setQuery(mentions.query)}
              />
            </div>
          )}
          <AiChatComposer
            id="shirley-composer"
            label="发送消息"
            value={input}
            placeholder="给 Shirley 发消息…（输入 @ 引用文件）"
            busy={busy}
            textareaRef={mentions.textareaRef}
            textareaOnKeyDown={mentions.handleKeyDown}
            onValueChange={mentions.handleValueChange}
            onSend={() => void send()}
            onStop={stop}
            trailingAction={
              <ModelSelector current={model} onSelect={(value) => void switchModel(value)} />
            }
          >
            <FileChips
              references={mentions.references}
              onRemove={mentions.removeReference}
              className="px-2 pt-2"
            />
          </AiChatComposer>
        </div>
      </section>
    </div>
  );
}

/** 一个 assistant 轮次的展示数据：思考文本 + 工具执行（按调用点带 content_offset）。 */
type AssistantTurnData = {
  reasoning: string;
  executions: AiToolExecution[];
};

function safeParseArgs(args: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(args);
    return typeof parsed === "object" && parsed ? parsed : undefined;
  } catch {
    return undefined;
  }
}
