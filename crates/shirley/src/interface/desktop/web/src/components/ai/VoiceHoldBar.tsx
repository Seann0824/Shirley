import { useRef } from "react";
import { cn } from "@/ui/utils";
import { HOLD_TAP_MS, pressIntent } from "@/lib/voice-hold";

/**
 * The phone's whole input: one pill that answers "talk or type" by how long the finger stays.
 *
 * This used to be a dedicated hold-to-talk bar with a separate mode switch elsewhere in the
 * action row, which is not how a phone chat input works: you look at one line, and a tap means the
 * keyboard while a press means the microphone. So the bar *is* the field -- it takes the touch,
 * decides between tap and hold (see `pressIntent`), and then either hands the press to the
 * recording overlay or asks the composer to open the editor in this same slot.
 *
 * **Two moments, not one.** `onPress` fires on `pointerdown`, inside the gesture, and that is the
 * only moment a browser i.e. Safari lets us open the microphone at all: `SpeechRecognition.start()`
 * called from a timer or a promise continuation opens the audio session -- the OS shows its
 * recording indicator -- and then never delivers a result. Deferring the microphone until the hold
 * was confirmed is exactly how the second utterance stopped producing text. So the microphone is
 * opened in the gesture, and the 220 ms only decides what that utterance *was*:
 *
 * - released before `HOLD_TAP_MS` (`onTap`) -- it was a tap: drop it, open the keyboard;
 * - still down at `HOLD_TAP_MS` (`onHold`) -- it was a hold: raise the overlay;
 * - wandered off (`onAbandon`) -- it was a scroll: drop it, do nothing else.
 *
 * The release of a confirmed hold belongs to `VoiceHoldOverlay`, which listens on `window`, so a
 * finger that slid off the pill still ends in exactly one place.
 */
export function VoiceHoldBar({
  pressed,
  draft,
  onPress,
  onHold,
  onTap,
  onAbandon,
  className,
}: {
  pressed: boolean;
  /**
   * Words already in the draft but not sent -- what the 编辑 zone left behind, or what the user
   * typed and then dismissed the keyboard from. Tapping the pill edits them, holding appends.
   */
  draft?: string;
  /** Finger down, in the gesture. The microphone opens here and nowhere else. */
  onPress: (origin: { x: number; y: number }) => void;
  /** Still down after `HOLD_TAP_MS`: this is a hold, raise the recording overlay. */
  onHold: (origin: { x: number; y: number }) => void;
  /** A short touch: the utterance is dropped and the same pill becomes a text field. */
  onTap: () => void;
  /** The finger moved too far to be either gesture: drop the utterance silently. */
  onAbandon: () => void;
  className?: string;
}) {
  const originRef = useRef({ x: 0, y: 0 });
  const movedRef = useRef(0);
  const holdTimerRef = useRef<number | null>(null);
  /** True once the hold was confirmed: from then on the overlay owns the release. */
  const heldRef = useRef(false);

  const clearHold = () => {
    if (holdTimerRef.current !== null) {
      window.clearTimeout(holdTimerRef.current);
      holdTimerRef.current = null;
    }
  };

  /** The utterance this press opened is over. Only the first ending of a press counts. */
  const settle = (outcome: () => void) => {
    if (heldRef.current) return;
    heldRef.current = true;
    clearHold();
    outcome();
  };

  return (
    <button
      type="button"
      onPointerDown={(event) => {
        event.preventDefault();
        const origin = { x: event.clientX, y: event.clientY };
        originRef.current = origin;
        movedRef.current = 0;
        heldRef.current = false;
        clearHold();
        onPress(origin);
        holdTimerRef.current = window.setTimeout(() => {
          holdTimerRef.current = null;
          if (heldRef.current) return;
          if (pressIntent(HOLD_TAP_MS, movedRef.current) !== "hold") return;
          heldRef.current = true;
          onHold(origin);
        }, HOLD_TAP_MS);
      }}
      onPointerMove={(event) => {
        if (heldRef.current) return;
        const dx = event.clientX - originRef.current.x;
        const dy = event.clientY - originRef.current.y;
        movedRef.current = Math.max(movedRef.current, Math.hypot(dx, dy));
        // A finger that wandered is scrolling or selecting: neither gesture is ours any more.
        if (pressIntent(0, movedRef.current) === "abandoned") settle(onAbandon);
      }}
      onPointerUp={() => settle(onTap)}
      onPointerCancel={() => settle(onAbandon)}
      // A hold is a long press: without this the browser pops its own copy / selection balloon
      // on top of the recording overlay.
      onContextMenu={(event) => event.preventDefault()}
      aria-label={draft ? "编辑或按住继续说话" : "按住说话，轻点打字"}
      className={cn(
        // No text selection and no scrolling: a 700 ms press on a touch screen otherwise turns
        // into a selection drag, and the release then lands on the wrong element.
        "flex h-9 w-full touch-none select-none items-center justify-center rounded-full bg-canvas text-body-sm transition-interactive [-webkit-touch-callout:none]",
        pressed && "bg-selected text-ink",
        draft ? "text-ink" : "text-muted",
        className,
      )}
    >
      {pressed ? (
        "正在听…"
      ) : draft ? (
        <span className="line-clamp-1 px-3 text-left">{draft}</span>
      ) : (
        "按住 转文字"
      )}
    </button>
  );
}
