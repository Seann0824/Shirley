# Shirley

A bot platform written in Rust, named after Shirley from *Code Geass*. One server runs
your bots; a phone-first web UI talks to them.

A **bot** is a persona (system prompt) plus the tools and skills it may use. Every bot has
one ongoing conversation, its own private memory, and shares a common memory with the
other bots. Work you hand over from the home screen goes into a task queue: the bot runs
it in the background and you come back to accept or return the result.

| Layer | Location | Responsibility |
| --- | --- | --- |
| Application | `crates/shirley/src/` | The server: bots, sessions, memory, task pool, HTTP/SSE API |
| Web UI | `crates/shirley/src/interface/web/` | Vite + React + TypeScript client |
| SDK | `crates/shirley-agent-sdk/` | Business-agnostic Agent primitives (published to crates.io) |
| Macros | `crates/shirley-agent-sdk-macros/` | The `#[tool]` attribute macro |

## Running

```sh
cp .env.example .env   # set LOCAL_BASE_URL / LOCAL_API_KEY / LOCAL_MODEL
cargo run              # http://127.0.0.1:8787
```

`cargo run` also builds the web UI (it needs Node.js; the first run does `npm install`).
For UI development run `npm run dev` in `crates/shirley/src/interface/web` instead
(http://localhost:5173, `/api` is proxied to the server).

`SHIRLEY_SERVER_ADDR` changes the listen address; listening outside loopback requires
`SHIRLEY_SERVER_TOKEN` (open the UI with `?token=...`).

Tools execute directly, without a permission prompt. The shell backend does not isolate
filesystem or network access; see [security](docs/security.md).

## Bots and data

Everything lives under `~/.shirley/`:

```text
config.toml                 model + runtime settings
memory/                     shared memory (every bot reads it)
tasks.json                  the task queue
bots/<name>/spec.toml       a bot's definition (optional for the built-in bots)
bots/<name>/sessions/       its conversation (main.jsonl) and task runs
bots/<name>/memory/         memory only this bot sees
```

```toml
# ~/.shirley/bots/clerk/spec.toml
name = "clerk"
display_name = "Clerk"
tools = ["read", "grep"]            # of: bash grep read media web_search; empty = all
skills = []                         # installed SKILL.md names; empty = all
system_prompt_extra = "You are Clerk. You tidy documents and write summaries."
```

`nono` (generalist), `patch` (code) and `scout` (research) are built in. New bots can
also be created from the UI's "+" button, which asks NONO to write the spec.

## Model configuration

Configuration comes from three layers, each overriding the previous one field by field:
built-in defaults, `~/.shirley/config.toml`, then environment variables (`LOCAL_*`).

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

To change the protocol, edit `LOCAL_PROTOCOL` or the global config file and restart.

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
