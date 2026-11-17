import { useEffect, useState } from "react";

import { api } from "../api/bridge";
import { Icon } from "./Icon";

/** A file in a bot's library, to look at. */
export interface ViewedFile {
  bot: string;
  id: string;
  name: string;
  mime: string;
}

const OPEN_EVENT = "shirley:view-file";

/** Open `file` in the in-app viewer (never a new tab: the user stays in the conversation). */
export function openViewer(file: ViewedFile): void {
  window.dispatchEvent(new CustomEvent<ViewedFile>(OPEN_EVENT, { detail: file }));
}

function baseMime(mime: string): string {
  return mime.split(";")[0].trim().toLowerCase();
}

export function isHtml(mime: string): boolean {
  return baseMime(mime) === "text/html";
}

function isText(mime: string): boolean {
  const base = baseMime(mime);
  return base.startsWith("text/") || base === "application/json";
}

/** The page's source, for a sandboxed `srcdoc` frame. */
function useAssetText(bot: string, id: string) {
  const [state, setState] = useState<{ text: string | null; failed: boolean }>({ text: null, failed: false });
  useEffect(() => {
    let alive = true;
    api
      .assetText(bot, id)
      .then((value) => alive && setState({ text: value.text, failed: false }))
      .catch(() => alive && setState({ text: null, failed: true }));
    return () => {
      alive = false;
    };
  }, [bot, id]);
  return state;
}

function useAssetUrl(bot: string, id: string) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    api.assetUrl(bot, id).then((value) => alive && setUrl(value)).catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [bot, id]);
  return url;
}

/**
 * A web page the bot made. Sandboxed without same-origin access: its scripts run, but it cannot
 * reach Shirley (storage, API, the app around it). It arrives as text and goes in via `srcdoc`.
 */
function HtmlFrame({ file }: { file: ViewedFile }) {
  const { text, failed } = useAssetText(file.bot, file.id);
  if (failed) return <p className="viewer-note">打不开这个网页</p>;
  if (text === null) return <p className="viewer-note">正在打开 {file.name}…</p>;
  return <iframe title={file.name} srcDoc={text} sandbox="allow-scripts allow-popups allow-forms allow-modals" />;
}

/** A page shown in a message or a task card; 放大 opens it over the conversation. */
export function HtmlPreview({ file }: { file: ViewedFile }) {
  return (
    <figure className="html-preview">
      <HtmlFrame file={file} />
      <figcaption>
        <span>{file.name}</span>
        <button onClick={() => openViewer(file)} aria-label={`放大查看 ${file.name}`}>
          放大
        </button>
      </figcaption>
    </figure>
  );
}

function TextBody({ file }: { file: ViewedFile }) {
  const { text, failed } = useAssetText(file.bot, file.id);
  if (failed) return <p className="viewer-note">打不开这个文件</p>;
  return <pre className="viewer-text">{text ?? "正在打开…"}</pre>;
}

/** Whatever the file is, the best way to show it here. */
function ViewerBody({ file }: { file: ViewedFile }) {
  const url = useAssetUrl(file.bot, file.id);
  const mime = baseMime(file.mime);
  if (isHtml(mime)) return <HtmlFrame file={file} />;
  if (isText(mime)) return <TextBody file={file} />;
  if (!url) return <p className="viewer-note">正在打开…</p>;
  if (mime.startsWith("image/")) return <img className="viewer-image" src={url} alt={file.name} />;
  if (mime === "application/pdf") return <iframe title={file.name} src={url} />;
  if (mime.startsWith("video/")) return <video className="viewer-media" src={url} controls autoPlay />;
  if (mime.startsWith("audio/")) return <audio className="viewer-audio" src={url} controls autoPlay />;
  return (
    <div className="viewer-note">
      <p>这种文件没法在这里预览。</p>
      <a className="viewer-download" href={url} download={file.name}>
        下载 {file.name}
      </a>
    </div>
  );
}

/** The viewer, over the conversation. Mounted once by the app; opened with `openViewer`. */
export function FileViewer() {
  const [file, setFile] = useState<ViewedFile | null>(null);
  useEffect(() => {
    const onOpen = (event: Event) => setFile((event as CustomEvent<ViewedFile>).detail);
    window.addEventListener(OPEN_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_EVENT, onOpen);
  }, []);
  useEffect(() => {
    if (!file) return;
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && setFile(null);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [file]);
  if (!file) return null;
  return (
    <div className="file-viewer" role="dialog" aria-modal="true" aria-label={file.name}>
      <header>
        <button onClick={() => setFile(null)} aria-label="关闭">
          <Icon name="chevron" size={20} />
        </button>
        <strong>{file.name}</strong>
        <span />
      </header>
      <div className="file-viewer-body" key={file.id}>
        <ViewerBody file={file} />
      </div>
    </div>
  );
}
