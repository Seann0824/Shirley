import { useEffect, useRef, useState, type ClipboardEvent, type ReactNode } from "react";

import { api } from "../api/bridge";
import { askNotificationPermission } from "../state/notify";
import { filesToAttachments, pastedFiles, toAttachments, type Attachment } from "./attachments";
import { Icon } from "./Icon";
import {
  PERMISSION_HINT,
  microphoneError,
  microphoneWillAsk,
  previewInterval,
  startRecording,
  type Recording,
} from "./recorder";

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

// Fallback when the server has no whisper.cpp. The browser recogniser cannot detect the
// language; it must be told one. Chinese, not `navigator.language`: the UI is Chinese, and an
// English system with a Chinese-speaking user would otherwise transcribe Chinese as English.
const VOICE_LANG = "zh-CN";

function recognizer(): Recognizer | null {
  const ctor = (window as unknown as { SpeechRecognition?: new () => Recognizer; webkitSpeechRecognition?: new () => Recognizer });
  const Ctor = ctor.SpeechRecognition ?? ctor.webkitSpeechRecognition;
  if (!Ctor) return null;
  const instance = new Ctor();
  instance.lang = VOICE_LANG;
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

/** How far (px) to slide up from the hold-to-talk button to send on release. */
const SLIDE_TO_SEND = 60;

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
  draft,
  claimsPagePaste = false,
}: {
  onSend: (text: string, attachments: Attachment[]) => void;
  onListeningChange?: (listening: boolean, transcript: string) => void;
  busy?: boolean;
  onStop?: () => void;
  placeholder?: string;
  /** Rendered inside the dock, above the controls (e.g. the "send to" picker). */
  above?: ReactNode;
  /** Text to put back in the field (a recalled message); a new `nonce` puts it again. */
  draft?: { text: string; nonce: number };
  /**
   * Take pastes that land on the page rather than in a text field (in hold-to-talk mode nothing
   * in the dock has focus). Only one dock on screen should claim them.
   */
  claimsPagePaste?: boolean;
}) {
  // While the bot replies the input stays open: sending interrupts the reply, and with nothing
  // typed the send button stops it.
  const canStop = Boolean(busy && onStop);
  // A keyboard is the natural input on a wide screen; hold-to-talk is the phone default.
  const prefersText = window.matchMedia("(min-width: 900px)").matches;
  const [textMode, setTextMode] = useState(prefersText);
  const [value, setValue] = useState("");
  const [listening, setListening] = useState(false);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [showSheet, setShowSheet] = useState(false);
  const [voiceError, setVoiceError] = useState<string | null>(null);
  const [liveText, setLiveText] = useState("");
  const [transcribing, setTranscribing] = useState(false);
  const speech = useRef<Recognizer | null>(null);
  const transcript = useRef("");
  // Server transcription (preferred): one recording per press, previews while it grows.
  const [serverAsr, setServerAsr] = useState(false);
  const recording = useRef<Recording | null>(null);
  const pressed = useRef(false);
  const utterance = useRef(0);
  const language = useRef<string | null>(null);
  const previewTimer = useRef<number | null>(null);
  // Hold-to-talk gesture: slide up past the threshold to send on release instead of editing.
  const pressY = useRef(0);
  const slideSend = useRef(false);
  const [sliding, setSliding] = useState(false);

  useEffect(() => {
    let alive = true;
    void api.asrStatus().then((status) => alive && setServerAsr(status.available));
    return () => {
      alive = false;
    };
  }, []);

  useEffect(
    () => () => {
      speech.current?.abort();
      recording.current?.cancel();
      if (previewTimer.current !== null) window.clearInterval(previewTimer.current);
    },
    [],
  );

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
    // The first message is the natural moment to ask: "tell me when the reply is ready".
    askNotificationPermission();
    onSend(trimmed, attachments);
    setValue("");
    setAttachments([]);
    setTextMode(prefersText);
  };
  // A recalled message comes back here to be edited.
  useEffect(() => {
    if (!draft) return;
    setValue(draft.text);
    setTextMode(true);
  }, [draft]);

  // The transcript lands after an await; these see the latest field and attachments.
  const sendRef = useRef(send);
  sendRef.current = send;
  const valueRef = useRef(value);
  valueRef.current = value;

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

  // Released in place, heard text goes into the field to be checked; slid up first, it is sent.
  const placeHeard = (heard: string) => {
    const text = heard.trim();
    if (!text) return false;
    const typed = valueRef.current.trim();
    const combined = typed ? `${typed} ${text}` : text;
    if (slideSend.current) {
      slideSend.current = false;
      setSliding(false);
      sendRef.current(combined);
      return true;
    }
    setValue(combined);
    setTextMode(true);
    return true;
  };

  const startServerListening = async () => {
    if (recording.current || transcribing) return;
    const id = (utterance.current += 1);
    pressed.current = true;
    language.current = null;
    setVoiceError(null);
    setLiveText("");
    setListening(true);
    if (await microphoneWillAsk()) setLiveText(PERMISSION_HINT);
    let rec: Recording;
    try {
      rec = await startRecording();
    } catch (cause) {
      setListening(false);
      setVoiceError(microphoneError(cause));
      return;
    }
    // Released while the microphone was still opening (or a permission prompt was answered).
    if (!pressed.current || utterance.current !== id) {
      rec.cancel();
      setListening(false);
      return;
    }
    recording.current = rec;
    setLiveText((text) => (text === PERMISSION_HINT ? "" : text));
    let inFlight = false;
    let lastSent = performance.now();
    previewTimer.current = window.setInterval(() => {
      const seconds = rec.seconds();
      if (inFlight || seconds < 0.8 || performance.now() - lastSent < previewInterval(seconds)) return;
      inFlight = true;
      lastSent = performance.now();
      api
        .transcribe(rec.snapshot(), { partial: true, language: language.current })
        .then((result) => {
          if (utterance.current !== id || recording.current !== rec) return;
          if (result.language) language.current = result.language;
          if (result.text !== null) setLiveText(result.text);
        })
        .catch(() => undefined)
        .finally(() => {
          inFlight = false;
        });
    }, 200);
  };

  const stopServerListening = async () => {
    pressed.current = false;
    const rec = recording.current;
    if (!rec) return;
    recording.current = null;
    if (previewTimer.current !== null) window.clearInterval(previewTimer.current);
    previewTimer.current = null;
    const id = utterance.current;
    const tooShort = rec.seconds() < 0.3;
    const wav = rec.stop();
    setListening(false);
    if (tooShort) {
      setLiveText("");
      setVoiceError("按住按钮，说完再松开");
      return;
    }
    setTranscribing(true);
    try {
      const result = await api.transcribe(wav, { language: language.current });
      if (utterance.current !== id) return;
      if (!placeHeard(result.text ?? "")) setVoiceError("没有识别到内容，请再说一次");
    } catch (cause) {
      if (utterance.current === id) {
        setVoiceError(cause instanceof Error ? cause.message : "语音转写失败，请再试一次");
      }
    } finally {
      if (utterance.current === id) {
        setTranscribing(false);
        setLiveText("");
      }
    }
  };

  const startListening = () => {
    if (serverAsr) {
      void startServerListening();
      return;
    }
    if (speech.current) return;
    const instance = recognizer();
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
      if (!placeHeard(transcript.current) && !failed) {
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
    if (serverAsr) {
      void stopServerListening();
      return;
    }
    speech.current?.stop();
  };
  const stopRef = useRef(stopListening);
  stopRef.current = stopListening;

  // The release can land outside the page (a permission prompt, another window): never stay
  // stuck in "listening".
  useEffect(() => {
    if (!listening) return;
    const release = () => stopRef.current();
    window.addEventListener("pointerup", release);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("blur", release);
    };
  }, [listening]);

  return (
    <>
      <div ref={dockRef} className={`composer-dock ${listening ? "is-listening" : ""} ${listening && sliding ? "is-sliding" : ""}`} onPaste={onPaste}>
        {above}
        {attachments.length > 0 && (
          <AttachmentTray
            attachments={attachments}
            onRemove={(id) => setAttachments((items) => items.filter((item) => item.id !== id))}
          />
        )}
        {(listening || transcribing) && (
          <p className={`voice-live ${liveText ? "" : "waiting"}`} aria-live="polite">
            {liveText || (transcribing ? "正在整理…" : "正在听…")}
            {transcribing && liveText && <span className="voice-live-tail"> · 整理中</span>}
          </p>
        )}
        {voiceError && <p className="composer-hint">{voiceError}</p>}
        <div className="composer-controls">
          {textMode ? (
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
              {canStop && !value.trim() && !attachments.length ? (
                <button type="button" className="unified-send stop" onClick={onStop} aria-label="停止">
                  <Icon name="stop" size={16} />
                </button>
              ) : (
                <button
                  type="submit"
                  className="unified-send"
                  disabled={!value.trim() && !attachments.length}
                  aria-label="发送"
                >
                  <Icon name="arrow" size={18} />
                </button>
              )}
            </form>
          ) : (
            <>
              {canStop && (
                <button className="composer-side stop" onClick={onStop} aria-label="停止">
                  <Icon name="stop" size={16} />
                </button>
              )}
              <button className="composer-side" onClick={() => setShowSheet(true)} aria-label="添加附件">
                <Icon name="plus" size={20} />
              </button>
              <button
                className="composer-voice"
                onPointerDown={(event) => {
                  // Keep receiving the release even if the pointer drifts off the button.
                  event.currentTarget.setPointerCapture(event.pointerId);
                  pressY.current = event.clientY;
                  slideSend.current = false;
                  setSliding(false);
                  startListening();
                }}
                onPointerMove={(event) => {
                  if (!listening) return;
                  const up = pressY.current - event.clientY > SLIDE_TO_SEND;
                  if (up !== slideSend.current) {
                    slideSend.current = up;
                    setSliding(up);
                  }
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
                <strong>
                  {listening ? (sliding ? "松开发送" : "松开编辑 · 上滑发送") : transcribing ? "整理中…" : "按住说话"}
                </strong>
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
