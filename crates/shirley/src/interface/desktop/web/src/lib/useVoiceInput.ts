/**
 * Dictation, wired to this device.
 *
 * There is no state machine in this file on purpose. What can go wrong in dictation -- the
 * finger, the microphone, the recogniser and the transcriber all moving at once -- is decided in
 * `voice-machine.ts`, where it is testable and where every async arrival has to quote the
 * generation it belongs to. This hook is the other half: it turns browser and hardware callbacks
 * into machine events, performs the effects the machine asks for, and hands the words to the
 * composer. Read it as an adapter, not as logic.
 *
 * The one rule that matters here: **when a callback comes back, ask the machine whether that
 * generation is still alive before touching the microphone again.** `alive(gen)` is that
 * question. A recogniser that finishes opening after the finger is up must find nothing to do.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { agentBridge, type AsrCapabilities } from "@/lib/bridge";
import {
  MAX_UTTERANCE_MS,
  ensureMicPermission,
  micPermissionSettled,
  releaseMicSoon,
  speechRecognitionAvailable,
  speechRecognitionCtor,
  startWavCapture,
  type SpeechRecognitionLike,
  type VoiceEngine,
  type WavCapture,
} from "@/lib/voice";
import { RecognizerSlot } from "@/lib/recognizer";
import { composeUtterance } from "@/lib/voice-hold";
import {
  NO_DICTATION,
  NO_RESPONSE,
  START_REFUSED,
  idleVoice,
  voiceReduce,
  type TranscriberKind,
  type VoiceDelivery,
  type VoiceEffect,
  type VoiceEvent,
  type VoiceMachineState,
} from "@/lib/voice-machine";

export type { VoiceDelivery } from "@/lib/voice-machine";

/** The phases the composer renders. `opening` reads as "starting" to the user. */
export type VoiceState = "idle" | "starting" | "listening" | "transcribing";

export type VoiceInput = {
  /** False on a device with no transcriber at all: the button is not offered. */
  available: boolean;
  state: VoiceState;
  /** The utterance as far as it is known -- live words included. */
  heard: string;
  /** The still-changing tail, for the live line above the input row. */
  interim: string;
  /** One human sentence. Empty unless the last attempt failed. */
  error: string;
  /** The finger went down: always begins a new utterance. */
  begin: (delivery?: VoiceDelivery) => void;
  /** The finger came up: deliver what was heard, in this mode. */
  commit: (delivery?: VoiceDelivery) => void;
  cancel: () => void;
  /** Click-to-start / click-to-stop, for the button form (desktop). */
  toggle: () => void;
};

const VOICE_LANGUAGE_KEY = "shirley.voice.language";
const DEFAULT_VOICE_LANGUAGE = "zh-CN";
/**
 * How long a recogniser may take to actually start capturing.
 *
 * Short on purpose: `onaudiostart` fires as soon as capture begins, so a session that has not
 * reached it in two seconds is not "slow", it is dead -- typically a `start()` the browser
 * accepted outside a user gesture, which opens the audio session (the OS recording indicator
 * comes on) and then never delivers a word. Reporting that in two seconds is the difference
 * between "press again" and "the app is broken".
 */
const WEB_AUDIO_START_MS = 2_000;
/**
 * How long a local transcription may take before the utterance is declared lost. Whisper on a
 * laptop finishes a spoken sentence in a second or two; past this it is not slow, it is stuck.
 */
const FINISH_STALL_MS = 15_000;

type ActiveUtterance =
  /** The shared recogniser is working for this generation; it owns the session, not the arm. */
  | { gen: number; kind: "web" }
  | { gen: number; kind: "local"; capture: WavCapture };

/** The recogniser's result list, typed locally so a browser quirk cannot break the build. */
type TranscriptEvent = {
  results: {
    length: number;
    [index: number]: { isFinal: boolean; [alternative: number]: { transcript: string } | undefined };
  };
};

function preferredLanguage(): string {
  try {
    return localStorage.getItem(VOICE_LANGUAGE_KEY) ?? DEFAULT_VOICE_LANGUAGE;
  } catch {
    return DEFAULT_VOICE_LANGUAGE;
  }
}

/**
 * The whole utterance as this recogniser session knows it.
 *
 * Cumulative rather than a delta: with `continuous` on, the browser keeps every result, so
 * re-reading the list is idempotent -- which is what lets the machine treat `heard` as "this is
 * what was said", instead of accumulating deltas that arrive twice and vanish on restart.
 */
function readTranscript(event: TranscriptEvent): { settled: string; interim: string } {
  let settled = "";
  let interim = "";
  for (let index = 0; index < event.results.length; index += 1) {
    const result = event.results[index];
    const transcript = result?.[0]?.transcript ?? "";
    if (result?.isFinal) settled += transcript;
    else interim += transcript;
  }
  return { settled: settled.trim(), interim: interim.trim() };
}

/** One human sentence per failure: enough to decide whether to try again. */
function describeSpeechError(code: string): string {
  switch (code) {
    case "not-allowed":
    case "service-not-allowed":
      return "没有麦克风权限，所以听不到你说话。";
    case "audio-capture":
      return "找不到麦克风，换一个设备试试。";
    case "network":
      return "语音识别要联网，现在连不上。";
    case "language-not-supported":
      return "这个语言暂时不能转写。";
    default:
      return START_REFUSED;
  }
}

function describeMicrophoneError(cause: unknown): string {
  const name = cause instanceof Error ? cause.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "没有麦克风权限，所以听不到你说话。";
  if (name === "NotFoundError") return "找不到麦克风，换一个设备试试。";
  return START_REFUSED;
}

/** Phase -> what the composer shows. */
function viewOf(state: VoiceMachineState): { state: VoiceState; heard: string; interim: string; error: string } {
  return {
    state:
      state.phase === "idle"
        ? "idle"
        : state.phase === "opening"
          ? "starting"
          : state.phase === "listening"
            ? "listening"
            : "transcribing",
    heard: composeUtterance(state.settled, state.interim),
    interim: state.interim,
    error: state.error,
  };
}

export function useVoiceInput(onText: (text: string, delivery: VoiceDelivery) => void): VoiceInput {
  const [capabilities, setCapabilities] = useState<AsrCapabilities | null>(null);
  const [probed, setProbed] = useState(false);
  const [machine, setMachine] = useState<VoiceMachineState>(idleVoice);

  /** The live machine state. Read synchronously by callbacks; `machine` only drives rendering. */
  const stateRef = useRef<VoiceMachineState>(machine);
  const onTextRef = useRef(onText);
  onTextRef.current = onText;

  // Which transcriber this device has. Nothing is offered until the probe has answered.
  useEffect(() => {
    let cancelled = false;
    void agentBridge()
      .then((bridge) => bridge.asrCapabilities())
      .then((value: AsrCapabilities) => {
        if (cancelled) return;
        setCapabilities(value);
        setProbed(true);
      })
      .catch(() => {
        if (cancelled) return;
        setCapabilities(null);
        setProbed(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const candidates = useMemo<VoiceEngine[]>(() => {
    if (!probed) return [];
    const list: VoiceEngine[] = [];
    // The local engine transcribes on this machine; the browser one runs in the page.
    if (capabilities?.local_available) list.push("local");
    if (speechRecognitionAvailable()) list.push("web");
    return list;
  }, [capabilities, probed]);
  const engine: TranscriberKind | null = candidates[0] ?? null;
  const engineRef = useRef<TranscriberKind | null>(engine);
  engineRef.current = engine;

  /** The one utterance that owns the microphone right now, and which generation it is. */
  const activeRef = useRef<ActiveUtterance | null>(null);
  /** Text carried over from an earlier recogniser session of the same generation. */
  const carriedRef = useRef<{ gen: number; settled: string }>({ gen: -1, settled: "" });
  const capTimerRef = useRef<number | null>(null);
  const stallTimerRef = useRef<number | null>(null);
  const finishTimerRef = useRef<number | null>(null);
  /**
   * One recogniser for the life of the screen.
   *
   * It used to be one per press, which is what broke dictation after the first message: the new
   * recogniser was started while the previous one's audio session was still closing, and Safari
   * answers that with a lit microphone and no words until the old session finally goes away.
   * `RecognizerSlot` owns that rule (see `recognizer.ts`); this ref is only the object identity.
   */
  const recognitionRef = useRef<SpeechRecognitionLike | null>(null);
  const slotRef = useRef(new RecognizerSlot());
  /** The generation the shared recogniser works for. Its handlers read this, not a closure. */
  const genRef = useRef(0);
  /** Words already settled for the current generation, carried into a re-armed session. */
  const baseRef = useRef("");

  function alive(gen: number): boolean {
    return stateRef.current.gen === gen;
  }

  function clearTimers(): void {
    if (capTimerRef.current !== null) {
      window.clearTimeout(capTimerRef.current);
      capTimerRef.current = null;
    }
    if (stallTimerRef.current !== null) {
      window.clearTimeout(stallTimerRef.current);
      stallTimerRef.current = null;
    }
    if (finishTimerRef.current !== null) {
      window.clearTimeout(finishTimerRef.current);
      finishTimerRef.current = null;
    }
  }

  /**
   * Give generation `gen` back, if it is still the live one. Idempotent.
   *
   * `hard` is set when the machine has given up on the generation (a failure): the recogniser is
   * presumed dead, so the next press must be allowed to start fresh instead of queueing behind a
   * session that will never report its end.
   */
  function abortActive(gen: number, hard: boolean): void {
    const active = activeRef.current;
    if (!active || active.gen !== gen) return;
    activeRef.current = null;
    if (active.kind === "web") {
      // Both branches abort: an utterance that is over -- delivered, cancelled or abandoned --
      // has nothing left to collect, so ending it gracefully only means holding the audio session
      // open longer than the next press can wait. `hard` adds "and forget the books", for a
      // recogniser we have given up on.
      if (hard) slotRef.current.reset();
      else slotRef.current.abort();
    } else {
      active.capture.cancel();
    }
    releaseMicSoon();
  }

  function emit(event: VoiceEvent): void {
    const step = voiceReduce(stateRef.current, event, { engine: engineRef.current });
    stateRef.current = step.state;
    setMachine(step.state);
    // Which kind of ending the effect is comes from the event that caused it: a failure retires
    // a session we have given up on, everything else ends one we are done with.
    const hard = event.type === "failed";
    for (const effect of step.effects) apply(effect, hard);
  }

  function apply(effect: VoiceEffect, hard: boolean): void {
    switch (effect.type) {
      case "open":
        void openMicrophone(effect.gen);
        break;
      case "close":
        void transcribeLocal(effect.gen);
        break;
      case "abort":
        clearTimers();
        abortActive(effect.gen, hard);
        break;
      case "deliver":
        onTextRef.current(effect.text, effect.delivery);
        break;
    }
  }

  async function openMicrophone(gen: number): Promise<void> {
    if (engineRef.current === "web") await openWeb(gen);
    else if (engineRef.current === "local") await openLocal(gen);
    else emit({ type: "failed", gen, message: NO_DICTATION });
  }

  /**
   * Two seconds, spent once per attempt, is the whole budget for the operating system to hand
   * over audio. `onaudiostart` clears it; every further attempt re-arms it, so a session that
   * starts late is not reported dead -- but a session that never starts is.
   */
  function armAudioStart(gen: number): void {
    if (stallTimerRef.current !== null) window.clearTimeout(stallTimerRef.current);
    stallTimerRef.current = window.setTimeout(() => {
      stallTimerRef.current = null;
      if (alive(gen)) emitRef.current({ type: "failed", gen, message: NO_RESPONSE });
    }, WEB_AUDIO_START_MS);
  }

  /**
   * The recogniser closed a session. When the finger is still down there is exactly one thing to
   * do, and it is the whole answer to "dictation needs a few seconds after the first message":
   *
   * - a press arrived while the previous session was still closing. `RecognizerSlot` has it owed
   *   and starts it *itself* the moment the browser reports the end, so the wait is a few hundred
   *   milliseconds instead of however long the platform takes to let the audio session go;
   * - nothing is owed: either the tail of an utterance that is already over (nothing to do), or
   *   Chrome ending a session on its own timer while the finger is still down (continue it).
   */
  function onSessionEnd(): void {
    const gen = genRef.current;
    const restarted = slotRef.current.ended();
    if (restarted) {
      if (!alive(gen)) return;
      emitRef.current({ type: "opened", gen });
      armUtteranceCap(gen);
      armAudioStart(gen);
      return;
    }
    if (!alive(gen) || stateRef.current.phase !== "listening") return;
    if (slotRef.current.start() !== "started") {
      emitRef.current({ type: "failed", gen, message: START_REFUSED });
      return;
    }
    armAudioStart(gen);
  }

  async function openWeb(gen: number): Promise<void> {
    // Started from `pointerdown`, so this runs inside the user gesture -- and it must *stay*
    // inside it. Anything that awaits here pushes `recognition.start()` into a later task, and
    // iOS Safari accepts such a start without ever reporting a result: the microphone lights up
    // and the transcript stays empty. Hence the preflight only happens while the origin still
    // needs to be granted; afterwards this path is synchronous all the way to `start()`.
    if (!micPermissionSettled()) {
      try {
        // The permission conversation is the long pole, and the finger may well be gone by the
        // time it answers -- which is exactly the case that used to leave a recogniser running
        // with nobody holding the button.
        await ensureMicPermission();
      } catch (cause) {
        if (alive(gen)) emit({ type: "failed", gen, message: describeMicrophoneError(cause) });
        return;
      }
      if (!alive(gen)) return;
    }

    const Ctor = speechRecognitionCtor();
    if (!Ctor) {
      emit({ type: "failed", gen, message: NO_DICTATION });
      return;
    }

    // **One recogniser for the life of the screen**, built on the first press and reused after
    // that. Constructing a fresh one per press is what broke dictation for a few seconds after a
    // send: the previous instance's audio session is still closing, and a `start()` on a second
    // instance inside that window is either refused or -- worse -- accepted with a lit microphone
    // and no words. `RecognizerSlot` owns that rule; this only creates the object once.
    let recognition = recognitionRef.current;
    if (!recognition) {
      recognition = new Ctor();
      recognition.lang = preferredLanguage();
      recognition.continuous = true;
      recognition.interimResults = true;
      recognition.maxAlternatives = 1;
      // Bound once, so they read the live generation out of `genRef` rather than closing over
      // whichever one happened to be current when this object was created.
      recognition.onaudiostart = () => {
        if (stallTimerRef.current !== null) {
          window.clearTimeout(stallTimerRef.current);
          stallTimerRef.current = null;
        }
      };
      recognition.onresult = (event) => {
        const live = genRef.current;
        if (!alive(live)) return;
        const read = readTranscript(event as unknown as TranscriptEvent);
        const settled = composeUtterance(baseRef.current, read.settled);
        carriedRef.current = { gen: live, settled };
        emitRef.current({ type: "heard", gen: live, settled, interim: read.interim });
      };
      recognition.onerror = (event) => {
        const live = genRef.current;
        if (!alive(live)) return;
        const code = event.error ?? "";
        // Silence and our own aborts are not failures: one is retried, the other is the user.
        if (code === "no-speech" || code === "aborted") return;
        emitRef.current({ type: "failed", gen: live, message: describeSpeechError(code) });
      };
      recognition.onend = () => onSessionEnd();
      recognitionRef.current = recognition;
    }
    slotRef.current.attach(recognition);

    genRef.current = gen;
    baseRef.current = carriedRef.current.gen === gen ? carriedRef.current.settled : "";
    activeRef.current = { gen, kind: "web" };

    const outcome = slotRef.current.start();
    if (outcome === "refused") {
      emit({ type: "failed", gen, message: START_REFUSED });
      return;
    }
    if (outcome === "queued") {
      // The previous session is still closing; `onSessionEnd` opens this one the moment the
      // browser says it is done. The watchdog stays armed so a session that never ends is
      // reported instead of leaving the bar waiting forever.
      armAudioStart(gen);
      return;
    }
    emit({ type: "opened", gen });
    armUtteranceCap(gen);
    armAudioStart(gen);
  }

  async function openLocal(gen: number): Promise<void> {
    try {
      const capture = await startWavCapture();
      // Same rule as the permission path: capture that finishes after the finger is up is
      // cancelled rather than left recording.
      if (!alive(gen)) {
        capture.cancel();
        return;
      }
      activeRef.current = { gen, kind: "local", capture };
      emit({ type: "opened", gen });
      armUtteranceCap(gen);
    } catch (cause) {
      if (alive(gen)) emit({ type: "failed", gen, message: describeMicrophoneError(cause) });
    }
  }

  async function transcribeLocal(gen: number): Promise<void> {
    const active = activeRef.current;
    if (!active || active.gen !== gen || active.kind !== "local") return;
    activeRef.current = null;
    clearTimers();
    // The local engine is the one place where the finger is up and the text is still being made.
    // It is also the one place with no partial results to fall back on, so a wedged transcription
    // is answered by the machine's own `expired` rather than by an indefinite `finishing`.
    finishTimerRef.current = window.setTimeout(() => {
      finishTimerRef.current = null;
      if (alive(gen)) emit({ type: "expired", gen });
    }, FINISH_STALL_MS);
    try {
      const { base64 } = await active.capture.stop();
      if (!alive(gen)) return;
      const result = await (await agentBridge()).transcribe(base64, preferredLanguage());
      if (alive(gen)) emit({ type: "settled", gen, text: result.text ?? "" });
    } catch (cause) {
      if (alive(gen)) emit({ type: "failed", gen, message: describeMicrophoneError(cause) });
    } finally {
      if (finishTimerRef.current !== null) {
        window.clearTimeout(finishTimerRef.current);
        finishTimerRef.current = null;
      }
    }
  }

  /** The 60 s cap belongs to the machine: it ends the utterance the way letting go would. */
  function armUtteranceCap(gen: number): void {
    if (capTimerRef.current !== null) window.clearTimeout(capTimerRef.current);
    capTimerRef.current = window.setTimeout(() => {
      capTimerRef.current = null;
      emit({ type: "expired", gen });
    }, MAX_UTTERANCE_MS);
  }

  // Leaving the screen mid-utterance must not leave a recording behind.
  const teardownRef = useRef<() => void>(() => {});
  teardownRef.current = () => {
    clearTimers();
    slotRef.current.dispose();
    const active = activeRef.current;
    if (active?.kind === "local") active.capture.cancel();
    activeRef.current = null;
  };
  useEffect(() => () => teardownRef.current(), []);

  const begin = useCallback((delivery: VoiceDelivery = "append") => {
    emitRef.current({ type: "press", delivery });
  }, []);
  const commit = useCallback((delivery: VoiceDelivery = "append") => {
    emitRef.current({ type: "release", delivery });
  }, []);
  const cancel = useCallback(() => emitRef.current({ type: "cancel" }), []);
  const toggle = useCallback(() => {
    const phase = stateRef.current.phase;
    if (phase === "idle") emitRef.current({ type: "press", delivery: "append" });
    else if (phase === "listening") emitRef.current({ type: "release", delivery: "append" });
    else if (phase === "opening") emitRef.current({ type: "cancel" });
  }, []);

  // `emit` is recreated every render (it closes over the current `onText`); the callbacks we
  // hand out are not, so they go through a ref instead of being rebuilt on every keystroke.
  const emitRef = useRef(emit);
  emitRef.current = emit;

  const view = useMemo(() => viewOf(machine), [machine]);
  return {
    available: engine !== null,
    state: view.state,
    heard: view.heard,
    interim: view.interim,
    error: view.error,
    begin,
    commit,
    cancel,
    toggle,
  };
}
