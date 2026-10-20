import { useEffect, useState } from "react";

/**
 * Global hover preview for image chips.
 *
 * A single instance is mounted once (in `App`); it listens for `mouseover` / `mouseout`
 * at the document level and, when the pointer enters an element carrying the
 * `[data-image-preview]` marker, shows the image it contains.
 *
 * The marker is a **property, not a component**: image chips are rendered both by React
 * (`FileChip`) and as raw DOM inside the contenteditable editor (`createChipElement`),
 * and the latter is not managed by React. A delegated listener lets one component serve
 * both, so the editor's DOM chip and the echoed message chip preview identically.
 *
 * The preview is `position: fixed` and `pointer-events: none`: it never reflows the page
 * and never intercepts the pointer, so it cannot fight with the chip under the cursor.
 */
export function ImageHoverPreview() {
  const [preview, setPreview] = useState<{ src: string; x: number; y: number } | null>(null);

  useEffect(() => {
    const srcOf = (target: EventTarget | null): string | null => {
      if (!(target instanceof Element)) return null;
      const chip = target.closest("[data-image-preview]");
      if (!chip) return null;
      const img = chip.querySelector("img");
      return img?.getAttribute("src") ?? null;
    };

    const onOver = (event: MouseEvent) => {
      const src = srcOf(event.target);
      if (!src) return;
      const rect = (event.target as Element).closest("[data-image-preview]")!.getBoundingClientRect();
      setPreview({ src, x: rect.left + rect.width / 2, y: rect.top });
    };
    const onOut = (event: MouseEvent) => {
      // Leaving the marker (including moving between its children) clears the preview;
      // `relatedTarget` inside the same chip keeps it open.
      const chip = (event.target as Element | null)?.closest("[data-image-preview]");
      if (!chip) return;
      const next = event.relatedTarget as Element | null;
      if (next && chip.contains(next)) return;
      setPreview(null);
    };

    document.addEventListener("mouseover", onOver);
    document.addEventListener("mouseout", onOut);
    return () => {
      document.removeEventListener("mouseover", onOver);
      document.removeEventListener("mouseout", onOut);
    };
  }, []);

  if (!preview) return null;
  return (
    <div
      role="tooltip"
      className="pointer-events-none fixed z-50 -translate-x-1/2 -translate-y-full rounded-control border border-line bg-surface p-1 shadow-lg"
      style={{ left: preview.x, top: preview.y - 8 }}
    >
      <img
        src={preview.src}
        alt=""
        className="max-h-64 max-w-64 rounded-[4px] object-contain"
      />
    </div>
  );
}
