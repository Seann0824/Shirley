import { AiBirdIcon } from "./AiBirdIcon";
import { cn } from "@/ui/utils";

export function AiAssistantAvatar({ responding = false }: { responding?: boolean }) {
  return (
    <span
      role="img"
      aria-label={responding ? "Shirley 正在回复" : "Shirley"}
      className="relative inline-flex size-9 shrink-0 items-center justify-center"
    >
      <AiBirdIcon size={32} className="size-8" />
      {responding && (
        <span
          aria-hidden="true"
          className={cn(
            "pointer-events-none absolute inset-0 rounded-full border-2 border-line border-t-accent",
            "animate-spin-soft motion-reduce:animate-none",
          )}
        />
      )}
    </span>
  );
}
