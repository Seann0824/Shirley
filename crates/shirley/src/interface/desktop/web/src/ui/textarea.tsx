import { forwardRef, type TextareaHTMLAttributes } from "react";
import { cn } from "./utils";

export const Textarea = forwardRef<
  HTMLTextAreaElement,
  TextareaHTMLAttributes<HTMLTextAreaElement>
>(function Textarea({ className, ...props }, ref) {
  return (
    <textarea
      ref={ref}
      className={cn(
        "w-full resize-y rounded-control border border-transparent bg-inset p-3.5 text-base leading-relaxed text-ink outline-none transition-[background-color,border-color,box-shadow] duration-150 ease-product placeholder:text-muted hover:bg-selected focus:border-line-strong focus:bg-surface focus:ring-2 focus:ring-ink/8 aria-invalid:border-danger aria-invalid:bg-danger-soft disabled:cursor-not-allowed disabled:opacity-45",
        className,
      )}
      {...props}
    />
  );
});
