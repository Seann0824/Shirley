import {
  Archive,
  Folder,
  FolderOpen,
  FolderPlus,
  Loader2,
  MoreHorizontal,
  PanelLeftClose,
  Pencil,
  Pin,
  PinOff,
  Plus,
  Trash2,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { ContextMenu, ContextMenuItem, ContextMenuSeparator } from "@/ui/context-menu";
import { ScrollArea } from "@/ui/scroll-area";
import { HoverScrollLabel } from "@/ui/hover-scroll-label";
import { cn } from "@/ui/utils";
import type { ProjectEntry, SessionEntry } from "@/lib/bridge";

/**
 * 左侧 sidebar：多项目 + 会话导航。
 *
 * 结构（自上而下）：
 * 1. **NewChat**：唯一常驻可见的按钮——「新建会话」（作用于当前项目）。
 * 2. **Pinned**：手动置顶的会话（仅本地 `localStorage`，不跨端 / 不落库）。
 * 3. **Projects**：已关联的工作区目录——**每个项目是一个可折叠分组**，展开后是它的
 *    会话。
 *
 * **交互**：会话行把常用操作提到行内图标（默认隐藏、悬停 / 聚焦才现），其余仍走右键菜单：
 * - 会话行悬停：`pin` 图标（置顶 / 取消置顶，已置顶时常驻）、`more`（⋯）图标（打开与右键同款菜单）。
 * - 会话行右键：置顶 / 取消置顶、重命名、删除（菜单内二次确认）。
 * - 项目行右键：重命名、归档（菜单内二次确认）；「项目」分组标题行有常驻 `+`（新建项目）。
 *
 * 数据与操作全来自 Rust 侧 `ProjectRegistry` / `SessionCatalog` / `SessionManager`
 * （与 TUI 共用同一份 `<root>/.shirley/sessions`），前端不持有会话状态；「置顶」与
 * 「折叠」是纯前端的位置标注（localStorage）。
 *
 * 「移除项目」= **归档**：不删磁盘数据，只置标志；归档项目不在 sidebar 展示。
 */

const PINNED_STORAGE_KEY = "shirley.desktop.pinned-sessions";
/** 折叠状态（按项目 id 存，仅本地）。 */
const COLLAPSED_STORAGE_KEY = "shirley.desktop.collapsed-projects";

function readStringArray(key: string): string[] {
  try {
    const raw = window.localStorage.getItem(key);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

function writeStringArray(key: string, values: string[]) {
  try {
    window.localStorage.setItem(key, JSON.stringify(values));
  } catch {
    // localStorage 不可用（隐私模式等）时静默降级：状态仅当次会话有效。
  }
}

/** 一个项目分组：项目元数据 + 其会话（由 App 传入的当前项目决定是否拉取）。 */
type ProjectGroup = {
  entry: ProjectEntry;
  sessions: SessionEntry[];
};

export function SessionSidebar({
  projects,
  activeProject,
  current,
  onAddProject,
  onRenameProject,
  onArchiveProject,
  onSelect,
  onNew,
  onRename,
  onDelete,
  collapsed = false,
  onToggleCollapse,
  className,
}: {
  /** 活跃项目（非归档），按最近打开降序。 */
  projects: ProjectEntry[];
  /** 当前前台项目 id（无则 null）。 */
  activeProject: string | null;
  /** 当前会话名（无则 null）。 */
  current: string | null;
  onAddProject: () => void;
  onRenameProject: (id: string, name: string) => void;
  onArchiveProject: (id: string) => void;
  /** 选中会话：会话名只在项目内唯一，故带 `project`。 */
  onSelect: (project: string, name: string) => void;
  onNew: (project: string) => void;
  /** 重命名会话（按项目 + 名定位）。 */
  onRename: (project: string, name: string, title: string) => void;
  /** 删除会话（按项目 + 名定位）。 */
  onDelete: (project: string, name: string) => void;
  /** 折叠态：整体隐藏（宽度归零），由外层在左上角浮出「展开」按钮。 */
  collapsed?: boolean;
  /** 切换折叠（手动）。 */
  onToggleCollapse?: () => void;
  className?: string;
}) {
  // 每个项目的会话列表（按项目 id 缓存，App 不持有）。所有已加载项目共享同一轮询。
  const [sessionsByProject, setSessionsByProject] = useState<Record<string, SessionEntry[]>>({});
  const aliveRef = useRef(true);
  const [pinned, setPinned] = useState<string[]>(() => readStringArray(PINNED_STORAGE_KEY));
  // 折叠的项目 id（默认全部展开）。
  const [collapsedProjects, setCollapsedProjects] = useState<string[]>(() => readStringArray(COLLAPSED_STORAGE_KEY));
  // 正在重命名的目标（项目 id 或会话名）与草稿值。
  const [editingProject, setEditingProject] = useState<string | null>(null);
  const [editingSession, setEditingSession] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  // 待确认删除的会话名（菜单内二次确认，避免误删）。
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);
  // 待确认归档的项目 id（菜单内二次确认）。
  const [confirmingArchive, setConfirmingArchive] = useState<string | null>(null);

  // 每个项目展示自己的会话（会话名只在项目内唯一）。
  const groups: ProjectGroup[] = projects.map((entry) => ({
    entry,
    sessions: sessionsByProject[entry.id] ?? [],
  }));

  // 拉取「每个项目」的会话；轮询刷新各项目后台会话的 `running`。这样每个项目都能
  // 独立展开并看到自己的会话，而不是只有当前激活项目才有内容。
  const projectIdsKey = projects.map((entry) => entry.id).join("\n");
  useEffect(() => {
    aliveRef.current = true;
    const ids = projectIdsKey ? projectIdsKey.split("\n") : [];
    if (ids.length === 0) {
      return () => {
        aliveRef.current = false;
      };
    }
    const load = async () => {
      try {
        const { agentBridge } = await import("@/lib/bridge");
        const bridge = await agentBridge();
        const results = await Promise.all(
          ids.map(async (id) => [id, await bridge.listSessions(id)] as const),
        );
        if (!aliveRef.current) return;
        setSessionsByProject((prev) => {
          const next = { ...prev };
          for (const [id, list] of results) next[id] = list;
          return next;
        });
      } catch {
        // 静默轮询失败时保留上一次结果，别把列表清空。
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 1500);
    return () => {
      aliveRef.current = false;
      window.clearInterval(timer);
    };
  }, [projectIdsKey]);

  const togglePin = (name: string) => {
    setPinned((prev) => {
      const next = prev.includes(name) ? prev.filter((item) => item !== name) : [...prev, name];
      writeStringArray(PINNED_STORAGE_KEY, next);
      return next;
    });
  };

  const toggleCollapsed = (project: string) => {
    setCollapsedProjects((prev) => {
      const next = prev.includes(project)
        ? prev.filter((item) => item !== project)
        : [...prev, project];
      writeStringArray(COLLAPSED_STORAGE_KEY, next);
      return next;
    });
  };

  const submitSessionRename = (project: string, name: string) => {
    const title = editValue.trim();
    if (title) onRename(project, name, title);
    setEditingSession(null);
  };

  const submitProjectRename = (id: string) => {
    const name = editValue.trim();
    if (name) onRenameProject(id, name);
    setEditingProject(null);
  };

  const removeEntry = (project: string, name: string) => {
    onDelete(project, name);
    setConfirmingDelete(null);
  };

  // 置顶只对当前项目的会话有意义（会话名只在项目内唯一）。
  const pinnedEntries = (sessionsByProject[activeProject ?? ""] ?? []).filter((entry) =>
    pinned.includes(entry.name),
  );

  /** 会话行的右键菜单：置顶 / 重命名 / 删除（二次确认）。 */
  const sessionMenu = (entry: SessionEntry, group: "pinned" | "project", project: string) => {
    // 编辑态 / 删除确认按 `project::name` 定位——会话名只在项目内唯一。
    const key = `${project}::${entry.name}`;
    const isPinned = pinned.includes(entry.name);
    const isConfirming = confirmingDelete === key;
    return (
      <>
        <ContextMenuItem
          icon={isPinned || group === "pinned" ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
          onSelect={() => togglePin(entry.name)}
        >
          {isPinned || group === "pinned" ? "取消置顶" : "置顶"}
        </ContextMenuItem>
        <ContextMenuItem
          icon={<Pencil className="size-3.5" />}
          onSelect={() => {
            setEditingSession(key);
            setEditValue(entry.label);
          }}
        >
          重命名
        </ContextMenuItem>
        <ContextMenuSeparator />
        {isConfirming ? (
          <ContextMenuItem
            icon={<Trash2 className="size-3.5" />}
            tone="danger"
            onSelect={() => removeEntry(project, entry.name)}
          >
            确认删除？
          </ContextMenuItem>
        ) : (
          <ContextMenuItem
            icon={<Trash2 className="size-3.5" />}
            tone="danger"
            closeOnSelect={false}
            onSelect={() => setConfirmingDelete(key)}
          >
            删除
          </ContextMenuItem>
        )}
      </>
    );
  };

  const renderRow = (entry: SessionEntry, group: "pinned" | "project", project: string) => {
    // 选中态必须同项目同会话名——会话名只在项目内唯一。
    const active = project === activeProject && entry.name === current;
    const isEditing = editingSession === `${project}::${entry.name}`;
    const isPinned = group === "pinned" || pinned.includes(entry.name);
    // 行内操作：pin（置顶/取消置顶）与 more（右键菜单同款，收起重命名/删除）。
    // 默认隐藏、悬停或聚焦时显示；已置顶的 pin 常驻可见以指示状态。
    const actions = (open: (event: React.MouseEvent) => void) => (
      <div className="hidden shrink-0 items-center gap-0.5 group-hover:flex group-focus-within:flex">
        <button
          type="button"
          aria-label="更多操作"
          title="更多操作"
          className="flex size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
          onClick={(event) => {
            event.stopPropagation();
            open(event);
          }}
        >
          <MoreHorizontal className="size-3.5" />
        </button>
        <button
          type="button"
          aria-label={isPinned ? "取消置顶" : "置顶"}
          title={isPinned ? "取消置顶" : "置顶"}
          className="flex size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
          onClick={(event) => {
            event.stopPropagation();
            togglePin(entry.name);
          }}
        >
          {/* 置顶态用实心 Pin（同一位置），未置顶用 PinOff 提示可置顶。 */}
          {isPinned ? <Pin className="size-3.5" /> : <PinOff className="size-3.5" />}
        </button>
      </div>
    );
    return (
      <ContextMenu
        key={entry.name}
        menu={sessionMenu(entry, group, project)}
        className={cn(
          "group flex h-8 items-center gap-1.5 rounded-control pr-2 pl-[26px] transition-colors",
          active ? "bg-selected" : "hover:bg-quiet",
        )}
      >
        {(open) => (
          <>
            {isEditing ? (
              <input
                autoFocus
                value={editValue}
                className="min-w-0 flex-1 rounded-control bg-inset px-2 py-1 text-body-sm text-ink outline-none"
                onChange={(event) => setEditValue(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") submitSessionRename(project, entry.name);
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    setEditingSession(null);
                  }
                }}
                onBlur={() => setEditingSession(null)}
              />
            ) : (
              <>
                <button
                  type="button"
                  role="option"
                  aria-selected={active}
                  className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
                  onClick={() => {
                    if (!active) onSelect(project, entry.name);
                  }}
                >
                  <HoverScrollLabel
                    text={entry.label}
                    className={cn("flex-1 text-body-sm text-ink", active && "font-medium")}
                  />
                  {entry.running && (
                    <Loader2 className="size-3.5 shrink-0 animate-spin text-muted" aria-label="运行中" />
                  )}
                </button>
                {actions(open)}
              </>
            )}
          </>
        )}
      </ContextMenu>
    );
  };

  return (
    <aside
      aria-hidden={collapsed || undefined}
      className={cn(
        "flex h-full shrink-0 flex-col overflow-hidden border-r border-line bg-sidebar transition-[width] duration-200 ease-product",
        collapsed ? "w-0 border-r-0" : "w-64",
        className,
      )}
    >
      {/* 顶部操作行：折叠按钮常驻在右上角，始终可点。 */}
      <div className="flex items-center justify-end px-2 pt-2">
        <button
          type="button"
          aria-label="收起侧边栏"
          title="收起侧边栏"
          className="flex size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
          onClick={() => onToggleCollapse?.()}
        >
          <PanelLeftClose className="size-4" />
        </button>
      </div>
      <ScrollArea type="auto" className="min-h-0 flex-1" viewportClassName="px-2 pb-3">
        {/* Pinned：手动置顶（仅本地）。无置顶时不显示该分组。 */}
        {pinnedEntries.length > 0 && (
          <section className="mb-3">
            <h2 className="px-2 py-1 text-caption font-medium tracking-wide text-muted uppercase">
              置顶
            </h2>
            <div role="listbox" aria-label="置顶会话" className="flex flex-col gap-0.5">
              {activeProject &&
                pinnedEntries.map((entry) => renderRow(entry, "pinned", activeProject))}
            </div>
          </section>
        )}

        {/* Projects：已关联目录列表，每个项目可折叠；操作全在右键菜单里。 */}
        <section>
          <ContextMenu
            menu={
              <ContextMenuItem
                icon={<FolderPlus className="size-3.5" />}
                onSelect={onAddProject}
              >
                新建项目
              </ContextMenuItem>
            }
          >
            <div className="group flex items-center justify-between gap-1 px-2 py-1">
              <h2 className="text-caption font-medium tracking-wide text-muted uppercase">项目</h2>
              <button
                type="button"
                aria-label="新建项目"
                title="新建项目"
                className="flex size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
                onClick={onAddProject}
              >
                <FolderPlus className="size-3.5" />
              </button>
            </div>
          </ContextMenu>
          <div className="flex flex-col gap-1">
            {groups.length === 0 && (
              <p className="px-2 py-1.5 text-caption text-muted">还没有项目，右键「项目」新建。</p>
            )}
            {groups.map(({ entry, sessions: projectSessions }) => {
              const isCollapsed = collapsedProjects.includes(entry.id);
              const isEditing = editingProject === entry.id;
              const isConfirming = confirmingArchive === entry.id;
              const projectMenu = (
                <>
                  <ContextMenuItem
                    icon={<Pencil className="size-3.5" />}
                    onSelect={() => {
                      setEditingProject(entry.id);
                      setEditValue(entry.name);
                    }}
                  >
                    重命名
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                  {isConfirming ? (
                    <ContextMenuItem
                      icon={<Archive className="size-3.5" />}
                      tone="danger"
                      onSelect={() => {
                        onArchiveProject(entry.id);
                        setConfirmingArchive(null);
                      }}
                    >
                      确认归档？
                    </ContextMenuItem>
                  ) : (
                    <ContextMenuItem
                      icon={<Archive className="size-3.5" />}
                      tone="danger"
                      closeOnSelect={false}
                      onSelect={() => setConfirmingArchive(entry.id)}
                    >
                      归档
                    </ContextMenuItem>
                  )}
                </>
              );
              return (
                <Collapsible
                  key={entry.id}
                  open={!isCollapsed}
                  onOpenChange={() => toggleCollapsed(entry.id)}
                >
                  <ContextMenu menu={projectMenu}>
                    <div className="group flex h-8 items-center gap-1 rounded-control px-2 transition-colors hover:bg-quiet">
                      {isEditing ? (
                        <input
                          autoFocus
                          value={editValue}
                          className="min-w-0 flex-1 rounded-control bg-inset px-2 py-1 text-body-sm text-ink outline-none"
                          onChange={(event) => setEditValue(event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") submitProjectRename(entry.id);
                            if (event.key === "Escape") {
                              event.stopPropagation();
                              setEditingProject(null);
                            }
                          }}
                          onBlur={() => setEditingProject(null)}
                        />
                      ) : (
                        <CollapsibleTrigger
                          className="flex min-w-0 flex-1 items-center gap-1 text-left"
                          title={entry.path}
                        >
                          {/* 展开/折叠共用同一位置的文件夹图标，随状态切换（不用箭头）。 */}
                          {isCollapsed ? (
                            <Folder className="size-3.5 shrink-0 text-muted" />
                          ) : (
                            <FolderOpen className="size-3.5 shrink-0 text-muted" />
                          )}
                          <span className="min-w-0 flex-1 truncate text-body-sm text-ink">
                            {entry.name}
                          </span>
                          <span className="shrink-0 text-caption text-muted">
                            {projectSessions.length}
                          </span>
                        </CollapsibleTrigger>
                      )}
                      {!isEditing && (
                        <button
                          type="button"
                          aria-label={`在「${entry.name}」新建会话`}
                          title="新建会话"
                          className="hidden size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors group-hover:flex hover:bg-selected hover:text-ink"
                          onClick={(event) => {
                            event.stopPropagation();
                            onNew(entry.id);
                          }}
                        >
                          <Plus className="size-3.5" />
                        </button>
                      )}
                    </div>
                  </ContextMenu>
                  <CollapsibleContent>
                    <div
                      role="listbox"
                      aria-label={`${entry.name} 的会话`}
                      className="mt-0.5 flex flex-col gap-0.5"
                    >
                      {projectSessions.length === 0 ? (
                        <p className="px-2 py-1.5 text-caption text-muted">还没有会话。</p>
                      ) : (
                        projectSessions.map((session) => renderRow(session, "project", entry.id))
                      )}
                    </div>
                  </CollapsibleContent>
                </Collapsible>
              );
            })}
          </div>
        </section>
      </ScrollArea>
    </aside>
  );
}
