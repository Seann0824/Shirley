import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { cn } from "@/ui/utils";

/**
 * Full-screen image preview.
 *
 * One component, used by every place that shows a picture (message parts, markdown images,
 * the echoed user message). The enlarged view stays **inside the app** — a fixed overlay,
 * not the system image viewer, so the conversation never loses its place.
 *
 * Close paths, because a preview that traps you is worse than no preview: the backdrop, the
 * close button, and Escape. Clicking the picture itself does **not** close (that is the one
 * spot the user is aiming at), and the page behind is scroll-locked while it is open.
 */
export function ImageLightbox({
  src,
  alt,
  caption,
  onClose,
}: {
  src: string;
  alt: string;
  caption?: string | null;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      window.removeEventListener("keydown", onKey);
      document.body.style.overflow = previous;
    };
  }, [onClose]);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={alt || "Image preview"}
      onClick={onClose}
      className="fixed inset-0 z-50 flex cursor-zoom-out items-center justify-center bg-black/80 p-6 backdrop-blur-sm"
    >
      <button
        type="button"
        aria-label="Close preview"
        onClick={onClose}
        className="absolute top-4 right-4 rounded-full bg-white/10 p-2 text-white transition-colors hover:bg-white/20"
      >
        <X className="size-5" />
      </button>
      <img
        src={src}
        alt={alt}
        onClick={(event) => event.stopPropagation()}
        className="max-h-full max-w-full cursor-default rounded-control object-contain shadow-2xl"
      />
      {caption ? (
        <p className="absolute bottom-4 left-1/2 max-w-[80%] -translate-x-1/2 truncate rounded-full bg-black/50 px-3 py-1 text-caption text-white">
          {caption}
        </p>
      ) : null}
    </div>
  );
}

/**
 * A picture shown on its own, not inline in a sentence.
 *
 * Bordered, rounded and height-capped, so a tall screenshot cannot push the conversation
 * off-screen; the full resolution is one click away through {@link ImageLightbox}.
 *
 * The root is a `span` with `block` display rather than a `figure`: markdown images are
 * rendered inside a `<p>`, and a block-level element there is invalid nesting.
 */
export function ImageBlock({
  src,
  alt,
  caption,
  className,
  imageClassName,
}: {
  src: string;
  alt: string;
  /** Shown under the picture; the lightbox reuses it as the preview caption. */
  caption?: string | null;
  className?: string;
  imageClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  if (!src) return null;
  return (
    <span className={cn("my-2 block min-w-0 max-w-full", className)}>
      <button
        type="button"
        onClick={() => setOpen(true)}
        title="Click to preview"
        aria-label={`Preview image${alt ? `: ${alt}` : ""}`}
        className="block max-w-full cursor-zoom-in overflow-hidden rounded-card border border-line bg-inset transition-colors hover:border-ink/30"
      >
        <img
          src={src}
          alt={alt}
          loading="lazy"
          className={cn("max-h-80 max-w-full object-contain", imageClassName)}
        />
      </button>
      {caption ? <span className="mt-1 block text-caption text-muted">{caption}</span> : null}
      {open ? (
        <ImageLightbox src={src} alt={alt} caption={caption} onClose={() => setOpen(false)} />
      ) : null}
    </span>
  );
}
