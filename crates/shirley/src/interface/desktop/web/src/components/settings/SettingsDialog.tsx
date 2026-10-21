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
  { id: "general", label: "General", icon: <Settings2 className="size-4" />, group: "Personal", keywords: "general appearance theme dark light interface" },
  { id: "models", label: "Model Providers", icon: <Sparkles className="size-4" />, group: "Models", keywords: "models provider api key endpoint context" },
  { id: "connection", label: "Connection", icon: <Cloud className="size-4" />, group: "Models", keywords: "connection cloud local sync remote" },
  { id: "about", label: "About", icon: <Info className="size-4" />, group: "Other", keywords: "about version info" },
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
      <SettingsSection title="Appearance">
        <SettingsRow title="Theme" description="Follow the system, or pin light / dark.">
          <SettingsSelect<ThemeMode>
            label="Theme"
            value={mode}
            onChange={change}
            options={[
              { value: "system", label: "System" },
              { value: "light", label: "Light" },
              { value: "dark", label: "Dark" },
            ]}
          />
        </SettingsRow>
      </SettingsSection>
      <SettingsSection title="Chat">
        <SettingsRow title="Model selection" description="Switch anytime from the model selector next to the input; manage providers and model catalogs here." />
      </SettingsSection>
    </>
  );
}

function AboutPage() {
  return (
    <SettingsSection title="About Shirley">
      <SettingsRow title="Shirley" description="A coding agent desktop built on Englife-1.0." />
      <SettingsRow title="Workspace" description="Each project maps to one workspace directory; sessions and config are isolated per workspace." />
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
            <h2 id={titleId} className="text-body font-medium">Settings</h2>
            <button type="button" aria-label="Close settings" onClick={close} className="rounded-control p-1.5 text-muted hover:bg-selected hover:text-ink md:hidden"><X className="size-4" /></button>
          </div>
          <div className="px-3 pb-2">
            <div className="flex items-center gap-2 rounded-control border border-line bg-canvas px-2.5 py-1.5">
              <Search className="size-3.5 shrink-0 text-muted" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search settings"
                aria-label="Search settings"
                className="min-w-0 flex-1 bg-transparent text-body-sm text-ink outline-none placeholder:text-muted"
              />
            </div>
          </div>
          <nav aria-label="Settings navigation" className="min-h-0 flex-1 overflow-auto px-2 pb-4">
            {groups.length === 0 && <p className="px-3 py-4 text-caption text-muted">No matching settings.</p>}
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
            <Button size="icon-sm" variant="ghost" aria-label="Close settings" onClick={close}><X className="size-4" /></Button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-6 pb-12 md:px-12">
            <div className="mx-auto w-full max-w-2xl">
              <h1 className="text-title-sm font-medium text-ink">{active.label}</h1>
              <div className="mt-2">
                {page === "general" && <GeneralPage />}
                {page === "models" && <ProviderSettingsPanel />}
                {page === "connection" && <ConnectionSettingsPanel />}
                {page === "about" && <AboutPage />}
              </div>
            </div>
          </div>
        </div>
      </div>
    </dialog>
  );
}
