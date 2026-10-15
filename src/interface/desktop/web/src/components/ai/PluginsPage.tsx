import { useCallback, useEffect, useState } from "react";
import { AlertCircle, Boxes, ExternalLink, LoaderCircle, RefreshCw, Search } from "lucide-react";
import { agentBridge, type PluginEntry, type PluginSource } from "@/lib/bridge";
import { cn } from "@/ui/utils";

/**
 * Plugins page: the **entry point + download-source integration** for the plugin
 * ecosystem.
 *
 * Scope (deliberately narrow): it lists the registered *sources* (marketplaces)
 * and lets the user fetch a source's index to browse the normalized plugin
 * entries it offers. It does **not** install / enable / resolve plugins yet —
 * the download & compatibility logic is a later phase (see
 * `docs/plugin-marketplace.md`, `docs/plugin-marketplace-design.md`). The page
 * is a thin UI over two Rust commands (`agent_list_plugin_sources` /
 * `agent_fetch_plugin_source`); it holds no source state of its own.
 */
export function PluginsPage() {
  const [sources, setSources] = useState<PluginSource[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [entries, setEntries] = useState<PluginEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");

  // Load the source registry once (pure metadata, no network).
  useEffect(() => {
    let disposed = false;
    void (async () => {
      try {
        const list = await (await agentBridge()).listPluginSources();
        if (disposed) return;
        setSources(list);
        if (list.length > 0) setSelected((prev) => prev ?? list[0].id);
      } catch (cause) {
        if (!disposed) setError(String(cause));
      }
    })();
    return () => {
      disposed = true;
    };
  }, []);

  const load = useCallback(async (sourceId: string) => {
    setLoading(true);
    setError("");
    try {
      const list = await (await agentBridge()).fetchPluginSource(sourceId);
      setEntries(list);
    } catch (cause) {
      setEntries([]);
      setError(String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  // Fetch the selected source's index whenever the selection changes.
  useEffect(() => {
    if (!selected) return;
    void load(selected);
  }, [selected, load]);

  const active = sources.find((source) => source.id === selected) ?? null;
  const filtered = query.trim()
    ? entries.filter((entry) =>
        `${entry.name} ${entry.description} ${entry.author ?? ""} ${entry.category ?? ""}`
          .toLowerCase()
          .includes(query.trim().toLowerCase()),
      )
    : entries;

  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      {/* Source list (left): the registered marketplaces. */}
      <aside className="flex w-72 shrink-0 flex-col border-r border-line">
        <div className="flex h-12 shrink-0 items-center gap-2 px-4">
          <Boxes className="size-4 text-muted" />
          <span className="text-body-sm font-medium">Plugin sources</span>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {sources.map((source) => (
            <button
              key={source.id}
              type="button"
              onClick={() => setSelected(source.id)}
              className={cn(
                "flex w-full flex-col items-start gap-0.5 rounded-control px-3 py-2 text-left transition-colors",
                selected === source.id ? "bg-selected text-ink" : "text-muted hover:bg-selected/60 hover:text-ink",
              )}
            >
              <span className="flex w-full items-center gap-1.5">
                <span className="truncate text-body-sm font-medium">{source.name}</span>
                {source.large && (
                  <span className="shrink-0 rounded-full bg-line px-1.5 text-caption text-muted">large</span>
                )}
              </span>
              <span className="line-clamp-2 text-caption text-muted">{source.description}</span>
            </button>
          ))}
          {sources.length === 0 && !error && (
            <p className="px-3 py-2 text-caption text-muted">Loading sources…</p>
          )}
        </div>
      </aside>

      {/* Entries (right): the normalized plugins of the selected source. */}
      <main className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="flex h-12 shrink-0 items-center gap-2 px-4">
          <span className="truncate text-body-sm font-medium">{active?.name ?? "Plugins"}</span>
          {active && (
            <span className="truncate text-caption text-muted">
              {entries.length > 0 ? `${entries.length} plugins` : ""}
            </span>
          )}
          <div className="ml-auto flex items-center gap-2">
            {active?.homepage && (
              <a
                href={active.homepage}
                target="_blank"
                rel="noreferrer"
                className="flex items-center gap-1 rounded-control px-2 py-1 text-caption text-muted hover:bg-selected hover:text-ink"
              >
                <ExternalLink className="size-3.5" /> Homepage
              </a>
            )}
            <button
              type="button"
              disabled={!selected || loading}
              onClick={() => selected && void load(selected)}
              className="flex items-center gap-1 rounded-control px-2 py-1 text-caption text-muted hover:bg-selected hover:text-ink disabled:opacity-40"
            >
              <RefreshCw className={cn("size-3.5", loading && "animate-spin")} /> Refresh
            </button>
          </div>
        </div>

        <div className="px-4 pb-2">
          <div className="flex items-center gap-2 rounded-control border border-line px-2.5 py-1.5">
            <Search className="size-3.5 shrink-0 text-muted" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Filter plugins…"
              className="min-w-0 flex-1 bg-transparent text-body-sm outline-none placeholder:text-muted"
            />
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
          {loading && entries.length === 0 && (
            <div className="flex items-center gap-2 py-6 text-body-sm text-muted">
              <LoaderCircle className="size-4 animate-spin" /> Fetching {active?.name}…
            </div>
          )}
          {error && (
            <div className="my-3 flex items-start gap-2 rounded-control border border-line bg-selected/40 px-3 py-2 text-body-sm text-danger-ink">
              <AlertCircle className="mt-0.5 size-4 shrink-0" />
              <span className="min-w-0 break-words">{error}</span>
            </div>
          )}
          {!loading && !error && filtered.length === 0 && (
            <p className="py-6 text-body-sm text-muted">No plugins.</p>
          )}
          <ul className="flex flex-col gap-2">
            {filtered.map((entry, index) => (
              <li
                key={`${entry.source_id}:${entry.name}:${index}`}
                className="flex flex-col gap-1 rounded-card border border-line px-3 py-2.5"
              >
                <div className="flex items-center gap-2">
                  <span className="truncate text-body-sm font-medium">{entry.name}</span>
                  {entry.category && (
                    <span className="shrink-0 rounded-full bg-line px-1.5 text-caption text-muted">
                      {entry.category}
                    </span>
                  )}
                  {entry.author && (
                    <span className="shrink-0 truncate text-caption text-muted">by {entry.author}</span>
                  )}
                </div>
                {entry.description && (
                  <p className="line-clamp-2 text-caption text-muted">{entry.description}</p>
                )}
                {(entry.source || entry.homepage) && (
                  <p className="truncate text-caption text-muted/70">{entry.source ?? entry.homepage}</p>
                )}
              </li>
            ))}
          </ul>
        </div>
      </main>
    </div>
  );
}
