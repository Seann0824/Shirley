import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { agentBridge, type AsrCapabilities } from "@/lib/bridge";
import {
  MAX_UTTERANCE_MS,
  speechRecognitionAvailable,
  speechRecognitionCtor,
  startWavCapture,
  type SpeechRecognitionLike,
  type VoiceEngine,
  type WavCapture,
} from "@/lib/voice";

export type VoiceState = "idle" | "listening" | "transcribing";

export type VoiceInput = {
  /** Something on this device can transcribe; otherwise the button hides itself. */
  available: boolean;
  state: VoiceState;
  /** Live partial transcript. Only browser dictation streams one. */
  interim: string;
  /** One short line for the composer (permission denied, transcriber missing, ...). */
  error: string;
  /** Start, or stop-and-commit. */
  toggle: () => void;
};

/**
 * Dictation state for the composer.
 *
 * **Recording and transcription both happen on the user's own device.** Two paths, one
 * lifecycle: `web` is the browser's own recogniser and streams partial results; `local` is
 * this computer's `whisper.cpp`, reached through a Tauri command on the same machine. There is
 * no upload path and no engine picker -- a phone that opens a cloud project dictates with the
 * phone's recogniser, a desktop app with the computer's whisper. See `docs/voice-input.md`.
 *
 * The transcript is delivered through `onText` (the caller appends it to its own input state),
 * so this hook never owns the composer's text.
 */
export function useVoiceInput(onText: (text: string) => void): VoiceInput {
  const [capabilities, setCapabilities] = useState<AsrCapabilities | null>(null);
  /** Whether the probe of this machine has answered (either way) -- see `candidates`. */
  const [probed, setProbed] = useState(false);
  const [state, setState] = useState<VoiceState>("idle");
  const [interim, setInterim] = useState("");
  const [error, setError] = useState("");

  const onTextRef = useRef(onText);
  onTextRef.current = onText;
  const recognitionRef = useRef<SpeechRecognitionLike | null>(null);
  const captureRef = useRef<WavCapture | null>(null);
  const timerRef = useRef<number | null>(null);
  /** True while the user wants the microphone on (a recogniser stopping on its own is not a stop). */
  const listeningRef = useRef(false);

  // Device capabilities, read once per mount: installing whisper.cpp takes effect on the next
  // start, the same latency the model picker has. Not keyed on the project -- the transcriber
  // belongs to the machine, not to whichever runtime a session happens to live in.
  useEffect(() => {
    let disposed = false;
    void agentBridge()
      .then((bridge) => bridge.asrCapabilities())
      .then((value) => {
        if (!disposed) {
          setCapabilities(value);
          setProbed(true);
        }
      })
      .catch(() => {
        if (!disposed) {
          setCapabilities(null);
          setProbed(true);
        }
      });
    return () => {
      disposed = true;
    };
  }, []);

  /**
   * Which transcriber this device will use.
   *
   * **Capability decides, never the environment.** The machine's own transcriber comes first
   * because it is the one we can be certain about: it either exists or it does not, and it does
   * not depend on a service somebody else runs. The browser's recogniser is only used when
   * there is no local one -- on a phone, that is the only option there is.
   *
   * Asking "am I the desktop shell?" would have been a proxy for this question, and a wrong
   * one: a webview can ship the recogniser API with no dictation service behind it, and then it
   * looks available while producing nothing at all.
   */
  const candidates = useMemo<VoiceEngine[]>(() => {
    // Until the probe answers, "no local transcriber" and "not asked yet" look identical.
    // Waiting one local round trip beats opening a microphone nothing is listening to.
    if (!probed) return [];
    if (capabilities?.local_available === true) return ["local"];
    return speechRecognitionAvailable() ? ["web"] : [];
  }, [capabilities, probed]);

  const engine = candidates[0] ?? null;
  const engineRef = useRef<VoiceEngine | null>(engine);
  engineRef.current = engine;

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  /** Timer for "the web recogniser never said anything at all". */
  const watchdogRef = useRef<number | null>(null);
  const clearWatchdog = useCallback(() => {
    if (watchdogRef.current !== null) {
      window.clearTimeout(watchdogRef.current);
      watchdogRef.current = null;
    }
  }, []);

  const armCap = useCallback(
    (stop: () => void) => {
      clearTimer();
      timerRef.current = window.setTimeout(stop, MAX_UTTERANCE_MS);
    },
    [clearTimer],
  );

  /** Stop the microphone and, for the local path, transcribe what was captured on this machine. */
  const stopListening = useCallback(() => {
    clearTimer();
    clearWatchdog();
    const recognition = recognitionRef.current;
    recognitionRef.current = null;
    listeningRef.current = false;
    // `stop()` (not `abort()`) so the recogniser still reports the trailing words.
    recognition?.stop();
    setInterim("");

    const capture = captureRef.current;
    captureRef.current = null;
    if (!capture) {
      setState("idle");
      return;
    }
    setState("transcribing");
    void (async () => {
      try {
        const audio = await capture.stop();
        const target = engineRef.current;
        if (target !== "local") return;
        const bridge = await agentBridge();
        // No project and no engine: the audio is transcribed by this computer's whisper.
        const result = await bridge.transcribe(audio.base64, preferredLanguage());
        const text = result.text.trim();
        if (text) {
          onTextRef.current(text);
          setError("");
        }
      } catch (cause) {
        setError(shortMessage(cause));
      } finally {
        setState("idle");
      }
    })();
  }, [clearTimer, clearWatchdog]);

  const stopListeningRef = useRef(stopListening);
  stopListeningRef.current = stopListening;

  /**
   * Give up on the web recogniser when it produces **nothing at all**.
   *
   * Its failure mode in a webview without a dictation service is silence: `start()` does not
   * throw, `onerror` never fires, `onend` never fires. No error handler can catch that, so the
   * only detector is a timer. Saying so beats a red button that pulses forever.
   */
  const armWatchdog = useCallback(
    (recognition: SpeechRecognitionLike) => {
      clearWatchdog();
      watchdogRef.current = window.setTimeout(() => {
        watchdogRef.current = null;
        if (!listeningRef.current) return;
        try {
          recognition.abort();
        } catch {
          // Already gone.
        }
        recognitionRef.current = null;
        listeningRef.current = false;
        setInterim("");
        setState("idle");
        setError("Voice input is unavailable in this window; type instead.");
      }, WEB_SILENCE_MS);
    },
    [clearWatchdog],
  );

  const startWeb = useCallback(() => {
    const Ctor = speechRecognitionCtor();
    if (!Ctor) {
      setError("This window has no speech recognition.");
      return;
    }
    const recognition = new Ctor();
    recognition.lang = preferredLanguage();
    // Continuous + interim: a long dictation should not stop at every pause, and partial text
    // is the only feedback the user gets while speaking.
    recognition.continuous = true;
    recognition.interimResults = true;
    recognition.maxAlternatives = 1;
    recognition.onaudiostart = () => clearWatchdog();
    recognition.onresult = (event) => {
      clearWatchdog();
      let settled = "";
      let partial = "";
      for (let index = event.resultIndex; index < event.results.length; index += 1) {
        const result = event.results[index];
        const transcript = result?.[0]?.transcript ?? "";
        if (result?.isFinal) settled += transcript;
        else partial += transcript;
      }
      const text = settled.trim();
      if (text) onTextRef.current(text);
      setInterim(partial.trim());
    };
    recognition.onerror = (event) => {
      clearWatchdog();
      if (event.error === "no-speech" || event.error === "aborted") return;
      listeningRef.current = false;
      recognitionRef.current = null;
      clearTimer();
      setInterim("");
      setState("idle");
      setError(describeSpeechError(event.error));
    };
    recognition.onend = () => {
      clearWatchdog();
      setInterim("");
      if (!listeningRef.current) return;
      // Chrome ends the session after a silence; restart so one dictation keeps going.
      try {
        recognition.start();
      } catch {
        listeningRef.current = false;
        setState("idle");
      }
    };
    recognitionRef.current = recognition;
    listeningRef.current = true;
    setError("");
    setState("listening");
    try {
      recognition.start();
    } catch {
      listeningRef.current = false;
      recognitionRef.current = null;
      setState("idle");
      setError("Speech recognition could not start.");
      return;
    }
    armCap(() => stopListeningRef.current());
    armWatchdog(recognition);
  }, [armCap, armWatchdog, clearTimer, clearWatchdog]);

  const startLocal = useCallback(() => {
    void (async () => {
      try {
        const capture = await startWavCapture();
        captureRef.current = capture;
        listeningRef.current = true;
        setError("");
        setState("listening");
        armCap(() => stopListeningRef.current());
      } catch (cause) {
        listeningRef.current = false;
        setState("idle");
        setError(describeMicrophoneError(cause));
      }
    })();
  }, [armCap]);

  const toggle = useCallback(() => {
    if (state === "transcribing") return;
    if (state === "listening") {
      stopListening();
      return;
    }
    if (!engine) {
      setError("Voice input isn't available here.");
      return;
    }
    if (engine === "web") startWeb();
    else startLocal();
  }, [engine, startLocal, startWeb, state, stopListening]);

  // Never leave the microphone open after the composer unmounts.
  useEffect(
    () => () => {
      clearTimer();
      listeningRef.current = false;
      recognitionRef.current?.abort();
      recognitionRef.current = null;
      captureRef.current?.cancel();
      captureRef.current = null;
    },
    [clearTimer, clearWatchdog],
  );

  return {
    available: candidates.length > 0,
    state,
    interim,
    error,
    toggle,
  };
}

/**
 * How long the web recogniser may stay completely silent before it is presumed dead.
 *
 * A working recogniser fires `audiostart` within a few hundred milliseconds of `start()`; eight
 * seconds of nothing is a dead service, not a user who is thinking.
 */
const WEB_SILENCE_MS = 8000;


/**
 * Dictation language.
 *
 * BCP-47 (`en-US`) is what the browser recogniser wants; the local model strips the region
 * itself (`whisper_language` in `infra/asr.rs`), because whisper.cpp only knows base codes and
 * an unknown one makes it print its usage and exit 0.
 */
function preferredLanguage(): string {
  return navigator.language || "en-US";
}

function describeSpeechError(code: string): string {
  switch (code) {
    case "not-allowed":
    case "service-not-allowed":
      return "Microphone permission denied.";
    case "audio-capture":
      return "No microphone found.";
    case "network":
      return "Speech service unreachable.";
    case "language-not-supported":
      return "This language is not supported.";
    default:
      return "Speech recognition failed.";
  }
}

function describeMicrophoneError(cause: unknown): string {
  const name = cause instanceof DOMException ? cause.name : "";
  // A denial is the OS holding the app back (on macOS: TCC has no `NSMicrophoneUsageDescription`
  // or the user said no), so point at where it is fixed instead of just naming the symptom.
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "Microphone permission denied — allow it in System Settings › Privacy & Security › Microphone.";
  }
  if (name === "NotFoundError") return "No microphone found.";
  if (name === "NotReadableError") return "The microphone is in use by another app.";
  // `navigator.mediaDevices` itself missing (a webview without a secure context) surfaces as a
  // TypeError, which must not be reported as a permission problem.
  if (cause instanceof TypeError) return "This window cannot reach the microphone.";
  return shortMessage(cause);
}

/** Backends already send short, user-facing text; keep it to one line either way. */
function shortMessage(cause: unknown): string {
  const raw = cause instanceof Error ? cause.message : String(cause);
  const line = raw.split("\n")[0]?.trim() ?? "";
  return line || "Dictation failed.";
}
