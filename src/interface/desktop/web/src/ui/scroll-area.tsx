import * as ScrollAreaPrimitive from "@radix-ui/react-scroll-area";
import { forwardRef, type ComponentPropsWithoutRef, type ElementRef, type Ref } from "react";
import { cn } from "./utils";

type ScrollAreaViewportProps = Omit<
  ComponentPropsWithoutRef<typeof ScrollAreaPrimitive.Viewport>,
  "children" | "className"
> & {
  [attribute: `data-${string}`]: string | number | boolean | undefined;
};

type ScrollAreaProps = ComponentPropsWithoutRef<typeof ScrollAreaPrimitive.Root> & {
  orientation?: "vertical" | "horizontal" | "both";
  viewportClassName?: string;
  viewportProps?: ScrollAreaViewportProps;
  scrollbarClassName?: string;
  viewportRef?: Ref<ElementRef<typeof ScrollAreaPrimitive.Viewport>>;
};

export const ScrollArea = forwardRef<ElementRef<typeof ScrollAreaPrimitive.Root>, ScrollAreaProps>(
  function ScrollArea(
    {
      children,
      className,
      orientation = "vertical",
      viewportClassName,
      viewportProps,
      viewportRef,
      scrollbarClassName,
      type = "hover",
      ...props
    },
    ref,
  ) {
    const showVertical = orientation === "vertical" || orientation === "both";
    const showHorizontal = orientation === "horizontal" || orientation === "both";

    return (
      <ScrollAreaPrimitive.Root
        ref={ref}
        className={cn("relative overflow-hidden", className)}
        data-scroll-area={orientation}
        type={type}
        {...props}
      >
        <ScrollAreaPrimitive.Viewport
          {...viewportProps}
          ref={viewportRef}
          className={cn(
            "h-full w-full [scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
            // Radix uses a table wrapper; vertical content must shrink to the viewport.
            orientation === "vertical" && "[&>div]:!block",
            viewportClassName,
          )}
          data-scroll-area-viewport
        >
          {children}
        </ScrollAreaPrimitive.Viewport>
        {showVertical && (
          <ScrollAreaScrollbar orientation="vertical" className={scrollbarClassName} />
        )}
        {showHorizontal && (
          <ScrollAreaScrollbar orientation="horizontal" className={scrollbarClassName} />
        )}
        {showVertical && showHorizontal && (
          <ScrollAreaPrimitive.Corner className="bg-transparent" />
        )}
      </ScrollAreaPrimitive.Root>
    );
  },
);

const ScrollAreaScrollbar = forwardRef<
  ElementRef<typeof ScrollAreaPrimitive.Scrollbar>,
  ComponentPropsWithoutRef<typeof ScrollAreaPrimitive.Scrollbar>
>(function ScrollAreaScrollbar({ className, orientation = "vertical", ...props }, ref) {
  return (
    <ScrollAreaPrimitive.Scrollbar
      ref={ref}
      orientation={orientation}
      className={cn(
        "absolute z-10 flex touch-none select-none p-0.5 motion-safe:data-[state=hidden]:animate-scrollbar-out motion-safe:data-[state=visible]:animate-scrollbar-in",
        orientation === "vertical" && "top-0 right-0 h-full w-2.5",
        orientation === "horizontal" && "bottom-0 left-0 h-2.5 w-full flex-col",
        className,
      )}
      data-scroll-area-scrollbar={orientation}
      {...props}
    >
      <ScrollAreaPrimitive.Thumb className="relative flex-1 rounded-full bg-ink/20 transition-colors hover:bg-ink/35 before:absolute before:top-1/2 before:left-1/2 before:size-full before:min-h-11 before:min-w-11 before:-translate-x-1/2 before:-translate-y-1/2" />
    </ScrollAreaPrimitive.Scrollbar>
  );
});
