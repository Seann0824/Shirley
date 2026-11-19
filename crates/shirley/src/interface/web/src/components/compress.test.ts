import { describe, expect, it } from "vitest";

import { MAX_SIDE, compressImage, compressible, fitWithin, webpName } from "./compress";

function file(name: string, type: string): File {
  return new File([new Uint8Array(64)], name, { type });
}

describe("compressible", () => {
  it("re-encodes photos and screenshots only", () => {
    expect(compressible(file("a.jpg", "image/jpeg"))).toBe(true);
    expect(compressible(file("a.png", "image/png"))).toBe(true);
    expect(compressible(file("a.gif", "image/gif"))).toBe(false);
    expect(compressible(file("a.svg", "image/svg+xml"))).toBe(false);
    expect(compressible(file("a.pdf", "application/pdf"))).toBe(false);
  });
});

describe("fitWithin", () => {
  it("leaves a picture that already fits alone", () => {
    expect(fitWithin(1200, 800)).toEqual({ width: 1200, height: 800 });
  });

  it("scales the longer side down to the limit, keeping the ratio", () => {
    expect(fitWithin(4000, 3000)).toEqual({ width: MAX_SIDE, height: 1920 });
    expect(fitWithin(3000, 6000)).toEqual({ width: 1280, height: MAX_SIDE });
  });
});

describe("webpName", () => {
  it("swaps the extension", () => {
    expect(webpName("IMG_0001.HEIC")).toBe("IMG_0001.webp");
    expect(webpName("粘贴图片-1.png")).toBe("粘贴图片-1.webp");
    expect(webpName("noext")).toBe("noext.webp");
  });
});

describe("compressImage", () => {
  it("hands back the original when it cannot re-encode", async () => {
    const pdf = file("a.pdf", "application/pdf");
    expect(await compressImage(pdf)).toBe(pdf);
    // The test environment has no image decoder: a photo goes up unchanged rather than failing.
    const photo = file("a.jpg", "image/jpeg");
    expect(await compressImage(photo)).toBe(photo);
  });
});
