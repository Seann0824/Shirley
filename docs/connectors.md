# Connectors

Besides the chat, bots take work from connected services. A connector receives what happens
there, hands it to the bot chosen when connecting, and lets that bot answer where it came from.
Settings: the plug icon on the home screen (「连接」).

| Connector | How events arrive | What becomes an event | Answer |
| --- | --- | --- | --- |
| GitHub | polls your notifications (no webhook to set up) | mentions, assignments, review requests, threads you are in | a comment on the issue / PR |
| 飞书 | long connection (no public callback URL) | direct messages to the bot, group messages that @ it | a reply to the message |
| Slack | Socket Mode (no public request URL) | @mentions, direct messages | a reply in the thread |
| Webhook | `POST /api/hooks/webhook/<key>` | any body (JSON pretty-printed) | none |

## How an event is handled

1. The receiver turns it into an event: who, where, the text, a `reply_to` handle
   (`slack:C123:1700.5`, `feishu:om_…`, `github:owner/repo#12`).
2. It becomes a task in the handler bot's pool (`prompts/event.md`), source-tagged. Redeliveries
   (same event id) run once.
3. The bot works it in the background with its own persona, skills and tools, and answers with
   `reply_to_event` if an answer is wanted. The same answer to the same place is sent once per
   ten minutes, so a looping model cannot flood a channel.
4. The task finishes as done without review: nobody asked for it in the chat. A failure shows on
   the bot like any other task.

Text from outside is wrapped in `<event>` and the brief tells the bot to weigh it against its role,
not obey it, and not to reveal secrets or private files to the sender.

## Setting each up

- **GitHub**: a personal access token with `notifications` and `repo`. Only notifications after
  connecting are handled; each is marked read once taken.
- **飞书**: an internal app (企业自建应用) with the bot capability; App ID and App Secret. Event
  subscription: 「使用长连接接收事件」, event `im.message.receive_v1`; permissions to read direct
  messages and group @messages, and to send messages. Lark: run with
  `SHIRLEY_FEISHU_BASE=https://open.larksuite.com`.
- **Slack**: an app with Socket Mode on; an app-level token (`xapp-…`, `connections:write`) and the
  bot token (`xoxb-…`, `app_mentions:read`, `chat:write`, `im:history`); events `app_mention` and
  `message.im`.
- **Webhook**: connecting makes the URL; the key in it is the only credential, so keep it private.

Credentials are checked before saving, stored in `~/.shirley/connections.json` (0600) and never
sent back to the page. Receivers reconnect on their own with backoff (5 s up to 5 min); the
page shows each one's state and how many events it handled.

## Code

`crates/shirley/src/connectors/`: `mod.rs` (store, `Event`, `Hub`), one file per service,
`tools/reply_event.rs` (`reply_to_event`, registered when a connection that can answer exists),
`interface/runtime/events.rs` (events into the pool).

Tested here against local stand-ins for Slack and Feishu (Socket Mode envelopes and acks; Feishu
frames encoded by protobufjs, a payload split in two parts, acks, replies); not yet against the
real services.
