import { useEffect, useRef, useState } from "react";
import { cn } from "./utils";

/**
 * 单行文本：宽度不够时截断，**鼠标悬停时横向滚动**把全文带出来（跑马灯）。
 *
 * 为什么不用原生 `title`：原生 tooltip 有 ~1s 延迟、样式不可控、且与右键菜单
 * 抢注意力。这里就地滚动，零延迟、零额外 UI。仅在真正溢出时才滚动——没溢出
 * 时静止不动，避免无意义抖动。`prefers-reduced-motion` 下退化为不滚动。
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
