import { cn } from "@/ui/utils";

export function AiAssistantAvatar({ responding = false }: { responding?: boolean }) {
  return (
    <span
      role="img"
      aria-label={responding ? "Shirley 正在回复" : "Shirley"}
      className="relative inline-flex size-9 shrink-0 items-center justify-center"
    >
      <img
        src="/brand/shirley-avatar.png"
        alt=""
        aria-hidden="true"
        width={32}
        height={32}
        className="size-8 rounded-lg object-contain"
        draggable={false}
      />
      {responding && (
        <span
          aria-hidden="true"
          className={cn(
            "pointer-events-none absolute bottom-0 right-0 size-2 rounded-full bg-ink ring-2 ring-canvas",
            "animate-pulse-soft motion-reduce:animate-none",
          )}
        />
      )}
    </span>
  );
}
