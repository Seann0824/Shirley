**Shirley technical design · Memory system**

> Optional cross-device personal-memory sync and legacy-entry review are implemented; see [memory-sync.md](./memory-sync.md). Project memory stays on the execution host. The original local-only privacy assumptions below no longer apply when sharing is enabled.


This document lands a plan for "remembering the user across sessions". It is the **cross-session
extension** of `compaction.md` / `todo.md`: compaction solves "a round does not fit", todo solves
"forgot where it got to after compaction", and memory solves **"a new session starts from zero and
forgets everything the user said last time"**.

The external basis of the plan is the storage format and update mechanism in *AI Agents in Depth*
chapter 3 (user memory and knowledge bases); the counter-example is AgentLab
(`/Users/sean/Desktop/repo/AgentLab`) -- its direction is right but its implementation is heavy, so
take only its intuitive parts and avoid the pits it fell into (see section 7).

**Scope statement**: this round only does the plan and the minimal landing (V1); no vector database /
embeddings / graph database.

> **Implementation status (V1 + V2 + V2.5 landed)**: `crates/shirley/src/memory/` (the seven modules `format` /
> `store` / `index` / `provider` / `curator` / `embed` / `vector`) + application-layer wiring
> (`bootstrap.rs` / `interface/session/mod.rs` / TUI / desktop). **The SDK is unchanged**; it only reuses
> the existing seam `ContextProvider`. V2.5 = retrieval upgraded to **hybrid retrieval (BM25 +
> semantic vectors, RRF fusion)**, honestly degrading to pure BM25 when embedding is not configured.
> Phases and acceptance criteria are in section 10, gaps in section 11.

---

**1. What to solve**

Shirley is now "each session is an island": `session.rs` persists each session's raw Messages as
JSONL and rebuilds the `Agent` on restore, but **switching to another session, or opening a new one,
leaves the model knowing nothing about the user**. `plan.md` lines 224-272 already set the direction,
and the pain point can be summed up in one sentence:

> The user does not want to re-introduce themselves and repeat "I don't like X, this project uses Y"
> every time they open a new session.

There are three hard constraints around it (all from the user's reflection on the previous version of
the memory design):

1. **Complexity**: the previous version (AgentLab-style) depends on PG + pgvector + Neo4j + Ollama,
   plus programmatic LLM extraction. **Shirley must be light and zero-external-service.**
2. **Evidence**: a memory entry with no provenance cannot be traced back when it is wrong. **Every
   memory must be able to answer "which piece of evidence did this come from".**
3. **Conflict handling**: relying on "keep the latest" or having the model guess irreversibly loses
   history. **The user's judgment: a conflict is essentially "something that needs time added to
   organize it".**

And one **experience** requirement:

4. **No async curator**: the previous version relied entirely on the AI proactively calling a memory
   tool, and whether it called depended on the model's mood, so the experience was unstable. **It
   must organize itself in the background, and every query must automatically carry relevant
   context.**

---

**2. Core decisions (set the tone first)**

**Decision 1: pure Markdown + zero external services; no vector database for now.**

Chapter 3 ranks storage formats by "decreasing simplicity / increasing expressiveness" into four
tiers (Simple Notes -> Enhanced Notes -> JSON Cards -> Advanced JSON Cards), and concludes with a
**hybrid mode**: structured cards for a small amount of key data, simple notes for a large amount of
non-key facts. Shirley uses a **Markdown directory + YAML frontmatter** to cover both tiers at once --
the body is the note, the frontmatter is the structured fields.

**Body contract (2026-10 revision)**: the **first line of the body is a one-sentence summary** (for
display by `index.md` and related injection), and after a blank line come the **cross-session
reusable concrete details** (dates / places / names / numbers / decisions / constraints, listed as
points). This is the actual content of the "on-demand detail layer (L2)" in the two-layer structure --
V1 once required `body` to be "explain it in one sentence", which made L2 structurally empty:
cross-session `recall` could get the entry but not any detail (especially noticeable for long tasks
like trip planning). The curator prompt now requires "few but polished entries, each complete",
guarded by the `build_prompt_keeps_detail_in_body` regression test.

Choosing Markdown over a dedicated database, the reasons (chapter 3 "the filesystem paradigm"): users
can read / edit / delete directly; it can go into Git version control and be rolled back; an Agent
with `write_file` can organize it autonomously; **zero deployment cost**.

Retrieval uses **keyword / topic matching** in V1 (zero dependencies), upgrades to BM25 in V2, and
**from V2.5 adds an optional embedding semantic leg to form hybrid retrieval** (see below). The
interface is always `query -> relevant entries`, so upgrades do not change the architecture --
**do not pay complexity now for a vector database you may never need** (echoing `plan.md`'s "do not
abstract for abstraction's sake").

**V2.5 supplement (still no vector library)**: embeddings go over **remote HTTP** (OpenAI-compatible
`POST /v1/embeddings`, `embed.rs`), vectors land in a **sidecar file** (one `vectors.json` per root,
`vector.rs`), and retrieval reads them into memory for **brute-force cosine** (entries number in the
hundreds, so a linear scan suffices). Still zero vector library, zero new dependencies, zero external
services -- just "calling one more optional HTTP interface". **When the embedding endpoint is not
configured the entire semantic leg is absent**, retrieval degrades to pure BM25, and it never
interrupts the conversation.

**Decision 2: a two-layer structure -- "always-resident overview + on-demand detail".**

This is the convergence point of chapter 3 and the only feasible solution to "every query
automatically carries context":

- **Always-resident overview layer (L0/L1)**: a small number of key facts (profile / explicit
  preferences / active projects) **injected automatically every round**, not depending on the AI
  calling proactively;
- **On-demand detail layer (L2)**: the full entry text and the original conversation, retrieved with
  `read` (or a future memory tool) when the AI needs it.

Why two layers are necessary: chapter 3 experiment 3-11's conclusion -- **relying only on the
always-resident layer loses detail due to capacity limits, and relying only on retrieval cannot find
cross-session associations due to the lack of a global view**. The previous version only did the lower
half (AI proactive retrieval), so it was "unstable".

**Decision 3: writing is handed to a background curator, not waiting for the AI to call a tool.**

Chapter 3's knowledge update is **dual-path**: event-triggered incremental updates + periodically
triggered full reorganization. Land it as two background tasks that **do not block the foreground**
(decision 3's "async" is the key to the experience):

- **Incremental update**: at session end, the curator reads the recent conversation + the existing
  relevant memories and produces a diff that is as small as possible yet complete;
- **Periodic reorganization**: triggered by period / threshold (new entry count, conflict count,
  retrieval-quality drop), doing dedup merging + verification against the original evidence +
  conflict-scenario qualification.

**Decision 4: conflicts are not deleted; they are expressed via a timeline.**

A new fact points `supersedes` at the old entry, the old entry is marked `status: superseded`, and
**history is permanently retained**. This is exactly the lesson from chapter 3's Mem0 evolving from v2
(UPDATE/DELETE on write) to v3 (append-only ADD): **an erroneous UPDATE/DELETE irreversibly loses
history**. The user said "conflict handling is essentially adding time to organize it" -- the
corresponding term in chapter 3 is **qualification**: if both sides of a contradiction hold at
different times / objects / conditions, write **each one's applicable scenario into the knowledge**,
rather than choosing one.

**Decision 5: memory belongs to the application layer; the SDK keeps only the generic seam.**

Fully isomorphic to `todo.md`: **"what to record, how to inject, when to organize" is a trade-off of
the coding-agent product**, not a generic fundamental capability. The SDK's existing `ContextProvider`
seam (`runtime/context.rs`) is enough to carry the injection, **adding no new SDK external types**
(consistent with `Agent.md`'s "keep the external contract small").

---

**3. Storage format**

**3.1 Directory layout**

**The ownership domain (personal / project) is carried by the storage root**, and is **orthogonal** to
the semantic type (`type`) -- two separate axes. On write, `Entry.domain` decides which root it lands
in (`store.rs::write_entry` -> `root_for_domain`): `personal` lands in the global root, `project`
lands in the workspace root. Each root has the same directory structure.

```
~/.shirley/memory/        # personal domain: user profile / preferences / cross-project facts
├── core.md                         # * always-resident layer: projection of this root's active entries (<= 2000 chars)
├── index.md                        # the program-maintained index page (one line each: id / type / summary / path)
├── vectors.json                    # semantic vector sidecar (V2.5, one per root)
├── episodic/                       # episodic memory: specific events (with timestamps)
├── preferences/                    # preferences / habits ("always like this from now on")
├── procedures/                     # procedural memory: behavior flows ("X first, then Y")
└── facts/                          # semantic facts (profile / cross-project knowledge)

~/.shirley/workspaces/<workspace-id>/memory/             # project domain: specific to the current workspace (stored globally, isolated per workspace)
├── core.md / index.md / vectors.json
├── episodic/ preferences/ procedures/ facts/
~/.shirley/workspaces/<workspace-id>/sessions/*.jsonl    # raw evidence layer (already present, append-only)
```

The semantic types align with chapter 3's three cognitive-science memory classes (episodic /
semantic / procedural) -- `episodic` is episodic, `facts` leans semantic, `procedures` is procedural,
and `preferences` is stable preferences.

> **Why use two roots rather than adding `EntryType::Project`**: the type axis (what this is) is
> orthogonal to the ownership axis (whose it is). Putting project memory into `type` inflates the
> types, and it would still land only in the global root and not travel with the repo. Carrying
> ownership with two roots, `core.md` / `index.md` / `vectors.json` are **rebuilt per root**
> (`curator::rebuild_core` / `rebuild_index` iterate `store.roots()`), and reads merge (the workspace
> overrides an `id` of the same name).

**3.2 Entry schema (frontmatter)**

```markdown
---
id: pref-rust-error-style
type: preference            # preference | fact | event | procedure (semantic type)
domain: personal            # personal | project (ownership domain: which storage root it lands in)
subject: rust-error-handling
created_at: 2026-05-10
valid_from: 2026-05-10      # effective time (the key to the timeline)
supersedes: pref-rust-error-style-v1   # which one it supersedes (does not delete the old entry)
status: active              # active | superseded | unconfirmed
confidence: high            # high | medium | low (low when evidence is insufficient)
scope: rust / domain error handling   # * applicable scenario (qualification): each limits itself on conflict, no either/or
utility: 0.8                # optional: historical usefulness (organize / sort reference, V2)
usage_count: 0              # optional: number of times hit by retrieval (V2 importance score)
source:                     # * evidence reference, traceable
  - session: 2026-05-10-abc.jsonl#turn:14
---

The user prefers defining domain errors with thiserror rather than anyhow.
```

Field descriptions:

| Field | Role | Source |
| --- | --- | --- |
| `type` / `subject` | classification and topic, for the index and retrieval | `plan.md` |
| `domain` | **ownership domain**: `personal` (global root) | `project` (workspace root, travels with the repo) | scheme A (orthogonal to `type`) |
| `created_at` / `valid_from` | **timeline**: when written, when effective | `plan.md` + decision 4 |
| `supersedes` / `status` | versioning: the supersede relation, history not deleted | decision 4 (the Mem0 v3 lesson) |
| `confidence` | degrade-label when evidence is insufficient | `plan.md`'s "distillation verification" |
| `scope` | **applicable scenario**: both sides of a conflict coexist under different conditions | decision 4 (qualification) |
| `utility` / `usage_count` | usefulness / hit count, for organization and sorting | the paper's `utility` + `usage` |
| `source` | **evidence reference**: points at `sessions/*.jsonl#turn:N` | pain point 2 evidence |

**3.3 Index page `index.md` (horizontal association)**

Chapter 3 explicitly warns: **a flat plain-text dump degrades into "islands"** -- the more knowledge,
the harder to find. The solution is to build entry pages and cross-links like Wikipedia. `index.md`
is maintained by the program, one line each:

```markdown
- [pref-rust-error-style](preferences/rust-error-style.md) · preference · thiserror not anyhow · 2026-05-10
- [event-ts-migration-kickoff](episodic/2026-05-12-ts-migration.md) · event · TS backend migration kickoff · 2026-05-12
```

> Chapter 3 also reminds: **most models do not proactively build links**. So the write prompt must
> explicitly require "before adding an entry, retrieve and link related entries, and update
> `index.md`" -- you cannot count on the model doing it spontaneously.

---

**4. Two-layer injection ("every query automatically carries context")**

Reuse the `ContextProvider` seam; the injection position is **exactly the same** as `todo.md` decision
2: as a `System` message **appended at the end of each round's request**. The reason is likewise that
it does not move the prefix, is cache-friendly, and compaction cannot touch it.

**4.1 Two injection levels**

1. **Always injected**: the full `core.md` (profile / explicit preferences / active projects). Small
   and stable, prefix-cache friendly -- this is the source of "stably visible".
2. **Relevant injection**: the program matches the top-k entries' **summaries** (not full text) from
   `index.md` by **the current query + working directory**, and injects them. When the AI wants the
   full text it retrieves it with `read`.

```
+-- Always-resident overview layer (L0/L1) ------------------+
|  core.md + the matched top-k entry summaries               |
|  -> injected automatically each round (appended by ContextProvider) |
+-- On-demand detail layer (L2) -----------------------------+
|  full entry text / raw conversation JSONL / evidence       |
|  -> retrieved by the AI with read / a future memory tool |
+------------------------------------------------------------+
```

**4.2 Render as XML**

The same style as `todo.md`'s `<task_state>` (`plan.md`: the tag form raises the model's attention to
structural information), making it easy for the model to distinguish "this is long-term memory" from
the current conversation:

```xml
<memory_context>
<core>
The user is Sean, prefers Rust, dislikes over-abstraction.
</core>
<relevant>
- [preference] The user prefers thiserror over anyhow (2026-05-10, active)
- [event] 2026-05-12 TS backend migration kickoff, currently migrating the user module
</relevant>
</memory_context>
```

**4.3 Budget guardrails**

- `core.md` hard cap (e.g. 500 tok / the implementation uses `MAX_CORE_CHARS = 2000` chars); when
  exceeded, `curator::rebuild_core` truncates by a fixed rule and labels it, so it does not balloon
  without bound;
- Relevant injection top-k cap (e.g. 3 entries) + a total injection character cap (e.g. 2000 chars);
  when exceeded, truncate and label -- preventing memory from monopolizing the context (the same idea
  as `todo.rs`'s `MAX_RENDER_CHARS`);
- **Empty memory is not injected** (`render()` returns `None`) -- avoiding one meaningless extra
  system per round.

---

**5. Write path: the background curator**

**5.1 Incremental update (triggered at session end)**

```
session end / every N rounds
  -> Curator(Proposer): read the recent conversation + existing relevant memories
       first retrieve existing entries (do not crudely append to the end of the file)
       -> produce a diff that is as small as possible yet complete: add / delete / modify
         + maintain links / index.md / source / time fields
  -> probe (optional, see 5.4): verify candidates with read-only workspace tools
       (counterexamples / untouched scenarios / stale mappings / preconditions / shorter paths)
        skip if there is no safe read-only surface, degrading to pure-trajectory curation
  -> self-check: verify each assertion has evidence, whether qualification is missing, whether it conflicts with other entries
  -> merge into memory/ (write markdown) + update index.md
```

**5.2 Periodic reorganization (triggered by period / threshold, i.e. chapter 3's "sleep learning")**

Three tasks, landed one by one:

1. **Dedup / remove-stale / merge**: scan all of `memory/`, identify semantically duplicate,
   already-superseded, over-fragmented entries -> merge / rewrite / relink, and rebuild `index.md`.
   (What is deleted is the knowledge expression; the **raw evidence is untouched**.)
2. **Verify against the raw data**: check entry by entry against `sessions/*.jsonl` whether an old
   entry missed a key fact, dropped a negation / time condition, or treated a guess as fact.
3. **Conflict resolution and scenario qualification**: see section 6.

**5.3 Review: honest degradation, not pretending to be dual-model**

Chapter 3 suggests Proposer / Reviewer use **models from different families** (cross-source mutual
review). A local CLI is expensive, so Shirley **degrades in V1** to "same model with an independent
prompt + deterministic checks":

- Deterministic checks (not relying on an LLM): the frontmatter schema is valid, the `source` file
  really exists, the `supersedes` target id exists, the time fields are valid, links are reachable;
- **Do not merge if it fails**, never pass by default;
- V2 allows configuring an independent curator model for cross-source review.

**5.4 Environment-probing curation (the paper cited by `plan.md`)**

Paper: *Grounding Agent Memory: Environment-Probing Curation for Enterprise Agents* (arXiv
2609.11060). It targets exactly this document's pain point 4 -- **the async curator already exists,
but can only see "completed trajectories"**, so it keeps errors, over-generalizes local evidence, and
retains stale knowledge (the paper gives three examples: one records a wrong aggregation and its
answer without giving an alternative procedure; one gives only a broad domain map while missing a key
relation; one keeps a deleted field name after a schema drift). Root cause: **a single trajectory is
only a single, local, possibly-wrong observation of the environment** -- exactly what `plan.md` calls
"not the optimal logic".

The paper's solution (without changing the task agent / retriever / memory representation / write
permission): after the task closes, give the existing async curator **a minimal set of read-only
environment tools**, letting it go through **propose-probe-commit** -- first propose candidate
memories, then do targeted read-only probing for specific uncertainties, then use the observation to
decide create / revise / narrow / delete / skip. It can: distinguish an accidental answer from a
reusable relation, compare shorter paths, verify a claimed relation on another slice, check the
preconditions of a procedure, check a state the trajectory missed, and re-query the current
environment when drift is suspected. **Probing is used only to evaluate candidate memories, not to
solve future tasks, and cannot change the environment / enter the trajectory / consume the task
agent's budget / expose future tasks**; **if there is no safe read-only surface, fall back to pure
trajectory curation**.

Landed in Shirley (V1 simplification):

- **The "environment" is the workspace**: read-only probing = the existing `read` / directory
  listing (the SDK's `WorkSpace` already guarantees out-of-bounds rejection); no new tools, no write
  permission needed;
- **The probe target is `procedure` / fact-class candidates**: such memories must cite **verified
  evidence**; if no evidence is available, set `confidence: low` or do not write, blocking it before
  the memory store, to prevent an accidental path from sedimenting into "experience";
- **Pre-distillation (optional)**: the paper has a **non-writing** distiller before the curator,
  compressing the raw trajectory into a fixed-label "evidence package" (`overview / history /
  work_done / technical_details / important_files / next_steps / checkpoint_title`). The key
  boundaries are worth copying: **the distiller cannot see terminal feedback, cannot write memory,
  and treats the rollout as partial evidence rather than ground truth** -- one pass does not
  automatically verify every intermediate assumption. Shirley V1 can skip the standalone distiller
  first, but the curator's input prompt should carry the same boundary declaration.

Effect (paper measurements, as directional reference): on CLBench, probing raises the pass rate
39%->73% and reward 8.60->22.60, with per-question queries 8.8->4.7; relative to pure trajectory
memory, probing upgrades the record from an answer-anchored warning like "don't use X (because it
computes wrong)" to an **executable procedure** like "which table to join, on what key, filtering
what, taking what grain". **The gain depends on the evidence gap left in the trajectory**: when the
trajectory is already enough to support an executable record the probing gain is small, and it is
largest when an unresolved join / position / procedure is left (the paper itself says this is a
mechanistic explanation, not an established subgroup effect).

---

**6. Conflict handling = adding time to organize it**

- **Never delete history**: a new fact `supersedes` the old entry, and the old entry gets
  `status: superseded`.
- **Each carries a timeline**: `created_at` / `valid_from`; retrieval sorts by time and filters out
  the expired.
- **Trace to the source on conflict**: when periodic reorganization hits a contradiction, go back to
  each one's `source` evidence and check whether they hold respectively under **different times /
  objects / tasks / preconditions**:
  - Both hold -> write the applicable scenario into the entries (qualification), **not choosing one**;
  - Evidence insufficient -> keep the conflict + `status: unconfirmed`, **do not force convergence**.
- This is consistent with chapter 3 experiment 3-11: adding a "time + person + intent" prefix to
  contradictory entries is what makes the contradiction block's priority judgeable.

> In one sentence: **a conflict is not a bug to be eliminated, but a normal state to be expressed
> with a timeline and applicable scenarios.**

---

**7. Trade-offs with AgentLab (critical)**

**Worth borrowing**:

- Layered memory types (working / episodic / semantic / perceptual) -> adopted as the directory
  classification;
- The `MemoryItem` soft-delete state machine (`active | invalidated | merged`) -> adopted as
  `status: active | superseded | unconfirmed`;
- `add_batch` batching to save LLM calls, the `candidate -> filter -> score -> take` retrieval
  pipeline, and the three `forget` strategies -> as design references for the curator and retrieval.

**Must avoid** (the parts AgentLab "implemented badly"):

- x Heavy backend (PG + pgvector + Neo4j + Ollama) -- violates Shirley's "zero dependency, light";
- x **Programmatic LLM extraction** (calling an LLM for conflict adjudication on every add) -- high
  cost, tying memory to model calls;
- x Hard-coded magic numbers for semantic routing / importance, `unwrap_or_default()` breaking the
  `Option` semantics, matching error strings, leaving `eprintln!` in production code, unbounded
  growth with no cleanup;
- x **Only registering tools + system_prompt guidance, without automatic injection** -- exactly the
  source of "unstable experience".

**Shirley does the opposite**: light, pure markdown, model-driven (the AI writes proactively + a
background curator organizes automatically), **automatically injecting the always-resident layer**.

---

**8. Relationship to existing pieces**

| Capability | What it records | Who triggers | How it is restored |
| --- | --- | --- | --- |
| compaction | a conversation summary | automatic (threshold) | the summary itself |
| todo | this round's task progress | the AI updates proactively | injected at the end of each round |
| **memory** | **cross-session user facts / preferences / events** | **the AI writes proactively + a background curator organizes** | **the always-resident layer injected at the end of each round + on-demand read** |

- **Isomorphic to todo, different responsibility**: todo manages "how far this round got", memory
  manages "who this user is". Both go through `ContextProvider` end-injection and can each append a
  system message in the same request.
- **Relationship to session**: `sessions/*.jsonl` is memory's **raw evidence layer** (already present,
  append-only), and memory entries reference it via `source` -- **no new evidence layer needed**.
- **Relationship to compaction**: memory does not go into `self.messages`; it is held separately,
  compaction cannot touch it, and it naturally survives across compactions and sessions.

---

**9. Implementation location**

```
crates/shirley/src/memory/
├── format.rs    # entry schema (frontmatter parse / serialize)
├── store.rs     # MemoryStore: markdown directory read/write
├── index.rs     # index.md maintenance + retrieval (BM25 / vector / hybrid_search RRF fusion)
├── provider.rs  # MemoryContextProvider: impl ContextProvider (injected each round)
├── curator.rs   # background curator (incremental + periodic reorganization + rebuild_core)
├── embed.rs     # OpenAI-compatible embeddings client (V2.5 semantic leg, optional)
└── vector.rs    # sidecar vector storage (cosine, multi-root merge; V2.5)
```

| File | Responsibility |
| --- | --- |
| `crates/shirley/src/memory/` (**application layer**) | the five submodules above |
| `crates/shirley/src/bootstrap.rs` (application layer) | `build_agent` creates a fresh `Arc<MemoryRuntime>` per session, and via `CompositeContextProvider` combines `TodoContextProvider` and `MemoryContextProvider` into a **single** `context_provider` injection (the SDK's `Agent` takes only one provider, hence this glue). Returns `BuiltAgent { agent, memory }` -- bringing the `MemoryRuntime` handle back to the driver |
| `crates/shirley/src/interface/session/mod.rs` (application layer) | `Session` holds `memory: Option<Arc<MemoryRuntime>>` (one per session, with an independent query slot); on leaving the foreground, `switch_to` / `create_new` calls `spawn_curation` (`Handle::try_current()` guard, background best-effort) |
| `crates/shirley/src/interface/tui/mod.rs` / `desktop/shell.rs` (application layer) | before starting a round, `memory.set_query(this round's user input)` and **`await prefetch_query_vector()`** -- `ContextProvider::context()` is a **synchronous** interface while embedding is async HTTP, so the query vector must be **pre-fetched before entering `run_stream`** and put into an in-memory slot; `context()` only reads the slot and does no network |
| `crates/shirley-agent-sdk/src/runtime/context.rs` (SDK) | reuses the existing generic seam `ContextProvider`, **unchanged** |

- The optional `remember` / `recall` memory tools go through an application-layer `Tool` (a
  hand-written `impl Tool`), **not into the SDK**.
- **The SDK adds no new external types**; `lib.rs`'s `pub use` is untouched.

---

**10. Phased landing**

| Phase | Content | Solves |
| --- | --- | --- |
| **V1 (stable) · implemented** | `core.md` always-resident injection + `index.md` keyword-retrieval injection + session-end incremental curator (same model + deterministic self-check) + temporalized conflict fields | automatic carry, async curator, conflicts not deleting history |
| **V2 · implemented** | periodic reorganization (sleep learning) + verification against the original evidence + retrieval upgraded to BM25 + a configurable independent curator model | full dedup / merge, evidence re-query, cross-source review |
| **V2.5 · implemented** | **hybrid retrieval**: BM25 keyword leg + optional embedding semantic leg (RRF fusion) + Chinese bigram tokenization + deterministic automatic rebuild of `core.md` + curation before exit | recall for Chinese natural-language queries, synonym rewrites, the always-resident layer never missing |
| **V3** | cross-workspace memory, multimodality (chapter 3.3.7) | large-scale recall, proactive service |

**V1 acceptance criteria**:

1. When a new session is opened, `core.md` **automatically appears in the request** (not relying on
   the AI calling a tool);
2. The curator can produce an entry with `source` after the session ends;
3. Two conflicting preferences can coexist via a `supersedes` timeline, and retrieval sorts by time;
4. When memory is empty, **it is not injected** and does not degrade the status quo.

**V2 landing notes** (implementation in `crates/shirley/src/memory/`):

- **V2-A retrieval upgraded to BM25** (`index.rs`): term-frequency saturation + document-length
  normalization + IDF; field differences are kept as **weighted term frequency** (`subject` x3 / body
  x1 / `id`+`scope` x0.5), and V1's "subject hits beat body hits" semantics still holds under BM25.
  The retrieval interface (`query -> relevant entries`) is unchanged.
- **V2-B periodic reorganization / sleep learning** (`curator.rs::consolidate`): full scan -> the
  model produces a `ConsolidateOp` (`Merge` / `Supersede` / `Requalify`) -> each op is
  **deterministically self-checked** before execution. The self-check rejects: merge with no sources,
  a reference to an unknown source, requalify with an empty body / no change / acting on an
  already-superseded entry. **History is never deleted** (old entries are only set to `superseded`);
  insufficient evidence does not force convergence, and different conditions get a `requalify` adding
  `scope`. When a new entry lacks `source`, it inherits from the union of the old entries, **not
  fabricated**.
- **V2-B verification against the original evidence**: `collect_evidence` parses `source`
  (`<session file>#turn:N`, taking that line 1-based, otherwise the whole file truncated to 800
  chars); a missing file records `[missing evidence]`; the evidence injection cap is
  `MAX_EVIDENCE_CHARS = 6000`.
- **V2-C independent curator model**: the `[memory]` table can configure `curator_model` /
  `curator_base_url` / `curator_api_key` / `curator_protocol` (all `Option`, merged layer by layer;
  when unconfigured it **honestly falls back to the main model**); `consolidate_after_entries` is the
  trigger threshold (`0` = no automatic trigger). `bootstrap::build_curator_config` overrides only
  the explicitly given fields and inherits the rest from the main config.
- **V2-D `usage_count` / `utility` maintenance**: `MemoryRuntime` records a hit on every relevant
  injection (an in-memory `HashMap`), and when curation is persisted `flush_usage` writes back the
  `usage_count` accumulation in one go, deriving `utility` from the saturation curve `n/(n+5)`;
  retrieval then multiplies by a mild `utility` factor (`0.9 ~ 1.1`, missing = `1.0`) -- only a
  fine-tune when relevance is close, not overpowering BM25 relevance.
- **Trigger timing**: when a session is switched away / created, `spawn_curation` first does an
  incremental curation, then best-effort triggers one `consolidate` by `should_consolidate()` (entry
  count reaching the threshold); the whole thing is in the background, and a failure only logs,
  never blocking session switching.

**V2.5 landing notes** (hybrid retrieval + fixes for three real-world pits):

- **V2.5-A hybrid retrieval (`index.rs`)**: `hybrid_search` fuses the two legs' **rankings** with
  **RRF (Reciprocal Rank Fusion, k=60)** -- `score = sum 1/(k + rank_i)`. Each leg recalls
  `max(limit*4, 20)` candidates, then they are fused and the top-k taken. **Fusing by ranking rather
  than score** is because the BM25 score and the cosine component have different dimensions, and
  directly weighting them needs tuning and is unstable; RRF looks only at rank, is naturally
  tuning-free and dimension-immune. When no query vector is provided, `hybrid_search` **directly
  degrades to pure BM25** (`search`), with zero branch cost.
- **V2.5-B Chinese bigram tokenization (`index.rs::tokenize`)**: V1/V2 treated **a whole contiguous
  CJK run as one term**, so a Chinese natural-language query ("what is my name") almost necessarily
  got 0 hits (confirmed in practice; see `docs`). Change to: split a CJK string into **adjacent
  two-character bigrams** (e.g. "the user's name" -> the adjacent pairs of the characters), while
  English / digits are still split by word. Bigrams need no dictionary and are robust to
  out-of-vocabulary words, making them the zero-dependency baseline for Chinese retrieval.
- **V2.5-C semantic leg (`embed.rs` + `vector.rs`)**: `embed.rs` is an OpenAI-compatible
  `POST /v1/embeddings` client (batching `MAX_BATCH=64`, ordering by the response `index`, no
  redirect-following, connect 5s / total 15s timeout, response body capped). `vector.rs` stores
  `id -> vector` as one `vectors.json` sidecar per root (including the **model identifier** that
  generated it; when the model changes the whole old vector set is invalidated, preventing "querying
  a B-model query with an A-model vector"). Retrieval reads it into memory for brute-force cosine.
  **Still zero vector library**.
- **V2.5-D query vector prefetch**: `ContextProvider::context()` is a **synchronous** interface and
  embedding is async HTTP -- so the query vector is computed by the driver (TUI `run_agent` /
  desktop `agent_send`) with `await prefetch_query_vector()` **before entering `run_stream`** and
  stored in an in-memory slot; `context()` only reads the slot and does not go to the network. **A
  prefetch failure / unconfigured embedding is always a no-op**, and the semantic leg is silently
  absent.
- **V2.5-E deterministic rebuild of `core.md` (`curator::rebuild_core`)**: V1's `core.md` depended on
  manual / application explicit maintenance, and in practice it was never generated (`write_core` was
  `dead_code`), so the always-resident layer was never injected. Change to **the program projecting
  it from active entries by a fixed rule**: take only `status: active`, ordered `preference -> fact ->
  procedure -> event`, newest-to-oldest within a group and then stably sorted by `id`; each line
  `- [type] summary`; truncated with a label past `MAX_CORE_CHARS = 2000`; delete the old file if the
  store is empty. It is called at the tail of **both** `curate()` and `consolidate()`, ensuring the
  always-resident layer stays in sync after entries change. **Not generated by an LLM** -- the
  always-resident layer must be small and stable and prefix-cache friendly, and only a deterministic
  projection is reproducible.
- **V2.5-F curation before exit (`SessionManager::finish`)**: V1 triggered curation only on "leaving
  the foreground" (switching sessions / creating a new one), so chatting in a single session to the
  end and **exiting the program did not trigger it** -> memory was never organized. Add
  `SessionManager::finish()`: the exit path runs one curation + `refresh_vectors` for the
  **foreground session**, and the TUI `await`s it on exit. Still best-effort; a failure only logs.

---

**11. Known gaps**

1. **No Git integration**: the plan suggests putting the memory directory into Git for
   versioning / rollback; V1 did not hook it up.
2. **Limited review capability**: V1 is same-model self-review + deterministic checks, weaker than
   chapter 3's cross-source Reviewer; complex evidence can still be merged wrongly.
3. **Retrieval precision**: V1 keyword matching is insensitive to synonym rewrites, with limited
   recall, compensated by `index.md` summaries.
4. **Curator trigger timing (V1 approximates)**: V1 treats "leaving the foreground" (`switch_to` /
   `create_new`) as session end and triggers one curation -- the user just temporarily switching away
   and back also triggers it an extra time; **exiting the application does not trigger it** (the
   runtime may be shut down first). A more precise determination (idle timeout / explicit end /
   flush before exit) is left for later. The trigger is best-effort: it is silently skipped when
   there is no memory, no messages, or not in a tokio runtime (unit tests).
5. **Privacy**: chapter 3 suggests PII redaction before storing; automatic PII redaction is not implemented. Optional cloud sync sends explicitly personal entries; legacy entries require review first (see `memory-sync.md`).
6. **Injection loses the prefix cache**: the same cost as `todo.md` -- when `core.md` changes, the
   prefix after it is invalidated, minimized by "put it at the end + stable content".
7. **Limited coverage of environment probing** (paper 2609.11060): V1's "environment" is only the
   workspace, and probing = read-only `read` / directory listing; it cannot probe external
   systems, running state, or future tasks. So probing's gain **depends on the evidence gap left in
   the trajectory** -- it is small when the trajectory is already enough to support an executable
   record, and largest when an unresolved join / position / procedure is left (the paper also
   stresses this is a mechanistic explanation, not an established subgroup effect). When there is no
   safe read-only surface, it should **degrade to pure trajectory curation**, not force it.
