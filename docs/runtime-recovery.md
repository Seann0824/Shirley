# docs/runtime-recovery.md (lines 1-35)

# Request retry, interruption retention, and compaction labels

A model call is automatically retried up to **5 times** inside `adapter::invoke`, so with the initial request there are at most 6 attempts.
It relies only on `SdkError::is_retryable()`: connection/response-body transport errors, HTTP 429, and 5xx are retryable;
other 4xx, and encoding and decoding errors, return immediately. The backoff is 0.5, 1, 2, 4, 8 seconds.
Once an increment or a completion event has been emitted to the caller, it is no longer retried, to avoid duplicate output.

Model requests now have a progress deadline (`ModelConfig.request_timeout`, default 180 seconds).
Each decoded model event resets it; a healthy long response has no total duration limit.
Waiting for headers, reading a non-streaming body, and streams that only send keepalive comments
are all covered. The Agent reuses one HTTP client with a 15-second connection timeout.
An EOF without a completion event is a transport failure, not a successful empty response;
ChatCompletions also honors `[DONE]` without waiting for the connection to close.

After partial output, the runtime can continue at most twice on retryable failures. It commits
partial text, retains completed tool results, and appends `prompts/stream_recovery.md` to the next
request. It does not replay the original task or execute incomplete tool-call fragments.
Both ordinary retries and continuations emit `Retrying`, shown as a connection-retry status in
the desktop. Exhausted recovery returns an error and preserves the conversation.

The desktop supervises each turn with an unwind boundary. Normal completion, cancellation,
failure, panic, and unexpected stream exhaustion all return the Agent before publishing a
terminal event. Title generation runs after task completion with a separate 20-second budget;
it cannot keep a completed turn loading. On failure, outstanding tool indicators stop and report
that their actual execution state needs confirmation.

TUI Esc and desktop stop both preserve this round's user input, already-completed messages, and the half-finished assistant reply.
After the driver drops the stream it calls `Agent::finish_interrupted_turn()`, which appends the returned new messages to the session log.
Tool calls that have not returned a result are filled in with an interruption result, so the next request does not carry an orphaned tool_call;
that result only indicates that the wait was interrupted, and it must not be used to assert that the tool had no side effects.
The TUI also waits for the cancellation to settle when it exits while running. A new draft in the input box is preserved; the original input is not restored.
Only an explicit `/rewind` still rolls back messages and truncates the log.

The shared Session projection turns a ContextSummary into a standalone Compact item; the desktop's live events and
history snapshot both use the compact type. By default both ends show only "context compacted", and reveal the summary only after expanding.
The TUI can click a label to toggle it individually, or use Ctrl+S to toggle the most recent one; the desktop clicks a label to toggle.
After reloading a session it is collapsed by default.

## Manual verification (performed by the user; the UI effect has not been verified yet)

1. On both ends, send a request that produces a long reply, then press TUI Esc / desktop stop after output starts.
   Expected: the user message and the half-finished reply are preserved, and stopping waits; keep typing and send the next round without deleting the previous round's content.
2. While a reply is in progress, type a new draft in the input box and then interrupt.
   Expected: the draft is preserved; the previous round's original input is not automatically filled back into the input box.
3. After interrupting, switch sessions and switch back, then restart and load that session.
   Expected: the preserved user message and half-finished assistant reply are still there; the next round can refer to them.
4. Trigger automatic compaction under a smaller context window.
   Expected: progress is shown during compaction; when it finishes only the "context compacted" label is shown, without laying out the summary body directly.
5. Click a label to expand, click again to collapse; on the TUI also check Ctrl+S. After triggering compaction multiple times, click each label separately.
   Expected: each label can be expanded individually; Ctrl+S toggles only the most recent one; ordinary replies and summaries are preserved in chronological order.
6. After compaction, switch sessions, restart and load, then narrow the window and scroll.
   Expected: history summaries are still collapsed by default, the labels are operable, and the body's line wrapping and scroll position are normal.
7. Start a task against a local model fixture/proxy, then interrupt the upstream connection
   after some output; restore it for the next request.
   Expected: a connection-retry status appears, the same task continues with saved output and
   completed tool results, and the status disappears when output resumes.
8. Keep the upstream unavailable, or configure a development fixture to panic during execution.
   Expected: retries are bounded, a visible error replaces loading, unfinished tool indicators
   stop, and a new message can be sent. Switch away and back: the session must remain idle.
9. Complete a first turn while the title endpoint is slow/unavailable.
   Expected: loading stops as soon as the task completes; title generation does not block sending.

The backend contract coverage is in `crates/shirley-agent-sdk/tests/runtime_recovery.rs`; no new UI unit tests are added.
