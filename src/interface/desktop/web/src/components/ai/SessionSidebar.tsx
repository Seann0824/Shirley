import { ChevronRight, FolderGit2, Loader2, Pencil, Pin, PinOff, Plus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { ScrollArea } from "@/ui/scroll-area";
import { cn } from "@/ui/utils";
import type { SessionEntry } from "@/lib/bridge";

/**
 * 左侧 sidebar：会话导航。
 *
 * 结构（自上而下）：
 * 1. **NewChat**：独立 item（置顶的「新建会话」按钮）。
 * 2. **Pinned**：手动置顶的会话（仅本地 `localStorage`，不跨端 / 不落库）。
 * 3. **Projects**：已关联的目录列表——**每个项目是一个可折叠分组**，展开后是它的会话。
 *
 * `Pinned` / `Projects` 只是 **分组标题（group 标记）**，不嵌套锁进；项目分组本身可折叠。
 * 数据与操作全来自 Rust 侧 `SessionCatalog` / `SessionManager`（与 TUI 共用同一份
 * `<root>/.shirley/sessions`），前端不持有会话状态；「置顶」是纯前端的位置标注。
 *
 * **多项目**：Shirley 作为独立 app 不绑定单一目录，Projects 是「已关联目录」的集合。
 * 当前后端仍单工作区，故这里只渲染 1 个项目；`projects` 已按数组建模，接上多工作区
 * 后端后无需改布局。
 */

const PINNED_STORAGE_KEY = "shirley.desktop.pinned-sessions";
/** 折叠状态（按项目名存，仅本地）。 */
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

/** 一个项目（= 一个已关联目录）及其会话。 */
type Project = {
  /** 项目标识（当前 = 工作区名；多项目后 = 目录路径）。 */
  name: string;
  sessions: SessionEntry[];
};

export function SessionSidebar({
  current,
  onSelect,
  onNew,
  onRename,
  onDelete,
  className,
}: {
  /** 当前会话名（无则 null）。 */
  current: string | null;
  onSelect: (name: string) => void;
  onNew: () => void;
  onRename: (name: string, title: string) => void;
  onDelete: (name: string) => void;
  className?: string;
}) {
  const [entries, setEntries] = useState<SessionEntry[]>([]);
  const [workspace, setWorkspace] = useState("");
  const [pinned, setPinned] = useState<string[]>(() => readStringArray(PINNED_STORAGE_KEY));
  // 折叠的项目名（默认全部展开）。
  const [collapsed, setCollapsed] = useState<string[]>(() => readStringArray(COLLAPSED_STORAGE_KEY));
  // 正在重命名的会话名与草稿值。
  const [editing, setEditing] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  // 待确认删除的会话名（两次点击确认，避免误删）。
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);
  // 组件已卸载标记，避免轮询回调 setState。
  const aliveRef = useRef(true);

  const load = async () => {
    try {
      const { agentBridge } = await import("@/lib/bridge");
      const bridge = await agentBridge();
      const [list, name] = await Promise.all([bridge.listSessions(), bridge.workspaceName()]);
      if (!aliveRef.current) return;
      setEntries(list);
      setWorkspace(name);
    } catch {
      // 静默轮询失败时保留上一次结果，别把列表清空。
    }
  };

  useEffect(() => {
    aliveRef.current = true;
    void load();
    // 轮询刷新后台会话的 `running`（只活在 Rust 内存，未订阅的前端收不到事件）。
    const timer = window.setInterval(() => void load(), 1500);
    return () => {
      aliveRef.current = false;
      window.clearInterval(timer);
    };
  }, []);

  const togglePin = (name: string) => {
    setPinned((prev) => {
      const next = prev.includes(name) ? prev.filter((item) => item !== name) : [...prev, name];
      writeStringArray(PINNED_STORAGE_KEY, next);
      return next;
    });
  };

  const toggleCollapsed = (project: string) => {
    setCollapsed((prev) => {
      const next = prev.includes(project)
        ? prev.filter((item) => item !== project)
        : [...prev, project];
      writeStringArray(COLLAPSED_STORAGE_KEY, next);
      return next;
    });
  };

  const submitRename = (name: string) => {
    const title = editValue.trim();
    if (title) onRename(name, title);
    setEditing(null);
  };

  const removeEntry = (name: string) => {
    onDelete(name);
    // 只本地移除该项，不重新拉列表（避免抖动）。
    setEntries((prev) => prev.filter((item) => item.name !== name));
    setConfirmingDelete(null);
  };

  const pinnedEntries = entries.filter((entry) => pinned.includes(entry.name));
  // 未置顶的会话归到项目下（置顶的只出现在 Pinned 区，不重复）。
  // 当前后端单工作区 → 一个项目；`projects` 按数组建模以便接多工作区。
  const projects: Project[] = [
    { name: workspace || "工作区", sessions: entries.filter((entry) => !pinned.includes(entry.name)) },
  ];

  const renderRow = (entry: SessionEntry, group: "pinned" | "project") => {
    const active = entry.name === current;
    const isEditing = editing === entry.name;
    const isConfirming = confirmingDelete === entry.name;
    const isPinned = pinned.includes(entry.name);
    return (
      <div
        key={entry.name}
        className={cn(
          "group flex items-center gap-1.5 rounded-control px-2 py-1.5 transition-colors",
          active ? "bg-selected" : "hover:bg-quiet",
        )}
      >
        {isEditing ? (
          <input
            autoFocus
            value={editValue}
            className="min-w-0 flex-1 rounded-control bg-inset px-2 py-1 text-body-sm text-ink outline-none"
            onChange={(event) => setEditValue(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") submitRename(entry.name);
              if (event.key === "Escape") {
                event.stopPropagation();
                setEditing(null);
              }
            }}
            onBlur={() => setEditing(null)}
          />
        ) : (
          <button
            type="button"
            role="option"
            aria-selected={active}
            className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
            onClick={() => {
              if (!active) onSelect(entry.name);
            }}
          >
            {entry.running && (
              <Loader2 className="size-3.5 shrink-0 animate-spin text-accent" aria-label="运行中" />
            )}
            <span className="min-w-0 flex-1">
              <span
                className={cn("block truncate text-body-sm text-ink", active && "font-medium")}
              >
                {entry.label}
              </span>
              {entry.preview && (
                <span className="mt-0.5 block truncate text-caption text-muted">
                  {entry.preview}
                </span>
              )}
            </span>
          </button>
        )}

        {!isEditing && (
          <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
            {isConfirming ? (
              <Button
                type="button"
                variant="danger"
                size="sm"
                className="h-7 min-h-0 px-2 text-caption"
                onClick={() => removeEntry(entry.name)}
              >
                删除
              </Button>
            ) : (
              <>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label={isPinned ? "取消置顶" : "置顶"}
                  className="size-7 min-h-0 text-muted"
                  onClick={() => togglePin(entry.name)}
                >
                  {isPinned || group === "pinned" ? (
                    <PinOff className="size-3.5" />
                  ) : (
                    <Pin className="size-3.5" />
                  )}
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label="重命名"
                  className="size-7 min-h-0 text-muted"
                  onClick={() => {
                    setEditing(entry.name);
                    setEditValue(entry.label);
                  }}
                >
                  <Pencil className="size-3.5" />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label="删除"
                  className="size-7 min-h-0 text-muted hover:text-danger-ink"
                  onClick={() => setConfirmingDelete(entry.name)}
                >
                  <Trash2 className="size-3.5" />
                </Button>
              </>
            )}
          </div>
        )}
      </div>
    );
  };

  return (
    <aside
      className={cn(
        "flex h-full w-64 shrink-0 flex-col border-r border-line bg-sidebar",
        className,
      )}
    >
      {/* NewChat：独立 item，置顶。 */}
      <div className="shrink-0 p-2">
        <button
          type="button"
          className="flex w-full items-center gap-2 rounded-control px-2 py-2 text-left text-body-sm text-ink transition-colors hover:bg-quiet"
          onClick={onNew}
        >
          <Plus className="size-4 shrink-0 text-muted" />
          <span>新建会话</span>
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
              {pinnedEntries.map((entry) => renderRow(entry, "pinned"))}
            </div>
          </section>
        )}

        {/* Projects：已关联目录列表，每个项目可折叠。 */}
        <section>
          <h2 className="px-2 py-1 text-caption font-medium tracking-wide text-muted uppercase">
            项目
          </h2>
          <div className="flex flex-col gap-1">
            {projects.map((project) => {
              const isCollapsed = collapsed.includes(project.name);
              return (
                <Collapsible
                  key={project.name}
                  open={!isCollapsed}
                  onOpenChange={() => toggleCollapsed(project.name)}
                >
                  <CollapsibleTrigger
                    className={cn(
                      "group flex w-full items-center gap-1 rounded-control px-2 py-1.5 text-left transition-colors hover:bg-quiet",
                    )}
                  >
                    <ChevronRight
                      className={cn(
                        "size-3.5 shrink-0 text-muted transition-transform",
                        !isCollapsed && "rotate-90",
                      )}
                    />
                    <FolderGit2 className="size-3.5 shrink-0 text-muted" />
                    <span className="min-w-0 flex-1 truncate text-body-sm text-ink">
                      {project.name}
                    </span>
                    <span className="shrink-0 text-caption text-muted">{project.sessions.length}</span>
                  </CollapsibleTrigger>
                  <CollapsibleContent>
                    <div
                      role="listbox"
                      aria-label={`${project.name} 的会话`}
                      className="mt-0.5 ml-3 flex flex-col gap-0.5 border-l border-line pl-1"
                    >
                      {project.sessions.length === 0 ? (
                        <p className="px-2 py-1.5 text-caption text-muted">还没有会话。</p>
                      ) : (
                        project.sessions.map((entry) => renderRow(entry, "project"))
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
