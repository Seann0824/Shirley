import { ImageOff, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { cn } from "@/ui/utils";

/** Native modal keeps keyboard focus inside the preview and restores it on close. */
export function ImageLightbox({ src, alt, caption, onClose }: {
  src: string;
  alt: string;
  caption?: string | null;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    dialog.showModal();
    return () => {
      dialog.close();
      document.body.style.overflow = previous;
    };
  }, []);

  return createPortal(
    <dialog
      ref={dialogRef}
      aria-label={alt || "Image preview"}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
      onKeyDown={(event) => {
        if (event.key === "Escape") event.stopPropagation();
      }}
      onClick={(event) => { if (event.target === event.currentTarget) onClose(); }}
      className="fixed inset-0 m-0 flex h-dvh max-h-none w-screen max-w-none flex-col items-center justify-center gap-3 border-0 bg-black/90 p-6 text-white backdrop:bg-black/40"
    >
      <button
        type="button"
        autoFocus
        aria-label="Close preview"
        onClick={onClose}
        className="absolute top-4 right-4 z-10 rounded-full bg-black/50 p-2 text-white hover:bg-white/20 focus-visible:outline-2 focus-visible:outline-white"
      >
        <X className="size-5" />
      </button>
      <img src={src} alt={alt} className="min-h-0 max-w-full flex-1 object-contain" />
      {caption || alt ? <p className="max-w-full shrink-0 truncate text-caption">{caption || alt}</p> : null}
    </dialog>,
    document.body,
  );
}

/** Shared image interaction for drafts, sent attachments and assistant output. */
export function ImageBlock({ src, alt, caption, className, imageClassName, thumbnail = false }: {
  src: string;
  alt: string;
  caption?: string | null;
  className?: string;
  imageClassName?: string;
  thumbnail?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const failed = failedSrc === src;
  if (!src) return null;
  return (
    <span className={cn("my-2 block min-w-0 max-w-full", className)}>
      {failed ? (
        <span role="status" className="flex min-h-20 items-center gap-2 rounded-control border border-line bg-inset p-3 text-caption text-muted">
          <ImageOff className="size-4 shrink-0" />
          <span className="max-w-48 truncate">{alt || "Image unavailable"}</span>
        </span>
      ) : (
        <button
          type="button"
          onClick={(event) => {
            event.preventDefault();
            event.stopPropagation();
            setOpen(true);
          }}
          title={alt || "Click to preview"}
          aria-label={`Preview image${alt ? `: ${alt}` : ""}`}
          className="block max-w-full cursor-zoom-in overflow-hidden rounded-card border border-line bg-inset transition-colors hover:border-ink/30 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ink/50"
        >
          <img
            src={src}
            alt={alt}
            loading="lazy"
            onError={() => setFailedSrc(src)}
            className={cn(thumbnail ? "size-24 object-cover" : "max-h-96 max-w-full object-contain", imageClassName)}
          />
        </button>
      )}
      {caption && !thumbnail ? <span className="mt-1 block text-caption text-muted">{caption}</span> : null}
      {open && !failed ? <ImageLightbox src={src} alt={alt} caption={caption} onClose={() => setOpen(false)} /> : null}
    </span>
  );
}
