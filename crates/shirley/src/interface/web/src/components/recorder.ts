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

/** The text shown while the browser's microphone prompt is open. */
export const PERMISSION_HINT = "请在浏览器弹窗里选「访问此网站时允许」，以后就不会再问了";

/**
 * Whether the browser will ask before opening the microphone. Chrome remembers "allow" per site
 * (scheme + host + port): asked again only after "allow this time", or at a different address.
 */
export async function microphoneWillAsk(): Promise<boolean> {
  try {
    const status = await navigator.permissions?.query({ name: "microphone" as PermissionName });
    return status?.state === "prompt";
  } catch {
    return false; // Firefox and older Safari cannot query it: say nothing.
  }
}

/**
 * The microphone stays open for a while after a recording ends. Mobile Chrome is slow (sometimes
 * unable) to hand the microphone out again right after it was released, which made a quick
 * second sentence fail; reusing the open stream makes the next press instant.
 */
const KEEP_OPEN_MS = 30_000;

interface Session {
  stream: MediaStream;
  context: AudioContext;
  source: MediaStreamAudioSourceNode;
  processor: ScriptProcessorNode;
  /** Receives every audio block while a recording is active. */
  sink: ((block: Float32Array) => void) | null;
  idle: number | null;
}

let session: Session | null = null;

function sessionAlive(current: Session): boolean {
  return (
    current.context.state !== "closed" && current.stream.getAudioTracks().some((track) => track.readyState === "live")
  );
}

function closeSession(target: Session) {
  if (target.idle !== null) window.clearTimeout(target.idle);
  target.processor.onaudioprocess = null;
  target.source.disconnect();
  target.processor.disconnect();
  target.stream.getTracks().forEach((track) => track.stop());
  void target.context.close().catch(() => undefined);
  if (session === target) session = null;
}

async function openSession(): Promise<Session> {
  if (session && sessionAlive(session)) {
    if (session.idle !== null) window.clearTimeout(session.idle);
    session.idle = null;
    if (session.context.state === "suspended") await session.context.resume().catch(() => undefined);
    return session;
  }
  if (session) closeSession(session);
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
  });
  const context = new AudioContext();
  // A context created after an await may start suspended (Safari).
  if (context.state === "suspended") await context.resume().catch(() => undefined);
  const source = context.createMediaStreamSource(stream);
  // ScriptProcessorNode is deprecated but works everywhere without serving a worklet file.
  const processor = context.createScriptProcessor(4096, 1, 1);
  const opened: Session = { stream, context, source, processor, sink: null, idle: null };
  processor.onaudioprocess = (event) => {
    opened.sink?.(event.inputBuffer.getChannelData(0));
  };
  source.connect(processor);
  // The processor only runs while connected to an output; it writes nothing, so this is silent.
  processor.connect(context.destination);
  session = opened;
  return opened;
}

export async function startRecording(): Promise<Recording> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new DOMException("no media devices", "SecurityError");
  }
  const open = await openSession();
  const chunks: Float32Array[] = [];
  let length = 0;
  open.sink = (input) => {
    chunks.push(new Float32Array(input));
    length += input.length;
  };
  const rate = open.context.sampleRate;
  let closed = false;

  const release = () => {
    if (closed) return;
    closed = true;
    if (open.sink) open.sink = null;
    if (open.idle !== null) window.clearTimeout(open.idle);
    open.idle = window.setTimeout(() => closeSession(open), KEEP_OPEN_MS);
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
