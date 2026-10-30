import {
  ArrowLeft,
  ArrowUpDown,
  ChevronDown,
  FileText,
  Grid2x2,
  Image as ImageIcon,
  LayoutList,
  Loader2,
  Plus,
  Search,
  Upload,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { agentBridge, type LibraryEntry } from "@/lib/bridge";
import { ContextMenu, ContextMenuItem } from "@/ui/context-menu";
import { Button } from "@/ui/button";
import { FIELD_SHELL } from "@/ui/field";
import { cn } from "@/ui/utils";

/**
 * The media **library**, as a page rather than a dialog.
 *
 * It is an inventory, not a view of one session: everything the user has ever handed us (pasted,
 * dropped, uploaded, or produced by a tool) lands in the same `ArtifactStore`, so this page works
 * across session switches, compaction and rewinds. That is the whole reason it needs its own
 * destination — "which file did I send you last week?" should not require scrolling a transcript.
 *
 * There is no second storage layer: the page lists the store that ingest writes to, so the jail,
 * the mime allow-list and the 64MiB cap all apply unchanged. Entries carry only an opaque id; a
 * thumbnail resolves it through `assetUrl`, exactly like a part in the transcript does.
 *
 * Layout follows the reference information architecture — title left / actions right, a row of
 * plain text filters, a section label, then an uncaptioned masonry wall — while every colour,
 * radius and type step comes from `styles/tokens.css`. Filter categories are only the ones the
 * store can actually classify; there are no aspirational "Favorites"/"Folders" tabs.
 */
export function LibraryPage({
  project,
  onClose,
  onReference,
}: {
  /** Which project's library to show (ids are project-scoped, so the jail is too). */
  project: string;
  /** Leave the page — used after handing an entry back to the composer. There is no
   *  in-page close button: the rail's Library icon is the way in and out. */
  onClose: () => void;
  /** Hand the entry back to the composer as a `media` chip. */
  onReference: (entry: LibraryEntry) => void;
}) {
  const [entries, setEntries] = useState<LibraryEntry[] | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [sort, setSort] = useState<Sort>("new");
  const [layout, setLayout] = useState<Layout>("grid");
  const [preview, setPreview] = useState<LibraryEntry | null>(null);
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [busy, setBusy] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      const bridge = await agentBridge();
      const list = await bridge.listLibrary(project);
      setEntries(list);
      setError("");
    } catch (e) {
      setEntries([]);
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [project]);

  useEffect(() => {
    void load();
  }, [load]);

  /** Adding files from here goes through the same ingest as a drop on the composer. */
  const upload = useCallback(
    async (files: FileList | null) => {
      if (!files || files.length === 0) return;
      setBusy(true);
      try {
        const bridge = await agentBridge();
        for (const file of Array.from(files)) {
          await bridge.ingestBytes(project, file.name, file.type || "application/octet-stream", file);
        }
        await load();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
        if (fileInput.current) fileInput.current.value = "";
      }
    },
    [project, load],
  );

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const list = (entries ?? []).filter((entry) => {
      if (filter !== "all" && classify(entry.mime) !== filter) return false;
      return !needle || entry.name.toLowerCase().includes(needle) || entry.mime.toLowerCase().includes(needle);
    });
    if (sort === "old") return [...list].sort((a, b) => a.addedMs - b.addedMs);
    if (sort === "name") return [...list].sort((a, b) => a.name.localeCompare(b.name));
    return list;
  }, [entries, filter, query, sort]);

  const shown = visible.slice(0, limit);
  const pick = (entry: LibraryEntry) => {
    onReference(entry);
    onClose();
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col overflow-hidden bg-canvas">
      <div className="mx-auto flex w-full max-w-[80rem] shrink-0 flex-col gap-5 px-6 pt-8 md:px-10">
        {/* Title left, actions right. Same order as the reference: sort · density · search · new. */}
        <header className="flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
          <div className="flex min-w-0 items-center gap-2">
            {/* Leaving the library is the one thing this page must always offer: on a phone it is
                a full-surface page with no rail behind it, so without this the only exits were
                "pick a file" or reload. */}
            <button
              type="button"
              aria-label="Back to chat"
              title="Back to chat"
              onClick={onClose}
              className={ICON_BUTTON}
            >
              <ArrowLeft className="size-4" />
            </button>
            <h1 className="truncate font-display text-title-sm text-ink">Library</h1>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <ContextMenu
              menu={
                <>
                  {SORTS.map((item) => (
                    <ContextMenuItem key={item.value} onSelect={() => setSort(item.value)}>
                      {item.label}
                    </ContextMenuItem>
                  ))}
                </>
              }
            >
              {(open) => (
                <button type="button" aria-label="Sort" title="Sort" onClick={open} className={ICON_BUTTON}>
                  <ArrowUpDown className="size-4" />
                </button>
              )}
            </ContextMenu>

            {/* Density: images read better as a wall, mixed documents read better as rows. */}
            <div className="flex items-center gap-0.5 rounded-full bg-inset p-0.5">
              <DensityButton active={layout === "grid"} label="Grid view" onClick={() => setLayout("grid")}>
                <Grid2x2 className="size-4" />
              </DensityButton>
              <DensityButton active={layout === "list"} label="List view" onClick={() => setLayout("list")}>
                <LayoutList className="size-4" />
              </DensityButton>
            </div>

            {/* The composer's field shell: one definition, so search and chat input cannot drift. */}
            <label className={cn(FIELD_SHELL, "flex h-9 w-56 items-center gap-2 px-3.5")}>
              <Search className="size-4 shrink-0 text-muted" />
              <input
                value={query}
                onChange={(event) => {
                  setQuery(event.target.value);
                  setLimit(PAGE_SIZE);
                }}
                placeholder="Search library"
                aria-label="Search library"
                className="min-w-0 flex-1 bg-transparent text-body-sm text-ink placeholder:text-muted focus:outline-hidden"
              />
            </label>

            <ContextMenu
              menu={
                <ContextMenuItem icon={<Upload className="size-4" />} onSelect={() => fileInput.current?.click()}>
                  Upload files
                </ContextMenuItem>
              }
            >
              {(open) => (
                <button
                  type="button"
                  onClick={open}
                  disabled={busy}
                  className="flex h-9 items-center gap-1 rounded-full bg-ink pr-3 pl-4 text-body-sm text-canvas transition-opacity disabled:opacity-60"
                >
                  {busy ? <Loader2 className="size-4 animate-spin" /> : "New"}
                  <ChevronDown className="size-3.5 opacity-70" />
                </button>
              )}
            </ContextMenu>
            <input
              ref={fileInput}
              type="file"
              multiple
              className="hidden"
              onChange={(event) => void upload(event.target.files)}
            />
          </div>
        </header>

        {/* Plain text filters with a single filled pill for the current one — not a row of
            outlined chips. `All` sits last, as in the reference. */}
        <nav aria-label="Filter" className="flex flex-wrap items-center gap-1">
          {FILTERS.map((item) => {
            const active = filter === item.value;
            return (
              <button
                key={item.value}
                type="button"
                aria-pressed={active}
                onClick={() => {
                  setFilter(item.value);
                  setLimit(PAGE_SIZE);
                }}
                className={cn(
                  "h-8 rounded-full px-3.5 text-body-sm transition-colors",
                  active ? "bg-selected font-medium text-ink" : "text-muted hover:bg-quiet hover:text-ink",
                )}
              >
                {item.label}
              </button>
            );
          })}
        </nav>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-6 pt-6 pb-12 md:px-10">
        <div className="mx-auto w-full max-w-[80rem]">
          {entries === null ? (
            <div className="flex items-center gap-2 py-16 text-body-sm text-muted">
              <Loader2 className="size-4 animate-spin" />
              Loading library…
            </div>
          ) : visible.length === 0 ? (
            error ? (
              <p className="py-16 text-center text-body-sm text-danger-ink">{error}</p>
            ) : (
              <div className="flex flex-col items-center gap-1.5 py-16 text-center">
                <ImageIcon className="mb-1 size-6 text-muted/50" />
                <p className="text-body-sm text-ink">
                  {entries.length === 0 ? "Nothing here yet" : "Nothing matches your search"}
                </p>
                <p className="text-caption text-muted">
                  {entries.length === 0
                    ? "Drop a file into the composer, or use New."
                    : "Try a different name, type or filter."}
                </p>
              </div>
            )
          ) : (
            <>
              {layout === "grid" ? (
                <>
                  {/* The wall keeps the reference's "Items" caption: a grid of pictures explains
                      itself, and the caption is the only labelling it gets. */}
                  <h2 className="pb-3 text-body-sm text-muted">Items</h2>
                  {/* Masonry, not a fixed grid: pictures keep their own aspect, so the wall packs
                      tight the way the reference does and nothing is letterboxed. */}
                  <div className="columns-2 gap-3 sm:columns-3 lg:columns-4 xl:columns-5 2xl:columns-6">
                    {shown.map((entry) => (
                      <div key={entry.id} className="mb-3 break-inside-avoid">
                        <LibraryTile
                          project={project}
                          entry={entry}
                          onPreview={() => setPreview(entry)}
                          onReference={() => pick(entry)}
                        />
                      </div>
                    ))}
                  </div>
                </>
              ) : (
                // Same table as the reference: Name · Modified · Size, no outer frame. The caption
                // is not repeated here — the column header already says what each column is.
                <div role="table" aria-label="Library items" className="flex flex-col">
                  <div role="rowgroup">
                    <div role="row" className={cn(ROW_GRID, "sticky top-0 z-10 bg-canvas pb-2 text-caption text-muted")}>
                      <span role="columnheader" className="sr-only">
                        Preview
                      </span>
                      <span role="columnheader">Name</span>
                      <span role="columnheader">Modified</span>
                      <span role="columnheader" className="text-right">
                        Size
                      </span>
                      <span role="columnheader" className="sr-only">
                        Actions
                      </span>
                    </div>
                  </div>
                  <div role="rowgroup" className="flex flex-col">
                    {shown.map((entry) => (
                      <LibraryRow
                        key={entry.id}
                        project={project}
                        entry={entry}
                        onPreview={() => setPreview(entry)}
                        onReference={() => pick(entry)}
                      />
                    ))}
                  </div>
                </div>
              )}

              {visible.length > shown.length && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="mx-auto mt-6 flex"
                  onClick={() => setLimit((current) => current + PAGE_SIZE)}
                >
                  Show {Math.min(PAGE_SIZE, visible.length - shown.length)} more ({visible.length - shown.length} left)
                </Button>
              )}
            </>
          )}
        </div>
      </div>

      {preview && (
        // Preview stays in the page: a separate window would lose the library context (and on the
        // desktop build would need another webview). `ink` is the project's own dark, not pure black.
        <div
          role="presentation"
          className="absolute inset-0 z-20 flex animate-overlay-in flex-col bg-ink/92 p-6 backdrop-blur-sm"
          onClick={() => setPreview(null)}
        >
          <div className="flex items-center justify-between gap-4 pb-4 text-body-sm text-canvas">
            <span className="min-w-0 truncate">
              {preview.name}{" "}
              <span className="font-utility text-meta text-canvas/55">{formatBytes(preview.bytes)}</span>
            </span>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Close preview"
              className="text-canvas hover:bg-canvas/10"
              onClick={() => setPreview(null)}
            >
              <X className="size-4" />
            </Button>
          </div>
          <div
            className="flex min-h-0 flex-1 items-center justify-center rounded-card border border-canvas/15 p-3 shadow-detail"
            onClick={(event) => event.stopPropagation()}
          >
            <LibraryAsset project={project} id={preview.id} mime={preview.mime} name={preview.name} mode="full" />
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * How many tiles we mint handles for at once.
 *
 * Handles are capped globally (an overflow clears the whole table, which would break images
 * already on screen in the transcript), so a library with thousands of entries must not resolve
 * every thumbnail at once. Paging keeps the working set small and the cost bounded.
 */
const PAGE_SIZE = 60;

/**
 * The list view's column template — preview · name · modified · size · actions.
 *
 * One definition for the sticky header and every row: the whole point of the reference's table is
 * that the header sits exactly over the columns, and two copies of a `grid-cols-[...]` string is
 * exactly how that alignment rots.
 */
const ROW_GRID = "grid grid-cols-[2.25rem_minmax(0,1fr)_7.5rem_5rem_1.75rem] items-center gap-3";

/** One small square control; the page has several and they must not drift apart. */
const ICON_BUTTON =
  "flex size-9 items-center justify-center rounded-control text-muted transition-colors hover:bg-quiet hover:text-ink";

type Filter = "all" | "images" | "docs" | "other";
type Sort = "new" | "old" | "name";
type Layout = "grid" | "list";

/** Only categories we can actually classify — no aspirational "Favourites"/"Folders" tabs. */
const FILTERS: { value: Filter; label: string }[] = [
  { value: "images", label: "Images" },
  { value: "docs", label: "Documents" },
  { value: "other", label: "Other" },
  { value: "all", label: "All" },
];

const SORTS: { value: Sort; label: string }[] = [
  { value: "new", label: "Newest first" },
  { value: "old", label: "Oldest first" },
  { value: "name", label: "Name" },
];

function classify(mime: string): Exclude<Filter, "all"> {
  if (mime.startsWith("image/")) return "images";
  if (mime.startsWith("text/") || /pdf|word|excel|powerpoint|officedocument/.test(mime)) return "docs";
  return "other";
}

function DensityButton({
  active,
  label,
  onClick,
  children,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "flex size-8 items-center justify-center rounded-full transition-colors",
        active ? "bg-selected text-ink shadow-card" : "text-muted hover:text-ink",
      )}
    >
      {children}
    </button>
  );
}

function LibraryTile({
  project,
  entry,
  onPreview,
  onReference,
}: {
  project: string;
  entry: LibraryEntry;
  onPreview: () => void;
  onReference: () => void;
}) {
  return (
    // No caption row: the name lives in the tooltip and in the preview, so the wall stays dense.
    // The Reference action is a sibling of the preview button, never a child of it — nesting
    // interactive elements is invalid HTML and confuses keyboard focus.
    <div className="group relative overflow-hidden rounded-card bg-inset">
      <button type="button" onClick={onPreview} title={entry.name} className="block w-full text-left">
        <LibraryAsset project={project} id={entry.id} mime={entry.mime} name={entry.name} />
      </button>
      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex justify-end bg-linear-to-t from-ink/40 to-transparent p-2 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
        <button
          type="button"
          onClick={onReference}
          className="pointer-events-auto flex items-center gap-1 rounded-full bg-ink/90 px-2.5 py-1 text-caption text-canvas"
        >
          <Plus className="size-3" />
          Reference
        </button>
      </div>
    </div>
  );
}

/** One table row: the same five columns as the sticky header, in the same order. */
function LibraryRow({
  project,
  entry,
  onPreview,
  onReference,
}: {
  project: string;
  entry: LibraryEntry;
  onPreview: () => void;
  onReference: () => void;
}) {
  return (
    // A full-bleed row with a divider, not a card: the reference's table has no outer frame, and
    // a long inventory reads as one continuous list. Modified and Size get their own columns
    // instead of a "mime · bytes · date" caption, so they line up down the page and can be
    // scanned. The Reference action only surfaces on hover/focus — the same gesture the grid
    // tiles use — because every row carrying a button is what makes a table look busy.
    <div
      role="row"
      className={cn(ROW_GRID, "group h-14 border-b border-line/70 transition-colors hover:bg-quiet")}
    >
      <div role="cell">
        <button
          type="button"
          onClick={onPreview}
          aria-label={`Preview ${entry.name}`}
          className="flex size-9 items-center justify-center overflow-hidden rounded-control border border-line bg-inset"
        >
          <LibraryAsset project={project} id={entry.id} mime={entry.mime} name={entry.name} mode="thumb" />
        </button>
      </div>
      <div role="cell" className="min-w-0">
        <button
          type="button"
          onClick={onPreview}
          title={`${entry.name} · ${entry.mime}`}
          className="block w-full truncate text-left text-body-sm text-ink"
        >
          {entry.name}
        </button>
      </div>
      <div role="cell" className="truncate text-body-sm text-muted">
        {formatModified(entry.addedMs)}
      </div>
      <div role="cell" className="truncate text-right font-utility text-body-sm text-muted">
        {formatBytes(entry.bytes)}
      </div>
      <div role="cell" className="flex justify-end">
        <button
          type="button"
          onClick={onReference}
          title="Reference in chat"
          aria-label={`Reference ${entry.name} in chat`}
          className="flex size-7 items-center justify-center rounded-control text-muted opacity-0 transition-opacity group-hover:opacity-100 hover:bg-selected hover:text-ink focus-visible:opacity-100"
        >
          <Plus className="size-3.5" />
        </button>
      </div>
    </div>
  );
}

/**
 * One entry's picture, resolved through the same opaque-handle channel as the transcript.
 *
 * An image we cannot resolve (mock bridge, expired handle, non-image mime) degrades to a glyph,
 * never to a broken image: the library is a viewer, not a place to surface transport errors.
 *
 * `mode`: `wall` keeps the picture's own aspect (masonry), `thumb` fills a square, `full` fits
 * inside the preview stage.
 */
function LibraryAsset({
  project,
  id,
  mime,
  name,
  mode = "wall",
}: {
  project: string;
  id: string;
  mime: string;
  name: string;
  mode?: "wall" | "thumb" | "full";
}) {
  const isImage = mime.startsWith("image/");
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    if (!isImage) return;
    let cancelled = false;
    void (async () => {
      try {
        const bridge = await agentBridge();
        const resolved = await bridge.assetUrl(project, id);
        if (!cancelled) setUrl(resolved || null);
      } catch {
        if (!cancelled) setUrl(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [project, id, isImage]);

  if (!isImage) {
    const Icon = classify(mime) === "docs" ? FileText : ImageIcon;
    if (mode === "thumb") return <Icon className="size-4 text-muted/80" strokeWidth={1.5} />;
    if (mode === "full") {
      return (
        <div className="flex flex-col items-center gap-2 text-canvas/70">
          <Icon className="size-8" strokeWidth={1.5} />
          <span className="max-w-80 truncate text-body-sm">{name}</span>
          <span className="font-utility text-meta text-canvas/45 uppercase">{mimeLabel(mime)}</span>
        </div>
      );
    }
    // A document has no picture to show, so the card carries what the wall would otherwise lose:
    // the name. Everything else stays quiet.
    return (
      <div className="flex aspect-[4/3] w-full flex-col items-center justify-center gap-2 px-4 text-center">
        <Icon className="size-7 text-muted/70" strokeWidth={1.5} />
        <span className="max-w-full truncate text-caption text-ink" title={name}>
          {name}
        </span>
        <span className="font-utility text-micro tracking-eyebrow text-muted uppercase">{mimeLabel(mime)}</span>
      </div>
    );
  }

  if (!url) {
    return (
      <div className={cn("flex items-center justify-center", mode === "wall" && "aspect-[4/3] w-full")}>
        <Loader2 className="size-4 animate-spin text-muted" />
      </div>
    );
  }
  return (
    <img
      src={url}
      alt=""
      loading="lazy"
      className={cn(
        "block object-cover",
        mode === "wall" && "h-auto w-full",
        mode === "thumb" && "size-full",
        mode === "full" && "max-h-full w-auto object-contain",
      )}
    />
  );
}

/** Short badge text for non-image tiles (`application/pdf` is just noise at 9px). */
function mimeLabel(mime: string): string {
  if (mime.includes("pdf")) return "PDF";
  const subtype = mime.split("/")[1] ?? mime;
  return subtype.split(".").pop()?.slice(0, 5).toUpperCase() ?? "FILE";
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Modified, in the reference's own voice: `Today` / `Yesterday` for the recent past, then a short
 * local date. The year only appears when it is not the current one — a library is mostly this
 * year's files, and repeating the year on every row is noise. A date, not a clock: "when did I add
 * this" is a day-granularity question.
 */
function formatModified(ms: number): string {
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "—";
  const now = new Date();
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (ms >= startOfToday) return "Today";
  if (ms >= startOfToday - 86_400_000) return "Yesterday";
  return date.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(date.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
  });
}
