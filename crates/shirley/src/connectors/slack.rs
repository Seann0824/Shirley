//! Slack over Socket Mode: @mentions and direct messages as events, a thread reply as the answer.
//!
//! Socket Mode means the server dials Slack (`apps.connections.open` with the app-level token,
//! then a WebSocket), so no public request URL or signing secret is involved. Every envelope is
//! acknowledged at once -- Slack redelivers what is not acknowledged within three seconds -- and
//! the event is handled after. Replies go out with the bot token (`chat.postMessage`), in the
//! thread of the message that asked.

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use super::{Connection, Event, Kind, Sink, client, one_line};

/// `SHIRLEY_SLACK_API` points elsewhere (a test double).
fn api() -> String {
    std::env::var("SHIRLEY_SLACK_API").unwrap_or_else(|_| "https://slack.com/api".to_owned())
}

async fn call(method: &str, token: &str, body: &Value) -> Result<Value, String> {
    let value: Value = client()?
        .post(format!("{}/{method}", api()))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    if value["ok"].as_bool() == Some(true) {
        Ok(value)
    } else {
        Err(format!("Slack {method}: {}", value["error"].as_str().unwrap_or("failed")))
    }
}

/// The bot's identity, and that the app token can open a socket.
pub async fn verify(connection: &Connection) -> Result<String, String> {
    let auth = call("auth.test", connection.setting("bot_token")?, &Value::Null)
        .await
        .map_err(|error| format!("Bot token 无效：{error}"))?;
    call("apps.connections.open", connection.setting("app_token")?, &Value::Null)
        .await
        .map_err(|error| format!("App-level token 无效或没打开 Socket Mode：{error}"))?;
    Ok(format!(
        "{} @ {}",
        auth["user"].as_str().unwrap_or("bot"),
        auth["team"].as_str().unwrap_or("Slack")
    ))
}

pub async fn run(connection: &Connection, sink: &Sink) -> Result<(), String> {
    let opened = call("apps.connections.open", connection.setting("app_token")?, &Value::Null).await?;
    let url = opened["url"].as_str().ok_or("Slack gave no socket URL")?;
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| format!("Slack socket: {error}"))?;
    let bot_user = call("auth.test", connection.setting("bot_token")?, &Value::Null)
        .await
        .ok()
        .and_then(|auth| auth["user_id"].as_str().map(str::to_owned));
    while let Some(frame) = socket.next().await {
        let frame = frame.map_err(|error| format!("Slack socket: {error}"))?;
        let text = match frame {
            Message::Text(text) => text.to_string(),
            Message::Ping(payload) => {
                let _ = socket.send(Message::Pong(payload)).await;
                continue;
            }
            Message::Close(_) => return Err("Slack closed the socket".to_owned()),
            _ => continue,
        };
        let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        match envelope["type"].as_str() {
            Some("hello") => sink.connected(None).await,
            // Slack rotates sockets every few hours and says so first.
            Some("disconnect") => return Err("Slack asked to reconnect".to_owned()),
            _ => {}
        }
        if let Some(id) = envelope["envelope_id"].as_str() {
            let ack = serde_json::json!({ "envelope_id": id }).to_string();
            socket
                .send(Message::Text(ack.into()))
                .await
                .map_err(|error| format!("Slack socket: {error}"))?;
        }
        if envelope["type"].as_str() == Some("events_api")
            && let Some(event) = event_for(&envelope["payload"], bot_user.as_deref())
        {
            sink.event(event).await;
        }
    }
    Err("Slack socket ended".to_owned())
}

/// A mention or a direct message to the bot; nothing the bot itself (or another bot) said.
fn event_for(payload: &Value, bot_user: Option<&str>) -> Option<Event> {
    let event = &payload["event"];
    let kind = event["type"].as_str()?;
    let direct = kind == "message" && event["channel_type"].as_str() == Some("im");
    if kind != "app_mention" && !direct {
        return None;
    }
    if event.get("bot_id").is_some_and(|id| !id.is_null())
        || event.get("subtype").is_some_and(|sub| !sub.is_null())
        || event["user"].as_str() == bot_user
    {
        return None;
    }
    let user = event["user"].as_str().unwrap_or("someone");
    let channel = event["channel"].as_str()?;
    let ts = event["ts"].as_str()?;
    // Answer in the thread the message is in, or start one under it.
    let thread = event["thread_ts"].as_str().unwrap_or(ts);
    // The mention that summoned the bot says nothing about the request.
    let raw = event["text"].as_str().unwrap_or("");
    let text = bot_user.map_or_else(|| raw.to_owned(), |bot| raw.replace(&format!("<@{bot}>"), ""));
    let text = text.trim();
    let place = if direct { "a direct message".to_owned() } else { format!("channel <#{channel}>") };
    Some(Event {
        kind: Kind::Slack,
        title: format!("<@{user}>: {}", one_line(text, 40)),
        details: format!("<@{user}> wrote in {place}:\n\n{text}"),
        reply_to: Some(format!("slack:{channel}:{thread}")),
        key: format!(
            "slack:{}",
            payload["event_id"].as_str().map_or_else(|| format!("{channel}:{ts}"), str::to_owned)
        ),
    })
}

/// Post `text` in the thread `target` (`channel:thread_ts`).
pub async fn reply(connection: &Connection, target: &str, text: &str) -> Result<String, String> {
    let (channel, thread) = target
        .split_once(':')
        .ok_or_else(|| format!("not a Slack reply handle: {target}"))?;
    call(
        "chat.postMessage",
        connection.setting("bot_token")?,
        &serde_json::json!({ "channel": channel, "thread_ts": thread, "text": text }),
    )
    .await?;
    Ok("posted in the Slack thread".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(event: Value) -> Value {
        serde_json::json!({ "event_id": "Ev1", "event": event })
    }

    #[test]
    fn mentions_and_direct_messages_become_events() {
        let mention = payload(serde_json::json!({
            "type": "app_mention", "user": "U1", "text": "<@B1> summarise this", "channel": "C1", "ts": "1.1"
        }));
        let event = event_for(&mention, Some("B1")).unwrap();
        assert_eq!(event.reply_to.as_deref(), Some("slack:C1:1.1"));
        assert!(event.details.contains("summarise this"));
        assert!(!event.title.contains("<@B1>"), "{}", event.title);
        assert_eq!(event.key, "slack:Ev1");

        let threaded = payload(serde_json::json!({
            "type": "message", "channel_type": "im", "user": "U1", "text": "hi", "channel": "D1", "ts": "2.2", "thread_ts": "1.0"
        }));
        assert_eq!(event_for(&threaded, Some("B1")).unwrap().reply_to.as_deref(), Some("slack:D1:1.0"));
    }

    #[test]
    fn the_bot_itself_and_channel_chatter_are_ignored() {
        let own = payload(serde_json::json!({
            "type": "message", "channel_type": "im", "user": "B1", "text": "done", "channel": "D1", "ts": "3"
        }));
        assert!(event_for(&own, Some("B1")).is_none());
        let other_bot = payload(serde_json::json!({
            "type": "app_mention", "bot_id": "X", "user": "U2", "text": "hi", "channel": "C1", "ts": "4"
        }));
        assert!(event_for(&other_bot, Some("B1")).is_none());
        let channel = payload(serde_json::json!({
            "type": "message", "channel_type": "channel", "user": "U1", "text": "hi", "channel": "C1", "ts": "5"
        }));
        assert!(event_for(&channel, Some("B1")).is_none());
    }
}
