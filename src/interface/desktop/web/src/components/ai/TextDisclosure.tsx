import { useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/ui/collapsible";
import { cn } from "@/ui/utils";

/**
 * Text disclosure (mirroring Codex's "Worked for 20s"): the trigger is a line of **low-key
 * plain text** + a rotating chevron, not a bordered/filled button. It collapses the
 * "process" so the final result stands out on its own.
 *
 * Difference from `AiToolActivityDisclosure`: the latter is the tool-group disclosure
 * inside a process card, also using a text disclosure; this component is the more general
 * layer (used for whole-turn process disclosure / nested disclosure).
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
  /** Disclosure label (a line of text). */
  label: ReactNode;
  defaultOpen?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  /** Icon to the left of the label (optional, size self-managed). */
  leading?: ReactNode;
  /** Content to the right of the label, before the chevron (optional, e.g. a count). */
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
