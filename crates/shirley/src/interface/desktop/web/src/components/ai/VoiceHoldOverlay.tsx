import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Loader2, Pencil, X } from "lucide-react";
import { cn } from "@/ui/utils";
import type { VoiceInput } from "@/lib/useVoiceInput";
import { HOLD_LIFT_PX, liftLineOf, zoneAt, type HoldTarget, type VoiceHoldZone } from "@/lib/voice-hold";

export type { VoiceHoldZone } from "@/lib/voice-hold";

/** Short vibration when the finger crosses into a zone. Silence is the gesture's worst enemy. */
function buzz(pattern: number | number[]): void {
  try {
    navigator.vibrate?.(pattern);
  } catch {
    // Desktop, or a browser without the API: the visual state is the feedback.
  }
}

/**
 * Fullscreen dictation: the words appear huge because while you are talking to it you are not
 * reading the conversation -- you are checking whether it heard you.
 *
 * The gesture is the phone gesture the user already has in their hand: press the bar and talk,
 * **lift the finger above the line to cancel**, and let go without lifting to send. The two
 * outcomes that are not "send" live at the two edges, small and out of the way, because they are
 * the exceptions: 取消 on the left (where the lift already lands you) and 编辑 on the right for
 * "let me fix a word before this goes out".
 *
 * The earlier version drew two half-width boxes on the bottom row and asked the finger to land
 * on them. On a phone that was the wrong shape twice over: they were the size of the primary
 * action while being the rare ones, and the only way to hit them was to slide *sideways*, because
 * the thumb starts on the bar at the very bottom of the screen. Lifting is what a thumb does
 * naturally, so lifting is what the gesture reads now -- `HOLD_LIFT_PX` above where the press
 * started, drawn as a line so the rule is visible rather than guessed.
 *
 * Three states, three different things to say, and they must not look alike:
 *
 * - `starting` -- the microphone is being opened (and maybe authorised). Saying "正在听…" here is
 *   a lie: the user talks into a capture that does not exist yet.
 * - `listening` -- live. Elapsed time ticks, partial words stream in.
 * - `transcribing` -- the finger is up but the audio is still becoming text. This is why the
 *   overlay outlives the press (`App` keeps it mounted while `state === "transcribing"`):
 *   dropping it on release left a several-second hole where nothing at all happened.
 */
export function VoiceHoldOverlay({
  voice,
  origin,
  armed,
  onFinish,
}: {
  voice: VoiceInput;
  origin: { x: number; y: number };
  /** False once the finger is up: the overlay stays for the transcript, but takes no pointers. */
  armed: boolean;
  onFinish: (zone: VoiceHoldZone) => void;
}) {
  const [zone, setZone] = useState<VoiceHoldZone>("send");
  const [elapsed, setElapsed] = useState(0);
  const [targets, setTargets] = useState<{ cancel: HoldTarget | null; edit: HoldTarget | null }>({
    cancel: null,
    edit: null,
  });
  const cancelRef = useRef<HTMLDivElement | null>(null);
  const editRef = useRef<HTMLDivElement | null>(null);
  const targetsRef = useRef(targets);
  targetsRef.current = targets;
  const liftLine = liftLineOf(origin.y);
  const liftLineRef = useRef(liftLine);
  liftLineRef.current = liftLine;
  const onFinishRef = useRef(onFinish);
  onFinishRef.current = onFinish;
  const zoneRef = useRef<VoiceHoldZone>("send");
  zoneRef.current = zone;

  const error = voice.error.trim();
  const transcribing = voice.state === "transcribing";
  const starting = voice.state === "starting";

  // Measure once per hold (+ on rotation, which is a real case for a phone held in one hand).
  useEffect(() => {
    const box = (element: HTMLDivElement | null): HoldTarget | null => {
      const rect = element?.getBoundingClientRect();
      return rect ? { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom } : null;
    };
    const measure = () => setTargets({ cancel: box(cancelRef.current), edit: box(editRef.current) });
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("orientationchange", measure);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("orientationchange", measure);
    };
  }, []);

  // Elapsed time is the only proof that a hold is doing something while the transcriber is
  // quiet: a fixed "正在听…" is indistinguishable from a stuck overlay after three seconds.
  useEffect(() => {
    if (!armed || starting) return;
    const began = Date.now();
    setElapsed(0);
    const tick = window.setInterval(() => setElapsed(Date.now() - began), 250);
    return () => window.clearInterval(tick);
  }, [armed, starting]);

  // A denied permission leaves `holding` true with nothing recording: without this the user has
  // to work out that releasing is the way out. Show the reason, then close on their behalf.
  useEffect(() => {
    if (!error) return;
    const id = window.setTimeout(() => onFinishRef.current("cancel"), 1800);
    return () => window.clearTimeout(id);
  }, [error]);

  useEffect(() => {
    if (!armed) return;
    const zoneOf = (event: PointerEvent): VoiceHoldZone =>
      zoneAt({
        x: event.clientX,
        y: event.clientY,
        liftLine: liftLineRef.current,
        cancel: targetsRef.current.cancel,
        edit: targetsRef.current.edit,
      });
    const move = (event: PointerEvent) => {
      // Touch scrolling would otherwise drag the transcript behind the overlay.
      event.preventDefault();
      const next = zoneOf(event);
      if (next !== zoneRef.current) {
        zoneRef.current = next;
        // Crossing the line has to be felt, not just drawn: the finger covers the screen.
        buzz(8);
      }
      setZone(next);
    };
    const up = (event: PointerEvent) => {
      buzz(12);
      onFinishRef.current(zoneOf(event));
    };
    const abort = () => onFinishRef.current("cancel");
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", abort);
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", abort);
    };
  }, [armed]);

  const said = voice.heard.trim();
  const seconds = Math.floor(elapsed / 1000);
  const clock = `${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
  const hint = error
    ? "松手 关闭"
    : zone === "cancel"
      ? "松手 取消"
      : zone === "edit"
        ? "松手 编辑"
        : "松手 发送";

  const bubble = error
    ? { text: error, tone: "bg-canvas text-danger-ink" }
    : starting
      ? { text: "正在打开麦克风…", tone: "bg-accent/40 text-ink/70" }
      : transcribing
        ? { text: "转写中…", tone: "bg-accent text-ink" }
        : said
          ? { text: said, tone: "bg-accent text-ink" }
          : { text: "正在听…", tone: "bg-accent/40 text-ink/70" };

  // Two small targets, one per edge: the lift lands on 取消 by default, so it only has to be
  // *readable*, while 编辑 is the one that needs aiming and gets a real rectangle.
  const targetClass = (active: boolean, tone: "danger" | "accent") =>
    cn(
      "flex h-9 items-center gap-1.5 rounded-full border px-3.5 text-body-sm transition-interactive",
      tone === "danger"
        ? active
          ? "border-danger-ink/60 bg-danger-ink/20 text-canvas"
          : "border-canvas/25 text-canvas/55"
        : active
          ? "border-accent bg-accent text-ink"
          : "border-canvas/25 text-canvas/55",
    );

  return createPortal(
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Dictation"
      className="fixed inset-0 z-50 flex animate-overlay-in touch-none select-none flex-col bg-ink/92 backdrop-blur-sm [-webkit-touch-callout:none]"
    >
      <div className="flex items-center justify-between px-4 pt-[max(0.75rem,env(safe-area-inset-top))]">
        <div ref={cancelRef} aria-hidden="true" className={targetClass(zone === "cancel", "danger")}>
          <X className="size-3.5" aria-hidden="true" />
          取消
        </div>
        <div ref={editRef} aria-hidden="true" className={targetClass(zone === "edit", "accent")}>
          <Pencil className="size-3.5" aria-hidden="true" />
          编辑
        </div>
      </div>

      <div className="flex flex-1 flex-col items-center justify-center gap-4 px-7">
        <div
          aria-live="polite"
          className={cn(
            "max-h-[45dvh] w-full animate-dialog-in overflow-hidden rounded-hero px-6 py-5 text-center text-title-sm leading-snug font-medium break-words",
            // The accent fill is this system's "live" surface, and it is the one pair with
            // guaranteed contrast against its own text -- this bubble must be readable from
            // arm's length, which is exactly the distance the phone is held at.
            bubble.tone,
          )}
        >
          <span className="line-clamp-4">{bubble.text}</span>
        </div>
        <div className="flex items-center gap-2 text-body-sm text-canvas/70">
          {transcribing || starting ? (
            <Loader2 className="size-3.5 animate-spin" aria-hidden="true" />
          ) : null}
          <span className="tabular-nums">{clock}</span>
          <span aria-hidden="true">·</span>
          <span>{hint}</span>
        </div>
      </div>

      {/* The rule itself, drawn: `HOLD_LIFT_PX` above where the press started. Without the line
          "lift to cancel" is an invisible threshold, and an invisible threshold is a gesture
          nobody trusts. It sits above the thumb, so it never hides under the finger. */}
      {armed && !error ? (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-x-0 flex flex-col items-center gap-1.5"
          style={{ top: liftLine }}
        >
          <span
            className={cn(
              "text-body-sm transition-interactive",
              zone === "cancel" ? "text-canvas/80" : "text-canvas/35",
            )}
          >
            抬到上面才取消
          </span>
          <div
            className={cn(
              "w-[min(22rem,80vw)] border-t border-dashed transition-interactive",
              zone === "cancel" ? "border-canvas/60" : "border-canvas/20",
            )}
          />
        </div>
      ) : null}
      <div className="h-[max(1.25rem,env(safe-area-inset-bottom))]" />
    </div>,
    document.body,
  );
}
