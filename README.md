# Shirley

A coding agent written in Rust, named after Shirley from *Code Geass*.


The repository is organized in three layers:

| Layer | Location | Responsibility |
| --- | --- | --- |
| Application | `crates/shirley/src/` | A runnable TUI chat agent: registers tools and drives the interface |
| SDK | `crates/shirley-agent-sdk/` | Business-agnostic Agent primitives (published to crates.io) |
| Macros | `crates/shirley-agent-sdk-macros/` | The `#[tool]` attribute macro |

## Running

Tools execute directly without a permission profile, Jev review, or human approval prompt.
The former `SHIRLEY_CAPABILITY_PROFILE`, `SHIRLEY_APPROVAL_*`, `SHIRLEY_SIDECAR_*`, and
`JEV_API_KEY` settings are no longer used. Tool-local file access checks, command
restrictions, and execution timeouts still apply. The current shell backend does not
isolate filesystem or network access; see [security](docs/security.md).

```sh
cp .env.example .env   # create it yourself using the keys documented in .env
cargo run
```

Configuration comes from three layers, each overriding the previous one field by field:

1. built-in defaults (protocol, model, context window)
2. global `<config_dir>/shirley/config.toml`
3. environment variables (`LOCAL_*`)

A layer only speaks for the keys it actually sets — anything omitted falls through to the layer below. All chats and projects read the same global configuration. `/login` saves there too.

Configure memory embeddings in the **global** config so ordinary Chat and project
sessions inherit the same semantic retrieval settings. On macOS this file is
`~/Library/Application Support/shirley/config.toml`:

```toml
[memory]
embedding_base_url = "http://localhost:11434/v1/embeddings"
embedding_model = "nomic-embed-text"
# embedding_api_key = "..." # only when the service requires authentication
```

The embedding service must be running. Restart Shirley after changing these settings,
since loaded chat/project runtimes retain their embedding client.

All mutable application data lives under `<config_dir>/shirley/` (macOS:
`~/Library/Application Support/shirley/`). `config.toml`, `projects.json` and personal
`memory/` are shared globally. Each canonical workspace path maps to
`workspaces/<workspace-id>/`, containing its `sessions/`, subagent transcripts, project
`memory/` and a `workspace.json` identity record. Ordinary Chat uses the same storage rule.

Existing project `.shirley/` directories are migrated on startup, including archived
projects registered with Shirley. Missing global settings are imported once; existing
global values win. Original project configs are retained as `legacy-config.toml` in the
global workspace state directory. Divergent same-path files cause an error and are
preserved rather than overwritten. Run `cargo run --features desktop -- --migrate-data`
to migrate without opening a chat. See [global storage](docs/storage.md).

The easiest path is to put it in `.env`:

```sh
LOCAL_BASE_URL=https://api.example.com/v1/chat/completions   # required, full endpoint
LOCAL_API_KEY=sk-...                                          # optional
LOCAL_MODEL=gpt-4o-mini
```

## Protocols

Three wire protocols are supported, selected with `LOCAL_PROTOCOL` (or `protocol` in a config file):

| `LOCAL_PROTOCOL` | Wire format | `base_url` points at |
| --- | --- | --- |
| `chat_completions` *(default)* | OpenAI Chat Completions | `…/v1/chat/completions` |
| `responses` | OpenAI Responses | `…/responses` |
| `anthropic_messages` | Anthropic Messages | `…/v1/messages` |

`base_url` is the **full request endpoint**, not a base path — the adapter POSTs to it verbatim. Aliases are accepted: `openai` for `chat_completions`, `anthropic` / `messages` for `anthropic_messages`.

### Chat Completions (default)

```sh
LOCAL_PROTOCOL=chat_completions
LOCAL_BASE_URL=https://api.deepseek.com/v1/chat/completions
LOCAL_MODEL=deepseek-chat
LOCAL_API_KEY=sk-...
```

### Responses

```sh
LOCAL_PROTOCOL=responses
LOCAL_BASE_URL=https://api.deepseek.com/responses
LOCAL_MODEL=deepseek-chat
LOCAL_API_KEY=sk-...
```

### Anthropic Messages

Anthropic uses an `x-api-key` header (not `Authorization: Bearer`); the adapter sets it for you, along with `anthropic-version`.

```sh
LOCAL_PROTOCOL=anthropic_messages
LOCAL_BASE_URL=https://api.deepseek.com/anthropic/v1/messages
LOCAL_MODEL=deepseek-chat
LOCAL_API_KEY=sk-...
```

The same three, written in the global config file (`<config_dir>/shirley/config.toml`):

```toml
# Chat Completions
[provider]
protocol = "chat_completions"
base_url = "https://api.deepseek.com/v1/chat/completions"
model = "deepseek-chat"
api_key = "sk-..."
```

```toml
# Responses
[provider]
protocol = "responses"
base_url = "https://api.deepseek.com/responses"
model = "deepseek-chat"
```

```toml
# Anthropic Messages
[provider]
protocol = "anthropic_messages"
base_url = "https://api.deepseek.com/anthropic/v1/messages"
model = "deepseek-chat"
api_key = "sk-..."
```

Protocol differences are absorbed entirely by the adapter layer — the `Message` model and the rest of the SDK are protocol-agnostic. See [`docs/adapter-layer.md`](docs/adapter-layer.md) and the per-protocol notes in [`docs/`](docs/).

To change the protocol, edit `LOCAL_PROTOCOL` or the global config file and restart. The TUI's `/login` can update `base_url` / `api_key` / `model` at runtime (writing the global config), and `/model` swaps the model name — but the protocol itself is set at startup.

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
