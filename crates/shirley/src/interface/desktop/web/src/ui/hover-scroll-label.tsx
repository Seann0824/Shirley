import { useEffect, useRef, useState } from "react";
import { cn } from "./utils";

/**
 * Single-line text: truncates when too narrow, and **scrolls horizontally on hover** to
 * reveal the full text (marquee).
 *
 * Why not the native `title`: the native tooltip has ~1s of delay, uncontrollable styling,
 * and competes for attention with the context menu. Scrolling in place is zero-delay and
 * adds no extra UI. It scrolls only when there is real overflow — static when there is
 * none, avoiding pointless jitter. Degrades to no scrolling under `prefers-reduced-motion`.
 */
export function HoverScrollLabel({
  text,
  className,
}: {
  text: string;
  className?: string;
}) {
  const containerRef = useRef<HTMLSpanElement>(null);
  const innerRef = useRef<HTMLSpanElement>(null);
  const [overflow, setOverflow] = useState(0);
  const [hovered, setHovered] = useState(false);

  useEffect(() => {
    const measure = () => {
      const container = containerRef.current;
      const inner = innerRef.current;
      if (!container || !inner) return;
      setOverflow(Math.max(0, inner.scrollWidth - container.clientWidth));
    };
    measure();
    const observer = new ResizeObserver(measure);
    if (containerRef.current) observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [text]);

  const scrolling = hovered && overflow > 0;
  return (
    <span
      ref={containerRef}
      className={cn("block min-w-0 overflow-hidden", className)}
      title={text}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <span
        ref={innerRef}
        className="inline-block whitespace-nowrap transition-transform ease-linear motion-reduce:transition-none"
        style={{
          transform: scrolling ? `translateX(-${overflow}px)` : "translateX(0)",
          transitionDuration: scrolling ? `${Math.max(500, overflow * 10)}ms` : "150ms",
        }}
      >
        {text}
      </span>
    </span>
  );
}
