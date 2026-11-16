import { useEffect, useRef, useState, type ReactNode } from "react";

import { Icon } from "./Icon";

export interface Attachment {
  id: string;
  file: File;
  name: string;
  kind: "image" | "pdf" | "file";
  url: string;
  size: string;
}

function toAttachments(files: FileList | null): Attachment[] {
  if (!files) return [];
  return Array.from(files).map((file) => ({
    id: `${file.name}-${file.lastModified}-${Math.random()}`,
    file,
    name: file.name,
    kind: file.type.startsWith("image/")
      ? "image"
      : file.type === "application/pdf" || file.name.toLowerCase().endsWith(".pdf")
        ? "pdf"
        : "file",
    url: URL.createObjectURL(file),
    size:
      file.size < 1024 * 1024
        ? `${Math.ceil(file.size / 1024)} KB`
        : `${(file.size / 1024 / 1024).toFixed(1)} MB`,
  }));
}

function AttachmentTray({
  attachments,
  onRemove,
}: {
  attachments: Attachment[];
  onRemove: (id: string) => void;
}) {
  return (
    <div className="attachment-tray">
      {attachments.map((attachment) => (
        <div className={`attachment-preview ${attachment.kind}`} key={attachment.id}>
          {attachment.kind === "image" ? (
            <img src={attachment.url} alt="" />
          ) : (
            <span>
              <Icon name={attachment.kind === "pdf" ? "file" : "paperclip"} size={18} />
              <b>{attachment.kind === "pdf" ? "PDF" : "FILE"}</b>
            </span>
          )}
          <div>
            <strong>{attachment.name}</strong>
            <small>{attachment.size}</small>
          </div>
          <button onClick={() => onRemove(attachment.id)} aria-label={`移除 ${attachment.name}`}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

function AttachmentSheet({
  onClose,
  onFiles,
}: {
  onClose: () => void;
  onFiles: (files: FileList | null) => void;
}) {
  const pick = (files: FileList | null) => {
    onFiles(files);
    onClose();
  };
  const options: { label: string; icon: "image" | "camera" | "folder" | "file"; accept: string; capture?: "environment" }[] = [
    { label: "照片", icon: "image", accept: "image/*" },
    { label: "拍摄", icon: "camera", accept: "image/*", capture: "environment" },
    { label: "文件", icon: "folder", accept: ".pdf,.txt,.md,.doc,.docx,.xls,.xlsx,.ppt,.pptx,.csv,.json" },
    { label: "扫描", icon: "file", accept: "image/*,.pdf", capture: "environment" },
  ];
  return (
    <div className="attachment-sheet-layer" role="dialog" aria-modal="true" aria-label="添加内容">
      <button className="sheet-backdrop" onClick={onClose} aria-label="关闭" />
      <div className="attachment-sheet">
        <div className="sheet-handle" />
        <div className="attachment-options">
          {options.map((option) => (
            <label key={option.label}>
              <span>
                <Icon name={option.icon} size={22} />
              </span>
              <strong>{option.label}</strong>
              <input
                type="file"
                accept={option.accept}
                capture={option.capture}
                multiple={!option.capture}
                onChange={(event) => pick(event.target.files)}
              />
            </label>
          ))}
        </div>
        <button className="sheet-cancel" onClick={onClose}>
          取消
        </button>
      </div>
    </div>
  );
}

// Browser speech recognition (Chrome / Safari). Typed loosely: lib.dom has no declaration.
type Recognizer = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  onresult: ((event: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => void) | null;
  onend: (() => void) | null;
  onerror: (() => void) | null;
  start(): void;
  stop(): void;
};

function recognizer(): Recognizer | null {
  const ctor = (window as unknown as { SpeechRecognition?: new () => Recognizer; webkitSpeechRecognition?: new () => Recognizer });
  const Ctor = ctor.SpeechRecognition ?? ctor.webkitSpeechRecognition;
  if (!Ctor) return null;
  const instance = new Ctor();
  instance.lang = navigator.language || "zh-CN";
  instance.continuous = true;
  instance.interimResults = true;
  return instance;
}

/**
 * The input dock: hold-to-talk by default, a text field on demand, attachments via a sheet.
 * `onSend` receives the text and the attached files; the caller decides what to do with them.
 */
export function ComposerDock({
  onSend,
  onListeningChange,
  busy,
  onStop,
  placeholder = "输入消息",
  above,
}: {
  onSend: (text: string, attachments: Attachment[]) => void;
  onListeningChange?: (listening: boolean, transcript: string) => void;
  busy?: boolean;
  onStop?: () => void;
  placeholder?: string;
  /** Rendered inside the dock, above the controls (e.g. the "send to" picker). */
  above?: ReactNode;
}) {
  // A keyboard is the natural input on a wide screen; hold-to-talk is the phone default.
  const prefersText = window.matchMedia("(min-width: 900px)").matches;
  const [textMode, setTextMode] = useState(prefersText);
  const [value, setValue] = useState("");
  const [listening, setListening] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [showSheet, setShowSheet] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const speech = useRef<Recognizer | null>(null);
  const transcript = useRef("");

  useEffect(() => () => speech.current?.stop(), []);

  const send = (text: string) => {
    const trimmed = text.trim();
    if (!trimmed && !attachments.length) return;
    onSend(trimmed, attachments);
    setValue("");
    setAttachments([]);
    setTextMode(prefersText);
  };

  const startListening = () => {
    const instance = recognizer();
    if (!instance) {
      setVoiceError("此浏览器不支持语音识别，请用文字输入");
      setTextMode(true);
      return;
    }
    transcript.current = "";
    instance.onresult = (event) => {
      let text = "";
      for (let index = 0; index < event.results.length; index += 1) {
        text += event.results[index][0]?.transcript ?? "";
      }
      transcript.current = text;
      onListeningChange?.(true, text);
    };
    instance.onerror = () => setVoiceError("没有听清，请再试一次");
    instance.onend = () => {
      setListening(false);
      onListeningChange?.(false, transcript.current);
      if (transcript.current.trim()) send(transcript.current);
      speech.current = null;
    };
    speech.current = instance;
    setVoiceError(null);
    setListening(true);
    onListeningChange?.(true, "");
    instance.start();
  };

  const stopListening = () => {
    speech.current?.stop();
  };

  return (
    <>
      <div className={`composer-dock ${listening ? "is-listening" : ""}`}>
        {above}
        {attachments.length > 0 && (
          <AttachmentTray
            attachments={attachments}
            onRemove={(id) => setAttachments((items) => items.filter((item) => item.id !== id))}
          />
        )}
        {voiceError && <p className="composer-hint">{voiceError}</p>}
        <div className="composer-controls">
          {busy && onStop ? (
            <button className="composer-stop" onClick={onStop}>
              <Icon name="stop" size={18} />
              <strong>停止</strong>
            </button>
          ) : textMode ? (
            <form
              className="unified-text-composer"
              onSubmit={(event) => {
                event.preventDefault();
                send(value);
              }}
            >
              <button type="button" onClick={() => setTextMode(false)} aria-label="切换语音">
                <Icon name="mic" size={20} />
              </button>
              <input
                value={value}
                onChange={(event) => setValue(event.target.value)}
                placeholder={attachments.length ? "添加说明…" : placeholder}
                autoFocus
              />
              <button
                type="submit"
                className="unified-send"
                disabled={!value.trim() && !attachments.length}
                aria-label="发送"
              >
                <Icon name="arrow" size={18} />
              </button>
            </form>
          ) : (
            <>
              <button className="composer-side" onClick={() => setShowSheet(true)} aria-label="添加附件">
                <Icon name="plus" size={20} />
              </button>
              <button
                className="composer-voice"
                onPointerDown={startListening}
                onPointerUp={stopListening}
                onPointerCancel={stopListening}
                onPointerLeave={() => listening && stopListening()}
                aria-label="按住说话"
              >
                <span className="button-wave" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                  <i />
                  <i />
                </span>
                <strong>{listening ? "松开发送" : "按住说话"}</strong>
              </button>
              <button className="composer-side" onClick={() => setTextMode(true)} aria-label="文字输入">
                <Icon name="keyboard" size={20} />
              </button>
            </>
          )}
        </div>
      </div>
      {showSheet && (
        <AttachmentSheet
          onClose={() => setShowSheet(false)}
          onFiles={(files) => setAttachments((items) => [...items, ...toAttachments(files)])}
        />
      )}
    </>
  );
}

/** Read an image file as a `data:` URL (what the server's media library ingests). */
export function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}
