import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  filesToAttachments,
  formatSize,
  kindOf,
  pastedFiles,
  toAttachments,
  userMessageParts,
} from "./attachments";

function file(name: string, type: string, bytes = 2048): File {
  return new File([new Uint8Array(bytes)], name, { type, lastModified: 1_700_000_000_000 });
}

function fakeClipboard(items: File[], exposedAsFiles: File[] = []): DataTransfer {
  return {
    items: items.map((item) => ({ kind: "file", type: item.type, getAsFile: () => item })),
    files: exposedAsFiles,
  } as unknown as DataTransfer;
}

// A screenshot pasted at 09:30:15 local time.
const PASTED_AT = new Date(2026, 10, 16, 9, 30, 15).getTime();

beforeEach(() => {
  let counter = 0;
  vi.stubGlobal("URL", Object.assign(Object.create(URL), {
    createObjectURL: () => `blob:test/${(counter += 1)}`,
  }));
});

describe("pastedFiles", () => {
  it("ignores a text-only paste so the input keeps its normal behaviour", () => {
    const clipboard = {
      items: [{ kind: "string", type: "text/plain", getAsFile: () => null }],
      files: [],
    } as unknown as DataTransfer;
    expect(pastedFiles(clipboard, PASTED_AT)).toEqual([]);
  });

  it("names an unnamed screenshot instead of leaving image.png in the tray", () => {
    const pasted = pastedFiles(fakeClipboard([file("image.png", "image/png")]), PASTED_AT);
    expect(pasted).toHaveLength(1);
    expect(pasted[0].name).toBe("粘贴图片-093015-1.png");
    expect(pasted[0].type).toBe("image/png");
  });

  it("numbers several screenshots in clipboard order and keeps their bytes", () => {
    const pasted = pastedFiles(
      fakeClipboard([file("", "image/jpeg", 10), file("blob", "image/webp", 20)]),
      PASTED_AT,
    );
    expect(pasted.map((entry) => entry.name)).toEqual([
      "粘贴图片-093015-1.jpg",
      "粘贴图片-093015-2.webp",
    ]);
    expect(pasted.map((entry) => entry.size)).toEqual([10, 20]);
  });

  it("keeps a real filename when the clipboard carries one", () => {
    const pasted = pastedFiles(fakeClipboard([file("海报-v2.png", "image/png")]), PASTED_AT);
    expect(pasted[0].name).toBe("海报-v2.png");
  });

  it("falls back to data.files for browsers that only expose the file list", () => {
    const pasted = pastedFiles(fakeClipboard([], [file("image.png", "image/png")]), PASTED_AT);
    expect(pasted).toHaveLength(1);
    expect(pasted[0].name).toBe("粘贴图片-093015-1.png");
  });

  it("accepts a pasted pdf as well, since the same tray renders it", () => {
    const pasted = pastedFiles(fakeClipboard([file("report.pdf", "application/pdf")]), PASTED_AT);
    expect(pasted.map((entry) => entry.name)).toEqual(["report.pdf"]);
  });
});

describe("attachments built from pasted files", () => {
  it("produces an image attachment ready for the existing upload path", () => {
    const [attachment] = filesToAttachments(pastedFiles(fakeClipboard([file("image.png", "image/png", 4096)]), PASTED_AT));
    expect(attachment.kind).toBe("image");
    expect(attachment.name).toBe("粘贴图片-093015-1.png");
    expect(attachment.size).toBe("4 KB");
    expect(attachment.url).toMatch(/^blob:test\//);
    expect(attachment.id).toContain("粘贴图片-093015-1.png");
  });

  it("gives every attachment a distinct id even for identical pastes", () => {
    const [first] = filesToAttachments([file("image.png", "image/png")]);
    const [second] = filesToAttachments([file("image.png", "image/png")]);
    expect(first.id).not.toBe(second.id);
  });

  it("keeps the file picker behaviour unchanged", () => {
    expect(toAttachments(null)).toEqual([]);
    const picked = toAttachments([file("a.pdf", "application/pdf")] as unknown as FileList);
    expect(picked.map((entry) => entry.kind)).toEqual(["pdf"]);
  });
});

describe("kind and size", () => {
  it("classifies by mime first, then by extension", () => {
    expect(kindOf(file("x.png", "image/png"))).toBe("image");
    expect(kindOf(file("x.pdf", ""))).toBe("pdf");
    expect(kindOf(file("x.txt", "text/plain"))).toBe("file");
  });

  it("switches from KB to MB at one megabyte", () => {
    expect(formatSize(1024)).toBe("1 KB");
    expect(formatSize(1024 * 1024 - 1)).toBe("1024 KB");
    expect(formatSize(1024 * 1024)).toBe("1.0 MB");
    expect(formatSize(3 * 1024 * 1024 + 600)).toBe("3.0 MB");
  });
});

describe("userMessageParts", () => {
  it("renders a pasted image in the sent bubble from its local preview", () => {
    const [image] = pastedFiles(
      {
        items: [{ kind: "file", type: "image/png", getAsFile: () => new File(["x"], "", { type: "image/png" }) }],
        files: [],
      },
      new Date("2026-05-01T09:07:00"),
    );
    const parts = userMessageParts([image]);
    expect(parts).toHaveLength(1);
    expect(parts[0]).toMatchObject({ kind: "image", source: { kind: "url" }, alt: image.name });
  });

  it("labels a non-image attachment as a file with its size", () => {
    const [file] = toAttachments([new File(["abc"], "report.pdf", { type: "application/pdf" })]);
    expect(userMessageParts([file])[0]).toMatchObject({
      kind: "file",
      name: "report.pdf",
      bytes: 3,
      mime: "application/pdf",
    });
  });

  it("has nothing to render when nothing was attached", () => {
    expect(userMessageParts([])).toEqual([]);
  });
});
