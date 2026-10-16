# sandbox

The process sandbox layer. Goal: turn "executing an external command" from "handing the shell to
the model" into "running inside a restricted fake world".

## Layers

| File | Responsibility |
|---|---|
| `spec.rs` | Platform-independent execution intent: what to run, what can be touched, network policy (bon builder) |
| `backend/mod.rs` | Backend abstraction `SandboxBackend` + self-describing capabilities `Capabilities` |
| `backend/process.rs` | Bare-process backend (no isolation), for development/testing and as the degraded baseline only |
| `output.rs` | Unified result: stdout/stderr/exit_code/timed_out/**degraded** |
| `mod.rs` | Facade `Sandbox`: applies the timeout uniformly and assembles the result |

`ProcessBackend` captures at most 32 KiB of raw bytes from **each** of stdout and stderr,
then appends a truncation notice with the omitted byte count. Both pipes are drained
concurrently through EOF even after reaching the cap, so large single-line output cannot
grow the capture buffers without bound or block the child on a full pipe. Invalid or cut
UTF-8 is decoded lossily. Small output and the process exit status are preserved.

## Four design principles

1. **The boundary is "what can be touched", not the command string.** The spec uses `program + args`, never a `bash -c` string that cannot be statically reasoned about.
2. **Deny by default.** Network is off by default, env vars are not inherited by default, and write paths are empty by default.
3. **Timeouts cannot be evaded.** Applied at the outermost layer by `Sandbox::run` with `tokio::time::timeout`; even if the backend hangs on, it will be dropped + killed.
4. **Degradation is transparent.** Constraints the backend cannot enforce must be written into `output.degraded`, so the caller can reject the result instead of being fooled by "it looked like it ran".

## Wiring up real isolation (next step)

`ProcessBackend` only gets the pipeline working. In production, swap per platform:

- **macOS**: add `backend::sandbox_exec`, compiling the spec into a `sandbox-exec` SBPL profile (`(deny default)` + allow read/write/network).
- **Linux**: add `backend::bwrap`, mapping to `bwrap` arguments (`--ro-bind` / `--tmpfs` / `--unshare-net` / `--die-with-parent`).
- **High assurance**: add `backend::microvm` (Firecracker), one isolated kernel per task.

A new backend only needs to implement `SandboxBackend`; the `Sandbox` facade and its callers need no changes.

## Relationship to the permission layer

This layer is only responsible for "execution and isolation". **"Whether it should be executed" is decided by the policy layer** (see section 5 of `docs/security.md`).
Correct order: `policy.check(spec) -> Allow/Deny/Ask` -> if allowed, hand off to `Sandbox::run`.
