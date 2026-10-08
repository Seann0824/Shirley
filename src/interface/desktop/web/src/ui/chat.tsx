import { cva, type VariantProps } from "class-variance-authority";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentPropsWithoutRef,
  type HTMLAttributes,
} from "react";
import { cn } from "./utils";
import { ScrollArea } from "./scroll-area";

const messageVariants = cva("flex min-w-0", {
  variants: {
    align: {
      start: "justify-start",
      end: "justify-end",
    },
  },
  defaultVariants: { align: "start" },
});

export function Message({
  align,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & VariantProps<typeof messageVariants>) {
  return <div className={cn(messageVariants({ align }), className)} {...props} />;
}

const bubbleVariants = cva("min-w-0 text-body-sm leading-relaxed", {
  variants: {
    variant: {
      default: "max-w-[92%] rounded-card bg-primary px-4 py-3 text-primary-foreground",
      ghost: "w-full text-ink",
      secondary: "max-w-[92%] rounded-card bg-subtle px-4 py-3 text-ink",
      outline: "max-w-[92%] rounded-card border border-line bg-canvas px-4 py-3 text-ink",
    },
  },
  defaultVariants: { variant: "ghost" },
});

export function Bubble({
  variant,
  className,
  ...props
}: HTMLAttributes<HTMLDivElement> & VariantProps<typeof bubbleVariants>) {
  return <div className={cn(bubbleVariants({ variant }), className)} {...props} />;
}

export function MessageScroller({
  className,
  viewportClassName,
  ...props
}: ComponentPropsWithoutRef<typeof ScrollArea>) {
  return (
    <ScrollArea
      data-slot="message-scroller"
      className={cn("min-h-0", className)}
      viewportClassName={cn("rounded-none [&>div]:!block [&>div]:!w-full", viewportClassName)}
      scrollbarClassName="w-1.5 py-1"
      {...props}
    />
  );
}

export function MessageScrollerContent({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("flex w-full min-w-0 flex-col gap-6", className)} {...props} />;
}

export function Marker({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="message-marker"
      role="status"
      className={cn(
        "flex min-w-0 items-start gap-3 rounded-control border border-line bg-canvas px-3 py-2.5 text-body-sm text-ink",
        className,
      )}
      {...props}
    />
  );
}

export function useMessageAutoScroll({
  forceKey,
  contentKey,
}: {
  forceKey: string;
  contentKey: string;
}) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const followingRef = useRef(true);
  const [followingLatest, setFollowingLatest] = useState(true);

  const scrollToLatest = useCallback((behavior: ScrollBehavior = "smooth") => {
    const viewport = viewportRef.current;
    if (!viewport) return;
    followingRef.current = true;
    setFollowingLatest(true);
    viewport.scrollTo({ top: viewport.scrollHeight, behavior });
  }, []);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) return;
    const updateFollowing = () => {
      const distance = viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop;
      const next = distance <= 72;
      followingRef.current = next;
      setFollowingLatest(next);
    };
    viewport.addEventListener("scroll", updateFollowing, { passive: true });
    return () => viewport.removeEventListener("scroll", updateFollowing);
  }, []);

  useLayoutEffect(() => {
    followingRef.current = true;
    setFollowingLatest(true);
    const frame = window.requestAnimationFrame(() => scrollToLatest("auto"));
    return () => window.cancelAnimationFrame(frame);
  }, [forceKey, scrollToLatest]);

  useLayoutEffect(() => {
    if (!followingRef.current) return;
    const frame = window.requestAnimationFrame(() => scrollToLatest("auto"));
    return () => window.cancelAnimationFrame(frame);
  }, [contentKey, scrollToLatest]);

  return { viewportRef, followingLatest, scrollToLatest };
}

export function AttachmentGroup({
  className,
  children,
  ...props
}: ComponentPropsWithoutRef<typeof ScrollArea>) {
  return (
    <ScrollArea
      orientation="horizontal"
      data-slot="attachment-group"
      className={cn("min-w-0", className)}
      scrollbarClassName="h-1.5 px-1"
      {...props}
    >
      <div className="flex w-max min-w-full gap-2 pb-2">{children}</div>
    </ScrollArea>
  );
}

export function Attachment({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="attachment"
      className={cn(
        "flex min-w-48 max-w-72 shrink-0 items-center gap-3 rounded-control border border-line bg-canvas p-2.5 text-ink",
        className,
      )}
      {...props}
    />
  );
}

export function AttachmentMedia({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      data-slot="attachment-media"
      className={cn(
        "flex size-10 shrink-0 items-center justify-center overflow-hidden rounded-control bg-subtle text-muted",
        className,
      )}
      {...props}
    />
  );
}

export function AttachmentContent({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("min-w-0 flex-1", className)} {...props} />;
}

export function AttachmentTitle({ className, ...props }: HTMLAttributes<HTMLParagraphElement>) {
  return <p className={cn("m-0 truncate text-body-sm text-ink", className)} {...props} />;
}

export function AttachmentDescription({
  className,
  ...props
}: HTMLAttributes<HTMLParagraphElement>) {
  return <p className={cn("mt-0.5 mb-0 truncate text-caption text-muted", className)} {...props} />;
}
