# docs/desktop-interface.md

**Shirley technical design · Desktop Interface**

> **Status: design phase (not yet implemented).** This document is the technical design for migrating
> the shiwen chat UI to a Shirley desktop interface.
> **Confirmed decisions**: form 1 (Tauri 2 + React/TS, direct copy) / coexists with the TUI /
> introducing a Node build chain is allowed / the approval flow is kept but not shown in phase one.
> After landing, please fill in the "implementation landing" section and sync the document index in
> `docs/README.md` / `Agent.md`.
>
> Reference source: `/Users/sean/Desktop/repo/shiwen-open-source` (hereafter **shiwen**).

---

**1. What this document solves**

The user likes shiwen's **chat interaction and UI** (not its business), and wants to add an interface
to Shirley, migrating shiwen's chat UI over with Tauri and stripping out shiwen's custom logic.

Shirley currently has only one interface: the ratatui TUI under `src/interface/`. This design adds a
second interface -- a Tauri desktop window + React frontend -- that **coexists** with the TUI, the
two **sharing the same underlying layer (LCA)**.

---

**2. Key conclusion: it is a "sibling interface", not "a web layer stacked on top of the TUI"**

A shape question that must be clarified first (the user asked in these terms):

- ❌ It is **not** "an extra web layer on top of the TUI". ratatui takes over the terminal
  (alternate screen / raw mode / mouse capture), the webview is an independent native window, and
  **the two cannot draw the same conversation at the same time**; moreover `interface/` is full of
  TUI-specific logic (`tui.rs` event loop, `ui.rs`'s `MessageCache`/line-wrap estimation,
  `selection.rs` mouse selection, `markdown.rs` converting to ratatui `Line`/`Span`), which is
  useless for the web.
- ✅ It **is** "two sibling interfaces sharing the same LCA". The TUI and desktop are **two consumers
  of the same `Agent`**, each rendering on its own, not interfering.

```
                    ┌───────────────────────────────┐
                    │  main.rs (assembly layer, the only entry) │
                    │  settings / session / tools /   │
                    │  prompt / model_config          │
                    └───────────────┬─────────────────┘
                                    │ constructs Agent + catalogs
                    ┌───────────────┴─────────────────┐
                    ▼                                 ▼
        ┌───────────────────────┐        ┌───────────────────────┐
        │  interface/ (TUI)     │        │  interface/desktop/    │
        │  ratatui rendering     │        │  Tauri shell + React web  │
        └───────────┬───────────┘        └───────────┬───────────┘
                    │ consumes AgentEvent               │ consumes AgentEvent
                    └───────────────┬─────────────────┘
                                    ▼
                    ┌───────────────────────────────┐
                    │  shirley-agent-sdk::Agent      │
                    │  run_stream() → AgentEvent stream   │
                    └───────────────────────────────┘
```

**LCA (common logic) = the SDK's `Agent` (`run_stream()` -> `AgentEvent` stream) + `main.rs`'s
application assembly.** `AgentEvent` is that common interface: the TUI consumes it in `tui.rs::apply`,
and the desktop consumes it in Tauri commands, turning it into webview events. **Both sides connect
to the same event stream, not the same rendering.**

---

**3. Shared boundary (the things to pin down)**

| Layer | Ownership | Notes |
| --- | --- | --- |
| `src/settings.rs` / `session.rs` / `models.rs` / `prompt.rs` / `tools/` | **shared** | modules at the crate root, interface-independent, used by both interfaces |
| `src/main.rs`'s assembly | **shared, needs a small change** | currently "assemble then directly `interface::run`"; extract into `Bootstrap` and dispatch by mode |
| `shirley-agent-sdk` (`Agent` / `AgentEvent`) | **shared** | untouched |
| `src/interface/` (tui/ui/app/update/event/selection/markdown/command) | **TUI-specific** | kept as-is |
| `src/interface/desktop/` (Tauri shell + `web/`) | **desktop-specific** | new |

**Two refactors needed first**

1. **Extract `Bootstrap` + dispatch in `main.rs`.** Extract the assembly currently at lines 26-104 of
   `main.rs` (settings / models catalog / tools / model_config / session / Agent) into a `Bootstrap`
   struct, so both interfaces get the same `Agent` + catalogs. **Not copying the assembly, but truly
   sharing it.**
   ```rust
   let bootstrap = Bootstrap::assemble(working_dir)?;
   match mode {                                  // from --desktop / environment variable
       Mode::Tui     => interface::tui::run(bootstrap).await,
       Mode::Desktop => interface::desktop::run(bootstrap).await,
   }
   ```
2. **Slash-command ownership** (`/login` `/model` `/session` `/rewind` in
   `src/interface/command.rs`). These are application-level behaviors that the desktop also needs.
   **Do not share them in the first step**: the desktop first does only rendering and send/receive,
   and the commands are sunk into a shared `commands` module in a later milestone.

---

**4. Migration scope: what to copy, what to delete**

**Principle: minimum migration cost -- copy the key code directly, then wire up the SDK.**

### 4.1 What to copy (shiwen path -> Shirley landing)

| shiwen source | Role | Disposition |
| --- | --- | --- |
| `apps/web/src/lib/components/ai/AiConversationSurface.tsx` | conversation shell (transcript + composer) | copy |
| `apps/web/src/lib/components/ai/AiConversationTranscript.tsx` | message list + auto-scroll + avatar | copy (the minimap is already extracted; the host `App.tsx` mounts it at the left edge of the chat area's outermost container `<main>`) |
| `apps/web/src/lib/components/ai/AiChatComposer.tsx` | input box (auto-resize / Enter to send / stop) | copy |
| `apps/web/src/lib/components/ai/AiMessageTimeline.tsx` | interleaves text blocks and tool cards by `content_offset` | copy |
| `apps/web/src/lib/components/ai/AiMarkdown.tsx` | streaming markdown (based on `streamdown`) | copy (drop the citation plugin) |
| `apps/web/src/lib/components/ai/AiAssistantAvatar.tsx` | assistant avatar | copy (swap for a Shirley figure or leave a placeholder) |
| `apps/web/src/lib/components/ai/AiToolExecutionCard.tsx` / `AiToolActivityDisclosure.tsx` | tool execution card / activity collapsible | copy (keep the approval-path code, do not render it in phase one) |
| `apps/web/src/lib/chat-input.ts` | Enter-to-send decision | copy |
| `packages/ui/src/chat.tsx` (`Message` / `Bubble` / `MessageScroller` / `useMessageAutoScroll`) | chat base primitives | copy |
| `packages/ui/src/tokens.css` | Tailwind 4 design tokens | copy |
| `packages/ui/src/utils.ts` (`cn`) | class merge | copy |
| the needed `packages/ui/src/{button,textarea,scroll-area,...}` | dependency primitives | copy as needed |

### 4.2 What to strip (shiwen custom logic, **confirm before migrating**)

| shiwen-specific | Why it belongs only to shiwen | Disposition |
| --- | --- | --- |
| **A2UI surfaces** (`A2uiTurnSurface` / `A2uiClientAction` / `AiQuestionSurface`) | shiwen's own server-driven structured UI protocol | delete |
| **Entity mentions / chips** (`EntityChips` / `EntityMentionPopover` / `useEntityMentions`) | article/space/document/material -- shiwen's knowledge base | **migrate its interaction skeleton, rebind the object to workspace files** (see 4.3) |
| **Citations** (`[[citation:id]]` plugin / `inlineCitationMarkdown` / `StreamCitations`) | bound to shiwen retrieval + entity URLs | delete |
| **Tool approval flow** (`approval_state` / `onToolDecision` / approve/reject on the card) | shiwen's human-in-the-loop gate; Shirley has no permission layer yet | **keep the code, do not show it in phase one** (the user confirmed) |
| **Entity/space/article anchoring** (`host_kind` / `promoted_space_id` / `host_repository`) | shiwen's workspace model | delete |
| **Conversation sharing** (`AiConversationShare` / public sharing) | shiwen feature | delete |
| **OCR / material upload** (`fetchMaterialOcrStatuses` / `handleMaterialUploaded`) | shiwen desktop app | delete |
| **Desktop workspace switcher** (`useDesktopWorkspaces` / `DesktopScreen`) | shiwen desktop login/workspace (bound to its account system) | delete the shiwen implementation; **Shirley builds its own multi-project UX** (see 4.5) |
| **SSE wire format** (`parseAiStreamEvent` / `streamAiMessage` in `packages/ai-session`) | shiwen server protocol | **rebind** to Shirley's `AgentEvent` (see section 6) |
| `AiConversationMinimap` (turn thumbnails) | a row of small dashes on the left of the chat area, hover to preview / click to jump to a historical user turn | copy (already enabled: mounted at the **left edge** of the **chat area's outermost container `<main>`** (the block that expands the content and excludes the sidebar) -- `App.tsx` holds `transcriptViewportRef` and passes it through the transcript to `MessageScroller`, then positions the minimap inside `main`, outside `section`, with `className="left-3"` (12px from the left edge of `main`); and it **auto-shows/hides based on the available whitespace width** -- `App.tsx` uses `ResizeObserver` to measure the `main` width, and hides it outright when the left whitespace `(main width - 760) / 2` cannot fit 12 + 40px, never overlapping the chat content) |
| shiwen dependencies beyond `lucide-react` (tiptap / blocknote / pdfjs / tesseract etc.) | shiwen document/OCR stack | do not introduce |

> **Confirmation points**: every row in the table above needs the user's sign-off.
>
> **Confirmed (user)**:
> - **Keep the approval-flow code, do not show it in phase one** -- the data structures and callback
>   chain of `approval_state` / `onToolDecision` are migrated as-is first, and the render layer does
>   not draw approval buttons in phase one; enable it later when the permission layer is wired up.
> - **Keep the `@` reference interaction, rebind the object to workspace files** (see 4.3) -- no
>   longer handled as "delete everything entity-related".
> - All other rows are deleted by default.

### 4.3 `@` references (migration + rebinding, **landed**)

shiwen's `@` logic (typing `@` triggers a candidate popover, selecting generates a chip, `EntityChips`
echoes it) has a skeleton worth keeping, but the object it binds to is a shiwen knowledge-base entity
(article/space/document/category/conversation/material). The only referenceable objects that really
exist in Shirley are **files and directories in the workspace**, so it is **rebound** rather than
copied:

| shiwen source | Shirley landing | Change |
| --- | --- | --- |
| `lib/entity-mentions/useEntityMentions.ts` | `web/src/lib/file-mentions/useFileMentionSearch.ts` | the object becomes `FileReference`; search goes through the Rust side `agent_search_files` (local filesystem), no more paging / cursor; **keep only the headless search controller** (popover state + async search), the input area DOM is managed by the editor |
| `lib/entity-mentions/EntityChips.tsx` | `web/src/lib/file-mentions/FileChips.tsx` | icons converge to file / directory; drop material OCR / delete-material; `FileChip` (single, inlineable) + `InlineReferences` (body interleaves placeholders and references) |
| `lib/entity-mentions/EntityMentionPopover.tsx` | `web/src/lib/file-mentions/FileMentionPopover.tsx` | drop grouping (article/space/...), OCR badges, paging "load more"; keep only the result list + keyboard selection (the search box is carried by the editor, `showSearch=false`) |
| `lib/entity-mentions/entityLoader.ts` | Rust `src/workspace_search.rs` + `agent_search_files` | search lands in the application layer (workspace traversal + keyword ranking), not through the SDK |

- **How a reference enters the model**: `@` only acts on the interface -- after selection an inline
  chip is inserted at the cursor, and on send `shell.rs::compose_prompt` splices the reference path
  into **this round's** prompt. **A reference is not part of the Agent's external contract**:
  neither `Message` nor `AgentEvent` changes, and the SDK adds no external type because of it (see
  section 8, acceptance item 4).
- **Search boundary**: traverse only under the workspace root, skipping `.git` / `node_modules` /
  `target` / `dist` etc., with a cap on the entry count; this is the same origin as the `read`
  tool's workspace constraint, but **implemented in the application layer**, not touching the SDK.

#### 4.3.0 Inline mixed layout: `contenteditable` rich input (**landed**)

The first version made references a "body-external `references` array + a `FileChips` block at the
top of the input box": the chips and the text **belonged to two layers**, always stacked above the
body, unable to have "text + tag on the same line". The root cause is that the input control was a
`<textarea>` -- it can only render plain text and **cannot carry inline nodes**. The fix is to swap
the input area for `contenteditable` rich input:

- **The body = a string with placeholders**. Inside the editor a chip is a `contenteditable=false`
  inline `<span>`; on serialization it is written as a `U+FFFC` (OBJECT REPLACEMENT CHARACTER)
  placeholder. Thus "text + reference" is still an ordered string: **the Nth placeholder <-> 
  `references[N]`**. Before sending, `stripTokens` strips the placeholders, and the references still
  go through the existing `references` array (not a single line of Rust `compose_prompt` changed).
- **Do not re-render the DOM during editing** (otherwise the caret is necessarily lost). React only
  renders the editor root `<div>`, and the inner content is maintained imperatively by
  `MentionEditor`; ordinary input is left to the browser's native editing, and `onInput` only does
  serialization reporting. Only on a **structural change** (insert / delete chip, external reset /
  clear) does it `rebuild` the whole DOM segment, restoring the caret with `pendingCaret`.
- **Delete a whole chip with the keyboard**: on backspace / forward-delete hitting a chip
  placeholder, manually delete that chip node + the corresponding `references[i]` + rebuild
  (otherwise the browser deletes a character and the references array goes out of alignment).
- **The `contenteditable=false` caret dead zones (fixed)**: a chip is a non-editable inline node, so
  the browser neither puts the caret "between the chip and the adjacent text", nor lets the arrow
  keys cross it, nor places a click reliably. Three patches: (1) manually `setCaretAt` when the arrow
  keys cross a chip boundary (ordinary text still uses native); (2) clicking a chip snaps the caret
  to before / after the chip based on which half was hit; (3) backspace / forward-delete deletes the
  whole block at a placeholder. All uniformly use the "serialized-text offset <-> DOM position"
  conversion (`caretOffset` / `setCaretAt` / `chipOffset`).
- **Use native `beforeinput`, not React's `onBeforeInput` (fixed)**: React 18's `onBeforeInput` is
  the old `textInput`/`keypress` composite interface, and `nativeEvent.inputType` is **always
  `undefined`** -- so the previous delete / newline branches based on it were all dead code (which is
  exactly the root cause of "deletion does not work"). Change to
  `addEventListener("beforeinput")` on the editor root node: only a native event can get `inputType`
  and `preventDefault` can stop the default editing.
- **Echo after sending**: the user message's `content` keeps the placeholders, and rendering goes
  through `InlineReferences`, interleaving by placeholder and `message.references`, so the chip
  returns to its original position in the body -- no longer stacked at the top.
- **Implementation landing**: under `web/src/lib/file-mentions/`, `editor-dom.ts` (serialization /
  caret / DOM rebuild), `MentionEditor.tsx` (rich input), `useFileMentionSearch.ts` (headless
  search), `FileChips.tsx` (`FileChip` / `InlineReferences`). `AiChatComposer` gains an `inputSlot`
  which, when passed, replaces the built-in textarea. `src/App.tsx` wires it with the two states
  `input` + `references`, and `send()` uses `stripTokens` for the emptiness check.

### 4.3.1 Markdown render alignment (**fixed**)

`AiMarkdown.tsx` is **byte-for-byte identical** to shiwen (only the `cn` import path differs), so the
inconsistent rendering is **not in the component** but in the **CSS**: Shirley's `styles/index.css`
is missing shiwen's `@source` line.

Tailwind 4 does **not scan `node_modules`** by default. streamdown writes the utility classes for
each markdown block element (`mt-6` / `text-2xl` / `border-border` / `text-muted-foreground` /
`divide-y` / `wrap-anywhere` ...) into its bundled JS strings; without explicitly telling Tailwind to
scan that JS, **not a single one** of these classes is compiled into the output CSS, and markdown
renders "unstyled" (headings not enlarged, lists without spacing, code blocks without border /
background, blockquotes without the vertical bar). shiwen's `index.css` has
`@source ".../node_modules/streamdown/dist/*.js"`, which was missed during the Shirley migration.

Fix: restore that `@source` in `web/src/styles/index.css` (path written per Shirley's `web/` level as
`../../node_modules/streamdown/dist/*.js`), and add the code-block scrollbar's `-track` /
`-thumb:hover` rules (aligning with shiwen's `globals.css`). **When changing the frontend directory
structure, the relative path of this `@source` line must be adjusted accordingly.**

### 4.3.2 Fixed chat width + consecutive tool-call collection (**aligned**)

shiwen's `AiConversationSurface` wraps **the transcript area + input box** together in
`<section className="mx-auto flex h-full w-full max-w-190 min-h-0 flex-col">`
(`max-w-190` = `calc(var(--spacing) * 190)` = 760px). Shirley originally lacked this wrapper, so the
chat area stretched to fill the window. Fix: `App.tsx` adds the same
`<section className="mx-auto w-full max-w-190 min-h-0 flex-1 flex-col">` wrapping
`AiConversationTranscript` + the error line + the input box, so both the chat area and the input box
are fixed-width and centered.

"Collect consecutive tool calls together": shiwen's `FreeChatAssistantMessage` only makes the
**user-visible** execution items (`pending` / approving / completed items with an `entity_url`) into
separate cards, and stuffs **all** the rest into a single `AiToolActivityDisclosure` collapsible,
rendered as one line "View process - N items". Shirley has no entity / approval rendering, so all
execution items go into the collapsible, and consecutive tool calls are collected into one line
rather than one card per call. Inside `AiToolActivityDisclosure` it still sorts by `content_offset`
and aggregates consecutive failures (`toolErrorUtils`). **The collapsible's landing is in 4.3.4**:
rather than all stacked at the top of the body, it is one segment in the linear paragraph flow,
interleaved with body / thinking in event order.

### 4.3.3 Markdown rendering benchmarked against the TUI (**landed**)

4.3.1's `@source` fix only solved "utility classes were not compiled into the output" -- streamdown's
default styling itself still has a "marketing page" feel (`text-3xl` headings, `bg-muted` inline
code, `my-4`/`space-y-4` large spacing), and it depends on a set of **shadcn-style tokens** that
Shirley's `tokens.css` does not fully provide. So "desktop markdown is less elegant than the TUI" is
a **two-layer problem**, and 4.3.1 only fixed the first layer. This subsection fixes the second
layer: **no longer chasing pixel-for-pixel parity with shiwen, but directly benchmarking the TUI's
(`src/interface/markdown.rs`) typography and color scheme.**

Three concrete bugs (all fixed):

1. **`--color-sidebar` undefined**. streamdown paints the code block / table outer container as
   `bg-sidebar` + `border-sidebar`, and Shirley's `tokens.css` has no such item -> these classes
   resolve to nothing, and the container has **no background, no border**. Fix: add
   `--color-sidebar` to `tokens.css` (light `#f1f1ee` / dark `#20201e`, same family as inset).
2. **`bg-muted` semantic conflict**. Shirley treats `--color-muted` as a **text gray** (`#676763`),
   while streamdown uses `bg-muted` as a **light background** (inline code, table header) -> it
   becomes "dark gray background + dark text", with terrible contrast. Fix: in `index.css` change the
   inline code back to a light inset background + thin border
   (`[data-streamdown="inline-code"]`), and likewise for the table header background.
3. **Headings enlarged by `twMerge`**. `AiMarkdown.tsx`'s `h1/h2/h3` overrides use
   `cn("... text-title-sm ...", className)`, and the `text-3xl`/`text-2xl` passed by streamdown come
   **after**, so after `twMerge` the later one wins -> headings too large. Fix: do not fight
   priority via `twMerge`; instead use `[data-streamdown^="heading-"]` in `index.css` to converge the
   font size level by level (H1 1.25rem -> H4+ 0.8125rem), and downgrade H4-H6 to the muted color.

Other alignments (written inside the `[data-ai-markdown]` scope in `web/src/styles/index.css`, **not
affecting the rest of the app UI**):

| Element | TUI (`markdown.rs`) | desktop override |
| --- | --- | --- |
| Block spacing | about one line between blocks | `[data-ai-markdown] > * > * + *` uniformly `0.75rem` (clearing `my-4`/`mt-6`) |
| Inline code | yellow foreground | light inset background + thin border, same font size |
| Code block | border + language label + dark | outer `sidebar` background + `line` border; body on `canvas` background |
| Blockquote | `│` bar + italic + dark | `border-left: 2px line-strong` + italic + muted |
| Table | `│` / `─┼─` grid | header `inset` background, cell `line` border |
| Link | blue underline | `--color-link` + underline |
| Rule | `─` dark | `line` color, `0.75rem` vertical margin |

> These rules are **not put into a Tailwind layer** (written after `@layer utilities`), so they can
> reliably override streamdown's default classes compiled into the utilities layer -- this is the
> standard way to "override a third-party component's default styles with CSS", not relying on
> `twMerge`'s order. When changing `AiMarkdown.tsx`'s `BASE_COMPONENTS`, remember: the component-layer
> font size is covered again by the CSS here, so the two must be looked at together.

### 4.3.4 Linear paragraphs and whole-turn process collapsing (code implemented, UI awaiting user verification)

`AiSegment` holds the `content`, `tools`, `compact` arranged in event order. Adjacent increments of
the same type are merged, and a tool completion event updates the corresponding execution item in
place. Raw thinking does not enter the visible paragraph flow.

Following the Codex screenshots provided by the user, a turn of assistant messages renders only one
process collapsible:

- While executing it shows "Working...", and the process is expanded by default; the user can
  collapse it manually, and later increments do not reopen it.
- On completion it collapses automatically, and the title shows the actual frontend timing such as
  "Worked for 1m 11s"; history without timing data shows "Process", without fabricating a duration.
  The divider is fixed below the collapsible title line, and the expanded content grows below the
  line, no longer hanging the divider at the bottom of the whole collapsible container. The final
  answer is shown outside.
- The compact record is included in the whole-turn process, keeping its original time order, and the
  summary can be expanded separately. compact no longer splits the outer layer into multiple groups.
- A tool group is summarized with text like "Read files, searched web - N items", and after expanding
  the process it shows concise activity lines by default. Each line can expand its arguments and
  output, and failures show a clear status; long content scrolls locally, and ordinary tools no
  longer fill a large card.
- The local cache retains the received tool output, failure status, and known durations; when a
  historical snapshot has no output it says so honestly. After a window restart, timing / tool output
  that was not persisted is not fabricated.
- Stop or failure shows the corresponding title, not treating the intermediate explanation before
  tool execution as the final answer.

Timing is sampled and cleaned up outside the React state-update function, avoiding loss when
StrictMode replays the updater. When switching back to a session still executing, the last turn
resumes streaming display; a start time already recorded in this process is retained.

**Manual acceptance (not yet actually verified by the user):**

1. Open an old session containing multiple compacts: each turn has only one "Process", with the final
   body below it; after expanding, the original tools and all compression summaries can be found in
   time order.
2. Start a task that needs to read files and run commands: expand the process while running; after it
   ends it collapses automatically and shows the real duration; manually re-expanding shows the
   explanation and tool activity lines, each item expandable for arguments and output.
3. Manually collapse the process while executing: new events do not automatically reclaim the
   expanded state; the running indicator is still visible, and the final body appears on completion.
4. After expanding tool details, switch sessions and switch back: the received output and failure
   status are retained; a running session still shows "working".
5. Stop or manufacture a tool/request error: stop and failure states are clear, the process can be
   reviewed, and the intermediate explanation does not masquerade as the final answer.
6. Narrow window and keyboard operation: the title truncates without widening the chat area, Tab can
   focus the collapsible title, and Enter/Space can expand/collapse.
7. Repeatedly expand/collapse the process, and expand tool details and compression summaries: the
   divider always follows the title, and its spacing and left/right boundaries stay unchanged; after
   collapsing, the hidden content's padding leaves no leftover blank space.

Do not write UI unit tests. Static verification only runs the frontend typecheck and production
build, which cannot replace the manual acceptance above.

### 4.3.5 Chat flight-route map (code implemented, UI awaiting user acceptance)

`AiMarkdown` registers a `flightmap` renderer; `FlightMapRenderer` loads the map and ECharts via
lazy import, and isolates load/render failures with a local error boundary. `FlightMap` must first
`echarts.use()` to install Geo, Tooltip, Lines, Scatter, EffectScatter, and Canvas, then register the
base map and create the chart. Importing the extensions alone does not complete the registration;
missing Canvas throws on initialization; without a local fallback this would white-screen the React
chat interface.

The map supports expanding to a large view, wheel/button zoom, drag to pan, reset, and clicking a
place and a leg to see details. Geo uses `preserveAspect: "contain"` to keep the base map's aspect
ratio; in ECharts 6, setting only `aspectScale` and then also specifying all four margins stretches
the map by the container's width and height separately. Both the inline map and the expanded large
view must fit proportionally. `flightMapLabels` measures text in screen pixels and re-avoids after
zoom, pan, and size changes; it hides labels that do not fit at a small range, and progressively
shows airport codes and notes as it zooms in. Hiding a label does not delete the place or leg; the
full information is still in the detail area.

`flightMapData` parses `path`, `closed`, and `routes`, connects them in explicit order, and shows
arrows and leg numbers. The same airport is defined only once, and `path` may revisit it; round-trip /
repeated legs are assigned different curvatures by direction, avoiding full overlap. Different legs
may still cross on the plane, so zero crossings cannot be guaranteed for an arbitrary route map. Only
messages with the old `origin + points` still render as radiating from the origin, and no itinerary
is guessed from the title; a correct leg-by-leg map requires regenerating the message with `path` or
`routes`. The output contract is in
[flight-map skill](../.agents/skills/flight-map/SKILL.md).

Manual acceptance (not executed, does not mean the UI passes):

1. Run `cargo desktop` and open the session that previously caused a white screen; expect the map and
   other messages to be viewable, and to be able to continue typing.
2. Generate a map with multiple adjacent airports, zoom out until the labels are crowded, then zoom
   in level by level; expect labels with no room to hide and reappear when there is room, and notes
   to show at a higher zoom level.
3. Click "Expand large view", use the wheel, the +/- buttons, and drag, then click reset; expect to
   zoom/pan, and after reset all legs are visible. Both Esc and the close button should return to the
   chat.
4. Use `path: ["A", "B", "C"]` and `closed: true` (A/B/C must be defined places); expect A->B->C->A,
   with numbering consistent with the arrow direction.
5. Use `path: ["A", "B", "A", "B"]`; expect three legs to arc separately, and clicking the numbered
   list to highlight the corresponding leg.
6. Click a map marker, a leg, and the list below; expect the detail area to show the corresponding
   code, note, direction, and leg order. Labels hidden at a small range are still viewable from the
   list.
7. Enter invalid JSON or a nonexistent airport reference; expect only the map card to show a parse
   error, and the rest of the chat to remain operable.
8. Change the window size and the light/dark theme; expect the map to re-fit, the text to re-avoid,
   and switching theme to preserve the current zoom and pan position.
9. Switch between a narrow window, a wide window, and the expanded large view, and zoom/reset;
   expect the base map north-up and the outline proportions consistent, possibly with whitespace,
   but not squashed or stretched by the container's width and height.

### 4.4 What to keep (the "chat interaction and UI" the user really likes)

- `AiChatComposer`: auto-growing textarea, Enter to send / Shift+Enter for a newline, send / stop
  buttons.
- `AiMessageTimeline`: interleaves text blocks and tool cards on a timeline by `content_offset`.
- `AiMarkdown`: streaming markdown rendering (`streamdown`, `parseIncompleteMarkdown`).
- `useMessageAutoScroll`: follows the latest (stops following only when scrolled off the bottom).
- The visuals of the tool execution card / activity collapsible.
- Fixed chat width (`max-w-190`, see 4.3.2).
- Consecutive tool calls collected into a collapsible (see 4.3.2).
- `tokens.css`'s warm-gray design language.

### 4.5 Multi-project (multi-workspace) UX (**landed**)

Shirley is a standalone app, not bound to a single directory. The left sidebar **registers multiple
workspaces as "projects"**, each with its own sessions:

**Interaction principle: each project item has a permanently visible "+ New session" button (you can
open a new session directly under that project), and all other operations are tucked into a
right-click menu** (hidden by default, not taking layout space) -- so the left sidebar stays clean
and is not flooded by a row of hover buttons even when operation density is high. The right-click
menu is a self-built lightweight primitive (`src/ui/context-menu.tsx`, `react-dom` portal, **no new
dependency**), closing on Esc / clicking blank / scroll / resize, and auto-recovering into the
viewport when near an edge. Destructive operations in menu items (archive / delete) are **confirmed
a second time inside the menu**.

- **New session** (the permanent "+" button on a project row): opens an empty session **under that
  project** and focuses the input box; if the target is not the current foreground project, switch to
  the foreground first, then open (the button works for any project, not just the current one).
- **Register a project**: right-click the "Projects" group header -> "New project" -> native folder
  picker (`tauri-plugin-dialog`); the same directory is deduped by canonical path, and a
  previously archived one is **restored** rather than added.
- **Rename a project**: right-click a project row -> "Rename" -> inline edit, changing only the
  display name (not the path / session data).
- **Archive a project** (= what the user calls "remove"): right-click a project row -> "Archive" ->
  confirm a second time in the menu. **Only sets `archived = true` and releases the in-memory
  runtime, not deleting on-disk data**; the project moves from the "Projects" group to the "Archived"
  group at the bottom (collapsed by default).
- **Restore a project**: right-click a project row in the archived group -> "Restore", the project
  returns to the "Projects" group and is lazily loaded next time.
- **Expand / collapse a project**: clicking the project name only toggles that group's
  expand/collapse, **not the current session** -- as long as the user has not actively switched /
  operated a session, switching projects should not change the selected session (clicking a project
  name no longer automatically opens that project's "current session"). Switching to another project
  (and its session) is only triggered by **explicit actions**: clicking a session row, or clicking a
  project row's "+ New session". A project's session pin/collapse state is remembered with
  `localStorage`.
- **Session-row operations** (right-click): pin / unpin, rename (inline edit), delete (confirm a
  second time in the menu).
- **Sidebar collapse / expand**: a permanently visible "Collapse" button in the top right
  (`PanelLeftClose`) shrinks the whole bar's width to 0 (`transition-[width]` smooth transition);
  after collapsing, an "Expand" button floats out at the top left of the main area (`PanelLeftOpen`,
  absolutely positioned, not taking layout space). The preference is stored in `localStorage`
  (`shirley.desktop.sidebar-collapsed`). **Auto-collapse when the window cannot fit**:
  `matchMedia('(max-width: 900px)')` collapses on entering a narrow viewport (900 = sidebar 256 +
  content 760 no longer fits), only one-way collapsing, not auto-popping back -- respecting the
  user's subsequent explicit expand.

The frontend view key = `project::session` (a session name is unique only within a project and may
collide across projects); events carry both `project` and `session` fields for routing. The registry
and command contract are in section 6.

---

**5. Technology choices**

| Dimension | Choice | Reason |
| --- | --- | --- |
| Desktop shell | **Tauri 2** | consistent with shiwen; the Rust backend can naturally call `Agent` directly |
| Frontend | **React 18 + TypeScript + Vite + Tailwind 4** | faithfully reproduces shiwen's interaction, form 1 |
| Package management | **npm** (an independent `package.json`, placed in the desktop frontend directory) | shiwen uses npm; does not conflict with the Rust workspace |
| markdown | `streamdown` | same as shiwen, consistent streaming render experience |
| Primitives | `@radix-ui/*` + `lucide-react` + `class-variance-authority` + `tailwind-merge` + `clsx` | shiwen `@shiwen/ui`'s dependency set, minimally introduced as needed |
| Frontend-backend communication | Tauri **command + event** | the frontend `invoke`s to send, the backend `emit`s to push `AgentEvent` |

**Cost (the user must be informed)**: this design will **introduce a Node/npm/Vite build chain into
this pure-Rust repository**. This is the inherent cost of form 1; if unacceptable, fall back to form
2 (native Rust webview + hand-written frontend), but then the interaction cannot be reproduced.

---

**6. SDK integration: swap shiwen's stream for Shirley's `AgentEvent`**

This is the core of "consuming our SDK". The shiwen frontend consumes SSE (`parseAiStreamEvent` ->
`onThinking` / `onDelta` / `onTool` / `onCitations` / `onA2ui` / `onDone`). The Shirley side replaces
it with consuming `AgentEvent`:

| Shirley `AgentEvent` | Frontend behavior |
| --- | --- |
| `ContentDelta(text)` | append to the current assistant message body (`onDelta`) |
| `ReasoningDelta(text)` | append to the thinking area (`onThinking`) |
| `MessageAdded(msg)` | add a message (end the streaming state when the assistant is persisted) |
| `ToolStarted { call_id, name, arguments }` | insert a "running" tool card (with call arguments) |
| `ToolFinished { call_id, name, ok, output, elapsed_ms }` | set the tool card to completed / failed |
| `Usage(usage)` | update the footer usage / cache hit rate |
| `ContextUsage { used, limit }` | update the context usage |
| `CompressionStarted` / `CompressionFinished` | compression status hint |
| `Finished(RunResult)` | end this round (corresponds to `onDone`) |
| `Err(AgentError)` | error state (corresponds to `onError`) |

**Rust-side bridge** (illustrative, already evolved for multi-project multi-session):
```rust
#[tauri::command]
async fn agent_send(state: State<'_, DesktopState>, app: AppHandle, project: String, text: String, session: Option<String>) -> Result<(), String> {
    // get that project's runtime by `project` (lazy load), then take it by **session name**
    // (falling back to that project's foreground session if not passed);
    // during the run, move that session's Agent into a background task, and put it back by name
    // when done. Different sessions each hold an independent Agent, so they can truly run in parallel.
    let runtime = state.ensure_runtime(&project).await?;
    let name = resolve_session(&runtime, session).await?;
    // `begin_turn` = take the Agent + optimistically record the user message (`apply_event`
    // intentionally ignores `MessageAdded(User)`, the user message must be recorded by the driver
    // -- symmetric with the TUI `submit`).
    let mut agent = runtime.sessions.lock().await
        .begin_turn(&name, text.clone()).ok_or("agent is already running")?;
    tauri::async_runtime::spawn(async move {
        let mut stream = agent.run_stream(&text);
        while let Some(event) = stream.next().await {
            let update = match event {
                Ok(event) => Ok(AgentEventWire::from_event(event).with_route(&project, &name)),
                Err(error) => Err(error.to_string()),
            };
            // the source of truth is in SessionManager: feed the event to the **corresponding
            // session** (by name, not relying on the active pointer).
            runtime.sessions.lock().await.apply_event(&name, update);
        }
        runtime.sessions.lock().await.restore_agent(&name, agent);
    });
    Ok(())
}
```
Each `Session` self-holds a `broadcast` channel; `agent_subscribe { project, session }` **first
`subscribe` then `snapshot`** under the same lock (guaranteeing no loss / no duplication), and starts
a per-session pump task that pushes that session's events to the frontend via `agent://event`;
`agent_unsubscribe { project, session }` aborts that pump. The frontend routes events to the
corresponding session view by `AgentEventWire`'s `project` + `session` (key = `project::session`, see
`web/src/lib/bridge.ts`).

**Tauri commands at a glance** (`src/interface/desktop/shell.rs`):

**Multi-project multi-session**: as a standalone app Shirley is not bound to a single directory and
can register **multiple workspaces** as "projects". The project registry `ProjectRegistry`
(`src/project.rs`) only records "where the project is", persisted to
`<config_dir>/shirley/projects.json`; each project has an `AgentFactory` (each reads that project's
`<root>/.shirley/config.toml`), and session logs still land in each project's own
`<config_dir>/shirley/workspaces/<workspace-id>/sessions/` (stored globally). **Project id = canonical path** (globally
unique), a session name is unique only within a project, so **the frontend route key =
`project::session`** (the same as the backend pump key, with events tagged `project` + `session`).
Runtimes are **lazily loaded per project** (`ensure_runtime`):
`runtimes: HashMap<project_id, Arc<Mutex<ProjectRuntime>>>`, each `ProjectRuntime { factory, sessions }`
holding its own `SessionManager`; **archiving a project = releasing its in-memory runtime** (removed
from the `runtimes` table), with on-disk data untouched, lazily loaded again next time it is opened.

| command | Role |
| --- | --- |
| `agent_list_projects { archived? }` | list projects (`archived = true` includes archived projects for the archived list), in descending order of last opened |
| `agent_add_project` | open the native folder picker to create a project (`tauri-plugin-dialog`); an already-registered directory is deduped by path / restored if archived; returns `null` if the user cancels |
| `agent_rename_project { project, name }` | rename a project (only the display name, not the path / session data) |
| `agent_archive_project { project }` | **archive** a project (= what the user calls "remove"): only sets `archived = true` + releases the in-memory runtime, **not deleting on-disk data** |
| `agent_restore_project { project }` | restore a project from archived (lazily loaded next time) |
| `agent_send { project, text, references?, session? }` | start a round of `run_stream` (`session` defaults to that project's foreground session), with events pushed via `agent://event` |
| `agent_model_name { project }` | that project's current model name (footer / selector display) |
| `agent_list_models` | list selectable models (forwards `ModelCatalog`, global) |
| `agent_set_model { project, model }` | hot-swap the model (`Agent::set_model`, no Agent rebuild) |
| `agent_search_files { project, query }` | workspace file search for `@` references (application-layer `workspace_search`, not touching the SDK) |
| `agent_list_sessions { project }` | list all sessions of that project (forwards `SessionCatalog::list`, in descending order of last modified) |
| `agent_current_session { project }` | that project's current session name (`null` if none) |
| `agent_new_session { project, title? }` | create a new session (`title` omitted means anonymous, the name is auto-generated by the AI later) and switch to it (`SessionManager::create_new`) |
| `agent_switch_session { project, name }` | switch to the specified session (`SessionManager::switch_to`, the same orchestrator as the TUI) |
| `agent_rename_session { project, name, title }` | rename a session (only the title sidecar, not the identifier / content) |
| `agent_delete_session { project, name }` | delete a session (together with the log and title) |
| `agent_load_history { project }` | replay that project's current session history (User / Assistant body) for the frontend to rebuild the transcript |
| `agent_subscribe { project, session }` | subscribe to a session: `subscribe` + `snapshot` under the same lock, returning `SessionSnapshotWire { session, items, running }` and starting a pump |
| `agent_unsubscribe { project, session }` | unsubscribe from a session: abort its pump |
| `agent_cancel { project, session }` | reserved (the SDK cancellation mechanism is not landed, see `Agent.md` gap 9) |

> **Model-switch selector**: placed next to the send button (`AiChatComposer`'s `trailingAction`
> slot, `ModelSelector.tsx`). Both the catalog and the switch go through the Rust side
> (`agent_list_models` / `agent_set_model`), and the frontend does not hold model state -- the same
> semantics as the TUI's `/model`, just changing from a "popover panel" to a "send-bar inline
> dropdown".

> **Session management (left sidebar `SessionSidebar.tsx`)**: modeled on Codex's `/resume` picker,
> providing list / switch / new / rename (inline edit) / delete (confirm a second time in the menu)
> under the project grouping; **each project row has a permanent "+ New session" button**, the
> project group header row right-click provides new project, a project row right-click provides
> rename / archive, and an archived row right-click provides restore (see 4.5). **All operations
> except "New session" on a project row are in the right-click menu**. **Clicking "New session" no
> longer requires entering a name first** -- it directly opens an empty session and focuses the input
> box (focusing on user intent rather than management logic); the session name is **auto-generated by
> the AI after the first message is sent** (see below), and the user can rename it if they want to
> change it. The session catalog and all operations go through the Rust side `SessionCatalog` --
> **sharing the same implementation and the same `<config_dir>/shirley/workspaces/<workspace-id>/sessions` data as the TUI** (see
> `docs/session.md` 8 decision 7). Switch / new / delete go through `SessionManager` (each session
> holds its own independent `Agent`, and while running only that session is rejected). **The frontend
> subscribes to events per session**: switching to a session does `agent_subscribe` (takes a
> `SessionSnapshot` to rebuild the transcript) + subscribes to its event stream; switching away does
> `agent_unsubscribe`. Background sessions keep running as usual, and events are routed to their
> respective views by `AgentEventWire.session`, without crosstalk.

> **Automatic session naming**: a new session's name is initially a timestamp placeholder
> (`fresh_name()`, see `docs/session.md`). After the first round of `run_stream`, the `agent_send`
> task calls `auto_title_session(&agent, &catalog, &name)` before `restore_agent`: if the session
> **does not yet have a custom title** (`label == name` in the catalog, i.e. never named / renamed),
> it uses its **first user message** (`session::first_user_message`) through one tool-free,
> non-streaming completion (SDK `Agent::complete`) to generate a short title, and after cleaning
> (`session::sanitize_title`) writes it to the `<name>.title` sidecar. Any step failing is silently
> ignored and retried next round; once the user has manually renamed it, `label != name` and
> automatic naming no longer overrides it. **This is the use of `Agent::complete`** -- a general
> primitive for "using one model call to do a small thing", not touching the `Agent`'s session state
> (callable on `&self`).

> **Note**: `AgentEvent` is currently `Debug + Clone` (`Clone` is needed for the per-session
> `broadcast` channel to reuse events), and **does not derive `Serialize`**. The bridge layer needs a
> `to_wire` to map `AgentEvent` / `Message` into frontend DTOs (not adding `Serialize` to the SDK
> directly, keeping the SDK contract small and protocol differences converging at the boundary --
> consistent with `docs/README.md` principle 1). Landed as `src/interface/desktop/wire.rs`.
>
> To support tool cards, the two existing variants `ToolStarted` / `ToolFinished` **had fields
> appended** (`arguments` / `ok` / `output` / `elapsed_ms`). This is a small change to the SDK's
> external contract: the fields are **appended**, and existing consumers (the TUI ignores them with
> `..`) need zero changes; it still does not derive `Serialize`.

---

**7. Task breakdown (milestones)**

**M0 · skeleton and documentation** (done)
- [x] This document `docs/desktop-interface.md`
- [x] Sync the `docs/README.md` / `Agent.md` index
- [x] Confirm the section-4 strip list (keep the approval code, do not show it in phase one; the
  Node build chain is allowed)

**M1 · minimal migration: render static fake data** (done)
- [x] Extract `Bootstrap` + `Mode` dispatch in `main.rs` (TUI behavior unchanged)
- [x] Create `src/interface/desktop/` (Tauri 2 shell) + `src/interface/desktop/web/` (React+TS+Vite+Tailwind 4)
- [x] copy the components from 4.1 and `tokens.css`, removing the shiwen import (`@shiwen/ui` -> local)
- [x] Delete the custom logic from 4.2 (keep the approval-path code, do not render it in phase one)
- [x] Frontend `npm run dev` renders a fake conversation with a mock bridge (not depending on the Tauri shell)

**M2 · integrate the SDK: send message -> streaming reply loop** (done)
- [x] `to_wire(AgentEvent) -> DTO` (`src/interface/desktop/wire.rs`)
- [x] Tauri command `agent_send` + event `agent://event` (`src/interface/desktop/shell.rs`)
- [x] Frontend `lib/bridge.ts` consumes events (Tauri / mock dual mode), replacing shiwen's SSE consumption
- [x] `ToolStarted` / `ToolFinished` events extended to carry `arguments` / `ok` / `output` / `elapsed_ms`

**M3 · complete sessions and commands**
- [x] Model-switch selector (placed next to the send button, `ModelSelector.tsx` + `agent_list_models` / `agent_set_model`)
- [x] `@` references: workspace file/directory mention popover + chip display (see 4.3; `agent_search_files` + `file-mentions/`)
- [x] Session management: list / switch / new (open an empty session, AI auto-names it) / rename / delete (`SessionSidebar.tsx` + `SessionCatalog`, sharing data with the TUI)
- [x] **Multi-project (multi-workspace)**: register / switch / rename / archive / restore (`ProjectRegistry` + `agent_*_project` commands, see 4.5)
- [ ] desktop presentation of commands like `/login` `/session`
- [ ] Interaction alignment with the TUI (thinking display / tool-argument expansion / scrolling)

**M4 · convergence and documentation**
- [x] Align markdown rendering with shiwen (add `@source` so Tailwind scans streamdown, see 4.3.1)
- [ ] If the command logic is duplicated, sink it into a shared `commands` module
- [ ] Update `Agent.md`, `docs/architecture.md` (add the desktop data-flow diagram)

---

**8. Acceptance criteria**

1. `cargo run` (default) behaves exactly as today (the TUI is unaffected).
2. `cargo desktop` (= `cargo run --features desktop -- --desktop`) opens a Tauri window, and can send
   messages and see streaming replies and tool execution cards.
3. The frontend shows no shiwen-specific concepts (A2UI / entity / citation / space / share); the
   approval-path code is kept but the approval state is not rendered in phase one. `@` references
   are a general interaction after **rebinding to workspace files**, not a shiwen concept.
4. The `Agent` / `AgentEvent` contract is unchanged (the SDK adds no external type because of the
   desktop interface).
5. `cargo test` is all green (including the pre-existing red-test baseline unchanged).

---

**9. Risks and open issues**

- ~~**Node toolchain**: introducing npm changes the nature of the repository and requires the user's
  explicit acceptance (already asked).~~ -> **Confirmed allowed**: the user accepts introducing the
  Node/npm/Vite build chain.
- **`streamdown` size**: the bundle size and offline availability need to be evaluated.
- **CJK fonts**: `tokens.css` depends on system fonts such as Songti / PingFang, which need a
  cross-platform fallback.
- ~~**Session consistency between the window and the TUI**: the two interfaces share the session
  catalog, so it needs to be confirmed whether "only one interface at a time" or coexistence is
  allowed.~~ -> **Decided: coexistence is allowed, no lock.** The two interfaces each scan the same
  `<config_dir>/shirley/workspaces/<workspace-id>/sessions`, and when the same session is written by both sides at the same time,
  overwriting each other is **known and acceptable** behavior (no lock, no coordination).
