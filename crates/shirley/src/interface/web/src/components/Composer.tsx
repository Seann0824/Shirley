import { useEffect, useRef, useState, type ClipboardEvent, type ReactNode } from "react";

import { filesToAttachments, pastedFiles, toAttachments, type Attachment } from "./attachments";
import { Icon } from "./Icon";

export type { Attachment } from "./attachments";

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
  onerror: ((event: { error?: string }) => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
};

// The language to recognise. Deliberately not `navigator.language`: an English system with a
// Chinese-speaking user is common, and the recogniser then transcribes Chinese as English.
const VOICE_LANGS = [
  { code: "zh-CN", label: "中文" },
  { code: "en-US", label: "English" },
] as const;
const VOICE_LANG_KEY = "shirley.voiceLang";

function loadVoiceLang(): string {
  const saved = localStorage.getItem(VOICE_LANG_KEY);
  return VOICE_LANGS.some((lang) => lang.code === saved) ? (saved as string) : "zh-CN";
}

function recognizer(lang: string): Recognizer | null {
  const ctor = (window as unknown as { SpeechRecognition?: new () => Recognizer; webkitSpeechRecognition?: new () => Recognizer });
  const Ctor = ctor.SpeechRecognition ?? ctor.webkitSpeechRecognition;
  if (!Ctor) return null;
  const instance = new Ctor();
  instance.lang = lang;
  instance.continuous = true;
  instance.interimResults = true;
  return instance;
}

/** What went wrong, in words the user can act on. `fatal` = voice cannot work here right now. */
function describeSpeechError(code: string | undefined): { text: string; fatal: boolean } | null {
  switch (code) {
    case "aborted":
      return null;
    case "no-speech":
      return { text: "没有听到声音，请按住后再说", fatal: false };
    case "not-allowed":
      return {
        text: "麦克风权限被拒绝。请在浏览器地址栏允许麦克风，并在「系统设置 › 隐私与安全性 › 麦克风」里允许此浏览器",
        fatal: true,
      };
    case "service-not-allowed":
      return {
        text: "系统的语音识别未开启。Safari 需要在「系统设置 › 键盘 › 听写」里打开听写",
        fatal: true,
      };
    case "audio-capture":
      return { text: "找不到麦克风，请检查输入设备", fatal: true };
    case "network":
      return { text: "连不上语音识别服务（Chrome 使用在线识别），请改用文字或换 Safari", fatal: true };
    case "language-not-supported":
      return { text: "当前语言不支持语音识别", fatal: true };
    default:
      return { text: `语音识别出错（${code ?? "unknown"}）`, fatal: false };
  }
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
  claimsPagePaste = false,
}: {
  onSend: (text: string, attachments: Attachment[]) => void;
  onListeningChange?: (listening: boolean, transcript: string) => void;
  busy?: boolean;
  onStop?: () => void;
  placeholder?: string;
  /** Rendered inside the dock, above the controls (e.g. the "send to" picker). */
  above?: ReactNode;
  /**
   * Take pastes that land on the page rather than in a text field (in hold-to-talk mode nothing
   * in the dock has focus). Only one dock on screen should claim them.
   */
  claimsPagePaste?: boolean;
}) {
  // A keyboard is the natural input on a wide screen; hold-to-talk is the phone default.
  const prefersText = window.matchMedia("(min-width: 900px)").matches;
  const [textMode, setTextMode] = useState(prefersText);
  const [value, setValue] = useState("");
  const [listening, setListening] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [showSheet, setShowSheet] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const [voiceLang, setVoiceLang] = useState(loadVoiceLang);
  const [liveText, setLiveText] = useState("");
  const speech = useRef<Recognizer | null>(null);
  const transcript = useRef("");

  useEffect(() => () => speech.current?.abort(), []);

  // The page keeps room for the dock below its last line; the dock grows with the tray, the
  // live transcript and the pills above it, so the room follows its real height.
  const dockRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const dock = dockRef.current;
    const pane = dock?.closest<HTMLElement>(".pane");
    if (!dock || !pane) return;
    const observer = new ResizeObserver(() => {
      pane.style.setProperty("--dock-height", `${dock.offsetHeight}px`);
    });
    observer.observe(dock);
    return () => observer.disconnect();
  }, []);

  const send = (text: string) => {
    const trimmed = text.trim();
    if (!trimmed && !attachments.length) return;
    onSend(trimmed, attachments);
    setValue("");
    setAttachments([]);
    setTextMode(prefersText);
  };

  // Screenshots arrive through the clipboard, not the file picker: turn them into the same
  // attachments the tray already renders, and let plain text pastes through untouched.
  const takePaste = (event: { clipboardData: DataTransfer | null; preventDefault(): void }) => {
    const files = pastedFiles(event.clipboardData);
    if (!files.length) return false;
    event.preventDefault();
    setAttachments((items) => [...items, ...filesToAttachments(files)]);
    setVoiceError(null);
    return true;
  };
  const onPaste = (event: ClipboardEvent<HTMLDivElement>) => {
    takePaste(event);
  };
  const takePasteRef = useRef(takePaste);
  takePasteRef.current = takePaste;

  useEffect(() => {
    if (!claimsPagePaste) return;
    const onPagePaste = (event: globalThis.ClipboardEvent) => {
      // Already taken by a dock, or meant for some other text field on the page.
      if (event.defaultPrevented) return;
      const focused = document.activeElement;
      if (
        focused instanceof HTMLElement &&
        (focused.isContentEditable || focused.tagName === "INPUT" || focused.tagName === "TEXTAREA")
      ) {
        return;
      }
      if (takePasteRef.current(event)) setTextMode(true);
    };
    document.addEventListener("paste", onPagePaste);
    return () => document.removeEventListener("paste", onPagePaste);
  }, [claimsPagePaste]);

  const startListening = () => {
    if (speech.current) return;
    const instance = recognizer(voiceLang);
    if (!instance) {
      setVoiceError("此浏览器不支持语音识别（可用 Chrome、Edge 或 Safari），请用文字输入");
      setTextMode(true);
      return;
    }
    transcript.current = "";
    let failed = false;
    instance.onresult = (event) => {
      let text = "";
      for (let index = 0; index < event.results.length; index += 1) {
        text += event.results[index][0]?.transcript ?? "";
      }
      transcript.current = text;
      setLiveText(text);
      onListeningChange?.(true, text);
    };
    instance.onerror = (event) => {
      const problem = describeSpeechError(event.error);
      if (!problem) return;
      failed = true;
      setVoiceError(problem.text);
      if (problem.fatal) setTextMode(true);
    };
    instance.onend = () => {
      // The button and the window both see the release, so `stop` can end a session twice.
      if (speech.current !== instance) return;
      speech.current = null;
      setListening(false);
      onListeningChange?.(false, transcript.current);
      setLiveText("");
      // Never send what the recogniser heard unseen: it goes into the text field to check first.
      const heard = transcript.current.trim();
      if (heard) {
        setValue((current) => (current.trim() ? `${current.trim()} ${heard}` : heard));
        setTextMode(true);
      } else if (!failed) {
        setVoiceError("没有识别到内容。按住按钮，说完再松开");
      }
    };
    speech.current = instance;
    setVoiceError(null);
    try {
      instance.start();
    } catch {
      speech.current = null;
      setVoiceError("语音识别没有启动，请再试一次");
      return;
    }
    setListening(true);
    onListeningChange?.(true, "");
  };

  const stopListening = () => {
    speech.current?.stop();
  };

  // The release can land outside the page (a permission prompt, another window): never stay
  // stuck in "listening".
  useEffect(() => {
    if (!listening) return;
    const release = () => speech.current?.stop();
    window.addEventListener("pointerup", release);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("blur", release);
    };
  }, [listening]);

  return (
    <>
      <div ref={dockRef} className={`composer-dock ${listening ? "is-listening" : ""}`} onPaste={onPaste}>
        {above}
        {attachments.length > 0 && (
          <AttachmentTray
            attachments={attachments}
            onRemove={(id) => setAttachments((items) => items.filter((item) => item.id !== id))}
          />
        )}
        {listening && (
          <p className={`voice-live ${liveText ? "" : "waiting"}`} aria-live="polite">
            {liveText || "正在听…"}
          </p>
        )}
        {voiceError && <p className="composer-hint">{voiceError}</p>}
        {!textMode && !busy && !listening && (
          <div className="voice-lang" role="radiogroup" aria-label="语音识别语言">
            {VOICE_LANGS.map((lang) => (
              <button
                key={lang.code}
                role="radio"
                aria-checked={voiceLang === lang.code}
                className={voiceLang === lang.code ? "active" : ""}
                onClick={() => {
                  setVoiceLang(lang.code);
                  localStorage.setItem(VOICE_LANG_KEY, lang.code);
                }}
              >
                {lang.label}
              </button>
            ))}
          </div>
        )}
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
                onPointerDown={(event) => {
                  // Keep receiving the release even if the pointer drifts off the button.
                  event.currentTarget.setPointerCapture(event.pointerId);
                  startListening();
                }}
                onPointerUp={stopListening}
                onPointerCancel={stopListening}
                onContextMenu={(event) => event.preventDefault()}
                aria-label="按住说话"
              >
                <span className="button-wave" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                  <i />
                  <i />
                </span>
                <strong>{listening ? "松开结束" : "按住说话"}</strong>
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
