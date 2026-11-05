import assert from "node:assert/strict";
import test from "node:test";
import { RecognizerSlot } from "./recognizer.ts";

/**
 * A recogniser that only records what was asked of it.
 *
 * `refuseStarts` is how the platform's `InvalidStateError` is simulated: the browser is still
 * tearing the previous audio session down, so `start()` throws. That is the *only* reason a start
 * may be deferred -- everything else is answered by the press itself.
 */
function fakeRecognizer(options: { refuse?: boolean } = {}) {
  const calls: string[] = [];
  /** Flipped by a test to say "the platform still holds an audio session": `start()` throws. */
  const state = { refuse: options.refuse ?? false };
  const handle = {
    calls,
    state,
    start: () => {
      calls.push("start");
      if (state.refuse) throw new Error("InvalidStateError");
    },
    stop: () => {
      calls.push("stop");
    },
    abort: () => {
      calls.push("abort");
    },
  };
  return handle;
}

test("a fresh slot starts its handle and calls the session open", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);

  assert.equal(slot.start(), "started");
  assert.deepEqual(fake.calls, ["start"]);
  assert.equal(slot.isOpen(), true);
});

test("no handle means no dictation", () => {
  const slot = new RecognizerSlot();
  assert.equal(slot.start(), "refused");
  assert.equal(slot.isOpen(), false);
});

test("a handle that throws is refused and stays closed", () => {
  const slot = new RecognizerSlot();
  slot.attach(fakeRecognizer({ refuse: true }));

  assert.equal(slot.start(), "refused");
  assert.equal(slot.isOpen(), false);
});

/**
 * The whole point of this module, after the second bug report.
 *
 * The previous session belongs to an utterance that is already over and delivered, so a press must
 * not queue behind it: kill it with `abort()` -- the one call that does not wait for final results
 * -- and record again inside the same call.
 */
test("a press during a live session aborts it and starts again in the same call", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();

  assert.equal(slot.start(), "started");
  assert.deepEqual(fake.calls, ["start", "abort", "start"]);
  assert.equal(slot.isOpen(), true);
});

/**
 * Queueing is the fallback, not the plan: it is what is left when even a fresh `start()` is
 * refused because the platform has not let the old session go.
 */
test("a start the browser refuses after the abort is owed, and the end pays it back", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();
  // Now the platform refuses: the previous session has not been let go yet.
  fake.state.refuse = true;

  assert.equal(slot.start(), "queued");
  assert.deepEqual(fake.calls, ["start", "abort", "start"]);
  assert.equal(slot.isOpen(), false, "音频还没起来，账上是关的");

  fake.state.refuse = false;
  assert.equal(slot.ended(), true);
  assert.deepEqual(fake.calls, ["start", "abort", "start", "start"]);
  assert.equal(slot.isOpen(), true);

  // A second `end` has nothing left to pay.
  assert.equal(slot.ended(), false);
});

test("an end with nothing owed just closes the session", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();

  assert.equal(slot.ended(), false);
  assert.equal(slot.isOpen(), false);

  // And the next press starts immediately again.
  assert.equal(slot.start(), "started");
  assert.deepEqual(fake.calls, ["start", "start"]);
});

/**
 * `abort()` is not an instant reset -- the browser's session is still closing -- but that is the
 * browser's business now: the next press aborts again and starts, instead of waiting to be told.
 */
test("abort keeps the session open until the browser reports its end", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();

  slot.abort();
  assert.deepEqual(fake.calls, ["start", "abort"]);
  assert.equal(slot.isOpen(), true);

  // The press that follows a send therefore records, instead of queueing behind a dying session.
  assert.equal(slot.start(), "started");
  assert.deepEqual(fake.calls, ["start", "abort", "abort", "start"]);
});

test("reset gives up on a session the browser never ended", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();

  slot.reset();
  assert.equal(slot.isOpen(), false);
  assert.deepEqual(fake.calls, ["start", "abort"]);

  // Nothing is owed any more, and the next press is a clean start.
  assert.equal(slot.ended(), false);
  assert.equal(slot.start(), "started");
});

test("stopping while a start is owed cancels it", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();
  fake.state.refuse = true;
  slot.start(); // refused after the abort -> queued

  // The finger came up before the browser let go: nothing may be started for it.
  slot.stop();
  assert.equal(slot.ended(), false);
  assert.equal(slot.isOpen(), false);
  assert.deepEqual(fake.calls, ["start", "abort", "start"], "end 回来也不许重开");
});

test("stop is ignored when nothing is open", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.stop();
  assert.deepEqual(fake.calls, []);
});

test("dispose drops the handle for good", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);
  slot.start();

  slot.dispose();
  assert.deepEqual(fake.calls, ["start", "abort"]);
  assert.equal(slot.isOpen(), false);
  assert.equal(slot.start(), "refused");
});

/**
 * The sequence that produced the second bug report: speak, send, press again immediately.
 * The second utterance has to record -- without waiting for the first session to end on its own.
 */
test("send then press immediately: the second utterance records without waiting", () => {
  const slot = new RecognizerSlot();
  const fake = fakeRecognizer();
  slot.attach(fake);

  // Utterance one: delivered on release, so its session is finished with.
  assert.equal(slot.start(), "started");
  slot.abort(); // what the release does: hard close, no waiting for final results

  // The finger is back down while the browser is still closing the first session.
  assert.equal(slot.start(), "started");
  assert.deepEqual(fake.calls, ["start", "abort", "abort", "start"]);
  assert.equal(slot.isOpen(), true, "第二句在录，不是在排队");
});
