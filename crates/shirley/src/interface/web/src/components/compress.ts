/**
 * Shrink a picture before it is uploaded: re-encode it as WebP, scaled so its longer side is at
 * most `MAX_SIDE`. A phone photo of several MB usually lands at a few hundred KB, which is what
 * the upload, the library and the model's image budget all want.
 *
 * Anything that cannot or should not be re-encoded goes up unchanged: non-images, GIFs (they may
 * be animated), SVGs (vector), a browser that cannot encode WebP, a decode failure, and any
 * result that is not actually smaller than the original.
 */

/** Longer side, in pixels, of an uploaded picture. Plenty for reading text in a screenshot. */
export const MAX_SIDE = 2560;
/** WebP quality: visually close to the source at a fraction of its size. */
export const QUALITY = 0.82;

const KEEP_AS_IS = new Set(["image/gif", "image/svg+xml"]);

/** Whether `file` is a picture this module would try to re-encode. */
export function compressible(file: File): boolean {
  return file.type.startsWith("image/") && !KEEP_AS_IS.has(file.type);
}

/** The dimensions that fit `width` x `height` inside `maxSide`, keeping the aspect ratio. */
export function fitWithin(width: number, height: number, maxSide = MAX_SIDE): { width: number; height: number } {
  const longer = Math.max(width, height);
  if (longer <= maxSide) return { width, height };
  const scale = maxSide / longer;
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

/** `photo.HEIC` -> `photo.webp`; a name without an extension just gains one. */
export function webpName(name: string): string {
  const dot = name.lastIndexOf(".");
  return `${dot > 0 ? name.slice(0, dot) : name || "image"}.webp`;
}

async function encode(file: File): Promise<Blob | null> {
  if (typeof createImageBitmap !== "function" || typeof document === "undefined") return null;
  // `from-image` applies the EXIF orientation, so a portrait phone photo stays upright.
  const bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  try {
    const { width, height } = fitWithin(bitmap.width, bitmap.height);
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext("2d");
    if (!context) return null;
    context.drawImage(bitmap, 0, 0, width, height);
    return await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/webp", QUALITY));
  } finally {
    bitmap.close();
  }
}

/** The file to upload in place of `file`: a smaller WebP when that works, else `file` itself. */
export async function compressImage(file: File): Promise<File> {
  if (!compressible(file)) return file;
  try {
    const blob = await encode(file);
    // A browser without a WebP encoder silently hands back a PNG: keep the original then.
    if (!blob || blob.type !== "image/webp" || blob.size >= file.size) return file;
    return new File([blob], webpName(file.name), { type: "image/webp", lastModified: file.lastModified || Date.now() });
  } catch {
    return file;
  }
}
