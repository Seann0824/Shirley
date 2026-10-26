/**
 * The render-part dispatcher — the "opening" from `docs/render-parts.md`.
 *
 * One switch on `part.kind`, nothing else. A new medium is a new `kind` here plus the Rust-side
 * variant; no existing renderer, no tool card and no session code changes. Two rules keep it safe:
 *
 * 1. **Data only.** A part can never carry markup or script. `data` parts are rendered by a
 *    component looked up in {@link DATA_RENDERERS}, which is populated **locally** — a remote tool
 *    naming a schema is not a rendering grant, it only gets the JSON disclosure.
 * 2. **Always degrade.** Every part has a text fallback, so an unknown kind or a missing asset
 *    shows something readable instead of a blank box (this is what the TUI does with the same
 *    contract, and what the session log / model request see).
 */
import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ComponentType,
  type ReactNode,
} from "react";
import { AiMarkdown } from "@/components/ai/AiMarkdown";
import { ImageBlock } from "@/components/ai/ImageBlock";
import { agentBridge } from "@/lib/bridge";
import type { AssetSourceWire, RenderPartWire } from "@/types/wire";

/** Payload handed to a `data` renderer. */
export type DataRendererProps = { value: unknown };

/**
 * Locally declared `schema -> component` table.
 *
 * Registering a schema is an explicit act of the app (a built-in feature, a skill, a plugin
 * shipped with the build) — never something a tool result can do at runtime.
 */
export const DATA_RENDERERS: Record<string, ComponentType<DataRendererProps>> = {};

/** Register a `data` renderer (kept for built-ins / skills; unknown schemas degrade to JSON). */
export function registerDataRenderer(
  schema: string,
  component: ComponentType<DataRendererProps>,
): void {
  DATA_RENDERERS[schema] = component;
}

/** What a `file` renderer receives. `url` is `null` when the artifact could not be resolved. */
export type FileRendererProps = {
  url: string | null;
  name: string;
  mime: string | null;
  bytes: number | null;
  /** The original source, for renderers that must fetch the artifact as text (interactive HTML). */
  source?: AssetSourceWire | null;
};

type FileRendererEntry = {
  /** The mimes this entry claims; the first match wins. Data field, never a remote instruction. */
  match: (mime: string) => boolean;
  component: ComponentType<FileRendererProps>;
};

/**
 * Locally declared `mime -> component` table for non-picture files.
 *
 * This is the second half of the "one channel, mime decides the renderer" rule: **a new medium
 * is a new entry here**, with no change to the Rust contract, the wire, the session log or the
 * tool cards. Order is significant (first match wins), and unknown mimes fall through to the
 * generic download card, so a producer from a tool, a skill or an MCP server never causes a blank
 * box -- it just shows the file.
 */
export const FILE_RENDERERS: FileRendererEntry[] = [];

/** Add a `file` renderer. Appended, so a built-in claim for a broad mime keeps priority. */
export function registerFileRenderer(entry: FileRendererEntry): void {
  FILE_RENDERERS.push(entry);
}

/**
 * Resolve an artifact id to a loadable URL.
 *
 * The id is project-scoped, so the resolver is scoped too: {@link AssetUrlProvider} holds the
 * project and a cache, and every part below just asks. `null` means "cannot resolve right now"
 * (mock bridge, expired handle) — the caller degrades to text instead of rendering a broken image.
 */
type AssetResolver = (id: string) => Promise<string | null>;

const AssetContext = createContext<AssetResolver | null>(null);

export function AssetUrlProvider({
  project,
  children,
}: {
  project: string | null;
  children: ReactNode;
}) {
  const resolver = useMemo<AssetResolver>(() => {
    const cache = new Map<string, string>();
    return async (id) => {
      if (!project) return null;
      const cached = cache.get(id);
      if (cached) return cached;
      try {
        const url = await (await agentBridge()).assetUrl(project, id);
        if (!url) return null;
        cache.set(id, url);
        return url;
      } catch {
        // A forged/expired artifact id is a normal miss, not an error worth showing.
        return null;
      }
    };
  }, [project]);
  const textResolver = useMemo<AssetTextResolver>(() => {
    const cache = new Map<string, string>();
    return async (id) => {
      if (!project) return null;
      const cached = cache.get(id);
      if (cached !== undefined) return cached;
      try {
        const result = await (await agentBridge()).assetText(project, id);
        cache.set(id, result.text);
        return result.text;
      } catch {
        // Not textual, expired handle, mock bridge: degrade to the file card.
        return null;
      }
    };
  }, [project]);
  return (
    <AssetContext.Provider value={resolver}>
      <AssetTextContext.Provider value={textResolver}>{children}</AssetTextContext.Provider>
    </AssetContext.Provider>
  );
}

type AssetTextResolver = (id: string) => Promise<string | null>;

const AssetTextContext = createContext<AssetTextResolver | null>(null);

/**
 * Fetch an artifact as **text**, so an HTML artifact can be injected with `srcDoc`.
 *
 * This is the interaction half of the channel: a URL would run the markup on the page's own origin,
 * while `srcDoc` + `sandbox` puts it on an opaque one. Only `artifact` sources qualify -- a remote
 * `url` is not ours to execute.
 */
function useAssetText(source: AssetSourceWire | null | undefined): string | null {
  const resolve = useContext(AssetTextContext);
  const id = source?.kind === "artifact" ? source.id : null;
  const [text, setText] = useState<string | null>(null);

  useEffect(() => {
    if (!id || !resolve) {
      setText(null);
      return;
    }
    let cancelled = false;
    void resolve(id).then((value) => {
      if (!cancelled) setText(value);
    });
    return () => {
      cancelled = true;
    };
  }, [id, resolve]);

  return text;
}

/** Direct `url` sources render as-is; `artifact` ids go through the out-of-band channel. */
function useAssetUrl(source: AssetSourceWire): string | null {
  const resolve = useContext(AssetContext);
  const direct = source.kind === "url" ? source.url : null;
  const id = source.kind === "artifact" ? source.id : null;
  const [resolved, setResolved] = useState<string | null>(null);

  useEffect(() => {
    if (!id || !resolve) {
      setResolved(null);
      return;
    }
    let cancelled = false;
    void resolve(id).then((url) => {
      if (!cancelled) setResolved(url);
    });
    return () => {
      cancelled = true;
    };
  }, [id, resolve]);

  return direct ?? resolved;
}

function FallbackText({ text }: { text: string }) {
  return <p className="text-caption text-muted">{text}</p>;
}

function RenderPartView({ part }: { part: RenderPartWire }) {
  switch (part.kind) {
    case "text":
      // Built-in: the one kind every interface understands, rendered exactly like before.
      return <AiMarkdown content={part.text} streaming={false} />;
    case "image":
      return <ImagePart part={part} />;
    case "file":
      return <FilePart part={part} />;
    case "link":
      return (
        <p className="my-1 text-caption">
          <a
            href={part.url}
            target="_blank"
            rel="noreferrer noopener"
            className="text-accent-ink underline decoration-dotted"
          >
            {part.title || part.url}
          </a>
        </p>
      );
    case "data": {
      const Renderer = DATA_RENDERERS[part.schema];
      if (Renderer) return <Renderer value={part.value} />;
      // Unregistered schema: show the payload, do not try to interpret it.
      return (
        <details className="my-1 text-caption">
          <summary className="cursor-pointer text-muted">{`data: ${part.schema}`}</summary>
          <pre className="mt-1 max-h-60 overflow-auto whitespace-pre-wrap break-words rounded-control bg-inset p-2 font-utility text-ink">
            {JSON.stringify(part.value, null, 2)}
          </pre>
        </details>
      );
    }
    default:
      // Unknown kind: an older build reading a newer payload. Silent, not fatal.
      return null;
  }
}

/**
 * An image render part. The picture is shown **on its own** (never inline in a sentence) and
 * clicking it opens an in-app preview; both live in {@link ImageBlock}, shared with markdown
 * images so every picture in the conversation behaves the same way.
 */
function ImagePart({
  part,
}: {
  part: Extract<RenderPartWire, { kind: "image" }>;
}) {
  const url = useAssetUrl(part.source);
  if (!url) return <FallbackText text={`[image: ${part.alt || part.mime || "image"}]`} />;
  return <ImageBlock src={url} alt={part.alt || part.mime || "image"} caption={part.alt || null} />;
}

/**
 * A non-picture file. The mime picks a renderer out of {@link FILE_RENDERERS}: PDF previews in a
 * frame, clips play, audio plays, office documents become an open/download card (no browser
 * renders those), anything else is a card too. Every branch shows the name and size, so the
 * fallback is never a blank box.
 */
function FilePart({ part }: { part: Extract<RenderPartWire, { kind: "file" }> }) {
  const url = part.source ? useAssetUrl(part.source) : null;
  const mime = part.mime?.split(";")[0]?.trim().toLowerCase() ?? "";
  const Renderer = FILE_RENDERERS.find((entry) => entry.match(mime))?.component ?? GenericFile;
  return (
    <div className="my-2">
      <Renderer
        url={url}
        name={part.name}
        mime={part.mime}
        bytes={part.bytes}
        source={part.source ?? null}
      />
    </div>
  );
}

/** The default: a file card. Whatever cannot be previewed can at least be seen and taken. */
function GenericFile({ url, name, mime, bytes, note }: FileRendererProps & { note?: string }) {
  return (
    <div className="flex items-center justify-between gap-3 rounded-control border border-line bg-inset px-3 py-2">
      <div className="min-w-0">
        <p className="truncate font-utility text-caption text-ink">{name}</p>
        <p className="text-caption text-muted">
          {[mime, bytes === null ? null : formatBytes(bytes), note ?? null]
            .filter(Boolean)
            .join(" · ")}
        </p>
      </div>
      {url ? (
        <a
          href={url}
          download={name}
          className="shrink-0 rounded-control border border-line px-2 py-1 text-caption text-accent-ink"
        >
          打开
        </a>
      ) : null}
    </div>
  );
}

/**
 * An interactive HTML artifact: a chart, a dashboard, a little tool the agent wrote for the user.
 *
 * It runs **for real** -- but on an opaque origin: `srcDoc` + `sandbox` without `allow-same-origin`
 * means the generated page cannot read the app's token, storage or Tauri IPC, and it never becomes
 * a same-origin document. That is what makes shipping executable output acceptable at all.
 */
function HtmlFile({ source, name, mime, bytes }: FileRendererProps) {
  const html = useAssetText(source);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  if (!html) {
    return <GenericFile url={null} name={name} mime={mime} bytes={bytes} note="预览加载中或不可用" />;
  }
  const frameClass = "w-full border-0 bg-white";
  return (
    <>
      <div className="overflow-hidden rounded-control border border-line">
        <iframe
          srcDoc={html}
          title={name}
          sandbox="allow-scripts allow-forms allow-modals allow-popups"
          className={`${frameClass} h-96`}
        />
      </div>
      <p className="mt-1 flex items-center justify-between gap-2 text-caption text-muted">
        <span>{[name, mime, bytes === null ? null : formatBytes(bytes)].filter(Boolean).join(" · ")}</span>
        <button type="button" onClick={() => setOpen(true)} className="shrink-0 text-accent-ink">
          放大
        </button>
      </p>
      {open ? (
        <div
          role="dialog"
          aria-modal="true"
          onClick={() => setOpen(false)}
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
        >
          <iframe
            srcDoc={html}
            title={name}
            sandbox="allow-scripts allow-forms allow-modals allow-popups"
            className={`${frameClass} h-full rounded-control shadow-2xl`}
          />
        </div>
      ) : null}
    </>
  );
}

/** Office documents: no browser renderer exists, and pretending otherwise wastes the user's time. */
function OfficeFile(props: FileRendererProps) {
  return <GenericFile {...props} note="需本地应用打开" />;
}

/** PDF: preview inline, click to read it full-screen. The bytes are served, not parsed by us. */
function PdfFile({ url, name, mime, bytes }: FileRendererProps) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);
  if (!url) return <GenericFile url={url} name={name} mime={mime} bytes={bytes} />;
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(true)}
        title="Open"
        className="block w-full overflow-hidden rounded-control border border-line"
      >
        <iframe src={url} title={name} className="pointer-events-none h-60 w-full bg-white" />
      </button>
      <p className="mt-1 text-caption text-muted">
        {[name, mime, bytes === null ? null : formatBytes(bytes)].filter(Boolean).join(" · ")}
      </p>
      {open ? (
        <div
          role="dialog"
          aria-modal="true"
          onClick={() => setOpen(false)}
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
        >
          <iframe
            src={url}
            title={name}
            className="h-full w-full rounded-control bg-white shadow-2xl"
          />
        </div>
      ) : null}
    </>
  );
}

/** Video / audio: use the element the browser already has, controls included. */
function VideoFile({ url, name, mime, bytes }: FileRendererProps) {
  if (!url) return <GenericFile url={url} name={name} mime={mime} bytes={bytes} />;
  return (
    <figure>
      <video src={url} controls className="max-h-80 w-full rounded-control border border-line" />
      <figcaption className="mt-1 text-caption text-muted">
        {[name, mime, bytes === null ? null : formatBytes(bytes)].filter(Boolean).join(" · ")}
      </figcaption>
    </figure>
  );
}

function AudioFile({ url, name, mime, bytes }: FileRendererProps) {
  if (!url) return <GenericFile url={url} name={name} mime={mime} bytes={bytes} />;
  return (
    <figure>
      <audio src={url} controls className="w-full" />
      <figcaption className="mt-1 text-caption text-muted">
        {[name, mime, bytes === null ? null : formatBytes(bytes)].filter(Boolean).join(" · ")}
      </figcaption>
    </figure>
  );
}

FILE_RENDERERS.push(
  { match: (mime) => mime === "text/html", component: HtmlFile },
  { match: (mime) => mime === "application/pdf", component: PdfFile },
  { match: (mime) => mime.startsWith("video/"), component: VideoFile },
  { match: (mime) => mime.startsWith("audio/"), component: AudioFile },
  {
    match: (mime) =>
      mime.includes("officedocument") ||
      mime.includes("msword") ||
      mime.includes("ms-excel") ||
      mime.includes("ms-powerpoint") ||
      mime.includes("opendocument"),
    component: OfficeFile,
  },
);

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function RenderParts({
  parts,
  className,
}: {
  parts: RenderPartWire[] | undefined;
  className?: string;
}) {
  if (!parts || parts.length === 0) return null;
  return (
    <div className={className ?? "space-y-1"}>
      {parts.map((part, index) => (
        <RenderPartView key={`${part.kind}-${index}`} part={part} />
      ))}
    </div>
  );
}
