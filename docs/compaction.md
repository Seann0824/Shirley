**Shirley technical design · Context compaction optimization**

This document converges the scattered judgments about context compaction in `plan.md` into an executable plan.
It follows section 6 of `runtime-hardening.md` (compaction robustness) and T6 of `docs/plan.md`, and introduces a formal paper
(Tirmazi et al., *Context Compaction Theory*, arXiv:2608.01326v1) as theoretical grounding,
answering three questions: **why today's compaction loses the task, what compaction can achieve in theory, and what we change this round**.

**2026-10-13 regression fix:** session `20261013-142807` had 14 compactions, 138 file reads, and
58 command invocations without ever entering the fix phase. The following implementation points were corrected:

- The new summary input includes the old summary, the conversation to be compacted, the original current task, and a task-ledger snapshot; the output is still a single summary.
- Tool results are no longer all cleared before the summary; when over budget, first remove lengthy reasoning drafts from the summary request,
  then trim the middle of large tool results, keeping the body, head/tail evidence, and the original call ID.
- Compaction after restoring a log and normal execution share the "latest summary + subsequent messages" working set, and must not compact already-archived history again.
  The original log is not rewritten by the choice of working set; the current task is restored from the archived prefix when necessary.
- `trigger_ratio` is only used to trigger compaction. The summary input budget is computed as the window minus the output reservation;
  an over-estimate only guides shrinking the input, it does not locally reject on that basis; the final decision is made by the model service's actual capacity check.
  When the server rejects, the original context is still kept; it does not pretend compaction succeeded.
- `ContextProvider::compaction_context()` is not injected by default; the provider explicitly chooses the necessary state.
  The current task ledger participates in the summary; full skills and retrieved memory are not copied in indiscriminately.
- The ledger detects long-term lack of progress based on effective-content changes; repeatedly appending the same conclusion does not reset the reminder.
- Normal execution requests also calibrate the token factor using the estimation convention of the actual messages and tool schema.
- Each round's user task defaults to at most 64 model execution requests, and compaction does not reset the count; after reaching the limit, completed
  tool results are kept, an unfinished notice is appended, and `MaxStepsReached` is returned. Adapter network retries still follow the original policy;
  this limit does not represent the total number of HTTP requests or a single tool's timeout.

A global or workspace `config.toml` can override the execution limit, with the workspace taking priority:

```toml
[runtime]
max_steps = 128 # must be greater than 0; SDK callers use the Agent builder's max_steps
```

Request-level regressions are in `tests/runtime_recovery.rs`, covering inheriting evidence across successive compactions, the execution limit across compactions,
the server rejection not destroying the original history, restoring a log without re-compacting archived history, reasoning degradation preserving evidence, and continuing the task after reaching the limit. The following sections keep the early design background;
the fix notes and the current implementation are authoritative.

---

**1. Why do this separately**

The trigger was a real incident: after compaction completed, the AI forgot the task the user had assigned before compaction,
and the compaction summary even gave a "next step" -- but that step was not something the user had ever mentioned.

This is not a small problem fixable by tuning the prompt. It exposed two structural defects in the current implementation (see section 2),
and the paper (see section 3) further shows: **the information loss of compaction cannot be fully avoided in theory,
and LLM summaries necessarily degrade on "exact facts"**. So compaction is not "writing a better prompt", but
redesigning **the cut, the retention strategy, and the fallback after information loss**.

---

**2. Dissecting the status quo**

**2.1 Current chain**

```
should_schedule_compression  ->  (input + output) * 100 >= limit * 80
        | set compression_pending
compress_context             ->  active_messages() + compression instruction (appended as a new system)
        | take the model's plain-text summary
push to the end of self.messages ->  Message::ContextSummary
        |
active_messages()            ->  the leading system + messages starting from the last ContextSummary
```

Corresponds to `crates/shirley-agent-sdk/src/runtime/mod.rs`.

**2.2 Two root causes (the same structural problem)**

**Root cause A -- the summary is appended at the end, which eats the tail too.**

`compress_context` pushes the `ContextSummary` to the **very end** of `self.messages`.
The next round, `active_messages()` finds "the last summary", and what is cut out is `[system, summary]`:

- the user task in progress this round (before the summary) is excluded from the request;
- a segment of recent conversation before and after compaction also no longer participates in the request.

The model is left with only a summary, so it can only live on the summary. **This is the direct cause of "forgetting the original task".**

**Root cause B -- the compression instruction asks the model to invent a "next step".**

`crates/shirley/src/main.rs`'s `compression_instruction` explicitly wrote "... unresolved questions, next step". If you ask it to output a next step, it will, even if the user never mentioned it.
At the same time the instruction did not emphasize "faithfully preserve the user's original instructions", so the summary had an over-reaching tendency.

**Conclusion**: the cut itself must change, not the prompt.

---

**3. Theoretical grounding (what we can take from the paper)**

The paper formalizes compaction as two games:

| | Context Selection Game (`S`) | Context Generation Game (`G`) |
| --- | --- | --- |
| Approach | choose a subset to keep | generate a bounded-length new message (summary) |
| Examples | truncation, dropping old messages, Aider repo map, LLMLingua | LLM summarization (Codex / Claude Code / Gemini CLI) |
| Relationship | `S ⊆ G`, generation is stronger (can create new tokens, reorder, use a lower encoding) | |

**Core theorem (Theorem 1)**: the Context Generation Game **is equivalent to one-way communication complexity**.
The compressor = Alice (condenser), the future LLM reading the summary = Bob (interpreter), the summary = the message Alice sends.

> To answer a set of queries within a target error rate ε, the minimum compression budget required = the one-way randomized communication complexity of the induced communication problem at ε.

From this come four conclusions directly binding on us:

1. **The optimal compression strategy cannot be decided independently of "what will be asked in the future".**
   This formally supports `plan.md`'s intuition -- "the value of compression is decided by future queries, not by what the original text looks like".
   Therefore a fixed programmatic compression strategy is doomed to be lossy, and the compression decision must be handed to the LLM (`G` type).
   This explains why our `ContextSummary` direction is right.

2. **A summary cannot preserve "exact facts".**
   The experiment in Appendix A: Anthropic's compaction endpoint + Opus 4.8 doing set-membership queries,
   the error rate after compaction ≈ random guessing (false negative rates 0.97 / 0.79 / 0.63),
   a same-size Bloom filter is about 1/3; the no-compaction control has an error rate of only 0.04.
   In the summary text the model itself wrote: "I cannot store this set losslessly, I can only guess by whether it looks like it."
   -> **Narrative information (goals, decisions, progress) can be preserved; exact items (sets, lists, error codes, file names,
   full file contents) cannot.** The latter must be covered by mechanisms outside the summary.

3. **The lower bound holds for all algorithms; the upper bound does not guarantee the LLM can compute it.**
   The paper's caveat 1: this is an information-theoretic conclusion. The lower bound is airtight; the upper bound only says "some algorithm can achieve it",
   not that an LLM summarizer can compute it. -> We can only optimize "good enough", not pursue "optimal".

4. **Multiple compactions only get worse.**
   The paper only models a single compaction, and points out that a single one is "the most favorable case";
   each compaction can only lose more information on top of the previous result. Codex itself warns that "multiple compactions degrade quality".
   -> Our design must **avoid summary-on-summary** (see 5.2).

**Two ways out given by the paper** (end of Appendix A): (1) maintain the sketch outside the LLM context (tools/memory);
(2) put the sketch's raw state into the context + a decoding instruction.
-> (1) is the direction of our task ledger (`todo.md`).

---

**4. Token accounting (the prerequisite for the cut-point budget)**

**4.1 Why it is needed**

The budget convention for retaining the tail is set to **20% of the context window**:

```
budget = context_window_tokens * 0.20
```

To decide "from the tail backwards, up to which message to accumulate", we must know each message's token usage.

A key fact: **`input_tokens` cannot be precisely decomposed per message.** It is the real total of the whole request, including system, tool schemas, role wrappers, and protocol-specific tokens.
Precise attribution would require replicating the chat template -- too costly, not worth it.

So the goal is **a good-enough attribution + calibration**, not absolute precision. Choosing a compaction cut point only needs **relative size accuracy**:
knowing "where to cut to fit the budget" is enough.

**4.2 Three-layer plan**

| Layer | Content | This round |
| --- | --- | --- |
| L1 heuristic estimation | CJK-aware char -> token estimation + message wrapper overhead | **do** |
| L2 API usage calibration | use the most recent real `input_tokens` to get a ratio, multiply back onto the estimate | **do** |
| L3 precise counting interface | the `TokenCounter` trait, later hooking into `/count_tokens` or a local vocabulary | leave a seam, do not implement |

**4.3 Estimation rules (L1)**

A CJK-aware character-level estimate, much more accurate than `chars/4`:

```
count_text(s):
  total = Σ for each char:
    CJK   (U+4E00–U+9FFF / U+3040–U+30FF / U+AC00–U+D7AF / U+3000–U+303F) -> 1.0
    ASCII (0x00–0x7F)                                                      -> 0.25
    other (emoji / symbols / other Unicode)                                -> 0.5
  return ceil(total)

count_message(m):
  return count_text(all text content of the message) + wrapper overhead
    wrapper overhead:
      role wrapper       ≈ 4
      each ToolCall      ≈ count_text(name) + count_text(arguments) + 8
      Tool message       ≈ count_text(tool_call_id) + 4
```

**4.3.1 Basis for the coefficients (literature support)**

The three coefficients are not arbitrary; they are backed by empirical literature:

- **CJK = 1.0 token/char**: Chinese PLM tokenizers usually treat each Han character as an indivisible token
  (Sub-Character Tokenization for Chinese PLMs, arXiv:2106.00400).
  "Vowel Signs Are Not Letters" (arXiv:2608.26449) measured that the **Han script's fertility factor is exactly 1.00x**,
  and Han characters are relatively stable under byte-level BPE. -> 1.0 is a **conservative upper bound** (see below).
- **ASCII = 0.25 token/char**: English ≈ 1.2 tokens/word (Tokenizer Tax Across 25 European
  Languages, arXiv:2605.24718), the average English word length is about 4.7 chars, giving ≈ 0.25.
- **other Unicode = 0.5 token/char**: failed BPE merges fragment text into single-byte tokens
  (Tokenizer Tax for Indian Languages, arXiv:2607.24276). 0.5 is on the conservative side.

**Why we must calibrate (4.4) instead of relying on fixed coefficients**:
"The Invisible Language Tax" (arXiv:2609.39001) measured 7 mainstream 2026 tokenizers (including DeepSeek V3/V4),
and concluded that the token premium **varies by model, with no unified coefficient** -- Simplified Chinese relative to English ranges from **-5% to +40%**.
Modern BPE (o200k / the new DeepSeek) can **merge two Han characters into one token**, so the true CJK value may be 0.5~1.0.
We take 1.0 to **prefer over-estimation** (trigger compaction earlier, the safe side); L2 calibration pulls it back to the true value.

The measurement convention follows the academically standard **tokens per character / characters per token**
(Is Sanskrit the most token-efficient?, arXiv:2601.06142).

**4.4 Calibration (L2)**

After each response obtains `real = usage.input_tokens`, we also locally compute a total estimate for this request:

```
est   = overhead_est(system + tools) + Σ count_message(m)     # the messages of this request
ratio = real / est
factor = 0.7 * factor + 0.3 * ratio                           # EMA smoothing, initial value 1.0
```

`factor` is **multiplied back uniformly at aggregation time**: each message's raw estimate is multiplied by the same coefficient.
Because the coefficient is the same, the accumulation order is unchanged and the cut result is stable.
Even if the heuristic is off by 30%, after calibration it can usually be brought within 10%.

**4.5 Cut-point algorithm (with the tool-pairing constraint)**

```text
fn compute_cut() -> Option<usize>:
    window = context_window_tokens                 # not configured -> return None (no compaction)
    budget = window / 5                            # 20%

    head = number of leading consecutive System messages  # always kept
    acc = 0; cut = messages.len()

    for i in (head..messages.len()).rev():         # accumulate from the tail backwards
        c = factor * count_message(messages[i])
        if acc > 0 and acc + c > budget:
            break                                  # adding more exceeds the budget
        acc += c
        cut = i                                    # cut is the start of the retained segment
    # keep at least one (the last user message), even if it alone exceeds the budget

    if cut <= head: return None                    # nothing to compact

    # ---- tool-pairing correction ----
    # The first message of the retained segment must not be a Tool result, otherwise an orphan tool_call appears.
    while cut > head and messages[cut] is Tool:
        cut -= 1                                   # back up to the Assistant{tool_calls} that produced it
    # Allow a slight over-budget as a result: correctness takes priority over budget (an orphan tool_call would be rejected with an API 400)

    if cut <= head: return None
    return Some(cut)
```

Key points:

- **The budget boundary floats**, so the pairing correction must be done explicitly, not by "roughly aligning".
- One `Assistant{tool_calls}` may correspond to multiple consecutive `Tool` results;
  the `while` back-up will swallow them all into the retained segment, so the pairing is naturally complete.
- The compacted region = `messages[head..cut]`; it is rebuilt as
  `[new ContextSummary] + messages[cut..]`, and the system prompt is separately regenerated and placed on top by the `Agent`.

---

**5. Compaction design**

**5.1 Mental model: cut-point compaction + keep the tail**

Compaction is not "compress everything", but **choose a cut point (see 4.5), compress everything before the cut into one, and keep everything after the cut as-is**:

```
[system prompt]              <- always kept
[ContextSummary structured block]  <- the segment before the cut is compressed into this one
[the conversation of the recent ~20% window, verbatim] <- keep the tail, do not compress
[current user message]       <- this round's task, always in the request
```

This corresponds to the hybrid strategy in the paper's Table 1 (Claude Code / OpenCode: first cheaply inline-compact large tool output,
then fall back to summarization), which is the layering we should learn from.

**5.2 Layered retention; no summary-on-summary**

The status quo `active_messages()` finds "the last summary" and the new summary overwrites it -- exactly the degradation path of "summary-on-summary".
New design: on compaction **rebuild** `self.messages`, letting the compacted segment **freeze**:

```
self.messages = [system (regenerated by Agent)] + [new ContextSummary] + messages[cut..]
```

That is, `ContextSummary` is no longer "appended at the end" but **inserted at the cut position**, replacing the segment before the cut.
On the next compaction, the new summary overwrites "the previous summary + the conversation after it", the semantics are clear,
and it avoids compacting the same segment repeatedly (paper caveat 4: multiple compactions only get worse).

**The system prompt does not participate in the rebuild**: it is held separately by the `Agent` (the `system_prompt` field),
and `CompactParts::rebuild` directly returns `[new ContextSummary] + current_task + remain`,
neither receiving nor copying any `System` from the original messages; `compress_context` then calls
`Agent::system_message()` to regenerate one and place it on top (when `SystemPrompt` is a function it is **re-resolved**,
so the latest state of the working directory / project guide is reflected). This way the system prompt is never polluted by compaction products,
and it will not accidentally carry the old summary in the rebuild due to "keeping the leading system".

**5.3 Summary template (XML tags + distinguishing narrative from exact)**

The content of `ContextSummary` changes from free text to structured blocks.
Basis: `plan.md` mentions that XML tags improve LLM attention; the paper says to distinguish information types.

```text
<current_goal>Faithfully restate the task the user is actually assigning now; do not add</current_goal>
<hard_constraints>constraints the user explicitly raised</hard_constraints>
<decisions>key decisions already made and why</decisions>
<progress>completed / current state</progress>
<open_questions>unresolved questions</open_questions>
<compacted_range>the previous conversation has been compacted; exact details (file contents / command output / errors / symbol lists) are not in the summary; re-read or re-run them when needed</compacted_range>
```

Key points:

- **Delete "next step"**. Whether there should be a next step is for the user to decide, not for the compactor to invent.
- Emphasize "only distill what has happened; do not extrapolate, do not add plans that did not appear".
- `<compacted_range>` tells the model "the compacted segment is no longer visible; rebuild it yourself when needed" -- because the paper proves exact information necessarily
  disappears under `G`, and no prompt can remedy that.
- The summary uses the **system role** (the current `encode_messages` already degrades `ContextSummary` to system), preserving the "background information" semantics.

**5.4 Retry and degradation**

Based on section 6.1 of `runtime-hardening.md` and `plan.md`'s "retrying a failed compaction sometimes succeeds":

- Retry only on **recoverable failures** (empty content / a response with tool_calls), 2~3 times.
- Still failing -> **do not abort**: record `AgentEvent::CompressionFailed`, set `compression_pending` back to `false`,
  keep the original messages and continue this round's task.
- Two consecutive failures -> no more compaction attempts this round; rely on provider-side truncation and report `ContextUsage`.
- **Suppress thrashing**: after compaction, if the summary itself is still near the threshold, do not immediately compact again.

---

**6. Landing point and change list**

Mainly lands in `crates/shirley-agent-sdk/src/runtime/mod.rs`, adding a `token` module, plus the compaction instruction copy in `crates/shirley/src/main.rs`.

| Change | Location | Description |
| --- | --- | --- |
| add a `token` module | `crates/shirley-agent-sdk/src/token/mod.rs` | `TokenCounter` trait + `HeuristicCounter` (with `factor`) |
| `compact()` replaces `compress_context` | `runtime/mod.rs` | compute the cut first -> summarize only `messages[..cut]` -> rebuild `self.messages` |
| cut-point computation `compute_cut()` | `runtime/mod.rs` | see 4.5: 20% budget + tool-pairing correction |
| calibration state | `runtime/mod.rs` | update `factor` with `input_tokens` after each response |
| `active_messages()` simplified/removed | `runtime/mod.rs` | messages are already rebuilt on compaction, the request side no longer needs slicing |
| summary template | `runtime/mod.rs` (where the compaction instruction is built) | XML tag structure |
| compaction instruction copy | `crates/shirley/src/main.rs` | delete "next step", add "faithfully preserve the user's instructions" "do not extrapolate" |
| compaction retry + degradation | `runtime/mod.rs` | see 5.4 |
| `AgentEvent::CompressionFailed` | `runtime/mod.rs` | a new event, consumed by the UI |
| `CompressionConfig` (strategy config) | `runtime/compaction.rs` | `trigger_ratio` (default 0.80) / `retain_ratio` (default 0.20) / `template` (default `COMPACTION_TEMPLATE`), passed in via the `Agent` builder's `compression_config` |

---

**6 supplement: compaction strategy config (landed)**

The "when to compact, how much to retain, what the summary looks like" of compaction changed from hard-coded to tunable, carried by the SDK's
[`CompressionConfig`] (`runtime/compaction.rs`):

| Field | Default | Meaning |
| --- | --- | --- |
| `trigger_ratio` | `0.80` | compaction is triggered when this round's `(input + output)` reaches `context_window * trigger_ratio` |
| `retain_ratio` | `0.20` | the token budget ratio of the tail (recent conversation) retained after compaction |
| `template` | `COMPACTION_TEMPLATE` | the summary output template, **appended** after `compression_instruction` |

- The SDK only provides the **strategy parameters**; the domain-related trade-offs (which facts to keep) remain in
  the `Agent`'s `compression_instruction` (passed in by the application layer).
- The application layer overrides via the `[compression]` table (`crates/shirley/src/settings.rs`):
  `trigger_ratio` / `retain_ratio` / `template`, defaulting to the SDK defaults when absent.
- Invalid `trigger_ratio` / `retain_ratio` are **clamped back to the default** in `CompressionSettings::to_sdk()`
  (without blocking startup): compaction is a background capability, and a misconfiguration should not prevent the program from starting.

---

**7. Acceptance**

1. Construct a long conversation (with tool calls); after triggering compaction, the model can still answer "what did I originally ask you to do".
2. The compaction summary **does not contain** a "next step" the user did not assign.
3. There is no orphan tool_call on either side of the cut (assert on the generated request body: every `Tool` message has a preceding `Assistant.tool_calls`).
4. The retained-segment token estimate does not exceed `window * 20%` (a small overshoot from the pairing correction is allowed).
5. Inject "compaction returns empty content"; the task continues rather than aborting, and the UI receives `CompressionFailed`.
6. With two consecutive compactions, the number of `ContextSummary` in `self.messages` is 1 (no stacking).
7. The cache hit rate does not drop noticeably (the first round after compaction may miss; subsequent rounds should recover).

---

**8. What we are not doing / open questions**

- **No programmatic selection strategy** (the paper proves `G` is strictly stronger than `S`, and the optimum depends on the query distribution).
- **No vector store / external memory storage** (consistent with section 5 of `roadmap.md`).
- **No optimality under an adaptive adversary** (the paper itself is an open problem).
- **Quality decay across multiple compactions** (a paper open problem): we can only achieve "no summary stacking + layered freezing", and cannot give a guarantee.
- **L3 precise counting is not implemented**: only the `TokenCounter` trait interface is left, to be landed when there is a clear need (e.g. a provider offering `/count_tokens`).
- **No local tokenizer**: the vocabulary is bound to a specific model, has heavy dependencies, and conflicts with the restraint principle of "do not abstract ahead of time for multiple protocols".
