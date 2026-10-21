import {
  Archive,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderInput,
  FolderOpen,
  FolderPlus,
  Loader2,
  MoreHorizontal,
  MessageSquarePlus,
  Pencil,
  Pin,
  Settings2,
  PinOff,
  Plus,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { ContextMenu, ContextMenuItem, ContextMenuSeparator } from "@/ui/context-menu";
import { ScrollArea } from "@/ui/scroll-area";
import { HoverScrollLabel } from "@/ui/hover-scroll-label";
import { cn } from "@/ui/utils";
import { useMobileLayout } from "@/lib/use-mobile-layout";
import { projectSource, type ProjectEntry, type SessionEntry } from "@/lib/bridge";

/**
 * Left sidebar: multi-project + session navigation.
 *
 * Structure (top to bottom):
 * 1. **NewChat**: the only always-visible button — "new session" (acting on the current
 *    context: a project session or a Chat session).
 * 2. **Recent**: recently aggregated sessions across projects (including Chat sessions) —
 *    chat and project sessions share one column, with the owning project noted in small
 *    text inline (Chat sessions have no label).
 * 3. **Pinned**: manually pinned sessions, aggregated across projects (local
 *    `localStorage` only; not synced / persisted). Pinned sessions are removed from the
 *    Projects and Recent lists so they appear exactly once.
 * 4. **Projects**: associated workspace directories — **each project is a collapsible
 *    group**, expanding to its sessions.
 *
 * **Interaction**: session rows surface common actions as inline icons (hidden by default,
 * shown on hover / focus); everything else still goes through the context menu:
 * - Session row hover: a `pin` icon (pin / unpin, always shown when pinned) and a `more`
 *   (...) icon (opens the same menu as right-click).
 * - Session row right-click: pin / unpin, rename, delete (with in-menu confirmation).
 * - Project row right-click: rename, archive (with in-menu confirmation); the "Projects"
 *   group header row has an always-visible `+` (new project).
 *
 * All data and actions come from the Rust side's `ProjectRegistry` / `SessionCatalog` /
 * `SessionManager` (sharing the same `<root>/.shirley/sessions` as the TUI); the frontend
 * holds no session state; "pin" and "collapse" are purely frontend position markers
 * (localStorage).
 *
 * "Remove project" = **archive**: no disk data deleted, only a flag set; archived
 * projects are not shown in the sidebar.
 */

const PINNED_STORAGE_KEY = "shirley.desktop.pinned-sessions";
/** Collapse state (stored per project id, local only). */
const COLLAPSED_STORAGE_KEY = "shirley.desktop.collapsed-projects";
/** Collapse state of group headers (the "Recent" / "Projects" groups, local only). */
const COLLAPSED_RECENT_KEY = "shirley.desktop.collapsed-recent";
const COLLAPSED_PROJECTS_KEY = "shirley.desktop.collapsed-projects-group";

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
    // Silently degrade when localStorage is unavailable (private mode, etc.): state is valid for this session only.
  }
}

function readBool(key: string): boolean {
  try {
    return window.localStorage.getItem(key) === "1";
  } catch {
    return false;
  }
}

function writeBool(key: string, value: boolean) {
  try {
    window.localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // Silently degrade when localStorage is unavailable (private mode, etc.): state is valid for this session only.
  }
}

/** Small inline marker so cloud-hosted projects / sessions are distinguishable in the merged view. */
function CloudBadge() {
  return (
    <span className="shrink-0 rounded-[4px] border border-line bg-inset px-1 text-[0.625rem] font-medium uppercase leading-4 tracking-wide text-muted">
      cloud
    </span>
  );
}

/** A project group: project metadata + its sessions (the current project passed from App decides whether to fetch). */
type ProjectGroup = {
  entry: ProjectEntry;
  sessions: SessionEntry[];
};

export function SessionSidebar({
  projects,
  activeProject,
  current,
  onAddProject,
  onEditProject,
  onRenameProject,
  onArchiveProject,
  onSelect,
  onNew,
  onRename,
  onDelete,
  onMove,
  collapsed = false,
  onClose,
  chatProjectIds = [],
  onCurrentTitleChange,
  className,
}: {
  /** Active projects (non-archived), most recently opened first. */
  projects: ProjectEntry[];
  /** Current foreground project id (null if none). */
  activeProject: string | null;
  /** Current session name (null if none). */
  current: string | null;
  onAddProject: () => void;
  /** Open the "Edit project" dialog (rename + manage source folders). */
  onEditProject: (entry: ProjectEntry) => void;
  onRenameProject: (id: string, name: string) => void;
  onArchiveProject: (id: string) => void;
  /** Select a session: session names are unique only within a project, so `project` is included. */
  onSelect: (project: string, name: string) => void;
  onNew: (project: string) => void;
  /** Rename a session (located by project + name). */
  onRename: (project: string, name: string, title: string) => void;
  /** Delete a session (located by project + name). */
  onDelete: (project: string, name: string) => void;
  /** Move a session to another project (located by project + name). */
  onMove: (project: string, name: string, targetProject: string) => void;
  /** Collapsed: hidden entirely (width zero), expanded via the top toolbar. */
  collapsed?: boolean;
  onClose?: () => void;
  /** Pseudo project ids hosting the "no associated project" Chat sessions, one per reachable source
   *  (namespaced: `local::__chat__` / `cloud::__chat__`). */
  chatProjectIds?: string[];
  onCurrentTitleChange: (title: string) => void;
  className?: string;
}) {
  const mobile = useMobileLayout();
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!mobile || collapsed) return;
    closeRef.current?.focus({ preventScroll: true });
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !document.querySelector('[role="menu"]')) onClose?.();
    };
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("keydown", escape);
      document.querySelector<HTMLElement>("[data-sidebar-toggle]")?.focus({ preventScroll: true });
    };
  }, [mobile, collapsed, onClose]);
  // Session list per project (cached by project id; App does not hold it). All loaded projects share one poll.
  const [sessionsByProject, setSessionsByProject] = useState<Record<string, SessionEntry[]>>({});
  const aliveRef = useRef(true);
  const [pinned, setPinned] = useState<string[]>(() => readStringArray(PINNED_STORAGE_KEY));
  // Collapsed project ids (all expanded by default).
  const [collapsedProjects, setCollapsedProjects] = useState<string[]>(() => readStringArray(COLLAPSED_STORAGE_KEY));
  // Collapse state of group headers (expanded by default).
  const [recentCollapsed, setRecentCollapsed] = useState(() => readBool(COLLAPSED_RECENT_KEY));
  const [projectsCollapsed, setProjectsCollapsed] = useState(() => readBool(COLLAPSED_PROJECTS_KEY));
  // Rename target (project id or session name) and draft value.
  const [editingProject, setEditingProject] = useState<string | null>(null);
  const [editingSession, setEditingSession] = useState<string | null>(null);
  const [editValue, setEditValue] = useState("");
  // Session name awaiting delete confirmation (in-menu confirmation to avoid accidental deletion).
  const [confirmingDelete, setConfirmingDelete] = useState<string | null>(null);
  // Key of the session whose "move to project" submenu is open (`project::name`).
  const [movingSession, setMovingSession] = useState<string | null>(null);
  // Project id awaiting archive confirmation (in-menu confirmation).
  const [confirmingArchive, setConfirmingArchive] = useState<string | null>(null);

  // Each project shows its own sessions (session names are unique only within a project).
  const groups: ProjectGroup[] = projects.map((entry) => ({
    entry,
    sessions: sessionsByProject[entry.id] ?? [],
  }));
  const currentTitle =
    (sessionsByProject[activeProject ?? ""] ?? []).find((entry) => entry.name === current)?.label ?? "";
  useEffect(() => onCurrentTitleChange(currentTitle), [currentTitle, onCurrentTitleChange]);

  // Unified list: chat pseudo project + all projects (sessions all go into "Recent"; project groups also need their own lists).
  const projectIdsKey = [...chatProjectIds, ...projects.map((entry) => entry.id)]
    .filter((id): id is string => Boolean(id))
    .join("\n");
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
        // Keep the previous result when a silent poll fails; don't clear the list.
      }
    };
    void load();
    const timer = window.setInterval(() => void load(), 1500);
    return () => {
      aliveRef.current = false;
      window.clearInterval(timer);
    };
  }, [projectIdsKey]);

  // Pin key = `project::name`: session names are unique only within a project, so a bare
  // name would collide across projects (and make pinned entries unfindable).
  const togglePin = (key: string) => {
    setPinned((prev) => {
      const next = prev.includes(key) ? prev.filter((item) => item !== key) : [...prev, key];
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

  const toggleRecentCollapsed = () => {
    setRecentCollapsed((prev) => {
      writeBool(COLLAPSED_RECENT_KEY, !prev);
      return !prev;
    });
  };

  const toggleProjectsCollapsed = () => {
    setProjectsCollapsed((prev) => {
      writeBool(COLLAPSED_PROJECTS_KEY, !prev);
      return !prev;
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

  const moveEntry = (project: string, name: string, targetProject: string) => {
    onMove(project, name, targetProject);
    setMovingSession(null);
    setConfirmingDelete(null);
  };

  // Pinned: aggregate across every loaded project (and Chat) — a pinned session may live in
  // any project. Order follows the pin insertion order; keys are `project::name`.
  const pinnedEntries = pinned
    .map((key) => {
      const sep = key.indexOf("::");
      if (sep < 0) return null;
      const project = key.slice(0, sep);
      const name = key.slice(sep + 2);
      const entry = (sessionsByProject[project] ?? []).find((item) => item.name === name);
      return entry ? { project, entry } : null;
    })
    .filter((item): item is { project: string; entry: SessionEntry } => item !== null);
  // "Recent": aggregated across projects (including Chat sessions), most recently
  // modified first. Chat and project sessions share one column, with the owning project
  // name noted inline in small text; Chat sessions (pseudo project) have no label.
  const projectNameById: Record<string, string> = Object.fromEntries(
    projects.map((entry) => [entry.id, entry.name]),
  );
  const recent = [
    ...chatProjectIds.flatMap((id) =>
      (sessionsByProject[id] ?? []).map((entry) => ({ project: id, entry })),
    ),
    ...projects.flatMap((project) =>
      (sessionsByProject[project.id] ?? []).map((entry) => ({ project: project.id, entry })),
    ),
  ]
    .filter(({ project, entry }) => !pinned.includes(`${project}::${entry.name}`))
    .sort((a, b) => b.entry.modified_ms - a.entry.modified_ms)
    .slice(0, 30);

  // Candidates for "move to project": the Chat pseudo project + all projects (excluding the source project).
  const moveTargets = [
    ...chatProjectIds.map((id) => ({ id, name: "Chat" })),
    ...projects.map((entry) => ({ id: entry.id, name: entry.name })),
  ];

  /** Session row context menu: pin / rename / move to project / delete (with confirmation). */
  const sessionMenu = (entry: SessionEntry, group: "pinned" | "project" | "recent", project: string) => {
    // Edit state / delete confirmation are located by `project::name` — session names are unique only within a project.
    const key = `${project}::${entry.name}`;
    const isPinned = group === "pinned" || pinned.includes(`${project}::${entry.name}`);
    const isConfirming = confirmingDelete === key;
    return (
      <>
        <ContextMenuItem
          icon={isPinned ? <PinOff className="size-3.5" /> : <Pin className="size-3.5" />}
          onSelect={() => togglePin(`${project}::${entry.name}`)}
        >
          {isPinned ? "Unpin" : "Pin"}
        </ContextMenuItem>
        <ContextMenuItem
          icon={<Pencil className="size-3.5" />}
          onSelect={() => {
            setEditingSession(key);
            setEditValue(entry.label);
          }}
        >
          Rename
        </ContextMenuItem>
        {moveTargets.some((target) => target.id !== project) && (
          <>
            <ContextMenuItem
              icon={<FolderInput className="size-3.5" />}
              closeOnSelect={false}
              onSelect={() => setMovingSession(movingSession === key ? null : key)}
            >
              Move to project…
            </ContextMenuItem>
            {movingSession === key &&
              moveTargets
                .filter((target) => target.id !== project)
                .map((target) => (
                  <ContextMenuItem
                    key={target.id}
                    icon={<Folder className="size-3.5" />}
                    onSelect={() => moveEntry(project, entry.name, target.id)}
                  >
                    {target.name}
                  </ContextMenuItem>
                ))}
          </>
        )}
        <ContextMenuSeparator />
        {isConfirming ? (
          <ContextMenuItem
            icon={<Trash2 className="size-3.5" />}
            tone="danger"
            onSelect={() => removeEntry(project, entry.name)}
          >
            Confirm delete?
          </ContextMenuItem>
        ) : (
          <ContextMenuItem
            icon={<Trash2 className="size-3.5" />}
            tone="danger"
            closeOnSelect={false}
            onSelect={() => setConfirmingDelete(key)}
          >
            Delete
          </ContextMenuItem>
        )}
      </>
    );
  };

  const renderRow = (
    entry: SessionEntry,
    group: "pinned" | "project" | "recent",
    project: string,
    projectName?: string,
  ) => {
    // The active state requires the same project and session name — session names are unique only within a project.
    const active = project === activeProject && entry.name === current;
    const isEditing = editingSession === `${project}::${entry.name}`;
    const isPinned = group === "pinned" || pinned.includes(`${project}::${entry.name}`);
    // Inline actions: pin (pin/unpin) and more (same menu as right-click, housing
    // rename/delete). Hidden by default, shown on hover or focus; a pinned pin stays
    // visible to indicate the state.
    const actions = (open: (event: React.MouseEvent) => void) => (
      <div className="hidden shrink-0 items-center gap-0.5 group-hover:flex group-focus-within:flex">
        <button
          type="button"
          aria-label="More actions"
          title="More actions"
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
          aria-label={isPinned ? "Unpin" : "Pin"}
          title={isPinned ? "Unpin" : "Pin"}
          className="flex size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink"
          onClick={(event) => {
            event.stopPropagation();
            togglePin(`${project}::${entry.name}`);
          }}
        >
          {/* Pinned uses the solid Pin (same position); unpinned uses PinOff to hint that pinning is available. */}
          {isPinned ? <Pin className="size-3.5" /> : <PinOff className="size-3.5" />}
        </button>
      </div>
    );
    return (
      <ContextMenu
        key={`${project}::${entry.name}`}
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
                    else if (mobile) onClose?.();
                  }}
                >
                  <HoverScrollLabel
                    text={entry.label}
                    className={cn("flex-1 text-body-sm text-ink", active && "font-medium")}
                  />
                  {projectName && (
                    <span className="max-w-[72px] shrink-0 truncate text-caption text-muted">
                      {projectName}
                    </span>
                  )}
                  {projectSource(project) === "cloud" && <CloudBadge />}
                  {entry.running && (
                    <Loader2 className="size-3.5 shrink-0 animate-spin text-muted" aria-label="Running" />
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
      id="session-sidebar"
      aria-label="Projects and sessions"
      data-session-sidebar
      aria-hidden={collapsed || undefined}
      className={cn(
        "session-sidebar flex h-full shrink-0 flex-col overflow-hidden transition-[width] duration-200 ease-product",
        collapsed ? "invisible w-0" : "w-64 border-r border-line",
        className,
      )}
    >
      <div className="flex h-12 shrink-0 items-center justify-between px-4 text-body-sm font-semibold">
        Shirley
        <button ref={closeRef} type="button" aria-label="Close sidebar" className="flex size-11 items-center justify-center rounded-control text-muted hover:bg-selected md:hidden" onClick={onClose}>
          <X className="size-5" />
        </button>
      </div>
      <ScrollArea type="scroll" className="min-h-0 flex-1" viewportClassName="px-2 pb-3">
        <button
          type="button"
          className="mb-4 flex h-9 w-full items-center gap-2 rounded-control px-2 text-body-sm hover:bg-selected"
          onClick={() => {
            // New Chat = Codex-style empty session: create a lazy empty session in the current context (project or Chat).
            if (activeProject) onNew(activeProject);
            else if (chatProjectIds.length) onNew(chatProjectIds[0]);
            else onAddProject();
          }}
        >
          <MessageSquarePlus className="size-4 text-muted" />New session
        </button>
        {/* Pinned: manually pinned sessions, aggregated across projects (ChatGPT-Work-style
            "Pinned" block above Projects). Order follows pin insertion order. */}
        {pinnedEntries.length > 0 && (
          <section className="mb-3">
            <div className="flex h-8 items-center px-2">
              <span className="min-w-0 truncate text-[0.875rem] font-medium text-muted">Pinned</span>
            </div>
            <div role="listbox" aria-label="Pinned sessions" className="flex flex-col gap-0.5">
              {pinnedEntries.map(({ project, entry }) =>
                renderRow(
                  entry,
                  "pinned",
                  project,
                  chatProjectIds.includes(project) ? undefined : projectNameById[project],
                ),
              )}
            </div>
          </section>
        )}
        {/* Projects: associated directories (placed before "Recent"). **Group header collapsible**, and each project is collapsible too. */}
        <section className="mb-3">
          <Collapsible open={!projectsCollapsed} onOpenChange={toggleProjectsCollapsed}>
            <ContextMenu
              menu={
                <ContextMenuItem
                  icon={<FolderPlus className="size-3.5" />}
                  onSelect={onAddProject}
                >
                  New project
                </ContextMenuItem>
              }
            >
              <div className="group flex h-8 items-center gap-1 px-2">
                <CollapsibleTrigger className="flex min-w-0 flex-1 items-center gap-1 text-left">
                  <span className="min-w-0 truncate text-[0.875rem] font-medium text-muted">
                    Projects
                  </span>
                  {projectsCollapsed ? (
                    <ChevronRight className="size-3.5 shrink-0 text-muted" />
                  ) : (
                    <ChevronDown className="size-3.5 shrink-0 text-muted" />
                  )}
                </CollapsibleTrigger>
                <button
                  type="button"
                  aria-label="New project"
                  title="New project"
                  className="hidden size-6 shrink-0 items-center justify-center rounded-control text-muted transition-colors group-hover:flex group-focus-within:flex hover:bg-selected hover:text-ink"
                  onClick={onAddProject}
                >
                  <Plus className="size-3.5" />
                </button>
              </div>
            </ContextMenu>
            <CollapsibleContent>
              <div className="flex flex-col gap-1">
                {groups.length === 0 && (
                  <p className="px-2 py-1.5 text-caption text-muted">No projects yet. Right-click "Projects" to create one.</p>
                )}
                {groups.map(({ entry, sessions: projectSessions }) => {
                  const isCollapsed = collapsedProjects.includes(entry.id);
                  const isEditing = editingProject === entry.id;
                  const isConfirming = confirmingArchive === entry.id;
                  const projectMenu = (
                    <>
                      <ContextMenuItem
                        icon={<Settings2 className="size-3.5" />}
                        onSelect={() => onEditProject(entry)}
                      >
                        Edit project
                      </ContextMenuItem>
                      <ContextMenuItem
                        icon={<Pencil className="size-3.5" />}
                        onSelect={() => {
                          setEditingProject(entry.id);
                          setEditValue(entry.name);
                        }}
                      >
                        Rename
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
                          Confirm archive?
                        </ContextMenuItem>
                      ) : (
                        <ContextMenuItem
                          icon={<Archive className="size-3.5" />}
                          tone="danger"
                          closeOnSelect={false}
                          onSelect={() => setConfirmingArchive(entry.id)}
                        >
                          Archive
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
                              {/* Expand/collapse share the same folder icon position, switching with state (no arrow). */}
                              {isCollapsed ? (
                                <Folder className="size-3.5 shrink-0 text-muted" />
                              ) : (
                                <FolderOpen className="size-3.5 shrink-0 text-muted" />
                              )}
                              <span className="min-w-0 flex-1 truncate text-body-sm text-ink">
                                {entry.name}
                              </span>
                              {projectSource(entry.id) === "cloud" && <CloudBadge />}
                              <span className="shrink-0 text-caption text-muted">
                                {projectSessions.length}
                              </span>
                            </CollapsibleTrigger>
                          )}
                          {!isEditing && (
                            <button
                              type="button"
                              aria-label={`New session in "${entry.name}"`}
                              title="New session"
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
                          aria-label={`Sessions of ${entry.name}`}
                          className="mt-0.5 flex flex-col gap-0.5"
                        >
                          {projectSessions.length === 0 ? (
                            <p className="px-2 py-1.5 text-caption text-muted">No sessions yet.</p>
                          ) : (
                            projectSessions
                              .filter((session) => !pinned.includes(`${entry.id}::${session.name}`))
                              .map((session) => renderRow(session, "project", entry.id))
                          )}
                        </div>
                      </CollapsibleContent>
                    </Collapsible>
                  );
                })}
              </div>
            </CollapsibleContent>
          </Collapsible>
        </section>

        {/* Recent: aggregated across projects (including Chat sessions), placed after "Projects". **Group header collapsible**.
            Chat and project sessions share a column, with the owning project noted inline in small text (Chat sessions have no label). */}
        {recent.length > 0 && (
          <section className="mb-3">
            <Collapsible open={!recentCollapsed} onOpenChange={toggleRecentCollapsed}>
              <CollapsibleTrigger className="flex h-8 w-full items-center gap-1 px-2 text-left">
                <span className="min-w-0 truncate text-[0.875rem] font-medium text-muted">
                  Recent
                </span>
                {recentCollapsed ? (
                  <ChevronRight className="size-3.5 shrink-0 text-muted" />
                ) : (
                  <ChevronDown className="size-3.5 shrink-0 text-muted" />
                )}
              </CollapsibleTrigger>
              <CollapsibleContent>
                <div role="listbox" aria-label="Recent sessions" className="flex flex-col gap-0.5">
                  {recent.map(({ project, entry }) =>
                    renderRow(entry, "recent", project, chatProjectIds.includes(project) ? undefined : projectNameById[project]),
                  )}
                </div>
              </CollapsibleContent>
            </Collapsible>
          </section>
        )}

      </ScrollArea>
    </aside>
  );
}
