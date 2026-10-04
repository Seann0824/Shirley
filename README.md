# Shirley

A coding agent written in Rust, named after Shirley from *Code Geass*.

The repository is organized in three layers:

| Layer | Location | Responsibility |
| --- | --- | --- |
| Application | `src/` | A runnable TUI chat agent: registers tools and drives the interface |
| SDK | `crates/shirley-agent-sdk/` | Business-agnostic Agent primitives (published to crates.io) |
| Macros | `crates/shirley-agent-sdk-macros/` | The `#[tool]` attribute macro |

## Running

```sh
cp .env.example .env   # create it yourself using the keys documented in .env
cargo run
```

Configuration is resolved in increasing priority: built-in defaults → global `<config_dir>/shirley/config.toml` → workspace `<root>/.shirley/config.toml` → environment variables.

The easiest path is to put it in `.env`:

```sh
LOCAL_BASE_URL=https://api.example.com/v1   # required
LOCAL_API_KEY=sk-...                        # optional
LOCAL_MODEL=gpt-4o-mini
```

## Build and test

```sh
cargo build
cargo test
cargo test -p shirley-agent-sdk
cargo clippy --all-targets
```

## SDK

`shirley-agent-sdk` can be used as a standalone library:

```sh
cargo add shirley-agent-sdk
```

See [`crates/shirley-agent-sdk/README.md`](crates/shirley-agent-sdk/README.md) for details.

## Documentation

- `plan.md` — design rationale and decision log
- `docs/` — phased technical proposals
- `Agent.md` — a project guide for the next agent (or human) picking up this repo

## License

MIT OR Apache-2.0
