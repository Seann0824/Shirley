/**
 * One dictation, as a state machine.
 *
 * Four things happen at once during dictation -- the finger, the microphone, the recogniser and
 * (on the local engine) the transcriber -- and every one of them is asynchronous. The first
 * versions of this feature put that in `setState` calls sprinkled through five callbacks, so the
 * state was simply whatever ran last: a release during the permission round trip was overwritten
 * by a recogniser that finished opening afterwards, and the bar sat on "listening" with nobody
 * holding it, forever. Adding another `if` around that is the same mistake with more guards.
 *
 * So the loop lives here instead, around one idea: **every press starts a new utterance with a
 * new generation number**, and every asynchronous callback has to quote that number to be heard.
 * Anything arriving from a generation the machine has already left is dropped on arrival, so
 * nothing can resurrect a gesture the user ended. A press is never swallowed either -- pressing
 * while something is still open retires it and opens a fresh one -- because a press that does
 * nothing is indistinguishable from a frozen app.
 *
 * This module is pure: no DOM, no React, no browser API. `voiceReduce` returns the next state
 * plus the effects somebody else should perform. `useVoiceInput` performs them; the transitions
 * themselves are proved in `voice-machine.test.ts`.
 */
import { composeUtterance } from "./voice-hold.ts";

/** Where the words go when the finger comes up. */
export type VoiceDelivery = "append" | "send" | "edit";

/** Which transcriber this device uses: the browser's own, or `whisper.cpp` on the machine. */
export type TranscriberKind = "web" | "local";

/**
 * - `opening` -- the microphone is being opened (permission sheet, capture, recogniser).
 * - `listening` -- audio is being consumed and words may arrive.
 * - `finishing` -- the finger is up and the audio is still becoming text (local engine only).
 */
export type VoicePhase = "idle" | "opening" | "listening" | "finishing";

export type VoiceEvent =
  /** The finger went down. Always begins a new generation. */
  | { type: "press"; delivery: VoiceDelivery }
  /** The finger came up: deliver what was heard, in this mode. */
  | { type: "release"; delivery: VoiceDelivery }
  | { type: "cancel" }
  /** The microphone is open. `gen` must match the live generation. */
  | { type: "opened"; gen: number }
  /** More words were recognised; `settled` is the whole utterance so far, not a delta. */
  | { type: "heard"; gen: number; settled: string; interim: string }
  /** The local transcriber finished. */
  | { type: "settled"; gen: number; text: string }
  /** The microphone or recogniser refused. One human sentence in `message`. */
  | { type: "failed"; gen: number; message: string }
  /** The utterance hit its maximum length: deliver it as if the finger had come up. */
  | { type: "expired"; gen: number };

export type VoiceEffect =
  /** Open the microphone for generation `gen`. */
  | { type: "open"; gen: number; delivery: VoiceDelivery }
  /** Stop recording and transcribe generation `gen` (local engine). */
  | { type: "close"; gen: number; delivery: VoiceDelivery }
  /** Throw away generation `gen`: abort the recogniser, cancel the capture. Idempotent. */
  | { type: "abort"; gen: number }
  /** Hand the text to the composer. */
  | { type: "deliver"; text: string; delivery: VoiceDelivery };

export type VoiceMachineState = {
  phase: VoicePhase;
  /** The live utterance. Every other generation is history. */
  gen: number;
  /** The delivery mode chosen at press time; `settled` arrives after the finger is up. */
  delivery: VoiceDelivery;
  /** Recognised, final words of this utterance. */
  settled: string;
  /** Recognised, still-changing words -- shown live, and part of what the user said. */
  interim: string;
  /** One human sentence. Empty unless the last attempt failed. */
  error: string;
};

/** One human sentence each: what the user needs to decide whether to try again. */
export const NO_DICTATION = "这台设备用不了语音。";
export const NO_RESPONSE = "麦克风没有响应，再按一次试试。";
export const START_REFUSED = "语音识别没能开始，再按一次试试。";
/**
 * The utterance ended without a single word.
 *
 * Silence is the one outcome a person cannot interpret: they held the button, the microphone
 * lit up, they spoke, and nothing happened -- which reads as a broken app. A session that
 * reports nothing is a session that failed, and it says so.
 */
export const HEARD_NOTHING = "没听清，再说一次。";
/**
 * The local engine took the audio and never came back with text.
 *
 * The browser engine has no such wait any more (§5.4: what is on screen is the utterance), but
 * whisper runs beside the app and can wedge. Silence is not an answer -- say it, and let the next
 * press start clean.
 */
export const TRANSCRIBE_STALLED = "转写没等到结果，再按一次试试。";

export function idleVoice(): VoiceMachineState {
  return { phase: "idle", gen: 0, delivery: "send", settled: "", interim: "", error: "" };
}

export type VoiceStep = { state: VoiceMachineState; effects: VoiceEffect[] };

/** Nothing happens: a callback from a generation that is no longer alive. */
function quiet(state: VoiceMachineState): VoiceStep {
  return { state, effects: [] };
}

/** Leave the utterance behind, keeping only enough of it to make the next generation distinct. */
function retire(state: VoiceMachineState, error = ""): VoiceMachineState {
  return { ...idleVoice(), gen: state.gen + 1, error };
}

/**
 * Letting go.
 *
 * `opening` is the interesting case and the one that used to break: the microphone has not
 * answered yet, so there is nothing to deliver -- but the *opening* must be voided, which is
 * what `abort` + a new generation does. The recogniser that finishes opening a moment later
 * then reports to a generation nobody is waiting for, and the machine stays idle.
 */
function release(
  state: VoiceMachineState,
  delivery: VoiceDelivery,
  options: { engine: TranscriberKind | null },
): VoiceStep {
  switch (state.phase) {
    case "idle":
    case "finishing":
      // The finger cannot come up twice; a second release changes nothing.
      return quiet(state);
    case "opening":
      return { state: retire(state), effects: [{ type: "abort", gen: state.gen }] };
    case "listening": {
      if (options.engine === "local") {
        // The local engine has no partial text: stop the capture, transcribe, then `settled`.
        return {
          state: { ...state, phase: "finishing" },
          effects: [{ type: "close", gen: state.gen, delivery }],
        };
      }
      // The browser has already shown the words, and asking it to "finish" means waiting for an
      // `end` the platform decides when to send -- on a phone that is seconds, and the next press
      // arriving inside it is the one that failed with "麦克风没有响应". Worse, iOS often only
      // ever produces *interim* results for push-to-talk, so the final list we used to wait for
      // came back empty and the utterance was thrown away as "heard nothing". What is on screen
      // is the utterance: deliver it now, abort the session hard, and never wait.
      const text = composeUtterance(state.settled, state.interim);
      return {
        state: retire(state, text ? "" : HEARD_NOTHING),
        effects: text
          ? [
              { type: "abort", gen: state.gen },
              { type: "deliver", text, delivery },
            ]
          : [{ type: "abort", gen: state.gen }],
      };
    }
  }
  return quiet(state);
}

/**
 * The whole machine, in one table.
 *
 * Read it as: which phase accepts which event, and what the outside world has to do about it.
 * Anything not listed for a phase leaves the state alone -- an unexpected event is worse at
 * guessing than doing nothing.
 */
export function voiceReduce(
  state: VoiceMachineState,
  event: VoiceEvent,
  options: { engine: TranscriberKind | null },
): VoiceStep {
  switch (event.type) {
    case "press": {
      if (options.engine === null) return { state: { ...state, error: NO_DICTATION }, effects: [] };
      // A press always means "start talking now". Whatever was open belongs to the previous
      // utterance and is retired first, so no leftover can hold this one hostage.
      const gen = state.gen + 1;
      const effects: VoiceEffect[] = [];
      if (state.phase !== "idle") effects.push({ type: "abort", gen: state.gen });
      effects.push({ type: "open", gen, delivery: event.delivery });
      return {
        state: { phase: "opening", gen, delivery: event.delivery, settled: "", interim: "", error: "" },
        effects,
      };
    }
    case "opened": {
      if (event.gen !== state.gen || state.phase !== "opening") return quiet(state);
      return { state: { ...state, phase: "listening" }, effects: [] };
    }
    case "heard": {
      if (event.gen !== state.gen) return quiet(state);
      if (state.phase !== "opening" && state.phase !== "listening") return quiet(state);
      return { state: { ...state, settled: event.settled, interim: event.interim }, effects: [] };
    }
    case "release":
      return release(state, event.delivery, options);
    case "expired": {
      // The 60 s cap is the machine's own finger: end the utterance the way letting go would.
      if (event.gen !== state.gen) return quiet(state);
      if (state.phase === "idle") return quiet(state);
      // `finishing` waits on a transcription that is not coming. Dropping the event here used to
      // leave the machine in `finishing` for good -- the microphone was gone and nothing said why.
      if (state.phase === "finishing") {
        return {
          state: retire(state, TRANSCRIBE_STALLED),
          effects: [{ type: "abort", gen: state.gen }],
        };
      }
      return release(state, state.delivery, options);
    }
    case "settled": {
      if (event.gen !== state.gen || state.phase !== "finishing") return quiet(state);
      const text = event.text.trim();
      return {
        state: retire(state, text ? "" : HEARD_NOTHING),
        effects: text ? [{ type: "deliver", text, delivery: state.delivery }] : [],
      };
    }
    case "failed": {
      if (event.gen !== state.gen) return quiet(state);
      const effects: VoiceEffect[] = state.phase === "idle" ? [] : [{ type: "abort", gen: event.gen }];
      return { state: retire(state, event.message), effects };
    }
    case "cancel": {
      if (state.phase === "idle") return quiet(state);
      return { state: retire(state), effects: [{ type: "abort", gen: state.gen }] };
    }
  }
}
