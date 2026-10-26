import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { FileText, Music, X } from "lucide-react";
import { agentBridge } from "@/lib/bridge";
import type { ResourceRefWire } from "@/types/wire";

/**
 * Renderer registry for content-addressed artifacts.
 *
 * A tool result may carry `ResourceRef`s (see `ToolOutcomeWire`); this component is the
 * single place that turns a reference into a visual. Dispatch is by **MIME type**, not by
 * the tool name, so any tool (built-in or plugin) that emits `image/png` gets the same
 * image renderer for free — that is the "dogfooding" contract of the media channel.
 *
 * Bytes are fetched lazily through `bridge.artifactUrl` (a `data:` URL on desktop, the
 * server `/api/artifact/{sha256}` URL in the cloud build). If the fetch fails, or the MIME
 * is unknown, we fall back to a compact file card (never crash, never block the turn).
 */
export function ResourcePreview({ resource }: { resource: ResourceRefWire }) {
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const kind = classify(resource.mime);

  useEffect(() => {
    let alive = true;
    agentBridge()
      .then((bridge) => bridge.artifactUrl(resource.artifact))
      .then((resolved) => { if (alive) setUrl(resolved); })
      .catch(() => { if (alive) setFailed(true); });
    return () => { alive = false; };
  }, [resource.artifact]);

  if (failed) return <FallbackCard resource={resource} note="Could not load this file." />;
  if (!url) return <FallbackCard resource={resource} note="Loading..." />;

  switch (kind) {
    case "image":
      return <ImageResource url={url} resource={resource} />;
    case "video":
      return (
        <video
          src={url}
          controls
          preload="metadata"
          className="max-h-96 max-w-full rounded-control border border-line bg-black"
        />
      );
    case "audio":
      return (
        <div className="flex max-w-md items-center gap-3 rounded-control border border-line bg-inset p-3">
          <Music size={16} className="shrink-0 text-muted" aria-hidden="true" />
          <audio src={url} controls preload="metadata" className="min-w-0 flex-1" />
        </div>
      );
    case "pdf":
      return (
        <a
          href={url}
          target="_blank"
          rel="noreferrer"
          className="flex max-w-md items-center gap-3 rounded-control border border-line bg-inset p-3 text-caption text-ink hover:bg-surface"
        >
          <FileText size={16} className="shrink-0 text-muted" aria-hidden="true" />
          <span className="min-w-0 flex-1 truncate">{resource.alt || "Document (PDF)"}</span>
          <span className="shrink-0 text-muted">Open</span>
        </a>
      );
    default:
      return <FallbackCard resource={resource} note={resource.alt || resource.mime} href={url} />;
  }
}

type ResourceKind = "image" | "video" | "audio" | "pdf" | "other";

function classify(mime: string): ResourceKind {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio";
  if (mime === "application/pdf") return "pdf";
  return "other";
}

/** Image thumbnail that opens a full-screen lightbox on click (Esc / backdrop closes). */
function ImageResource({ url, resource }: { url: string; resource: ResourceRefWire }) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setOpen(false); };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  const dimensions =
    resource.meta?.kind === "image" ? `${resource.meta.width}×${resource.meta.height}` : undefined;

  return (
    <div className="flex flex-col gap-1" data-image-preview>
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="w-fit cursor-zoom-in overflow-hidden rounded-control border border-line"
        aria-label={resource.alt || "Open image preview"}
      >
        <img
          src={url}
          alt={resource.alt || ""}
          loading="lazy"
          className="max-h-64 max-w-full object-contain"
        />
      </button>
      {(resource.alt || dimensions) && (
        <span className="text-caption text-muted">{resource.alt || dimensions}</span>
      )}
      {open &&
        createPortal(
          <div
            role="dialog"
            aria-modal="true"
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
            onClick={() => setOpen(false)}
          >
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="absolute right-4 top-4 rounded-full bg-white/10 p-2 text-white hover:bg-white/20"
              aria-label="Close preview"
            >
              <X size={18} aria-hidden="true" />
            </button>
            <img
              src={url}
              alt={resource.alt || ""}
              className="max-h-full max-w-full object-contain"
              onClick={(event) => event.stopPropagation()}
            />
          </div>,
          document.body,
        )}
    </div>
  );
}

/** Compact fallback for unknown MIME / load failure / still-loading. */
function FallbackCard({
  resource,
  note,
  href,
}: {
  resource: ResourceRefWire;
  note: string;
  href?: string;
}) {
  const label = resource.alt || resource.mime || "file";
  const content = (
    <>
      <FileText size={16} className="shrink-0 text-muted" aria-hidden="true" />
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="shrink-0 text-muted">{formatBytes(resource.bytes)}</span>
    </>
  );
  const className =
    "flex max-w-md items-center gap-3 rounded-control border border-line bg-inset p-3 text-caption text-ink";
  return href ? (
    <a href={href} target="_blank" rel="noreferrer" className={`${className} hover:bg-surface`} title={note}>
      {content}
    </a>
  ) : (
    <div className={className} title={note}>{content}</div>
  );
}

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 10 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}

/**
 * Parse a raw `AgentEventWire.output` string into structured resources.
 *
 * The runtime serializes a tool's `ToolOutcome` (when it returns one) as JSON; a plain
 * text result stays a bare string / non-object. This helper extracts the `resource` parts
 * and the model-facing `text`, so the tool card can render both the rich preview and the
 * textual summary without losing either.
 */
export function parseToolOutcome(raw: string | null | undefined): {
  resources: ResourceRefWire[];
  text: string | null;
} {
  if (!raw) return { resources: [], text: null };
  try {
    const parsed: unknown = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const outcome = parsed as { parts?: unknown; text?: unknown };
      const parts = Array.isArray(outcome.parts) ? outcome.parts : [];
      const resources = parts.filter(
        (part): part is ResourceRefWire =>
          Boolean(part) && typeof part === "object" && (part as { type?: unknown }).type === "resource",
      );
      const text = typeof outcome.text === "string" ? outcome.text : null;
      if (resources.length > 0) return { resources, text };
      // A ToolOutcome with only text parts still surfaces its text.
      if (text !== null) return { resources: [], text };
    }
    // A bare JSON string (plain-text tool result) or other JSON: render its text form.
    return { resources: [], text: typeof parsed === "string" ? parsed : JSON.stringify(parsed, null, 2) };
  } catch {
    return { resources: [], text: raw };
  }
}
