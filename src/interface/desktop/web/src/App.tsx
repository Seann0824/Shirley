import { useCallback, useEffect, useRef, useState } from "react";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { InlineReferences } from "@/lib/file-mentions/FileChips";
import { FileMentionPopover } from "@/lib/file-mentions/FileMentionPopover";
import {
  MentionEditor,
  type MentionEditorHandle,
} from "@/lib/file-mentions/MentionEditor";
import { useFileMentionSearch } from "@/lib/file-mentions/useFileMentionSearch";
import { stripTokens } from "@/lib/file-mentions/editor-dom";
import type { FileReference } from "@/lib/file-mentions/types";
import { agentBridge, type StreamHandle } from "@/lib/bridge";
import type { AiSegment, AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

export function App() {
  const [messages, setMessages] = useState<AiTranscriptMessage[]>([]);
  // 每个 assistant 轮次按事件发生顺序切成的线性段落流（思考/正文/工具组交错），
  // 对标 TUI 的 items 数组。放 state 才能触发重渲染（直接改 ref 数组不会重渲染——曾踩过）。
  const [turnSegments, setTurnSegments] = useState<Record<string, AiSegment[]>>({});
  const [input, setInput] = useState("");
  const [references, setReferences] = useState<FileReference[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [model, setModel] = useState("");
  const handleRef = useRef<StreamHandle | null>(null);

  const mentionSearch = useFileMentionSearch();
  const editorRef = useRef<MentionEditorHandle>(null);

  useEffect(() => {
    void agentBridge()
      .then((bridge) => bridge.modelName().then(setModel).catch(() => {}));
  }, []);

  const send = useCallback(async () => {
    // 发给模型 / 判空的正文要剥掉占位符（占位符不是用户文字）；但**回显用的
    // content 必须保留占位符**——`InlineReferences` 正是按占位符位置把 chip 落回
    // 正文的，剥掉就一个 tag 都渲染不出来（曾经的 bug）。
    const plain = stripTokens(input).trim();
    if (!plain || busy) return;
    const display = input.trim();
    const refs = references;
    setInput("");
    setReferences([]);
    setError("");
    setBusy(true);

    const assistantId = nextId();
    setMessages((prev) => [
      ...prev,
      { id: nextId(), role: "user", content: display, status: "complete", references: refs },
      { id: assistantId, role: "assistant", content: "", status: "streaming" },
    ]);
    setTurnSegments((prev) => ({ ...prev, [assistantId]: [] }));

    const patchMessage = (patch: (m: AiTranscriptMessage) => AiTranscriptMessage) => {
      setMessages((prev) => prev.map((m) => (m.id === assistantId ? patch(m) : m)));
    };
    // 线性追加/合并段落。合并规则对标 TUI `append_streaming_delta`：连续同类型
    // 的增量并进同一段，类型一换就新开一段——于是渲染顺序天然是
    // 思考 → 正文 → 工具 → 正文 → 思考 → 工具 ……，而不是按内容偏移归并。
    const appendSegment = (segment: AiSegment) => {
      setTurnSegments((prev) => {
        const segments = prev[assistantId] ?? [];
        const last = segments.at(-1);
        if (segment.kind === "content" && last?.kind === "content") {
          return { ...prev, [assistantId]: [...segments.slice(0, -1), { ...last, text: last.text + segment.text }] };
        }
        if (segment.kind === "reasoning" && last?.kind === "reasoning") {
          return { ...prev, [assistantId]: [...segments.slice(0, -1), { ...last, text: last.text + segment.text }] };
        }
        if (segment.kind === "tools" && last?.kind === "tools") {
          return { ...prev, [assistantId]: [...segments.slice(0, -1), { ...last, executions: [...last.executions, ...segment.executions] }] };
        }
        return { ...prev, [assistantId]: [...segments, segment] };
      });
    };

    const bridge = await agentBridge();
    handleRef.current = await bridge.send(
      plain,
      refs.map((reference) => reference.path),
      (event) => {
        switch (event.type) {
          case "content_delta":
            appendSegment({ kind: "content", text: event.text });
            patchMessage((m) => ({ ...m, content: m.content + event.text }));
            break;
          case "reasoning_delta":
            appendSegment({ kind: "reasoning", text: event.text });
            break;
          case "tool_started":
            appendSegment({
              kind: "tools",
              executions: [
                {
                  id: event.call_id,
                  call_id: event.call_id,
                  tool_name: event.name,
                  status: "running",
                  summary: "",
                  input: safeParseArgs(event.arguments),
                },
              ],
            });
            break;
          case "tool_finished":
            // 只改已有工具段的对应执行项（不新开段）。
            setTurnSegments((prev) => ({
              ...prev,
              [assistantId]: (prev[assistantId] ?? []).map((segment) =>
                segment.kind === "tools"
                  ? {
                      ...segment,
                      executions: segment.executions.map((tool) =>
                        tool.call_id === event.call_id
                          ? {
                              ...tool,
                              status: event.ok ? "complete" : "error",
                              summary: event.output.slice(0, 200),
                              error: event.ok ? null : event.output,
                            }
                          : tool,
                      ),
                    }
                  : segment,
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
  }, [busy, input, references]);

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
                segments={turnSegments[String(message.id)] ?? []}
              />
            ) : (
              <InlineReferences content={message.content} references={message.references ?? []} />
            )
          }
        />
        {error && (
          <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
            {error}
          </p>
        )}
        <div className="relative shrink-0 px-3 pb-3">
          {mentionSearch.popoverOpen && (
            <div className="absolute inset-x-3 bottom-full z-30 mb-1">
              <FileMentionPopover
                open={mentionSearch.popoverOpen}
                query={mentionSearch.query}
                results={mentionSearch.results}
                loading={mentionSearch.loading}
                error={mentionSearch.error}
                selectedIndex={mentionSearch.selectedIndex}
                onSelect={(result) => editorRef.current?.insertReference(result)}
                onRetry={mentionSearch.retry}
                showSearch={false}
              />
            </div>
          )}
          <AiChatComposer
            id="shirley-composer"
            label="发送消息"
            value={stripTokens(input)}
            placeholder="给 Shirley 发消息…（输入 @ 引用文件）"
            busy={busy}
            onValueChange={() => {}}
            onSend={() => void send()}
            onStop={stop}
            trailingAction={
              <ModelSelector current={model} onSelect={(value) => void switchModel(value)} />
            }
            inputSlot={
              <MentionEditor
                ref={editorRef}
                id="shirley-composer"
                label="发送消息"
                value={input}
                references={references}
                placeholder="给 Shirley 发消息…（输入 @ 引用文件）"
                autoFocus
                search={mentionSearch}
                onChange={(text, refs) => {
                  setInput(text);
                  setReferences(refs);
                }}
                onSubmit={() => void send()}
              />
            }
          />
        </div>
      </section>
    </div>
  );
}

function safeParseArgs(args: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(args);
    return typeof parsed === "object" && parsed ? parsed : undefined;
  } catch {
    return undefined;
  }
}
