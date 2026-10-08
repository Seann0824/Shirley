import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { AiTranscriptMessage } from "@/components/ai/AiConversationTranscript";
import { cn } from "@/ui/utils";

type ConversationTurn = {
  id: string;
  question: string;
  answer: string;
};

const RESTING_MARKER_WIDTH = 6;

function normalizePreview(content: string) {
  return content.replace(/\s+/g, " ").trim();
}

function buildConversationTurns(messages: AiTranscriptMessage[]) {
  const turns: ConversationTurn[] = [];
  for (const message of messages) {
    const content = normalizePreview(message.content);
    if (message.role === "user") {
      turns.push({ id: String(message.id), question: content || "未命名问题", answer: "" });
      continue;
    }
    const current = turns.at(-1);
    if (current && !current.answer) {
      current.answer = content;
      continue;
    }
    turns.push({ id: String(message.id), question: "AI 回答", answer: content });
  }
  return turns;
}

function findMessageElement(viewport: HTMLDivElement, messageId: string) {
  return Array.from(viewport.querySelectorAll<HTMLElement>("[data-message-id]")).find(
    (element) => element.dataset.messageId === messageId,
  );
}

export function AiConversationMinimap({
  messages,
  viewportRef,
}: {
  messages: AiTranscriptMessage[];
  viewportRef: RefObject<HTMLDivElement | null>;
}) {
  const turns = useMemo(() => buildConversationTurns(messages), [messages]);
  const [activeTurnId, setActiveTurnId] = useState(() => turns.at(-1)?.id || "");
  const [previewTurnId, setPreviewTurnId] = useState<string | null>(null);
  const markerStackRef = useRef<HTMLDivElement>(null);
  const scrubbingRef = useRef(false);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || turns.length === 0) return;
    let frame = 0;
    const update = () => {
      frame = 0;
      const viewportRect = viewport.getBoundingClientRect();
      const readingLine = viewportRect.top + Math.min(112, viewportRect.height * 0.35);
      let nextActive = turns[0].id;
      for (const turn of turns) {
        const element = findMessageElement(viewport, turn.id);
        if (!element || element.getBoundingClientRect().top > readingLine) break;
        nextActive = turn.id;
      }
      setActiveTurnId(nextActive);
    };
    const scheduleUpdate = () => {
      if (frame) return;
      frame = window.requestAnimationFrame(update);
    };
    const resizeObserver = new ResizeObserver(scheduleUpdate);
    resizeObserver.observe(viewport);
    if (viewport.firstElementChild) resizeObserver.observe(viewport.firstElementChild);
    viewport.addEventListener("scroll", scheduleUpdate, { passive: true });
    scheduleUpdate();
    return () => {
      if (frame) window.cancelAnimationFrame(frame);
      resizeObserver.disconnect();
      viewport.removeEventListener("scroll", scheduleUpdate);
    };
  }, [turns, viewportRef]);

  if (turns.length === 0) return null;

  const previewTurn = turns.find((turn) => turn.id === previewTurnId) || null;
  const focusedTurnId = previewTurnId || activeTurnId;
  const focusedTurnIndex = turns.findIndex((turn) => turn.id === focusedTurnId);
  const isInteracting = previewTurnId !== null;
  const markerStackHeight = turns.length * 6;

  function jumpToTurn(turn: ConversationTurn) {
    const viewport = viewportRef.current;
    if (!viewport) return;
    const element = findMessageElement(viewport, turn.id);
    if (!element) return;
    const top =
      viewport.scrollTop +
      element.getBoundingClientRect().top -
      viewport.getBoundingClientRect().top -
      24;
    viewport.scrollTo({
      top,
      behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth",
    });
    setActiveTurnId(turn.id);
    setPreviewTurnId(null);
  }

  function selectTurnAt(clientY: number) {
    const markerStack = markerStackRef.current;
    if (!markerStack) return null;
    const rect = markerStack.getBoundingClientRect();
    const relativeY = Math.min(rect.height - 1, Math.max(0, clientY - rect.top));
    const step = rect.height / turns.length;
    const index = Math.min(turns.length - 1, Math.floor(relativeY / step));
    const turn = turns[index];
    setPreviewTurnId(turn.id);
    return turn;
  }

  return (
    <nav
      className="absolute inset-y-0 -left-5 z-20 flex w-10 items-center"
      aria-label="对话轮次导航"
      data-ai-turn-minimap
    >
      <div
        className="relative w-full touch-none select-none"
        style={{ height: `${Math.max(markerStackHeight + 24, 44)}px` }}
        data-ai-turn-scrubber
        onPointerEnter={(event) => {
          if (event.pointerType === "mouse") selectTurnAt(event.clientY);
        }}
        onPointerMove={(event) => {
          if (event.pointerType === "mouse" || scrubbingRef.current) {
            selectTurnAt(event.clientY);
          }
        }}
        onPointerLeave={() => {
          if (!scrubbingRef.current) setPreviewTurnId(null);
        }}
        onPointerDown={(event) => {
          event.preventDefault();
          scrubbingRef.current = true;
          event.currentTarget.setPointerCapture(event.pointerId);
          selectTurnAt(event.clientY);
        }}
        onPointerUp={(event) => {
          event.preventDefault();
          const turn = selectTurnAt(event.clientY);
          scrubbingRef.current = false;
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
          if (turn) jumpToTurn(turn);
        }}
        onPointerCancel={(event) => {
          scrubbingRef.current = false;
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
          setPreviewTurnId(null);
        }}
      >
        <div
          ref={markerStackRef}
          className="absolute top-1/2 left-0 w-full -translate-y-1/2"
          style={{ height: `${markerStackHeight}px` }}
        >
          {turns.map((turn, index) => {
            const distance = Math.abs(index - focusedTurnIndex);
            const markerWidth = !isInteracting
              ? RESTING_MARKER_WIDTH
              : distance === 0
                ? 20
                : distance === 1
                  ? 14
                  : distance === 2
                    ? 10
                    : 8;
            return (
              <button
                key={turn.id}
                type="button"
                className="group pointer-events-none absolute left-2 flex h-1.5 w-8 items-center outline-none"
                style={{ top: `${index * 6}px` }}
                aria-label={`跳到第 ${index + 1} 轮：${turn.question}`}
                aria-current={turn.id === activeTurnId ? "true" : undefined}
                title={turn.question}
                data-turn-anchor-id={turn.id}
                onFocus={() => setPreviewTurnId(turn.id)}
                onBlur={() => setPreviewTurnId(null)}
                onClick={() => jumpToTurn(turn)}
              >
                <span
                  className={cn(
                    "h-0.5 transition-[width,background-color] duration-150 ease-product group-focus-visible:bg-ink motion-reduce:transition-none",
                    distance === 0 ? "bg-ink" : "bg-line-strong",
                  )}
                  style={{ width: `${markerWidth}px` }}
                  aria-hidden="true"
                />
              </button>
            );
          })}
        </div>
      </div>
      {previewTurn && (
        <div
          className="pointer-events-none absolute top-1/2 left-10 w-64 -translate-y-1/2 rounded-overlay bg-overlay p-3 text-left shadow-overlay"
          role="tooltip"
          data-ai-turn-preview
        >
          <p className="m-0 line-clamp-2 text-body-sm font-medium text-ink">
            {previewTurn.question}
          </p>
          <p className="mt-1.5 mb-0 line-clamp-3 text-body-sm leading-relaxed text-muted">
            {previewTurn.answer || "这轮回答仍在生成或尚未开始。"}
          </p>
        </div>
      )}
    </nav>
  );
}
