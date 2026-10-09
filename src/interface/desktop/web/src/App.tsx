import { useCallback, useEffect, useRef, useState } from "react";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { SessionSelector } from "@/components/ai/SessionSelector";
import { InlineReferences } from "@/lib/file-mentions/FileChips";
import { FileMentionPopover } from "@/lib/file-mentions/FileMentionPopover";
import {
  MentionEditor,
  type MentionEditorHandle,
} from "@/lib/file-mentions/MentionEditor";
import { useFileMentionSearch } from "@/lib/file-mentions/useFileMentionSearch";
import { stripTokens } from "@/lib/file-mentions/editor-dom";
import type { FileReference } from "@/lib/file-mentions/types";
import { agentBridge, type SessionSubscription } from "@/lib/bridge";
import type { AgentEventWire, SessionSnapshot } from "@/types/wire";
import type { AiSegment, AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

/**
 * 单个会话的视图状态（transcript + 每轮段落 + busy / 错误）。
 *
 * 多会话：事件按会话打标，每个会话各持一份视图；当前会话渲染，后台会话照常累加
 * （不因切走丢上下文）。切换会话 = 换渲染哪一份 + 换订阅哪个会话的事件流。
 */
type SessionView = {
  messages: AiTranscriptMessage[];
  turnSegments: Record<string, AiSegment[]>;
  busy: boolean;
  error: string;
};

function emptyView(): SessionView {
  return { messages: [], turnSegments: {}, busy: false, error: "" };
}

export function App() {
  // 每个会话一份视图（后台会话也保留，切回不丢）。
  const [views, setViews] = useState<Record<string, SessionView>>({});
  const [sessionName, setSessionName] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [references, setReferences] = useState<FileReference[]>([]);
  const [model, setModel] = useState("");
  // UI 级错误（切换 / 新建 / 删除 / 模型切换失败）；运行期错误进各会话视图的 `error`。
  const [uiError, setUiError] = useState("");

  const mentionSearch = useFileMentionSearch();
  const editorRef = useRef<MentionEditorHandle>(null);
  // 当前订阅句柄（切走时退订）。
  const subRef = useRef<SessionSubscription | null>(null);
  // 每个会话当前在跑的 assistant 轮次 id（事件按它落点）。
  const turnRef = useRef<Map<string, string>>(new Map());
  // 快照应用前缓冲事件（订阅与快照之间到达的增量不丢）。
  const readyRef = useRef<Set<string>>(new Set());
  const bufferRef = useRef<Map<string, AgentEventWire[]>>(new Map());

  // 把一个事件应用到指定会话的视图（纯函数式更新，避免闭包过期）。
  const applyEvent = useCallback((session: string, event: AgentEventWire) => {
    const needsTurn =
      event.type === "content_delta" ||
      event.type === "reasoning_delta" ||
      event.type === "tool_started" ||
      event.type === "tool_finished";
    let assistantId = turnRef.current.get(session) ?? null;
    if (needsTurn && !assistantId) {
      assistantId = nextId();
      turnRef.current.set(session, assistantId);
    }
    setViews((prev) => {
      const view = prev[session] ?? emptyView();
      let next = view;
      if (
        needsTurn &&
        assistantId &&
        !view.messages.some((message) => message.id === assistantId)
      ) {
        next = {
          ...view,
          messages: [
            ...view.messages,
            { id: assistantId, role: "assistant", content: "", status: "streaming" },
          ],
          turnSegments: { ...view.turnSegments, [assistantId]: [] },
        };
      }
      return { ...prev, [session]: applyWireEvent(next, event, assistantId) };
    });
  }, []);

  // 订阅一个会话：退订旧的，拿快照重建视图，再叠加之后的事件。
  const openSession = useCallback(
    async (session: string) => {
      const bridge = await agentBridge();
      subRef.current?.unsubscribe();
      subRef.current = null;
      readyRef.current.delete(session);
      bufferRef.current.delete(session);
      turnRef.current.delete(session);
      const sub = await bridge.openSession(session, (event) => {
        // 快照尚未应用：先缓冲，待快照落地后按序补放（不丢、不重）。
        if (!readyRef.current.has(session)) {
          const buffer = bufferRef.current.get(session) ?? [];
          buffer.push(event);
          bufferRef.current.set(session, buffer);
          return;
        }
        applyEvent(session, event);
      });
      subRef.current = sub;
      const { view, lastAssistantId } = viewFromSnapshot(sub.snapshot);
      if (lastAssistantId) turnRef.current.set(session, lastAssistantId);
      setViews((prev) => ({ ...prev, [session]: view }));
      readyRef.current.add(session);
      const buffered = bufferRef.current.get(session) ?? [];
      bufferRef.current.delete(session);
      for (const event of buffered) applyEvent(session, event);
      setSessionName(session);
    },
    [applyEvent],
  );

  useEffect(() => {
    void (async () => {
      const bridge = await agentBridge();
      await bridge.modelName().then(setModel).catch(() => {});
      const name = await bridge.currentSession().catch(() => null);
      if (name) await openSession(name).catch(() => {});
    })();
  }, [openSession]);

  const switchSession = useCallback(
    async (name: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        await bridge.switchSession(name);
        await openSession(name);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [openSession],
  );

  const newSession = useCallback(async () => {
    try {
      setUiError("");
      const bridge = await agentBridge();
      // 不带标题：名字在首条消息后由 AI 自动生成，用户想改再手动重命名。
      const entry = await bridge.newSession(null);
      await openSession(entry.name);
      // 直接聚焦输入框——新建会话的意图是"马上开始聊"，而不是先做管理。
      editorRef.current?.focus();
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
    }
  }, [openSession]);

  const renameSession = useCallback(async (name: string, title: string) => {
    try {
      setUiError("");
      await (await agentBridge()).renameSession(name, title);
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const deleteSession = useCallback(
    async (name: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        await bridge.deleteSession(name);
        // 删的若是当前会话，重新打开后端选中的会话（可能为空）。
        if (name === sessionName) {
          const current = await bridge.currentSession().catch(() => null);
          if (current) await openSession(current);
          else {
            subRef.current?.unsubscribe();
            subRef.current = null;
            setSessionName(null);
          }
        }
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [openSession, sessionName],
  );

  const send = useCallback(async () => {
    // 发给模型 / 判空的正文要剥掉占位符（占位符不是用户文字）；但**回显用的
    // content 必须保留占位符**——`InlineReferences` 正是按占位符位置把 chip 落回
    // 正文的，剥掉就一个 tag 都渲染不出来（曾经的 bug）。
    const plain = stripTokens(input).trim();
    const session = sessionName;
    if (!plain || !session) return;
    if ((views[session]?.busy ?? false)) return;
    const display = input.trim();
    const refs = references;
    setInput("");
    setReferences([]);
    setUiError("");

    const assistantId = nextId();
    turnRef.current.set(session, assistantId);
    setViews((prev) => {
      const view = prev[session] ?? emptyView();
      return {
        ...prev,
        [session]: {
          ...view,
          messages: [
            ...view.messages,
            { id: nextId(), role: "user", content: display, status: "complete", references: refs },
            { id: assistantId, role: "assistant", content: "", status: "streaming" },
          ],
          turnSegments: { ...view.turnSegments, [assistantId]: [] },
          busy: true,
          error: "",
        },
      };
    });

    const bridge = await agentBridge();
    try {
      await bridge.send(session, plain, refs.map((reference) => reference.path));
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setViews((prev) => {
        const view = prev[session] ?? emptyView();
        return { ...prev, [session]: { ...view, busy: false, error: message } };
      });
    }
  }, [input, references, sessionName, views]);

  const stop = useCallback(async () => {
    const session = sessionName;
    if (!session) return;
    await (await agentBridge()).cancel(session).catch(() => {});
    setViews((prev) => {
      const view = prev[session] ?? emptyView();
      return { ...prev, [session]: { ...view, busy: false } };
    });
  }, [sessionName]);

  const switchModel = useCallback(async (value: string) => {
    try {
      const bridge = await agentBridge();
      await bridge.setModel(value);
      setModel(value);
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const view = sessionName ? (views[sessionName] ?? emptyView()) : emptyView();
  const busy = view.busy;
  const error = uiError || view.error;

  return (
    <div className="flex h-screen w-full min-h-0 flex-col bg-canvas">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b border-line px-4">
        <img
          src="/brand/shirley-logo.png"
          alt=""
          aria-hidden="true"
          width={28}
          height={28}
          className="size-7 shrink-0 object-contain"
          draggable={false}
        />
        <span className="font-display text-body-sm text-ink">Shirley</span>
        <SessionSelector
          current={sessionName}
          onSelect={(name) => void switchSession(name)}
          onNew={() => void newSession()}
          onRename={(name, title) => void renameSession(name, title)}
          onDelete={(name) => void deleteSession(name)}
        />
      </header>
      {/* 固定宽度居中（对齐 shiwen AiConversationSurface 的 max-w-190 = 760px）：
          聊天区与输入框都不随窗口拉宽，长文本按阅读舒适宽度换行。 */}
      <section className="mx-auto flex w-full max-w-190 min-h-0 flex-1 flex-col">
        <AiConversationTranscript
          messages={view.messages}
          conversationKey={sessionName ?? "main"}
          extraContentKey={busy ? "busy" : ""}
          emptyContent={
            <div className="flex flex-col items-center gap-4 px-4 py-6 text-center sm:py-10">
              <img
                src="/brand/shirley-character.png"
                alt="《Code Geass》中的夏利·菲内特，橙色长发，身穿阿什弗德学园制服"
                width={768}
                height={1152}
                className="h-[clamp(140px,32vh,280px)] w-auto max-w-full object-contain"
                draggable={false}
              />
              <div className="space-y-2">
                <h1 className="font-display text-title-sm text-ink">Shirley</h1>
                <p className="text-body-sm text-muted">
                  开始和 Shirley 对话吧。输入 <kbd className="font-utility">@</kbd> 可引用工作区文件。
                </p>
              </div>
            </div>
          }
          renderMessageContent={(message) =>
            message.role === "assistant" ? (
              <AssistantMessage
                content={message.content}
                streaming={message.status === "streaming"}
                segments={view.turnSegments[String(message.id)] ?? []}
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
            onStop={() => void stop()}
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

/**
 * 把一个事件应用到视图（纯函数）。
 *
 * 合并规则对标 TUI `append_streaming_delta`：连续同类型的增量并进同一段，类型一换
 * 就新开一段——渲染顺序天然是 思考 → 正文 → 工具 → 正文 → 思考 → 工具 ……。
 */
function applyWireEvent(
  view: SessionView,
  event: AgentEventWire,
  assistantId: string | null,
): SessionView {
  switch (event.type) {
    case "content_delta": {
      if (!assistantId) return view;
      return {
        ...view,
        messages: view.messages.map((message) =>
          message.id === assistantId
            ? { ...message, content: message.content + event.text, status: "streaming" }
            : message,
        ),
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "content",
          text: event.text,
        }),
      };
    }
    case "reasoning_delta": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: appendSegment(view.turnSegments, assistantId, {
          kind: "reasoning",
          text: event.text,
        }),
      };
    }
    case "tool_started": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: appendSegment(view.turnSegments, assistantId, {
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
        }),
      };
    }
    case "tool_finished": {
      if (!assistantId) return view;
      return {
        ...view,
        turnSegments: updateTool(view.turnSegments, assistantId, event),
      };
    }
    case "error": {
      return {
        ...view,
        error: event.message,
        busy: false,
        messages: view.messages.map((message) =>
          message.id === assistantId ? { ...message, status: "error" } : message,
        ),
      };
    }
    case "finished": {
      return {
        ...view,
        busy: false,
        messages: view.messages.map((message) =>
          message.id === assistantId ? { ...message, status: "complete" } : message,
        ),
      };
    }
    default:
      return view;
  }
}

function appendSegment(
  segments: Record<string, AiSegment[]>,
  assistantId: string,
  segment: AiSegment,
): Record<string, AiSegment[]> {
  const current = segments[assistantId] ?? [];
  const last = current.at(-1);
  if (segment.kind === "content" && last?.kind === "content") {
    return {
      ...segments,
      [assistantId]: [...current.slice(0, -1), { ...last, text: last.text + segment.text }],
    };
  }
  if (segment.kind === "reasoning" && last?.kind === "reasoning") {
    return {
      ...segments,
      [assistantId]: [...current.slice(0, -1), { ...last, text: last.text + segment.text }],
    };
  }
  if (segment.kind === "tools" && last?.kind === "tools") {
    return {
      ...segments,
      [assistantId]: [
        ...current.slice(0, -1),
        { ...last, executions: [...last.executions, ...segment.executions] },
      ],
    };
  }
  return { ...segments, [assistantId]: [...current, segment] };
}

function updateTool(
  segments: Record<string, AiSegment[]>,
  assistantId: string,
  event: Extract<AgentEventWire, { type: "tool_finished" }>,
): Record<string, AiSegment[]> {
  return {
    ...segments,
    [assistantId]: (segments[assistantId] ?? []).map((segment) =>
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
  };
}

/**
 * 从会话快照重建视图（订阅一个已积累历史的会话时用）。
 *
 * 快照是 `ItemWire` 序列（message / tools）；两条 user 消息之间的所有 assistant /
 * 工具条目归并成**同一轮** assistant（与实时流的"一轮一个 assistantId"口径一致）。
 * 返回视图 + 最后一个 assistant 轮次 id（供后续增量落点）。
 */
function viewFromSnapshot(snapshot: SessionSnapshot): {
  view: SessionView;
  lastAssistantId: string | null;
} {
  const messages: AiTranscriptMessage[] = [];
  const turnSegments: Record<string, AiSegment[]> = {};
  let currentAssistantId: string | null = null;

  const ensureAssistant = (): string => {
    if (!currentAssistantId) {
      const id = nextId();
      currentAssistantId = id;
      messages.push({ id, role: "assistant", content: "", status: "complete" });
      turnSegments[id] = [];
    }
    return currentAssistantId;
  };

  for (const item of snapshot.items) {
    if (item.kind === "message") {
      if (item.role === "user") {
        messages.push({ id: nextId(), role: "user", content: item.content, status: "complete" });
        currentAssistantId = null;
        continue;
      }
      // assistant / summary / system / error 一律落到 assistant 轮次。
      const id = ensureAssistant();
      if (item.thinking) {
        turnSegments[id].push({ kind: "reasoning", text: item.content });
      } else {
        turnSegments[id].push({ kind: "content", text: item.content });
        const message = messages.find((m) => m.id === id);
        if (message) message.content += item.content;
      }
    } else {
      const id = ensureAssistant();
      turnSegments[id].push({
        kind: "tools",
        executions: item.calls.map(
          (call): AiToolExecution => ({
            id: call.name,
            call_id: call.name,
            tool_name: call.name,
            status: "complete",
            summary: "",
            input: safeParseArgs(call.arguments),
          }),
        ),
      });
    }
  }

  return {
    view: { messages, turnSegments, busy: snapshot.running, error: "" },
    lastAssistantId: currentAssistantId,
  };
}

function safeParseArgs(args: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(args);
    return typeof parsed === "object" && parsed ? parsed : undefined;
  } catch {
    return undefined;
  }
}
