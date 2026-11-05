/**
 * One recogniser, reused for the life of the screen.
 *
 * This exists because of a Safari rule we kept tripping over: a recogniser owns an **audio
 * session**, and that session outlives the JavaScript call. `abort()` returns immediately, but
 * the session is still closing for a while afterwards, and a `start()` issued inside that window
 * is either refused outright or -- far worse -- accepted: the OS microphone indicator lights up,
 * the audio session is open, and not a single result ever arrives. That is the exact symptom of
 * "I sent one message and now dictation needs a few seconds to come back".
 *
 * The old wiring made this the common case rather than a corner: **every press constructed a new
 * recogniser**, so each press raced the previous one's teardown. Reusing one object removes the
 * race; queueing the press behind the previous session's `end` makes the remaining wait
 * deterministic and short instead of "until the OS gets round to it".
 *
 * So the contract is small and stated once, here:
 *
 * - **One handle, created once.** Never `new` per utterance.
 * - **The press wins.** `start()` while a session is open aborts that session and starts again
 *   *in the same call*. The open session belongs to an utterance that is already over -- its
 *   words were handed to the composer the moment the finger came up -- so there is nothing left
 *   to collect from it, and waiting for its `end` is precisely the wait the user feels as
 *   "the second press failed". Queueing is only the fallback for a start the platform refuses
 *   even after an abort.
 * - `ended()` is how the browser tells us a session really closed. It pays back a start that was
 *   queued, and closes the books otherwise.
 * - `abort()` still leaves the session open until `end` arrives -- the browser decides when the
 *   audio session is gone, and pretending otherwise is what hid this bug for four rounds.
 *   `reset()` is for a session we have given up on (a dead recogniser).
 *
 * Pure bookkeeping: no DOM, no timers, no React. `recognizer.test.ts` drives it with a fake.
 */

/** The three lifecycle calls this module needs; the vendor recogniser has all of them. */
export type RecognizerHandle = {
  start: () => void;
  stop: () => void;
  abort: () => void;
};

/** What a `start()` actually did. */
export type StartOutcome =
  /** Audio capture is starting now. */
  | "started"
  /** The previous session is still closing; this start is owed and will fire on `end`. */
  | "queued"
  /** The handle refused (threw) -- the caller reports it. */
  | "refused";

export class RecognizerSlot {
  private handle: RecognizerHandle | null = null;
  /** A session is live from `start()` until the browser reports `end`. */
  private open = false;
  /** A press arrived while the previous session was still closing. */
  private owed = false;

  /** Bind the one handle. Called when the recogniser is created, and on every open. */
  attach(handle: RecognizerHandle): void {
    this.handle = handle;
  }

  isOpen(): boolean {
    return this.open;
  }

  start(): StartOutcome {
    const handle = this.handle;
    if (!handle) return "refused";
    if (this.open) {
      // The previous utterance is over and was delivered; end its session hard and start again
      // now. `abort()` does not wait for final results, which is the whole point: the press is
      // answered in this call instead of whenever the platform lets the audio session go.
      this.hangUp(handle);
      if (this.begin(handle)) return "started";
      // The browser is still tearing the old session down. Only now is waiting right -- remember
      // the intent, let `ended()` pay it back, and leave the caller's watchdog armed.
      this.owed = true;
      return "queued";
    }
    if (this.begin(handle)) return "started";
    // Not open by our books, yet it refused: the platform still holds a session we dropped.
    // Clear the way once and take it back; a second refusal is a real one and is reported.
    this.hangUp(handle);
    return this.begin(handle) ? "started" : "refused";
  }

  /**
   * The browser reported `end`. Returns true when this call paid back an owed start, i.e. audio
   * is starting again and the caller should re-arm its watchdog.
   */
  ended(): boolean {
    this.open = false;
    if (!this.owed) return false;
    this.owed = false;
    const handle = this.handle;
    if (!handle) return false;
    return this.begin(handle);
  }

  /**
   * Graceful end: let the session close on its own and report `end` when it does.
   *
   * Anything owed is dropped, because `stop()` is also how a generation ends. Without this, a
   * press that queues and is then abandoned (the finger came up while the previous session was
   * still closing) would be started again by the very `end` it was waiting for.
   */
  stop(): void {
    this.owed = false;
    if (!this.open) return;
    try {
      this.handle?.stop();
    } catch {
      // Nothing to stop.
    }
  }

  /** Harsh end, but the session is still open until `end` arrives -- that is the whole lesson. */
  abort(): void {
    this.owed = false;
    if (!this.open) return;
    try {
      this.handle?.abort();
    } catch {
      // Already dead.
    }
  }

  /** Give up on this session: stop waiting for an `end` that may never come. */
  reset(): void {
    this.abort();
    this.open = false;
    this.owed = false;
  }

  /** Unmount: nothing may outlive the screen. */
  dispose(): void {
    this.reset();
    this.handle = null;
  }

  /** Abort a session and forget it. The browser may still be closing it; that is its business. */
  private hangUp(handle: RecognizerHandle): void {
    try {
      handle.abort();
    } catch {
      // Already dead.
    }
    this.open = false;
    this.owed = false;
  }

  private begin(handle: RecognizerHandle): boolean {
    try {
      handle.start();
      this.open = true;
      return true;
    } catch {
      // Thrown when the browser still considers a session alive (`InvalidStateError`). The next
      // press is a better answer than a retry loop.
      this.open = false;
      return false;
    }
  }
}
