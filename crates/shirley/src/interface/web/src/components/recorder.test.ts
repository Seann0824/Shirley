import { describe, expect, it } from "vitest";

import { encodeWav, previewInterval } from "./recorder";

describe("encodeWav", () => {
  it("writes a 16 kHz mono 16-bit WAV from 48 kHz input", async () => {
    // One second of a constant half-scale signal, delivered in two chunks.
    const chunk = new Float32Array(24_000).fill(0.5);
    const wav = encodeWav([chunk, chunk], 48_000, 48_000);
    const bytes = new DataView(await wav.arrayBuffer());
    const text = (at: number) => String.fromCharCode(...[0, 1, 2, 3].map((i) => bytes.getUint8(at + i)));
    expect(text(0)).toBe("RIFF");
    expect(text(8)).toBe("WAVE");
    expect(bytes.getUint16(22, true)).toBe(1);
    expect(bytes.getUint32(24, true)).toBe(16_000);
    expect(bytes.getUint16(34, true)).toBe(16);
    expect(bytes.getUint32(40, true)).toBe(16_000 * 2);
    expect(wav.size).toBe(44 + 16_000 * 2);
    // The level survives resampling.
    expect(bytes.getInt16(44 + 2000, true)).toBe(Math.floor(0.5 * 0x7fff));
  });

  it("encodes a snapshot of a partly filled recording", async () => {
    const wav = encodeWav([new Float32Array(4800)], 4800, 48_000);
    expect(wav.size).toBe(44 + 1600 * 2);
  });
});

describe("previewInterval", () => {
  it("waits longer as the recording grows, up to three seconds", () => {
    expect(previewInterval(1)).toBe(1000);
    expect(previewInterval(12)).toBe(1500);
    expect(previewInterval(60)).toBe(3000);
  });
});
