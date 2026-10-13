import { useEffect, useState, type ReactNode } from "react";
import {
  ArrowLeft, ArrowRight, Folder, FolderPlus, House,
  MessageSquarePlus, Moon, MoreHorizontal, PanelLeftClose, PanelLeftOpen, SquarePen, Sun,
} from "lucide-react";
import { ContextMenu, ContextMenuItem, ContextMenuSeparator } from "@/ui/context-menu";
import { cn } from "@/ui/utils";

const isDesktop = "__TAURI_INTERNALS__" in window;
const isMacDesktop = isDesktop && /Mac/i.test(navigator.platform);
const THEME_KEY = "shirley.desktop.theme";

function IconButton({ label, children, active, className, ...props }: {
  label: string;
  children: ReactNode;
  active?: boolean;
} & Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "children">) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      aria-pressed={active}
      className={cn(
        "flex size-8 shrink-0 items-center justify-center rounded-control text-muted transition-colors hover:bg-selected hover:text-ink focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ink disabled:pointer-events-none disabled:opacity-30",
        active && "bg-selected text-ink",
        className,
      )}
      {...props}
    >
      {children}
    </button>
  );
}

export function DesktopChrome({
  title, projectName, sidebarCollapsed, canGoBack, canGoForward,
  navigationBusy, onBack, onForward, onToggleSidebar, onHome,
  onNewChat, onAddProject, children,
}: {
  title: string;
  projectName?: string;
  sidebarCollapsed: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  navigationBusy: boolean;
  onBack: () => void;
  onForward: () => void;
  onToggleSidebar: () => void;
  onHome: () => void;
  onNewChat: () => void;
  onAddProject: () => void;
  children: ReactNode;
}) {
  const [dark, setDark] = useState(() => document.documentElement.dataset.theme === "dark");
  const toggleTheme = () => {
    const next = !dark;
    setDark(next);
    document.documentElement.dataset.theme = next ? "dark" : "light";
    try { localStorage.setItem(THEME_KEY, next ? "dark" : "light"); } catch { /* 本次有效 */ }
  };

  useEffect(() => {
    document.title = `${title} — Shirley`;
  }, [title]);

  return (
    <div className="flex h-dvh min-h-0 w-full flex-col overflow-hidden bg-canvas">
      {/* 48px 标题栏与 tauri.conf.json 的 trafficLightPosition.y=26 配套；
          原生 y 是容器 inset，并不是按钮中心，20 会让红黄绿按钮偏高约 6px。 */}
      <header
        className={cn("flex h-12 shrink-0 items-center gap-1 border-b border-line bg-sidebar pr-3 select-none", isMacDesktop ? "pl-[88px]" : "pl-3")}
        data-tauri-drag-region
      >
        <IconButton label="后退" disabled={!canGoBack || navigationBusy} onClick={onBack}>
          <ArrowLeft className="size-4" />
        </IconButton>
        <IconButton label="前进" disabled={!canGoForward || navigationBusy} onClick={onForward}>
          <ArrowRight className="size-4" />
        </IconButton>
        <IconButton label={sidebarCollapsed ? "展开侧边栏" : "收起侧边栏"} aria-expanded={!sidebarCollapsed} onClick={onToggleSidebar}>
          {sidebarCollapsed ? <PanelLeftOpen className="size-4" /> : <PanelLeftClose className="size-4" />}
        </IconButton>
        <IconButton label="新建会话" onClick={onNewChat}><SquarePen className="size-4" /></IconButton>
        <div className="mx-2 h-5 w-px shrink-0 bg-line" />
        <div className="flex min-w-0 flex-1 items-center gap-2 self-stretch" data-tauri-drag-region>
          <Folder className="size-4 shrink-0 text-muted pointer-events-none" />
          <span className="truncate text-body-sm font-medium pointer-events-none">{title}</span>
          {projectName && <span className="hidden truncate text-caption text-muted pointer-events-none md:block">{projectName}</span>}
        </div>
        <ContextMenu menu={
          <>
            <ContextMenuItem icon={<MessageSquarePlus className="size-4" />} onSelect={onNewChat}>新建会话</ContextMenuItem>
            <ContextMenuItem icon={<FolderPlus className="size-4" />} onSelect={onAddProject}>添加项目</ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem icon={dark ? <Sun className="size-4" /> : <Moon className="size-4" />} onSelect={toggleTheme}>
              {dark ? "切换浅色外观" : "切换深色外观"}
            </ContextMenuItem>
          </>
        }>
          {(open) => <IconButton label="更多操作" aria-haspopup="menu" onClick={open}><MoreHorizontal className="size-4" /></IconButton>}
        </ContextMenu>
        <IconButton label="新建会话" onClick={onNewChat}><MessageSquarePlus className="size-4" /></IconButton>
      </header>
      <div className="flex min-h-0 flex-1">
        <nav aria-label="主导航" className="flex w-14 shrink-0 flex-col items-center gap-2 border-r border-line bg-sidebar py-3">
          <IconButton label="Home" active className="size-10" onClick={onHome}><House className="size-5" /></IconButton>
          <div className="flex-1" />
          <IconButton label={dark ? "切换浅色外观" : "切换深色外观"} className="size-10" onClick={toggleTheme}>{dark ? <Sun className="size-5" /> : <Moon className="size-5" />}</IconButton>
          <img src="/brand/shirley-logo.png" alt="Shirley" className="mt-1 size-8 rounded-full object-cover" draggable={false} />
        </nav>
        {children}
      </div>
    </div>
  );
}
