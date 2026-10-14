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
  onNewChat, onAddProject, topContent, children,
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
  /** 内容区顶栏的自定义内容；不传则回退到默认的「文件夹图标 + 标题 + 项目名」。
   *  让不同页面（Home / 各会话）可以定制顶栏右区。 */
  topContent?: ReactNode;
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

  // 全屏检测：进入全屏后 macOS 把红黄绿交通灯收到系统层级，标题栏左侧那段为交通灯
  // 预留的留白就成了空白——此时收掉留白，让后面的按钮补位。全屏切换会伴随一次
  // resize（窗口尺寸变化），故在 resize 回调里重查 `isFullscreen()`；非 mac / 非桌面
  // 环境（纯浏览器调试）无需检测。
  const [fullscreen, setFullscreen] = useState(false);
  useEffect(() => {
    if (!isMacDesktop) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;
    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      const sync = async () => {
        try {
          const next = await win.isFullscreen();
          if (!disposed) setFullscreen(next);
        } catch { /* 拿不到就当作未全屏 */ }
      };
      await sync();
      unlisten = await win.onResized(() => void sync());
    })();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return (
    <div className="flex h-dvh min-h-0 w-full flex-col overflow-hidden bg-frame">
      {/* 48px 标题栏与 tauri.conf.json 的 trafficLightPosition.y=26 配套；
          原生 y 是容器 inset，并不是按钮中心，20 会让红黄绿按钮偏高约 6px。 */}
      <header
        className="flex h-12 shrink-0 items-center bg-frame pr-3 select-none"
        data-tauri-drag-region
      >
        {/* 左区宽度对齐「图标栏 w-14 + 会话侧栏 w-64」= 19.5rem，使右端的分段竖线与
            下方 sidebar 的右边框同 x 对齐；竖线只是短线段，不向下贯穿成一条连续长线。
            侧栏折叠时左区不再定宽、也不画竖线（此时没有 sidebar 边界可对齐）。 */}
        <div
          className={cn(
            "flex h-full shrink-0 items-center gap-1 overflow-hidden transition-[width] duration-200 ease-product",
            isMacDesktop && !fullscreen ? "pl-[88px]" : "pl-3",
            sidebarCollapsed
              ? isMacDesktop && !fullscreen ? "w-[14.25rem]" : "w-[9.5rem]"
              : "w-[19.5rem]",
          )}
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
          {/* 新建会话：仅侧栏收起时在此展示——侧栏展开时侧栏内已有入口，避免重复。 */}
          {sidebarCollapsed && (
            <IconButton label="新建会话" onClick={onNewChat}><SquarePen className="size-4" /></IconButton>
          )}
          {/* 分段竖线：与下方会话侧栏的右边框同 x 对齐，但只是短线段、不向下贯穿。
              折叠时用透明度淡出（而非直接卸载），与左区宽度过渡同步。 */}
          <div
            className={cn(
              "ml-auto h-5 shrink-0 bg-line transition-[width,opacity] duration-200 ease-product",
              sidebarCollapsed ? "w-0 opacity-0" : "w-px",
            )}
          />
        </div>
        {/* 内容区顶栏（右区）：随页面定制，默认渲染文件夹图标 + 标题 + 项目名。 */}
        <div className="flex min-w-0 flex-1 items-center gap-2 self-stretch px-3" data-tauri-drag-region>
          {topContent ?? (
            <>
              <Folder className="size-4 shrink-0 text-muted pointer-events-none" />
              <span className="truncate text-body-sm font-medium pointer-events-none">{title}</span>
              {projectName && <span className="hidden truncate text-caption text-muted pointer-events-none md:block">{projectName}</span>}
            </>
          )}
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
      </header>
      <div className="flex min-h-0 flex-1">
        <nav aria-label="主导航" className="flex w-14 shrink-0 flex-col items-center gap-2 bg-frame py-3">
          <IconButton label="Home" active className="size-10" onClick={onHome}><House className="size-5" /></IconButton>
          <div className="flex-1" />
          <IconButton label={dark ? "切换浅色外观" : "切换深色外观"} className="size-10" onClick={toggleTheme}>{dark ? <Sun className="size-5" /> : <Moon className="size-5" />}</IconButton>
          <img src="/brand/shirley-logo.png" alt="Shirley" className="mt-1 size-8 rounded-full object-cover" draggable={false} />
        </nav>
        {/* 内容区 = 会话侧栏 + 聊天主区，作为一整块圆角面板嵌在框架（顶栏 + 左侧图标栏）里；
            左上内圆角透出下方 frame 色，形成框架与内容区的过渡。 */}
        <div className="flex min-h-0 min-w-0 flex-1 overflow-hidden rounded-tl-2xl bg-canvas">
          {children}
        </div>
      </div>
    </div>
  );
}
