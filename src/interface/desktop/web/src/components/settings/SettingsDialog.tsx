import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { Cloud, Info, Search, Settings2, Sparkles, X } from "lucide-react";
import { cn } from "@/ui/utils";
import { Button } from "@/ui/button";
import { SettingsRow, SettingsSection, SettingsSelect } from "@/components/settings/controls";
import { ProviderSettingsPanel } from "@/components/settings/ProviderSettingsPanel";
import { ConnectionSettingsPanel } from "@/components/settings/ConnectionSettingsPanel";
import { getThemeMode, setThemeMode, THEME_CHANGE_EVENT, type ThemeMode } from "@/lib/theme";

export type SettingsPage = "general" | "models" | "connection" | "about";
const OPEN_EVENT = "shirley-open-settings";

/** Open the unified Settings dialog from anywhere (buttons / menus / model selector). */
export function openSettings(page: SettingsPage = "general") {
  window.dispatchEvent(new CustomEvent<SettingsPage>(OPEN_EVENT, { detail: page }));
}

type NavItem = { id: SettingsPage; label: string; icon: ReactNode; group: string; keywords: string };
const NAV: NavItem[] = [
  { id: "general", label: "通用", icon: <Settings2 className="size-4" />, group: "个人", keywords: "general 通用 外观 主题 深色 浅色 界面" },
  { id: "models", label: "模型服务商", icon: <Sparkles className="size-4" />, group: "模型", keywords: "models 模型 服务商 provider api key 接口 上下文" },
  { id: "connection", label: "连接", icon: <Cloud className="size-4" />, group: "模型", keywords: "connection 连接 云端 本地 同步 remote" },
  { id: "about", label: "关于", icon: <Info className="size-4" />, group: "其他", keywords: "about 关于 版本 信息" },
];

function GeneralPage() {
  const [mode, setMode] = useState<ThemeMode>(() => getThemeMode());
  useEffect(() => {
    const sync = () => setMode(getThemeMode());
    window.addEventListener(THEME_CHANGE_EVENT, sync);
    return () => window.removeEventListener(THEME_CHANGE_EVENT, sync);
  }, []);
  const change = (next: ThemeMode) => { setMode(next); setThemeMode(next); };
  return (
    <>
      <SettingsSection title="外观">
        <SettingsRow title="主题" description="跟随系统，或固定为浅色 / 深色。">
          <SettingsSelect<ThemeMode>
            label="主题"
            value={mode}
            onChange={change}
            options={[
              { value: "system", label: "跟随系统" },
              { value: "light", label: "浅色" },
              { value: "dark", label: "深色" },
            ]}
          />
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title="对话">
        <SettingsRow title="模型选择" description="在输入框旁的模型选择器中随时切换；此处管理服务商与模型目录。" />
      </SettingsSection>
    </>
  );
}

function AboutPage() {
  return (
    <SettingsSection title="关于 Shirley">
      <SettingsRow title="Shirley" description="基于 Englife-1.0 的编码 Agent 桌面端。" />
      <SettingsRow title="工作区" description="每个项目对应一个工作区目录，会话与配置按工作区隔离。" />
    </SettingsSection>
  );
}

/**
 * Unified Settings dialog.
 *
 * One layout + one entry for every configuration surface: a searchable left rail
 * (grouped nav) and a centered content column, matching the reference design. Provider
 * and connection UIs are embedded pages rather than nested modals, shortening the path
 * from "open settings" to "edit a value".
 */
export function SettingsDialog() {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [page, setPage] = useState<SettingsPage>("general");
  const [query, setQuery] = useState("");

  useEffect(() => {
    const open = (event: Event) => {
      const next = (event as CustomEvent<SettingsPage>).detail;
      if (next) setPage(next);
      setQuery("");
      if (!dialog.current?.open) dialog.current?.showModal();
    };
    // Legacy entry point used by the composer's model selector: route to the Models page.
    const openModels = () => open(new CustomEvent<SettingsPage>(OPEN_EVENT, { detail: "models" }));
    window.addEventListener(OPEN_EVENT, open);
    window.addEventListener("open-model-providers", openModels);
    return () => {
      window.removeEventListener(OPEN_EVENT, open);
      window.removeEventListener("open-model-providers", openModels);
    };
  }, []);

  // Esc closes (native dialog behaviour); keep React state in sync via onClose.
  const close = () => dialog.current?.close();

  const results = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return NAV;
    return NAV.filter((item) => `${item.label} ${item.keywords}`.toLowerCase().includes(needle));
  }, [query]);

  const active = NAV.find((item) => item.id === page) ?? NAV[0];
  const groups = useMemo(() => {
    const map = new Map<string, NavItem[]>();
    for (const item of results) {
      const list = map.get(item.group) ?? [];
      list.push(item);
      map.set(item.group, list);
    }
    return [...map.entries()];
  }, [results]);

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      onClose={() => setQuery("")}
      onClick={(event) => { if (event.target === dialog.current) close(); }}
      className="m-auto h-[min(56rem,92dvh)] w-[min(72rem,calc(100vw-1.5rem))] overflow-hidden rounded-sheet border border-line bg-canvas p-0 text-ink shadow-sheet backdrop:bg-black/40"
    >
      <div className="flex h-full min-h-0">
        {/* Left rail: search + grouped navigation. */}
        <aside className="flex w-60 shrink-0 flex-col border-r border-line bg-surface">
          <div className="flex items-center justify-between px-4 pt-4 pb-2">
            <h2 id={titleId} className="text-body font-medium">设置</h2>
            <button type="button" aria-label="关闭设置" onClick={close} className="rounded-control p-1.5 text-muted hover:bg-selected hover:text-ink md:hidden"><X className="size-4" /></button>
          </div>
          <div className="px-3 pb-2">
            <div className="flex items-center gap-2 rounded-control border border-line bg-canvas px-2.5 py-1.5">
              <Search className="size-3.5 shrink-0 text-muted" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索设置"
                aria-label="搜索设置"
                className="min-w-0 flex-1 bg-transparent text-body-sm text-ink outline-none placeholder:text-muted"
              />
            </div>
          </div>
          <nav aria-label="设置导航" className="min-h-0 flex-1 overflow-auto px-2 pb-4">
            {groups.length === 0 && <p className="px-3 py-4 text-caption text-muted">没有匹配的设置项。</p>}
            {groups.map(([group, items]) => (
              <div key={group} className="mt-3">
                <p className="px-3 pb-1 text-caption font-medium text-muted">{group}</p>
                {items.map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    aria-current={item.id === page ? "page" : undefined}
                    onClick={() => { setPage(item.id); setQuery(""); }}
                    className={cn(
                      "flex w-full items-center gap-2.5 rounded-control px-3 py-2 text-left text-body-sm transition-colors",
                      item.id === page ? "bg-selected text-ink" : "text-muted hover:bg-quiet hover:text-ink",
                    )}
                  >
                    {item.icon}
                    <span className="truncate">{item.label}</span>
                  </button>
                ))}
              </div>
            ))}
          </nav>
        </aside>

        {/* Right content: fixed reading width, page title + sections. */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex h-12 shrink-0 items-center justify-end px-4">
            <Button size="icon-sm" variant="ghost" aria-label="关闭设置" onClick={close}><X className="size-4" /></Button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-6 pb-12 md:px-12">
            <div className="mx-auto w-full max-w-2xl">
              <h1 className="text-title-sm font-medium text-ink">{active.label}</h1>
              <div className="mt-2">
                {page === "general" && <GeneralPage />}
                {page === "models" && <ProviderSettingsPanel />}
                {page === "connection" && <ConnectionSettingsPanel canReload />}
                {page === "about" && <AboutPage />}
              </div>
            </div>
          </div>
        </div>
      </div>
    </dialog>
  );
}
