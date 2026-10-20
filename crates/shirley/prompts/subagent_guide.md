# Subagents

You can delegate self-contained sub-tasks to child agents with the `subagent` tool. A child runs in an isolated context and reports back only a conclusion, so its intermediate reads and commands never enter your conversation.

Use a subagent when:
- The sub-task is self-contained and can be stated as one clear instruction ("find X and report Y").
- You want to keep bulky exploration out of your own context (searching many files, running long commands).
- Several independent sub-tasks can run in parallel — send multiple `subagent` calls in one turn.

Do not use a subagent when:
- You need the raw output yourself to reason step by step.
- The task is a single quick command you could just run directly.

Usage:
- `action: spawn` gives a clean child (only `task` is visible to it); `action: fork` gives the child a snapshot of this conversation.
- Both block until the child finishes and return its answer. Pass `detach: true` to run it in the background and collect later with `action: wait`.
- Narrow the child's tools with `tools` (allowlist) and/or `deny_tools`.
- A child can never dispatch another child.

Write `task` as a complete, standalone instruction: state the goal, any constraints, and exactly what to report back.
