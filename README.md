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

The same three, written as a config file (`<root>/.shirley/config.toml` or `<config_dir>/shirley/config.toml`):

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

To change the protocol, edit `LOCAL_PROTOCOL` or the config file and restart. The TUI's `/login` can update `base_url` / `api_key` / `model` at runtime (writing the workspace config), and `/model` swaps the model name — but the protocol itself is set at startup.

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
