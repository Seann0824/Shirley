import { useCallback, useEffect, useRef, useState } from "react";
import {
  AiConversationTranscript,
  type AiTranscriptMessage,
} from "@/components/ai/AiConversationTranscript";
import { AiChatComposer } from "@/components/ai/AiChatComposer";
import { AssistantMessage } from "@/components/AssistantMessage";
import { ModelSelector } from "@/components/ai/ModelSelector";
import { SessionSidebar } from "@/components/ai/SessionSidebar";
import { AiConversationSkeleton } from "@/components/ai/AiConversationSkeleton";
import { InlineReferences } from "@/lib/file-mentions/FileChips";
import { FileMentionPopover } from "@/lib/file-mentions/FileMentionPopover";
import {
  MentionEditor,
  type MentionEditorHandle,
} from "@/lib/file-mentions/MentionEditor";
import { useFileMentionSearch } from "@/lib/file-mentions/useFileMentionSearch";
import { useSkillSlashSearch } from "@/lib/slash/useSkillSlashSearch";
import { SkillSlashPopover } from "@/lib/slash/SkillSlashPopover";
import { stripTokens } from "@/lib/file-mentions/editor-dom";
import type { FileReference } from "@/lib/file-mentions/types";
import { agentBridge, type ProjectEntry, type SessionSubscription } from "@/lib/bridge";
import type { AgentEventWire, SessionSnapshot } from "@/types/wire";
import type { AiSegment, AiToolExecution } from "@/types/ai";

let messageSeq = 0;
const nextId = () => `m${++messageSeq}`;

/**
 * 单个会话的视图状态（transcript + 每轮段落 + busy / 错误）。
 *
 * 多项目多会话：事件按 `project` + `session` 打标，每个会话各持一份视图；当前会话
 * 渲染，后台会话照常累加（不因切走丢上下文）。切换会话 = 换渲染哪一份 + 换订阅哪个
 * 事件流。视图键是 `project::session`——会话名只在项目内唯一，跨项目会重名。
 */
type SessionView = {
  messages: AiTranscriptMessage[];
  turnSegments: Record<string, AiSegment[]>;
  busy: boolean;
  error: string;
  /** 历史快照尚未到达：先乐观展示，渲染骨架屏。 */
  loading: boolean;
};

function emptyView(): SessionView {
  return { messages: [], turnSegments: {}, busy: false, error: "", loading: false };
}

/** 视图键：跨项目不重名。 */
const viewKey = (project: string, session: string) => `${project}::${session}`;

export function App() {
  // 每个会话一份视图（后台会话也保留，切回不丢），键为 `project::session`。
  const [views, setViews] = useState<Record<string, SessionView>>({});
  // 项目列表（活跃 + 归档分列）与当前项目。
  const [projects, setProjects] = useState<ProjectEntry[]>([]);
  const [archivedProjects, setArchivedProjects] = useState<ProjectEntry[]>([]);
  const [activeProject, setActiveProject] = useState<string | null>(null);
  const [sessionName, setSessionName] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [references, setReferences] = useState<FileReference[]>([]);
  const [model, setModel] = useState("");
  // UI 级错误（项目 / 会话 / 模型切换失败）；运行期错误进各会话视图的 `error`。
  const [uiError, setUiError] = useState("");

  const mentionSearch = useFileMentionSearch(activeProject);
  const slashSearch = useSkillSlashSearch(activeProject);
  const editorRef = useRef<MentionEditorHandle>(null);
  // 当前订阅句柄（切走时退订）。
  const subRef = useRef<SessionSubscription | null>(null);
  // 每个会话当前在跑的 assistant 轮次 id（事件按它落点），键为 `project::session`。
  const turnRef = useRef<Map<string, string>>(new Map());
  // 快照应用前缓冲事件（订阅与快照之间到达的增量不丢）。
  const readyRef = useRef<Set<string>>(new Set());
  const bufferRef = useRef<Map<string, AgentEventWire[]>>(new Map());

  const reloadProjects = useCallback(async () => {
    try {
      const bridge = await agentBridge();
      const [active, archived] = await Promise.all([
        bridge.listProjects(false),
        bridge.listProjects(true).then((all) => all.filter((entry) => entry.archived)),
      ]);
      setProjects(active);
      setArchivedProjects(archived);
      return active;
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
      return [] as ProjectEntry[];
    }
  }, []);

  // 把一个事件应用到指定会话的视图（纯函数式更新，避免闭包过期）。
  const applyEvent = useCallback((key: string, event: AgentEventWire) => {
    const needsTurn =
      event.type === "content_delta" ||
      event.type === "reasoning_delta" ||
      event.type === "tool_started" ||
      event.type === "tool_finished";
    let assistantId = turnRef.current.get(key) ?? null;
    if (needsTurn && !assistantId) {
      assistantId = nextId();
      turnRef.current.set(key, assistantId);
    }
    setViews((prev) => {
      const view = prev[key] ?? emptyView();
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
      return { ...prev, [key]: applyWireEvent(next, event, assistantId) };
    });
  }, []);

  // 先响应交互：立刻把前台会话切过去并进入加载态（渲染骨架屏），不等后端往返。
  // 打开会话 / 切换会话都在取历史快照**之前**调它——历史快照要走一次
  // `agent_subscribe` 往返，不能让用户对着旧会话干等。
  const beginSessionLoad = useCallback((project: string, session: string) => {
    const key = viewKey(project, session);
    setSessionName(session);
    setViews((prev) => {
      const existing = prev[key];
      // 已有缓存内容（后台会话跑过）直接复用、不闪骨架；否则占位加载。
      if (existing && existing.messages.length > 0) {
        return { ...prev, [key]: { ...existing, loading: true } };
      }
      return { ...prev, [key]: { ...emptyView(), loading: true } };
    });
  }, []);

  // 订阅一个会话：退订旧的，拿快照重建视图，再叠加之后的事件。
  const openSession = useCallback(
    async (project: string, session: string) => {
      const bridge = await agentBridge();
      const key = viewKey(project, session);
      beginSessionLoad(project, session);
      subRef.current?.unsubscribe();
      subRef.current = null;
      readyRef.current.delete(key);
      bufferRef.current.delete(key);
      turnRef.current.delete(key);
      let sub;
      try {
        sub = await bridge.openSession(project, session, (event) => {
          // 快照尚未应用：先缓冲，待快照落地后按序补放（不丢、不重）。
          if (!readyRef.current.has(key)) {
            const buffer = bufferRef.current.get(key) ?? [];
            buffer.push(event);
            bufferRef.current.set(key, buffer);
            return;
          }
          applyEvent(key, event);
        });
      } catch (e) {
        // 订阅失败：退出加载态并如实报错，别让骨架屏一直转下去。
        const message = e instanceof Error ? e.message : String(e);
        setViews((prev) => ({
          ...prev,
          [key]: { ...(prev[key] ?? emptyView()), loading: false, error: message },
        }));
        return;
      }
      subRef.current = sub;
      const { view, lastAssistantId } = viewFromSnapshot(sub.snapshot);
      if (lastAssistantId) turnRef.current.set(key, lastAssistantId);
      setViews((prev) => ({ ...prev, [key]: view }));
      readyRef.current.add(key);
      const buffered = bufferRef.current.get(key) ?? [];
      bufferRef.current.delete(key);
      for (const event of buffered) applyEvent(key, event);
    },
    [applyEvent, beginSessionLoad],
  );

  // 选中一个项目：切前台 + 打开它当前会话。
  const selectProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        if (project === activeProject) return;
        const bridge = await agentBridge();
        subRef.current?.unsubscribe();
        subRef.current = null;
        setSessionName(null);
        setActiveProject(project);
        await bridge.modelName(project).then(setModel).catch(() => setModel(""));
        const name = await bridge.currentSession(project).catch(() => null);
        if (name) await openSession(project, name).catch(() => {});
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, openSession],
  );

  // 启动：载入项目列表，选第一个（最近打开）并打开它当前会话。
  useEffect(() => {
    void (async () => {
      const bridge = await agentBridge();
      const active = await reloadProjects();
      const first = active[0]?.id ?? null;
      if (!first) return;
      setActiveProject(first);
      await bridge.modelName(first).then(setModel).catch(() => {});
      const name = await bridge.currentSession(first).catch(() => null);
      if (name) await openSession(first, name).catch(() => {});
    })();
  }, [reloadProjects, openSession]);

  const addProject = useCallback(async () => {
    try {
      setUiError("");
      const bridge = await agentBridge();
      const entry = await bridge.addProject();
      if (!entry) return; // 用户取消
      await reloadProjects();
      await selectProject(entry.id);
    } catch (e) {
      setUiError(e instanceof Error ? e.message : String(e));
    }
  }, [reloadProjects, selectProject]);

  const renameProject = useCallback(
    async (project: string, name: string) => {
      try {
        setUiError("");
        await (await agentBridge()).renameProject(project, name);
        await reloadProjects();
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [reloadProjects],
  );

  const archiveProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        await (await agentBridge()).archiveProject(project);
        // 归档的是当前项目：清空前台，切到下一个可用项目（可能为空）。
        if (project === activeProject) {
          subRef.current?.unsubscribe();
          subRef.current = null;
          setSessionName(null);
          setActiveProject(null);
          const active = await reloadProjects();
          const next = active[0]?.id ?? null;
          if (next) await selectProject(next);
        } else {
          await reloadProjects();
        }
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, reloadProjects, selectProject],
  );

  const restoreProject = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        await (await agentBridge()).restoreProject(project);
        await reloadProjects();
        await selectProject(project);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [reloadProjects, selectProject],
  );

  const switchSession = useCallback(
    async (name: string) => {
      if (!activeProject) return;
      try {
        setUiError("");
        // 先响应交互：立刻切前台 + 骨架屏，再等后端把 active 指针挪过去。
        beginSessionLoad(activeProject, name);
        const bridge = await agentBridge();
        await bridge.switchSession(activeProject, name);
        await openSession(activeProject, name);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, openSession, beginSessionLoad],
  );

  // 在指定项目下新建会话（项目行上的「＋」可直接作用于任意项目，不限于前台）。
  const newSession = useCallback(
    async (project: string) => {
      try {
        setUiError("");
        const bridge = await agentBridge();
        // 不带标题：名字在首条消息后由 AI 自动生成，用户想改再手动重命名。
        const entry = await bridge.newSession(project, null);
        // 若在非当前项目下新建，先把该项目切到前台，再打开新会话。
        if (project !== activeProject) await selectProject(project);
        await openSession(project, entry.name);
        // 直接聚焦输入框——新建会话的意图是"马上开始聊"，而不是先做管理。
        editorRef.current?.focus();
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject, selectProject, openSession],
  );

  const renameSession = useCallback(
    async (name: string, title: string) => {
      if (!activeProject) return;
      try {
        setUiError("");
        await (await agentBridge()).renameSession(activeProject, name, title);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject],
  );

  const deleteSession = useCallback(
    async (name: string) => {
      if (!activeProject) return;
      try {
        setUiError("");
        const bridge = await agentBridge();
        await bridge.deleteSession(activeProject, name);
        // 删的若是当前会话，重新打开后端选中的会话（可能为空）。
        if (name === sessionName) {
          const current = await bridge.currentSession(activeProject).catch(() => null);
          if (current) await openSession(activeProject, current);
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
    [activeProject, openSession, sessionName],
  );

  /** 把一条消息作为新一轮发给模型（回显 user + 起 assistant 流式占位）。
   *  `display` 是回显正文（保留 chip 占位符），`text` 是发给模型的纯文本。 */
  const dispatchTurn = useCallback(
    async (
      project: string,
      session: string,
      text: string,
      display: string,
      refs: FileReference[],
    ) => {
      const key = viewKey(project, session);
      if (views[key]?.busy ?? false) return;
      const assistantId = nextId();
      turnRef.current.set(key, assistantId);
      setViews((prev) => {
        const view = prev[key] ?? emptyView();
        return {
          ...prev,
          [key]: {
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
        await bridge.send(project, session, text, refs.map((reference) => reference.path));
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        setViews((prev) => {
          const view = prev[key] ?? emptyView();
          return { ...prev, [key]: { ...view, busy: false, error: message } };
        });
      }
    },
    [views],
  );

  const send = useCallback(async () => {
    // 发给模型 / 判空的正文要剥掉占位符（占位符不是用户文字）；但**回显用的
    // content 必须保留占位符**——`InlineReferences` 正是按占位符位置把 chip 落回
    // 正文的，剥掉就一个 tag 都渲染不出来（曾经的 bug）。
    const plain = stripTokens(input).trim();
    const project = activeProject;
    const session = sessionName;
    if (!plain || !project || !session) return;

    // `/` 技能入口（与 TUI 同语义）：`/name` 或 `/name args`。
    //   无参 = 只激活 + 本地系统提示（不发模型）；有参 = 激活 + 把 args 作为本轮 prompt。
    // desktop 无本地内置 slash 指令，命中 skill 才走这条路径；否则按普通消息发。
    if (plain.startsWith("/")) {
      const [head, ...rest] = plain.slice(1).split(/\s+/);
      const args = rest.join(" ").trim();
      if (head && slashSearch.hasSkill(head)) {
        const key = viewKey(project, session);
        if (views[key]?.busy ?? false) return;
        try {
          await (await agentBridge()).activateSkill(project, session, head);
        } catch (e) {
          const message = e instanceof Error ? e.message : String(e);
          setUiError(message);
          return;
        }
        slashSearch.close();
        setInput("");
        setReferences([]);
        setUiError("");
        if (!args) {
          // 无参：只激活，不打扰模型，给一条本地系统提示。
          setViews((prev) => {
            const view = prev[key] ?? emptyView();
            return {
              ...prev,
              [key]: {
                ...view,
                messages: [
                  ...view.messages,
                  {
                    id: nextId(),
                    role: "user",
                    content: `已激活技能 /${head}（本轮起每轮注入其指令）。`,
                    status: "complete",
                  },
                ],
              },
            };
          });
          return;
        }
        // 有参：激活后把 args 当本轮 prompt 继续走正常发送流程。
        await dispatchTurn(project, session, args, args, []);
        return;
      }
    }

    const refs = references;
    setInput("");
    setReferences([]);
    setUiError("");
    await dispatchTurn(project, session, plain, input.trim(), refs);
  }, [activeProject, input, references, sessionName, views, slashSearch, dispatchTurn]);

  const stop = useCallback(async () => {
    const project = activeProject;
    const session = sessionName;
    if (!project || !session) return;
    const key = viewKey(project, session);
    await (await agentBridge()).cancel(project, session).catch(() => {});
    setViews((prev) => {
      const view = prev[key] ?? emptyView();
      return { ...prev, [key]: { ...view, busy: false } };
    });
  }, [activeProject, sessionName]);

  const switchModel = useCallback(
    async (value: string) => {
      if (!activeProject) return;
      try {
        const bridge = await agentBridge();
        await bridge.setModel(activeProject, value);
        setModel(value);
      } catch (e) {
        setUiError(e instanceof Error ? e.message : String(e));
      }
    },
    [activeProject],
  );

  const view =
    activeProject && sessionName
      ? (views[viewKey(activeProject, sessionName)] ?? emptyView())
      : emptyView();
  const busy = view.busy;
  const error = uiError || view.error;

  return (
    <div className="flex h-screen w-full min-h-0 bg-canvas">
      {/* 左侧 sidebar：NewChat / 置顶 / 项目（多项目 + 会话导航）。 */}
      <SessionSidebar
        projects={projects}
        archivedProjects={archivedProjects}
        activeProject={activeProject}
        current={sessionName}
        onSelectProject={(id) => void selectProject(id)}
        onAddProject={() => void addProject()}
        onRenameProject={(id, name) => void renameProject(id, name)}
        onArchiveProject={(id) => void archiveProject(id)}
        onRestoreProject={(id) => void restoreProject(id)}
        onSelect={(name) => void switchSession(name)}
        onNew={(project) => void newSession(project)}
        onRename={(name, title) => void renameSession(name, title)}
        onDelete={(name) => void deleteSession(name)}
      />
      {/* 右侧主区：固定宽度居中（对齐 shiwen AiConversationSurface 的 max-w-190 = 760px）。
          聊天区与输入框都不随窗口拉宽，长文本按阅读舒适宽度换行。 */}
      <main className="flex min-h-0 flex-1 flex-col">
        <section className="mx-auto flex w-full max-w-190 min-h-0 flex-1 flex-col">
        {view.loading ? (
          // 历史快照尚未到达：先渲染骨架屏，给用户「正在加载消息记录」的即时反馈。
          <div className="min-h-0 flex-1 overflow-hidden">
            <AiConversationSkeleton />
          </div>
        ) : (
        <AiConversationTranscript
          messages={view.messages}
          conversationKey={activeProject && sessionName ? viewKey(activeProject, sessionName) : "main"}
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
        )}
        {error && (
          <p className="mx-3 mb-1 text-body-sm text-danger-ink" role="alert">
            {error}
          </p>
        )}
        <div className="relative shrink-0 px-3 pb-3">
          {slashSearch.open && (
            <div className="absolute inset-x-3 bottom-full z-30 mb-1">
              <SkillSlashPopover
                open={slashSearch.open}
                query={slashSearch.query}
                results={slashSearch.results}
                error={slashSearch.error}
                selectedIndex={slashSearch.selectedIndex}
                onSelect={(result) => editorRef.current?.insertSlashCommand(result.name)}
                onRetry={() => {}}
              />
            </div>
          )}
          {!slashSearch.open && mentionSearch.popoverOpen && (
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
                slash={slashSearch}
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
      </main>
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
    view: { messages, turnSegments, busy: snapshot.running, error: "", loading: false },
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
