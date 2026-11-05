/**
 * The hold-to-talk gesture, reduced to pure decisions.
 *
 * Two questions, two functions: *what does letting go mean* (`zoneAt`) and *what were the
 * words* (`composeUtterance`). Both live here rather than in a component because releasing the
 * finger once lost the utterance entirely, and neither answer was reachable from a test.
 */

/** The three outcomes of letting go. Sending is what happens without lifting. */
export type VoiceHoldZone = "send" | "cancel" | "edit";

/** A rectangle in viewport coordinates -- what `getBoundingClientRect` already returns. */
export type HoldTarget = { left: number; top: number; right: number; bottom: number };

/**
 * How far the finger has to rise above the input row before letting go means anything but send.
 *
 * The first version asked the finger to land **on** a target, which on a phone is a different
 * gesture: your thumb rests on the bar, the bar is at the bottom edge, and the two targets were
 * drawn right there -- so the only way to hit one was to slide sideways at the same height, never
 * up. Lifting is both easier to perform and the thing the user asked for: 拖到上面才取消.
 */
export const HOLD_LIFT_PX = 72;

/**
 * How far outside the 编辑 button still counts as being on it.
 *
 * A thumb lands tip-first and covers the button completely, so its contact point is never the
 * centre of what you see under it. 20px is ordinary touch slop; it is also small enough that the
 * button's two sides cannot both be "the nearest" of anything.
 */
export const HOLD_TARGET_SLOP_PX = 20;

/** The y a finger has to be *above* to be cancelling rather than sending. */
export function liftLineOf(originY: number): number {
  return originY - HOLD_LIFT_PX;
}

/** Distance from a point to a rectangle: 0 on the rectangle, growing outward in every direction. */
function distanceTo(target: HoldTarget, x: number, y: number): number {
  const dx = Math.max(target.left - x, 0, x - target.right);
  const dy = Math.max(target.top - y, 0, y - target.bottom);
  return Math.hypot(dx, dy);
}

/**
 * Which outcome letting go would have right now.
 *
 * - Still at the height of the input row: **send**. The finger never left the bar, which is the
 *   gesture's default and must stay the easy one.
 * - Lifted above the line and on 编辑: **edit** -- the deliberate "let me fix a word first".
 * - Lifted above the line anywhere else: **cancel**. This is the part you can do without aiming,
 *   which is what makes it usable while your thumb is covering the screen.
 */
export function zoneAt(input: {
  x: number;
  y: number;
  /** `liftLineOf(origin.y)`; above this is the cancel band. */
  liftLine: number;
  cancel: HoldTarget | null;
  edit: HoldTarget | null;
}): VoiceHoldZone {
  const { cancel, edit, liftLine, x, y } = input;
  if (y >= liftLine) return "send";
  if (edit && distanceTo(edit, x, y) <= HOLD_TARGET_SLOP_PX) return "edit";
  if (cancel && distanceTo(cancel, x, y) <= HOLD_TARGET_SLOP_PX) return "cancel";
  return "cancel";
}

/**
 * The words of one utterance.
 *
 * The recogniser streams a settled part and a live partial, and both are text the user watched
 * appear on screen, so both are what they said. Delivering only the settled part is why a short
 * Chinese sentence -- which often never settles before the finger comes up -- was released into
 * nothing at all.
 */
export function composeUtterance(...parts: string[]): string {
  return parts
    .map((part) => part.trim())
    .filter((part) => part.length > 0)
    .join(" ");
}

/**
 * How long a touch has to last before it stops being a tap and becomes a hold.
 *
 * One field now answers two questions -- *type a message* and *talk to it* -- and time is the
 * only thing that separates them. 220ms is short enough that a deliberate press feels immediate,
 * and long enough that a tap on a phone never turns into a recording.
 */
export const HOLD_TAP_MS = 220;

/**
 * How far the finger may drift and still count as a tap.
 *
 * A thumb never lands perfectly still. Past this the touch was a scroll or a selection, and
 * neither of those may open the microphone or pop the keyboard.
 */
export const HOLD_TAP_SLOP_PX = 10;

/** What a touch means once it is over (or once it has lasted long enough). */
export type PressIntent = "tap" | "hold" | "abandoned";

/**
 * The tap-or-hold rule, in one place.
 *
 * `abandoned` is a third answer on purpose: a finger that wandered is neither a tap (the user did
 * not ask for the keyboard) nor a hold (they did not ask to be recorded). Both outcomes stay shut
 * and the gesture simply never happened.
 */
export function pressIntent(heldMs: number, movedPx: number): PressIntent {
  if (movedPx > HOLD_TAP_SLOP_PX) return "abandoned";
  return heldMs >= HOLD_TAP_MS ? "hold" : "tap";
}
