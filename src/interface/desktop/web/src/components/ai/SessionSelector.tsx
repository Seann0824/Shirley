import { Pencil, Plus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/ui/button";
import { ScrollArea } from "@/ui/scroll-area";
import { Separator } from "@/ui/separator";
import { cn } from "@/ui/utils";
import type { SessionEntry } from "@/lib/bridge";

/**
 * 会话管理选择器（参照 Codex 的 `/resume` picker）。
 *
 * 只是「会话目录 + 切换 / 新建 / 重命名 / 删除」的薄 UI——目录与操作都由 Rust
 * 侧的 `SessionCatalog` 提供（与 TUI 共用同一份 `<root>/.shirley/sessions`），
 * 前端不持有会话状态。切换走后端 `Agent::switch_session`，与 TUI 同一接缝。
 *
 * 视觉与 `ModelSelector` / `FileMentionPopover` 对齐：`rounded-card` 弹层、
 * 列表行（hover `bg-quiet`、选中 `bg-selected`）、`ScrollArea` 滚动、`Separator` 分隔。
 */
export function SessionSelector({
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
  onNew: (title: string | null) => void;
  onRename: (name: string, title: string) => void;
  onDelete: (name: string) => void;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<SessionEntry[]>([]);
  const [loading, setLoading] = useState(false);
  // 新建行：点击「新建会话」后变成标题输入框（空标题 = 匿名）。
  const [creating, setCreating] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  // 正在重命名的会话名与草稿值。
  const [editing, setEditing] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  // 待确认删除的会话名（两次点击确认，避免误删）。
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  const refresh = async () => {
    setLoading(true);
    try {
      const { agentBridge } = await import("@/lib/bridge");
      setEntries(await (await agentBridge()).listSessions());
    } catch {
      setEntries([]);
    } finally {
      setLoading(false);
    }
  };

  const close = () => {
    setOpen(false);
    setCreating(false);
    setNewTitle("");
    setEditing(null);
    setConfirmingDelete(null);
  };

  // 点击组件外部 / Esc 关闭。
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const toggle = () => {
    if (open) {
      close();
      return;
    }
    setOpen(true);
    void refresh();
  };

  const currentLabel = entries.find((entry) => entry.name === current)?.label ?? current;

  const submitNew = () => {
    const title = newTitle.trim();
    onNew(title || null);
    close();
  };

  const submitRename = (name: string) => {
    const title = editValue.trim();
    if (title) onRename(name, title);
    setEditing(null);
  };

  return (
    <div ref={rootRef} className={cn("relative", className)}>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label="会话管理"
        className="max-w-[14rem] gap-1 px-2 text-muted"
        onClick={toggle}
      >
        <span className="truncate">{currentLabel || "会话"}</span>
      </Button>
      {open && (
        <div
          className="absolute left-0 top-[calc(100%+0.5rem)] z-30 w-80 overflow-hidden rounded-card border border-line bg-surface shadow-lg"
        >
          {creating ? (
            <div className="flex items-center gap-2 p-1.5">
              <input
                autoFocus
                value={newTitle}
                placeholder="会话标题（可留空）"
                className="min-w-0 flex-1 rounded-control bg-inset px-2 py-1.5 text-body-sm text-ink outline-none placeholder:text-muted"
                onChange={(event) => setNewTitle(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") submitNew();
                  if (event.key === "Escape") {
                    event.stopPropagation();
                    setCreating(false);
                    setNewTitle("");
                  }
                }}
              />
              <Button type="button" size="sm" onClick={submitNew}>
                新建
              </Button>
            </div>
          ) : (
            <button
              type="button"
              className="flex w-full items-center gap-2 px-3 py-2 text-left text-body-sm text-ink transition-colors hover:bg-quiet"
              onClick={() => setCreating(true)}
            >
              <Plus className="size-3.5 shrink-0 text-muted" />
              <span>新建会话</span>
            </button>
          )}

          <Separator />

          <ScrollArea type="auto" viewportClassName="max-h-80 overscroll-contain">
            <div role="listbox" aria-label="会话列表" className="py-1">
              {loading && <p className="px-3 py-2 text-body-sm text-muted">加载中…</p>}
              {!loading && entries.length === 0 && (
                <p className="px-3 py-2 text-body-sm text-muted">还没有其它会话。</p>
              )}
              {!loading &&
                entries.map((entry) => {
                  const active = entry.name === current;
                  const isEditing = editing === entry.name;
                  const isConfirming = confirmingDelete === entry.name;
                  return (
                    <div
                      key={entry.name}
                      className={cn(
                        "group flex items-center gap-2 px-3 py-2 transition-colors",
                        active ? "bg-selected" : "hover:bg-quiet",
                      )}
                    >
                      {isEditing ? (
                        <input
                          autoFocus
                          value={editValue}
                          className="min-w-0 flex-1 rounded-control bg-inset px-2 py-1.5 text-body-sm text-ink outline-none"
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
                          className="min-w-0 flex-1 text-left"
                          onClick={() => {
                            close();
                            if (!active) onSelect(entry.name);
                          }}
                        >
                          <span
                            className={cn(
                              "block truncate text-body-sm text-ink",
                              active && "font-medium",
                            )}
                          >
                            {entry.label}
                          </span>
                          <span className="mt-0.5 block truncate text-caption text-muted">
                            {entry.preview || "（空会话）"} · {entry.turns} 轮 ·{" "}
                            {formatTime(entry.modified_ms)}
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
                              onClick={() => {
                                onDelete(entry.name);
                                // 只本地移除该项，不重新拉列表（避免抖动）。
                                setEntries((prev) =>
                                  prev.filter((item) => item.name !== entry.name),
                                );
                                setConfirmingDelete(null);
                              }}
                            >
                              确认删除
                            </Button>
                          ) : (
                            <>
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
                })}
            </div>
          </ScrollArea>
        </div>
      )}
    </div>
  );
}

/** 毫秒时间戳 → 简短本地时间（今天只显示时分，否则显示月日）。 */
function formatTime(ms: number): string {
  if (!ms) return "未知";
  const date = new Date(ms);
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  return sameDay
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : date.toLocaleDateString([], { month: "numeric", day: "numeric" });
}
