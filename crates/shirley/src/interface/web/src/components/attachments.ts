import type { LibraryEntry, RenderPart } from "../api/types";
import { compressImage } from "./compress";

/** Attachment model shared by the composer, the preview tray and the chat view. */
export interface Attachment {
  id: string;
  file: File;
  name: string;
  kind: "image" | "pdf" | "file";
  url: string;
  size: string;
}

/** Names a browser hands out for clipboard images; they say nothing to the reader. */
const GENERIC_NAMES = new Set(["", "image.png", "image.jpg", "image.jpeg", "blob", "clipboard"]);

export function kindOf(file: File): Attachment["kind"] {
  if (file.type.startsWith("image/")) return "image";
  if (file.type === "application/pdf" || file.name.toLowerCase().endsWith(".pdf")) return "pdf";
  return "file";
}

export function formatSize(bytes: number): string {
  return bytes < 1024 * 1024
    ? `${Math.ceil(bytes / 1024)} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function extensionOf(type: string): string {
  const subtype = (type.split("/")[1] ?? "").split("+")[0]?.toLowerCase() ?? "";
  if (subtype === "jpeg") return "jpg";
  return subtype || "png";
}

/** A screenshot pasted from the clipboard has no usable filename; give it one. */
function renamed(file: File, index: number, stamp: string): File {
  if (!GENERIC_NAMES.has(file.name.toLowerCase())) return file;
  const type = file.type || "image/png";
  const name = `粘贴图片-${stamp}-${index + 1}.${extensionOf(type)}`;
  return new File([file], name, { type, lastModified: file.lastModified || Date.now() });
}

function stampOf(now: number): string {
  const at = new Date(now);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(at.getHours())}${pad(at.getMinutes())}${pad(at.getSeconds())}`;
}

export function filesToAttachments(files: File[]): Attachment[] {
  return files.map((file) => ({
    id: `${file.name}-${file.lastModified}-${Math.random().toString(36).slice(2)}`,
    file,
    name: file.name,
    kind: kindOf(file),
    url: URL.createObjectURL(file),
    size: formatSize(file.size),
  }));
}

export function toAttachments(files: FileList | null): Attachment[] {
  return files ? filesToAttachments(Array.from(files)) : [];
}

/**
 * Files carried by a paste event, in clipboard order. Empty when the clipboard holds text
 * only, which is what tells the composer to leave a normal text paste alone.
 */
export function pastedFiles(data: DataTransfer | null, now = Date.now()): File[] {
  if (!data) return [];
  const stamp = stampOf(now);
  const picked: File[] = [];
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind !== "file") continue;
    const file = item.getAsFile();
    // A pasted screenshot can arrive with size 0 while the browser still decodes it later.
    if (file && !file.type.startsWith("text/")) picked.push(file);
  }
  // Older Safari exposes pasted images through `files` only.
  if (!picked.length) {
    for (const file of Array.from(data.files ?? [])) {
      if (!file.type.startsWith("text/")) picked.push(file);
    }
  }
  return picked.map((file, index) => renamed(file, index, stamp));
}

/** Parts for the optimistic user bubble, so a pasted image shows before the round trip. */
export function userMessageParts(attachments: Attachment[]): RenderPart[] {
  return attachments.map((attachment) =>
    attachment.kind === "image"
      ? {
          kind: "image" as const,
          source: { kind: "url" as const, url: attachment.url },
          alt: attachment.name,
          mime: attachment.file.type || null,
        }
      : {
          kind: "file" as const,
          source: null,
          path: "",
          name: attachment.name,
          mime: attachment.file.type || null,
          bytes: attachment.file.size,
        },
  );
}

/**
 * Upload every attachment to `bot`'s media library, pictures shrunk to WebP first (`prepare`).
 * Failures are returned by name, not thrown.
 */
export async function uploadAll(
  attachments: Attachment[],
  upload: (file: File) => Promise<LibraryEntry>,
  prepare: (file: File) => Promise<File> = compressImage,
): Promise<{ entries: LibraryEntry[]; failed: string[] }> {
  const entries: LibraryEntry[] = [];
  const failed: string[] = [];
  for (const attachment of attachments) {
    try {
      entries.push(await upload(await prepare(attachment.file)));
    } catch {
      failed.push(attachment.name);
    }
  }
  return { entries, failed };
}

/**
 * A task brief that carries its attachments as library references, in the same one-line form a
 * chat turn uses (`[name · mime · size · id=…]`), so the bot can open them with its `media` tool.
 */
export function briefWithAttachments(brief: string, entries: LibraryEntry[]): string {
  if (!entries.length) return brief;
  const lines = entries.map(
    (entry) => `[${entry.name} · ${entry.mime} · ${formatSize(entry.bytes)} · id=${entry.id}]`,
  );
  return `${brief}\n\n${lines.join("\n")}`;
}
