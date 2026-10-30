import { File, ImageIcon, LoaderCircle, X } from "lucide-react";
import { ImageBlock } from "./ImageBlock";
import { RenderParts, useAssetUrl } from "./RenderParts";
import type { AiTranscriptMessage } from "./AiConversationTranscript";
import { CHIP_TOKEN } from "@/lib/file-mentions/editor-dom";
import type { FileReference } from "@/lib/file-mentions/types";
import type { AssetSourceWire, RenderPartWire } from "@/types/wire";

const isAttachment = (reference: FileReference) =>
  reference.kind === "image" || reference.kind === "media";

/** The runtime appends a media inventory for the model; display it as attachments. */
export function restoreUserMessage(content: string, parts: RenderPartWire[] = []) {
  const entries: { name: string; mime: string; id: string }[] = [];
  let text = content.trimEnd();
  while (true) {
    const match = /(?:^|\n)\[([^\n]*) · ([^\s]+\/[^\s]+) · [^\n]* · id=([a-fA-F0-9]{32}\.[a-zA-Z0-9]+)\]$/.exec(text);
    if (!match) break;
    entries.unshift({ name: match[1], mime: match[2], id: match[3] });
    text = text.slice(0, match.index).trimEnd();
  }
  if (entries.length === 0) return { content, parts };
  const media = parts.filter((part) => part.kind !== "text");
  for (const entry of entries) {
    const index = media.findIndex((part) =>
      (part.kind === "image" || part.kind === "file") &&
      part.source?.kind === "artifact" && part.source.id === entry.id);
    const part: RenderPartWire = entry.mime.startsWith("image/")
      ? { kind: "image", source: { kind: "artifact", id: entry.id }, alt: entry.name, mime: entry.mime }
      : { kind: "file", source: { kind: "artifact", id: entry.id }, name: entry.name, mime: entry.mime, bytes: null };
    if (index >= 0) media[index] = part;
    else media.push(part);
  }
  return { content: text, parts: media };
}

/** Older messages embed attachment placeholders among workspace references. */
export function splitMessageReferences(content: string, references: FileReference[]) {
  const inline: FileReference[] = [];
  let index = 0;
  const text = content.split(CHIP_TOKEN).map((part, partIndex) => {
    if (partIndex === 0) return part;
    const reference = references[index++];
    if (!reference || isAttachment(reference)) return part;
    inline.push(reference);
    return CHIP_TOKEN + part;
  }).join("");
  return { content: text, references: inline };
}

/** History contains render parts even when the transient composer references are gone. */
export function attachmentsFromMessage(message: AiTranscriptMessage): FileReference[] {
  const saved = (message.references ?? []).filter(isAttachment);
  if (saved.length > 0) return saved;
  return (message.parts ?? []).flatMap((part): FileReference[] => {
    if (part.kind !== "image" && part.kind !== "file") return [];
    if (!part.source) return [];
    const name = part.kind === "image" ? part.alt || "Image" : part.name;
    return [{
      path: part.source.kind === "artifact" ? part.source.id : part.source.url,
      name,
      kind: "media",
      artifactId: part.source.kind === "artifact" ? part.source.id : undefined,
      dataUrl: part.source.kind === "url" ? part.source.url : undefined,
      mime: part.mime ?? (part.kind === "image" ? "image/*" : undefined),
      bytes: part.kind === "file" ? part.bytes ?? undefined : undefined,
    }];
  });
}

function AttachmentPreview({ reference }: { reference: FileReference }) {
  const source: AssetSourceWire = reference.artifactId
    ? { kind: "artifact", id: reference.artifactId }
    : { kind: "url", url: reference.dataUrl ?? reference.path };
  const url = useAssetUrl(source);
  const image = reference.kind === "image" || reference.mime?.startsWith("image/");
  if (image && url) {
    return <ImageBlock src={url} alt={reference.name} thumbnail className="m-0" imageClassName="size-20" />;
  }
  const Icon = image ? ImageIcon : File;
  return (
    <span title={reference.name} className="flex size-20 flex-col items-center justify-center gap-1 rounded-control border border-line bg-inset px-2 text-muted">
      <Icon className="size-5 shrink-0" />
      <span className="w-full truncate text-center text-caption">{reference.name}</span>
    </span>
  );
}

export function ComposerAttachments({ attachments, uploading, onRemove }: {
  attachments: FileReference[];
  uploading: number;
  onRemove: (index: number) => void;
}) {
  if (attachments.length === 0 && uploading === 0) return null;
  return (
    <div className="order-0 flex max-h-48 flex-wrap gap-2 overflow-y-auto p-1.5" aria-label="Attachments">
      {attachments.map((reference, index) => (
        <div key={`${reference.path}:${index}`} className="relative shrink-0">
          <AttachmentPreview reference={reference} />
          <button
            type="button"
            aria-label={`Remove attachment: ${reference.name}`}
            title={`Remove ${reference.name}`}
            onClick={() => onRemove(index)}
            className="absolute -top-1 -right-1 flex size-5 items-center justify-center rounded-full border border-line bg-surface text-ink shadow-sm hover:bg-selected focus-visible:ring-2 focus-visible:ring-ink/35"
          >
            <X className="size-3" />
          </button>
        </div>
      ))}
      {uploading > 0 && (
        <div role="status" className="flex min-h-20 items-center gap-2 px-2 text-caption text-muted">
          <LoaderCircle className="size-4 animate-spin" /> Uploading {uploading}…
        </div>
      )}
    </div>
  );
}

function MessageImage({ part }: { part: Extract<RenderPartWire, { kind: "image" }> }) {
  const url = useAssetUrl(part.source);
  return url
    ? <ImageBlock src={url} alt={part.alt || "Image"} thumbnail className="m-0" />
    : <span className="flex size-24 items-center justify-center rounded-control border border-line bg-inset p-2 text-caption text-muted"><span className="truncate">{part.alt || "Image unavailable"}</span></span>;
}

export function UserMessageAttachments({ parts = [] }: { parts?: RenderPartWire[] }) {
  const images = parts.filter((part) => part.kind === "image");
  const other = parts.filter((part) => part.kind !== "image" && part.kind !== "text");
  if (images.length === 0 && other.length === 0) return null;
  return (
    <div className="flex max-w-full flex-col items-end gap-2" data-user-attachments>
      {images.length > 0 && (
        <div className="flex max-w-full flex-wrap justify-end gap-2">
          {images.map((part, index) => <MessageImage key={index} part={part} />)}
        </div>
      )}
      <RenderParts parts={other} className="max-w-full space-y-2" />
    </div>
  );
}
