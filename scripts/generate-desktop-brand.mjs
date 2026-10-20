// Re-export generated artwork without regenerating or changing the source illustrations.
// Uses macOS image tools and Swift; no additional npm dependencies are required.
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "darwin") {
  throw new Error("Export on macOS (sips + iconutil + Swift), then commit the generated assets.");
}

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "crates/shirley/src/interface/desktop/assets");
const brand = join(root, "crates/shirley/src/interface/desktop/web/public/brand");
const icons = join(root, "crates/shirley/icons");
const temporary = mkdtempSync(join(tmpdir(), "shirley-brand-"));
mkdirSync(brand, { recursive: true });
mkdirSync(icons, { recursive: true });

function resize(input, output, size) {
  execFileSync("sips", ["-Z", String(size), input, "--out", output], { stdio: "ignore" });
}

try {
  // Web artwork keeps the square, fully opaque portrait the interface expects.
  const appIcon = join(temporary, "shirley-app-icon-rgba.png");
  execFileSync("swift", [join(root, "scripts/encode-desktop-icon.swift"), join(source, "shirley-app-icon.png"), appIcon]);
  resize(join(source, "shirley-character.png"), join(brand, "shirley-character.png"), 1152);
  resize(appIcon, join(brand, "shirley-logo.png"), 256);
  resize(appIcon, join(brand, "shirley-avatar.png"), 256);

  // Native icons use the macOS grid: continuous-corner squircle on an 824/1024
  // body, so Dock and Finder match the size and shape of system icons. The
  // artwork is scaled up 10% inside that mask: the source keeps a wide margin
  // around the portrait, and at Dock size the pale background made the icon
  // read lighter and smaller than its neighbours even though the body was
  // already grid-correct.
  const artworkZoom = "1.10";
  const macIcon = join(temporary, "shirley-app-icon-macos.png");
  execFileSync("swift", [
    join(root, "scripts/render-desktop-icon.swift"),
    join(source, "shirley-app-icon.png"),
    macIcon,
    artworkZoom,
  ]);
  for (const [filename, size] of [["32x32.png", 32], ["128x128.png", 128], ["128x128@2x.png", 256], ["icon.png", 1024]]) {
    resize(macIcon, join(icons, filename), size);
  }

  const iconset = join(temporary, "Shirley.iconset");
  mkdirSync(iconset);
  for (const size of [16, 32, 128, 256, 512]) {
    resize(macIcon, join(iconset, `icon_${size}x${size}.png`), size);
    resize(macIcon, join(iconset, `icon_${size}x${size}@2x.png`), size * 2);
  }
  execFileSync("iconutil", ["-c", "icns", iconset, "-o", join(icons, "icon.icns")]);

  // ICO directory with PNG payloads for modern Windows at every common UI size.
  // Windows does not mask app icons, so these stay full-bleed and square.
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  const images = sizes.map((size) => {
    const file = join(temporary, `windows-${size}.png`);
    resize(appIcon, file, size);
    return readFileSync(file);
  });
  const directory = Buffer.alloc(6 + sizes.length * 16);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(sizes.length, 4);
  let offset = directory.length;
  sizes.forEach((size, index) => {
    const entry = 6 + index * 16;
    directory[entry] = size === 256 ? 0 : size;
    directory[entry + 1] = size === 256 ? 0 : size;
    directory.writeUInt16LE(1, entry + 4);
    directory.writeUInt16LE(32, entry + 6);
    directory.writeUInt32LE(images[index].length, entry + 8);
    directory.writeUInt32LE(offset, entry + 12);
    offset += images[index].length;
  });
  writeFileSync(join(icons, "icon.ico"), Buffer.concat([directory, ...images]));
  console.log("Exported Shirley web artwork, PNG app icons, macOS ICNS and Windows ICO.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
