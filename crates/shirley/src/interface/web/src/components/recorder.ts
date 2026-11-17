// Microphone -> 16 kHz mono 16-bit WAV, the format whisper.cpp reads without any conversion.
//
// `snapshot()` encodes everything recorded so far without stopping, which is what lets the
// server transcribe while the user is still speaking.

const TARGET_RATE = 16_000;

/**
 * How long to wait between live previews: a second at first, longer as the recording grows,
 * because each preview re-transcribes everything said so far.
 */
export function previewInterval(seconds: number): number {
  return Math.min(3000, 1000 + Math.floor(seconds / 10) * 500);
}

export interface Recording {
  /** The recording so far, as WAV. */
  snapshot(): Blob;
  /** Seconds recorded so far. */
  seconds(): number;
  /** Stop recording and release the microphone; returns the whole recording. */
  stop(): Blob;
  /** Release the microphone without a result. */
  cancel(): void;
}

/** Why a recording could not start, in words the user can act on. */
export function microphoneError(cause: unknown): string {
  if (!window.isSecureContext) {
    return "浏览器只允许 HTTPS 页面使用麦克风，请通过 https 地址访问";
  }
  const name = cause instanceof DOMException ? cause.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") {
    return "麦克风权限被拒绝，请在浏览器（以及系统设置）里允许使用麦克风";
  }
  if (name === "NotFoundError" || name === "OverconstrainedError") {
    return "找不到麦克风，请检查输入设备";
  }
  return "麦克风打不开，请再试一次";
}

export async function startRecording(): Promise<Recording> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new DOMException("no media devices", "SecurityError");
  }
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
  });
  const context = new AudioContext();
  // A context created after an await may start suspended (Safari).
  if (context.state === "suspended") await context.resume().catch(() => undefined);
  const source = context.createMediaStreamSource(stream);
  // ScriptProcessorNode is deprecated but works everywhere without serving a worklet file.
  const processor = context.createScriptProcessor(4096, 1, 1);
  const chunks: Float32Array[] = [];
  let length = 0;
  processor.onaudioprocess = (event) => {
    const input = event.inputBuffer.getChannelData(0);
    chunks.push(new Float32Array(input));
    length += input.length;
  };
  source.connect(processor);
  // The processor only runs while connected to an output; it writes nothing, so this is silent.
  processor.connect(context.destination);
  const rate = context.sampleRate;
  let closed = false;

  const release = () => {
    if (closed) return;
    closed = true;
    processor.onaudioprocess = null;
    source.disconnect();
    processor.disconnect();
    stream.getTracks().forEach((track) => track.stop());
    void context.close().catch(() => undefined);
  };

  return {
    snapshot: () => encodeWav(chunks, length, rate),
    seconds: () => length / rate,
    stop: () => {
      const wav = encodeWav(chunks, length, rate);
      release();
      return wav;
    },
    cancel: release,
  };
}

/** Join, resample to 16 kHz (averaging each output sample's span), and wrap as WAV. */
export function encodeWav(chunks: Float32Array[], length: number, rate: number): Blob {
  const samples = new Float32Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    samples.set(chunk.subarray(0, Math.min(chunk.length, length - offset)), offset);
    offset += chunk.length;
    if (offset >= length) break;
  }
  const ratio = rate / TARGET_RATE;
  const outLength = Math.floor(length / ratio);
  const pcm = new Int16Array(outLength);
  for (let index = 0; index < outLength; index += 1) {
    const start = Math.floor(index * ratio);
    const end = Math.min(length, Math.max(start + 1, Math.floor((index + 1) * ratio)));
    let sum = 0;
    for (let at = start; at < end; at += 1) sum += samples[at];
    const value = Math.max(-1, Math.min(1, sum / (end - start)));
    pcm[index] = value < 0 ? value * 0x8000 : value * 0x7fff;
  }
  const header = new ArrayBuffer(44);
  const view = new DataView(header);
  const text = (at: number, value: string) => {
    for (let index = 0; index < value.length; index += 1) view.setUint8(at + index, value.charCodeAt(index));
  };
  text(0, "RIFF");
  view.setUint32(4, 36 + pcm.byteLength, true);
  text(8, "WAVE");
  text(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, TARGET_RATE, true);
  view.setUint32(28, TARGET_RATE * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, pcm.byteLength, true);
  return new Blob([header, pcm.buffer], { type: "audio/wav" });
}
