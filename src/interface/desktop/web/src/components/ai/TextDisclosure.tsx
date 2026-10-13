import { useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";

/**
 * 文字折叠（对标 Codex 的「Worked for 20s」）：触发器是一行**低调的纯文字** +
 * 旋转 chevron，不是带边框/底色的按钮。用于把「过程」收起，让最终结果自己站出来。
 *
 * 与 `AiToolActivityDisclosure` 的区别：后者是过程卡片里的工具组折叠，同样走文字
 * 折叠；本组件是更通用的一层（整轮过程折叠 / 嵌套折叠都用它）。
 */
export function TextDisclosure({
  label,
  defaultOpen = false,
  open: controlledOpen,
  onOpenChange,
  leading,
  trailing,
  className,
  triggerClassName,
  contentClassName,
  children,
}: {
  /** 折叠标题（一行文字）。 */
  label: ReactNode;
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** 标题左侧图标（可选，尺寸自控）。 */
  leading?: ReactNode;
  /** 标题右侧、chevron 之前的内容（可选，如计数）。 */
  trailing?: ReactNode;
  className?: string;
  triggerClassName?: string;
  contentClassName?: string;
  children: ReactNode;
}) {
  const [internalOpen, setInternalOpen] = useState(defaultOpen);
  const open = controlledOpen ?? internalOpen;
  const setOpen = (next: boolean) => {
    if (controlledOpen === undefined) setInternalOpen(next);
    onOpenChange?.(next);
  };
  return (
    <Collapsible open={open} onOpenChange={setOpen} className={className}>
      <CollapsibleTrigger
        className={cn(
          "group inline-flex items-center gap-1.5 rounded-control text-body-sm text-muted",
          "transition-colors hover:text-ink focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
          triggerClassName,
        )}
      >
        {leading}
        <span className="min-w-0 truncate">{label}</span>
        {trailing}
        <ChevronRight
          size={14}
          aria-hidden="true"
          className={cn("shrink-0 transition-transform duration-150", open && "rotate-90")}
        />
      </CollapsibleTrigger>
      <CollapsibleContent className={cn("data-[state=closed]:hidden", contentClassName)}>
        {children}
      </CollapsibleContent>
    </Collapsible>
  );
}
