/**
 * Dictation plumbing for the composer: WAV capture and the browser's own recogniser.
 *
 * Two low-level primitives live here, and both are engine-agnostic:
 *
 * - {@link startWavCapture} records the microphone and hands back **16 kHz mono 16-bit
 *   WAV** (base64). Resampling happens in the page, so the Rust side never needs `ffmpeg`.
 * - {@link speechRecognitionCtor} exposes `webkitSpeechRecognition` when the webview has
 *   it (Chrome, Edge). That path streams audio to the browser vendor's service and never
 *   touches our backend, which is why it is a separate engine (`web`) from the Rust ones.
 *
 * See `docs/voice-input.md`.
 */

/** Engines the composer can offer. `web` is browser-only; the other two are Rust-side. */
export type VoiceEngine = "web" | "local";

/** What whisper / the OpenAI transcription API expect; pages usually run at 44.1 or 48 kHz. */
const TARGET_SAMPLE_RATE = 16_000;
/** One utterance cap. Longer than this and a mistake costs the user a whole paragraph. */
export const MAX_UTTERANCE_MS = 60_000;

/** Minimal shape of the vendor recogniser (not in TypeScript's DOM lib). */
type SpeechResultLike = { isFinal: boolean; length: number; 0?: { transcript?: string } };
type SpeechEventLike = {
  resultIndex: number;
  results: { length: number; [index: number]: SpeechResultLike };
};
type SpeechErrorLike = { error: string };

export type SpeechRecognitionLike = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  maxAlternatives: number;
  start: () => void;
  stop: () => void;
  abort: () => void;
  onresult: ((event: SpeechEventLike) => void) | null;
  onerror: ((event: SpeechErrorLike) => void) | null;
  onend: (() => void) | null;
  /** Fires when audio capture actually began. A dead recogniser never fires it. */
  onaudiostart?: (() => void) | null;
};

type SpeechRecognitionCtor = new () => SpeechRecognitionLike;

/** The vendor-prefixed constructor, or `null` when this webview ships none. */
export function speechRecognitionCtor(): SpeechRecognitionCtor | null {
  const scope = window as unknown as {
    SpeechRecognition?: SpeechRecognitionCtor;
    webkitSpeechRecognition?: SpeechRecognitionCtor;
  };
  return scope.SpeechRecognition ?? scope.webkitSpeechRecognition ?? null;
}

/** Whether the in-page recogniser can be offered at all. */
export function speechRecognitionAvailable(): boolean {
  return speechRecognitionCtor() !== null;
}


/** One in-flight recording. */
export type WavCapture = {
  /** Stop the microphone and return the encoded utterance. */
  stop: () => Promise<{ base64: string; mime: string }>;
  /** Stop and throw the audio away (unmount, error path). */
  cancel: () => void;
};

/**
 * Start recording. Throws when the microphone is refused or unavailable -- the caller maps
 * that to a one-line message.
 */
export async function startWavCapture(): Promise<WavCapture> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Ctor) {
    for (const track of stream.getTracks()) track.stop();
    throw new Error("Audio capture is unavailable.");
  }

  const context = new Ctor();
  const sampleRate = context.sampleRate;
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(4096, 1, 1);
  const chunks: Float32Array[] = [];
  processor.onaudioprocess = (event) => {
    chunks.push(new Float32Array(event.inputBuffer.getChannelData(0)));
  };
  source.connect(processor);
  // A ScriptProcessorNode is only pulled while connected to the destination. Its output is
  // left untouched (silence), so the microphone is not echoed back to the speakers.
  processor.connect(context.destination);

  let closed = false;
  const teardown = () => {
    if (closed) return;
    closed = true;
    processor.onaudioprocess = null;
    try {
      processor.disconnect();
    } catch {
      /* already detached */
    }
    try {
      source.disconnect();
    } catch {
      /* already detached */
    }
    for (const track of stream.getTracks()) track.stop();
    if (context.state !== "closed") void context.close().catch(() => {});
  };

  return {
    async stop() {
      teardown();
      const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
      const merged = new Float32Array(total);
      let offset = 0;
      for (const chunk of chunks) {
        merged.set(chunk, offset);
        offset += chunk.length;
      }
      chunks.length = 0;
      const resampled = downsample(merged, sampleRate, TARGET_SAMPLE_RATE);
      return { base64: base64FromBytes(encodeWav(resampled, TARGET_SAMPLE_RATE)), mime: "audio/wav" };
    },
    cancel() {
      teardown();
      chunks.length = 0;
    },
  };
}

/** Average-bucket resampling: cheap, and good enough for speech recognition. */
function downsample(input: Float32Array, from: number, to: number): Float32Array {
  if (to >= from) return input;
  const ratio = from / to;
  const length = Math.floor(input.length / ratio);
  const output = new Float32Array(length);
  for (let index = 0; index < length; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(input.length, Math.floor((index + 1) * ratio));
    let sum = 0;
    for (let cursor = start; cursor < end; cursor += 1) sum += input[cursor];
    output[index] = end > start ? sum / (end - start) : 0;
  }
  return output;
}

/** Encode mono float samples as a canonical 44-byte-header PCM WAV. */
function encodeWav(samples: Float32Array, sampleRate: number): Uint8Array {
  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);
  const ascii = (offset: number, text: string) => {
    for (let index = 0; index < text.length; index += 1) {
      view.setUint8(offset + index, text.charCodeAt(index));
    }
  };
  ascii(0, "RIFF");
  view.setUint32(4, 36 + samples.length * 2, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true); // fmt chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true); // byte rate
  view.setUint16(32, 2, true); // block align
  view.setUint16(34, 16, true); // bits per sample
  ascii(36, "data");
  view.setUint32(40, samples.length * 2, true);
  let offset = 44;
  for (const sample of samples) {
    const clamped = Math.max(-1, Math.min(1, sample));
    view.setInt16(offset, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
    offset += 2;
  }
  return new Uint8Array(buffer);
}

/** Base64 for an upload, chunked so a long utterance does not blow the argument limit. */
function base64FromBytes(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let index = 0; index < bytes.length; index += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunkSize));
  }
  return btoa(binary);
}
