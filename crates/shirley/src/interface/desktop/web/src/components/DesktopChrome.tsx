import { useEffect, useState, type ReactNode } from "react";
import {
  ArrowLeft, ArrowRight, Folder, FolderPlus, House,
  MessageSquarePlus, Moon, MoreHorizontal, PanelLeftClose, PanelLeftOpen, Settings2, SquarePen, Sun,
} from "lucide-react";
import { ContextMenu, ContextMenuItem, ContextMenuSeparator } from "@/ui/context-menu";
import { cn } from "@/ui/utils";
import { useMobileLayout } from "@/lib/use-mobile-layout";
import { SettingsDialog, openSettings } from "@/components/settings/SettingsDialog";
import { getThemeMode, resolveTheme, setThemeMode, THEME_CHANGE_EVENT, type ThemeMode } from "@/lib/theme";

const isDesktop = "__TAURI_INTERNALS__" in window;
const isMacDesktop = isDesktop && /Mac/i.test(navigator.platform);

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
  onNewChat, onAddProject, topContent, canReloadConnection = true, children,
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
  /** Custom content for the content-area top bar; falls back to the default
   *  "folder icon + title + project name". Lets different pages (Home / sessions)
   *  customize the top-bar right area. */
  topContent?: ReactNode;
  canReloadConnection?: boolean;
  children: ReactNode;
}) {
  const mobile = useMobileLayout();
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!mobile || !viewport) return;
    const update = () => {
      // Pinch zoom must not resize the app or fight the user's zoom gesture.
      if (viewport.scale === 1) document.documentElement.style.setProperty("--app-height", `${viewport.height}px`);
    };
    update();
    viewport.addEventListener("resize", update);
    return () => {
      viewport.removeEventListener("resize", update);
      document.documentElement.style.removeProperty("--app-height");
    };
  }, [mobile]);
  // Theme lives in the shared module so the Settings dialog and this chrome stay in sync.
  const [mode, setMode] = useState<ThemeMode>(() => getThemeMode());
  useEffect(() => {
    const sync = () => setMode(getThemeMode());
    window.addEventListener(THEME_CHANGE_EVENT, sync);
    return () => window.removeEventListener(THEME_CHANGE_EVENT, sync);
  }, []);
  const dark = resolveTheme(mode) === "dark";
  const toggleTheme = () => {
    const next: ThemeMode = dark ? "light" : "dark";
    setMode(next);
    setThemeMode(next);
  };

  useEffect(() => {
    document.title = `${title} — Shirley`;
  }, [title]);

  // Fullscreen detection: in fullscreen, macOS moves the red/yellow/green traffic
  // lights to the system layer, so the title-bar left area reserved for them becomes
  // blank — collapse that gutter and let the following buttons take its place. Fullscreen
  // toggling comes with a resize (window size change), so re-check `isFullscreen()` in
  // the resize callback; non-mac / non-desktop environments (plain-browser debugging)
  // need no detection.
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
        } catch { /* if unavailable, treat as not fullscreen */ }
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
    <div className="app-shell flex h-dvh min-h-0 w-full flex-col overflow-hidden bg-frame">
      {/* The 48px title bar pairs with trafficLightPosition.y=26 in tauri.conf.json;
          the native y is the container inset, not the button center, so 20 would make
          the traffic lights about 6px too high. */}
      <header
        className="app-header flex h-12 shrink-0 items-center bg-frame pr-3 select-none"
        data-tauri-drag-region
      >
        {/* The left area width aligns with "icon rail w-14 + session sidebar w-64" =
            19.5rem, so the divider tick at its right end lines up on the same x as the
            sidebar's right border below; the tick is only a short segment, not a
            continuous line running down. When the sidebar is collapsed, the left area is
            no longer fixed-width and the tick is not drawn (there is no sidebar border to
            align to). */}
        <div
          className={cn(
            "app-header-navigation flex h-full shrink-0 items-center gap-1 overflow-hidden transition-[width] duration-200 ease-product",
            isMacDesktop && !fullscreen ? "pl-[88px]" : "pl-3",
            sidebarCollapsed
              ? isMacDesktop && !fullscreen ? "w-[14.25rem]" : "w-[9.5rem]"
              : "w-[19.5rem]",
          )}
        >
          <IconButton className="hidden md:flex" label="Back" disabled={!canGoBack || navigationBusy} onClick={onBack}>
            <ArrowLeft className="size-4" />
          </IconButton>
          <IconButton className="hidden md:flex" label="Forward" disabled={!canGoForward || navigationBusy} onClick={onForward}>
            <ArrowRight className="size-4" />
          </IconButton>
          <IconButton label={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"} aria-expanded={!sidebarCollapsed} aria-controls="session-sidebar" data-sidebar-toggle onClick={onToggleSidebar}>
            {sidebarCollapsed ? <PanelLeftOpen className="size-4" /> : <PanelLeftClose className="size-4" />}
          </IconButton>
          {/* New session: shown here only when the sidebar is collapsed — when expanded, the sidebar already has an entry, avoiding duplication. */}
          {sidebarCollapsed && (
            <IconButton label="New session" onClick={onNewChat}><SquarePen className="size-4" /></IconButton>
          )}
          {/* Divider tick: lines up on the same x as the session sidebar's right border
              below, but is only a short segment, not running down. Fades out via opacity
              when collapsed (rather than unmounting), synced with the left area's width
              transition. */}
          <div
            className={cn(
              "hidden md:block ml-auto h-5 shrink-0 bg-line transition-[width,opacity] duration-200 ease-product",
              sidebarCollapsed ? "w-0 opacity-0" : "w-px",
            )}
          />
        </div>
        {/* Content-area top bar (right area): customized per page, default renders folder icon + title + project name. */}
        <div className="flex min-w-0 flex-1 items-center gap-2 self-stretch px-3" data-tauri-drag-region>
          {topContent ?? (
            <>
              <Folder className="size-4 shrink-0 text-muted pointer-events-none" />
              <span className="truncate text-body-sm font-medium pointer-events-none">{title}</span>
              {projectName && <span className="hidden truncate text-caption text-muted pointer-events-none md:block">{projectName}</span>}
            </>
          )}
        </div>
        <IconButton label="Settings" onClick={() => openSettings("general")}><Settings2 className="size-4" /></IconButton>
        <ContextMenu menu={
          <>
            <ContextMenuItem icon={<MessageSquarePlus className="size-4" />} onSelect={onNewChat}>New session</ContextMenuItem>
            <ContextMenuItem icon={<FolderPlus className="size-4" />} onSelect={onAddProject}>Add project</ContextMenuItem>
            <ContextMenuSeparator />
            <ContextMenuItem icon={dark ? <Sun className="size-4" /> : <Moon className="size-4" />} onSelect={toggleTheme}>
              {dark ? "Switch to light appearance" : "Switch to dark appearance"}
            </ContextMenuItem>
          </>
        }>
          {(open) => <IconButton label="More actions" aria-haspopup="menu" onClick={open}><MoreHorizontal className="size-4" /></IconButton>}
        </ContextMenu>
      </header>
      <div className="flex min-h-0 flex-1">
        <nav aria-label="Main navigation" className="hidden md:flex w-14 shrink-0 flex-col items-center gap-2 bg-frame py-3">
          <IconButton label="Home" active className="size-10" onClick={onHome}><House className="size-5" /></IconButton>
          <div className="flex-1" />
          <IconButton label="Settings" className="size-10" onClick={() => openSettings("general")}><Settings2 className="size-5" /></IconButton>
          <IconButton label={dark ? "Switch to light appearance" : "Switch to dark appearance"} className="size-10" onClick={toggleTheme}>{dark ? <Sun className="size-5" /> : <Moon className="size-5" />}</IconButton>
          <img src="/brand/shirley-logo.png" alt="Shirley" className="mt-1 size-8 rounded-full object-cover" draggable={false} />
        </nav>
        {/* Content area = session sidebar + chat main area, embedded as a single rounded
            panel in the frame (top bar + left icon rail); the top-left inner corner lets
            the frame color show through, forming a transition between frame and content. */}
        <div className="app-content relative flex min-h-0 min-w-0 flex-1 overflow-hidden md:rounded-tl-2xl bg-canvas">
          {children}
        </div>
      </div>
      <SettingsDialog />
    </div>
  );
}
