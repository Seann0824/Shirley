**Shirley technical design · Task ledger (todo)**

This document turns "let the model maintain its own task state" into a plan. It is a companion capability to
`compaction.md`: compaction solves "the context does not fit", while todo solves the
**positive feedback loop of "after compaction the model forgets where it got to, so it re-explores"**.

**Scope statement**: this round only does an in-memory implementation; the persistence layer is left empty.

**Ownership correction (later decision)**: the ledger **has moved from the SDK to the application layer** (`crates/shirley/src/todo.rs`).
"What to record, how to nag, what copy to inject" is a trade-off of the coding-agent product, not a generic fundamental
capability. The SDK keeps only one business-agnostic generic seam -- [`ContextProvider`]: when the `Agent` assembles
each round's request it calls it and appends the returned text as a system message **at the end**. The ledger is just one
application-layer implementation of that seam (`TodoContextProvider`).

---

**1. What it solves**

Compaction is **lossy** (`compaction.md` cites Tirmazi et al.'s information-theoretic conclusion: exact information is
necessarily lost in a summary). What is lost is not only "what the user said", but also **the model's own progress state**:

- what my goal for this round is, how it was broken into steps, which steps are done;
- what facts I have already verified (which file I read, what command I ran, what conclusion I reached);
- what questions remain unresolved.

This information was originally scattered across the assistant body, reasoning, and tool calls. After compaction clears
tool output and compresses the conversation into a summary, the model cannot see "I have already done X", so:

```
compaction -> forget X was done -> re-explore X -> context grows again -> compact again -> ...
```

The ledger takes a different path: it **pulls the "progress state" out of the conversation and holds it separately**,
injecting it unconditionally every round, so the model does not need to "remember to look it up" -- it is always in front of it.

---

**2. Three core decisions (set the tone first)**

**Decision 1: the ledger is written by the model; no programmatic inference.**

"How many steps a task was broken into" and "what counts as a completed conclusion" have no generic criterion; hard-coding
a program would only guess wrong. So the ledger is **maintained by the model itself**: the SDK exposes a `todo` tool, and
the model decides when to update -- **state judgment happens at the AI layer, more accurately than any program rule**.

**Decision 2: the ledger does not go into `self.messages`; each round it is injected as a system message appended at the end.**

- **Does not enter the working set**: `self.messages` is the object that compaction rebuilds; putting the ledger in would
  let it be compacted away -- which is exactly what we want to avoid. The ledger is held separately, compaction cannot touch
  it, and it naturally survives compaction.
- **Injection position is the end**: `active_messages()` renders the ledger into a `System` message when assembling the
  request and **appends it last**. Reasons:
  - appending at the tail **does not move the earlier prefix** -- when the ledger content is stable, the KV prefix cache
    still hits (this is exactly why `responses-api.md` says "keep system in place, do not hoist it to `instructions`");
  - the ledger only changes when the model calls `todo`, and only then does the prefix invalidate. This is the inherent cost
    of "must be visible every round"; it cannot be avoided, only minimized by compressing the change points.
  - the position at the end also matches "most recently visible": the model reads the latest task state.

> **Note**: the ledger is injected as a **`System`** message, not a `ContextSummary`. It does not participate in
> `active_messages()`'s "after the last summary" trimming -- because it is **appended at the end** by the function and does
> not come from `self.messages`. All three adapters (ChatCompletions / Responses / Anthropic) can already handle a system
> at any position: ChatCompletions outputs it as-is, Responses keeps it in place as a system item, and Anthropic extracts it
> to the top-level `system` (the Anthropic protocol itself has no mid-list system).

**Decision 3: patch semantics; only touch the explicitly provided fields.**

The `todo` tool's input is a **patch**, not a full overwrite:

| Field | Semantics | Reason |
| --- | --- | --- |
| `goal` | set / replace the goal | the goal is usually stable, changed occasionally |
| `steps` | **whole replacement** of the list | the list is short; resending the full list each round lets the model self-correct naturally (a missed step is added next round) |
| `add_findings` | **append** a conclusion | "I found X again" is more natural; appending does not lose history |
| `add_open_questions` | **append** a question | same as above |
| `clear` | reset the whole ledger | clear the field when switching tasks |

Unprovided fields stay as-is. This way the model can update only the changed part ("this step is done" = resend `steps`),
without resending the whole ledger every round.

---

**3. Data structures and rendering**

**3.1 `TodoStore`**

```rust
pub struct TodoStore { inner: Mutex<TaskState> }

struct TaskState {
    goal: Option<String>,
    steps: Vec<Step>,          // Step { text, done }
    findings: Vec<String>,
    open_questions: Vec<String>,
}
```

Shared via `Arc` (the runtime and the `todo` tool each hold one `Arc`); interior mutability goes through `Mutex`, so
`&self` suffices to write (`apply` / `clear` / `render`). Persistence is left empty.

**3.2 Rendered as XML**

`render()` assembles the ledger into an XML block (`plan.md`: tag form improves the model's attention to structured information):

```xml
<task_state>
<goal>Migrate the TS backend to Rust</goal>
<steps>
- [x] Survey the existing routes
- [ ] Migrate the user module
</steps>
<findings>
- The auth logic is in crates/shirley/src/auth.rs, using JWT
</findings>
<open_questions>
- Which crate to use for the DB connection pool?
</open_questions>
</task_state>
```

Key points:

- **An empty ledger is not injected** (`render()` returns `None`) -- to avoid an extra meaningless system message every round.
- **XML escaping**: `&` / `<` / `>` are escaped, to prevent model-written content from breaking the tag structure.
- **Character cap** `MAX_RENDER_CHARS = 4000`: truncated over the limit with an explicit marker
  `... (task ledger truncated; keep it concise)` -- the ledger itself takes context too; prevent it from monopolizing.
- When injecting, prepend a line of `TASK_STATE_HEADER` explanation ("this is your task ledger, survives compaction, update
  it with the `todo` tool, do not repeat completed work") -- kept separate from `render()`; the tool result does not need
  to repeat this explanation.

**3.3 The `todo` tool**

Hand-written `impl Tool`: the state `Arc<TodoStore>` is held by the tool itself, bound at construction, not going through
`ToolContext` injection. `invoke` deserializes `TodoUpdate` -> `store.apply()` -> returns the **full updated ledger** (so the
model can confirm the update took effect and continue accordingly). The parameter schema is a hand-written JSON Schema.

---

**4. Relationship to compaction**

The two do not overlap in responsibility:

| Capability | What it records | Who triggers | How it recovers |
| --- | --- | --- | --- |
| compaction | a conversation summary | automatic (80% threshold) | the summary itself |
| **todo** | **task progress state (goal / steps / findings / open questions)** | **the AI updates proactively** | **injected unconditionally every round** |

- **The ledger is not "summarize it again"**: a summary is lossy (that would be a second loss), while what is written in the
  ledger is facts the model **has itself confirmed** -- it is "distillation", not "compression".
- **The ledger does not enter `self.messages`**: it is held separately and survives compaction.
- **Session switching is naturally isolated**: after multi-session landed, each session holds its own independent `Agent`
  (`SessionManager`), and the ledger is shelved along with the `Agent` -- the old session's task state does not linger into
  the new session. (In the single-session era there was an explicit `todo.clear()` in `Agent::switch_session`; that seam has
  been removed.)

---

**5. Lifecycle**

- **Registration**: in `Agent::new`, `tools.register(TodoTool::new(todo.clone()))`. The application layer's `main.rs` needs
  no changes.
- **Injection**: appended at the end of `active_messages()` (see decision 2).
- **Clearing**: naturally isolated with session switching (each session has an independent `Agent`); no cross-session residue.
- **Persistence**: left empty (lost when the process ends). If persistence is needed in the future, `TodoStore`'s interface
  (`apply` / `render` / `clear`) is already enough to attach a backend.

---

**6. Implementation locations**

| File | Responsibility |
| --- | --- |
| `crates/shirley/src/todo.rs` (**application layer**) | `TodoStore` / `TodoStatus` / `TodoUpdate` / `TodoStep` / `TodoTool` / `TodoContextProvider` / `TASK_STATE_HEADER` / `TODO_NAG_REMINDER` / unit tests |
| `crates/shirley/src/bootstrap.rs` (application layer) | `build_agent` creates a fresh `Arc<TodoStore>` per `Agent`, registers `TodoTool`, and injects `TodoContextProvider` as `context_provider` (the two share the same `Arc`) |
| `crates/shirley-agent-sdk/src/runtime/context.rs` (SDK) | the generic seam `ContextProvider` (`context() -> Option<String>`); `agent.rs` calls it at the end of `active_messages()` |
| `crates/shirley-agent-sdk/src/runtime/agent.rs` (SDK) | holds `Option<Arc<dyn ContextProvider>>`, injects the returned text at the end of `active_messages()` |

**No longer exported**: `TodoStore` / `TodoTool` etc. have moved out of the SDK along with the module (`lib.rs` no longer
re-exports them). The SDK only exports the generic seam `ContextProvider`.

---

**7. Known gaps**

1. **No persistence**: lost when the process ends. The ledger is not rebuilt when restoring a session -- a cold-started
   session has no task state, and the model needs to `todo` again (if there is a session log, it could be rebuilt from the
   last segment in the future).
2. **No automatic cleanup**: the ledger is cleared only when the model calls `todo {clear: true}` or on session switching.
   In a long task, if the model forgets to clean up, the ledger keeps growing (`MAX_RENDER_CHARS` is the fallback truncation).
3. **Injection loses the prefix cache**: every time the model calls `todo`, the prefix "after that ledger system message"
   invalidates. This is the inherent cost of "visible every round"; currently minimized by "put the ledger at the end + write
   only the changes".
4. **The model may not use it**: whether to update the ledger is entirely up to the model. If the model does not call `todo`,
   the ledger stays empty and is not injected, and behavior falls back to the status quo (no degradation).
