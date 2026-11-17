//! Feishu (Lark) over its long connection: messages to the bot as events, a reply as the answer.
//!
//! The long connection is how Feishu delivers events without a public callback URL: the server
//! asks for an endpoint with the app's credentials, opens a WebSocket, and receives binary
//! frames in Feishu's small protobuf envelope (`pbbp2.Frame`, encoded by hand below; it is two
//! messages and nine fields). Control frames are ping / pong; data frames carry the event JSON,
//! sometimes split in parts, and each must be answered with the same frame carrying
//! `{"code":200}` or Feishu delivers it again.
//!
//! Events handled: `im.message.receive_v1` -- direct messages to the bot, and group messages that
//! @ it (Feishu only sends those with the `im:message.group_at_msg` permission). Replies use
//! the message reply API with a tenant access token.

use std::collections::HashMap;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use super::{Connection, Event, Kind, Sink, client, one_line};

/// Feishu's open platform; `SHIRLEY_FEISHU_BASE=https://open.larksuite.com` for Lark.
fn open_base() -> String {
    std::env::var("SHIRLEY_FEISHU_BASE").unwrap_or_else(|_| "https://open.feishu.cn".to_owned())
}
const METHOD_CONTROL: i32 = 0;
const METHOD_DATA: i32 = 1;
const DEFAULT_PING: Duration = Duration::from_secs(120);

async fn tenant_token(connection: &Connection) -> Result<String, String> {
    let value: Value = client()?
        .post(format!("{}/open-apis/auth/v3/tenant_access_token/internal", open_base()))
        .json(&serde_json::json!({
            "app_id": connection.setting("app_id")?,
            "app_secret": connection.setting("app_secret")?,
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    match value["tenant_access_token"].as_str() {
        Some(token) if value["code"].as_i64() == Some(0) => Ok(token.to_owned()),
        _ => Err(format!("飞书凭证无效：{}", value["msg"].as_str().unwrap_or("unknown error"))),
    }
}

/// Where to open the long connection, and how often to ping.
async fn endpoint(connection: &Connection) -> Result<(String, Duration), String> {
    let value: Value = client()?
        .post(format!("{}/callback/ws/endpoint", open_base()))
        .header("locale", "zh")
        .json(&serde_json::json!({
            "AppID": connection.setting("app_id")?,
            "AppSecret": connection.setting("app_secret")?,
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    if value["code"].as_i64() != Some(0) {
        return Err(format!(
            "飞书长连接不可用（检查事件订阅是否选了「使用长连接接收事件」）：{}",
            value["msg"].as_str().unwrap_or("unknown error")
        ));
    }
    let url = value["data"]["URL"].as_str().ok_or("飞书没有返回长连接地址")?.to_owned();
    let ping = value["data"]["ClientConfig"]["PingInterval"]
        .as_u64()
        .map_or(DEFAULT_PING, Duration::from_secs);
    Ok((url, ping))
}

/// The app's name, and that the long connection is open to it.
pub async fn verify(connection: &Connection) -> Result<String, String> {
    tenant_token(connection).await?;
    endpoint(connection).await?;
    Ok(connection.setting("app_id")?.to_owned())
}

pub async fn run(connection: &Connection, sink: &Sink) -> Result<(), String> {
    let (url, mut ping_every) = endpoint(connection).await?;
    let service: i32 = url::Url::parse(&url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "service_id")
                .and_then(|(_, value)| value.parse().ok())
        })
        .unwrap_or(0);
    let (mut socket, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .map_err(|error| format!("飞书长连接：{error}"))?;
    sink.connected(None).await;

    // Parts of split payloads, by message id.
    let mut parts: HashMap<String, Vec<Option<Vec<u8>>>> = HashMap::new();
    let mut ping = tokio::time::interval(ping_every);
    loop {
        tokio::select! {
            _ = ping.tick() => {
                let frame = Frame {
                    service,
                    method: METHOD_CONTROL,
                    headers: vec![("type".into(), "ping".into())],
                    ..Frame::default()
                };
                socket.send(Message::Binary(frame.encode().into())).await.map_err(|error| format!("飞书长连接：{error}"))?;
            }
            message = socket.next() => {
                let Some(message) = message else { return Err("飞书长连接断开了".to_owned()) };
                let bytes = match message.map_err(|error| format!("飞书长连接：{error}"))? {
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Ping(payload) => {
                        let _ = socket.send(Message::Pong(payload)).await;
                        continue;
                    }
                    Message::Close(_) => return Err("飞书关闭了长连接".to_owned()),
                    _ => continue,
                };
                let Some(mut frame) = Frame::decode(&bytes) else { continue };
                let kind = frame.header("type").unwrap_or_default().to_owned();
                if frame.method == METHOD_CONTROL {
                    // A pong may carry new settings.
                    if kind == "pong"
                        && let Some(seconds) = frame
                            .payload
                            .as_deref()
                            .and_then(|payload| serde_json::from_slice::<Value>(payload).ok())
                            .and_then(|config| config["PingInterval"].as_u64())
                    {
                        let next = Duration::from_secs(seconds.max(10));
                        if next != ping_every {
                            ping_every = next;
                            ping = tokio::time::interval(ping_every);
                            ping.tick().await;
                        }
                    }
                    continue;
                }
                if frame.method != METHOD_DATA {
                    continue;
                }
                let payload = assemble(&mut parts, &frame);
                // Acknowledge every data frame, part or whole, or it comes back.
                frame.headers.push(("biz_rt".into(), "0".into()));
                frame.payload = Some(br#"{"code":200}"#.to_vec());
                socket.send(Message::Binary(frame.encode().into())).await.map_err(|error| format!("飞书长连接：{error}"))?;
                if kind != "event" {
                    continue;
                }
                if let Some(event) = payload
                    .and_then(|payload| serde_json::from_slice::<Value>(&payload).ok())
                    .and_then(|value| event_for(&value))
                {
                    sink.event(event).await;
                }
            }
        }
    }
}

/// The whole payload once every part of it arrived.
fn assemble(parts: &mut HashMap<String, Vec<Option<Vec<u8>>>>, frame: &Frame) -> Option<Vec<u8>> {
    let payload = frame.payload.clone()?;
    let sum: usize = frame.header("sum").and_then(|sum| sum.parse().ok()).unwrap_or(1);
    if sum <= 1 {
        return Some(payload);
    }
    let seq: usize = frame.header("seq").and_then(|seq| seq.parse().ok())?;
    let id = frame.header("message_id")?.to_owned();
    let slots = parts.entry(id.clone()).or_insert_with(|| vec![None; sum]);
    if seq >= slots.len() {
        return None;
    }
    slots[seq] = Some(payload);
    if slots.iter().all(Option::is_some) {
        let whole = parts.remove(&id)?.into_iter().flatten().flatten().collect();
        Some(whole)
    } else {
        None
    }
}

/// A user's message to the bot.
fn event_for(value: &Value) -> Option<Event> {
    if value["header"]["event_type"].as_str() != Some("im.message.receive_v1") {
        return None;
    }
    let event = &value["event"];
    if event["sender"]["sender_type"].as_str() != Some("user") {
        return None;
    }
    let message = &event["message"];
    let message_id = message["message_id"].as_str()?;
    let text = message_text(message);
    let place = if message["chat_type"].as_str() == Some("p2p") { "a direct message" } else { "a group chat" };
    let sender = event["sender"]["sender_id"]["open_id"].as_str().unwrap_or("someone");
    Some(Event {
        kind: Kind::Feishu,
        title: one_line(&text, 40),
        details: format!("A Feishu user ({sender}) wrote in {place}:\n\n{text}"),
        reply_to: Some(format!("feishu:{message_id}")),
        key: format!(
            "feishu:{}",
            value["header"]["event_id"].as_str().unwrap_or(message_id)
        ),
    })
}

/// The readable text of a message: mentions by name, rich text flattened, other kinds named.
fn message_text(message: &Value) -> String {
    let content: Value = message["content"]
        .as_str()
        .and_then(|content| serde_json::from_str(content).ok())
        .unwrap_or(Value::Null);
    let mut text = match message["message_type"].as_str() {
        Some("text") => content["text"].as_str().unwrap_or_default().to_owned(),
        Some("post") => {
            let body = content.get("content").or_else(|| content.get("zh_cn").and_then(|v| v.get("content")));
            let title = content["title"].as_str().unwrap_or_default();
            let lines: Vec<String> = body
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .map(|row| {
                            row.as_array()
                                .map(|items| {
                                    items.iter().filter_map(|item| item["text"].as_str()).collect::<String>()
                                })
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{title}\n{}", lines.join("\n")).trim().to_owned()
        }
        Some(other) => format!("[{other}]"),
        None => String::new(),
    };
    for mention in message["mentions"].as_array().into_iter().flatten() {
        if let (Some(key), Some(name)) = (mention["key"].as_str(), mention["name"].as_str()) {
            text = text.replace(key, &format!("@{name}"));
        }
    }
    text.trim().to_owned()
}

/// Reply to the message `target` (its message id).
pub async fn reply(connection: &Connection, target: &str, text: &str) -> Result<String, String> {
    let token = tenant_token(connection).await?;
    let value: Value = client()?
        .post(format!("{}/open-apis/im/v1/messages/{target}/reply", open_base()))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string(),
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    if value["code"].as_i64() == Some(0) {
        Ok("replied in Feishu".to_owned())
    } else {
        Err(format!("Feishu refused the reply: {}", value["msg"].as_str().unwrap_or("failed")))
    }
}

/// `pbbp2.Frame`: the envelope of every long-connection message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Frame {
    seq_id: u64,
    log_id: u64,
    service: i32,
    method: i32,
    headers: Vec<(String, String)>,
    payload_encoding: Option<String>,
    payload_type: Option<String>,
    payload: Option<Vec<u8>>,
    log_id_new: Option<String>,
}

impl Frame {
    fn header(&self, key: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint_field(&mut out, 1, self.seq_id);
        put_varint_field(&mut out, 2, self.log_id);
        // int32 fields: negative values are sign-extended to 64 bits on the wire.
        put_varint_field(&mut out, 3, i64::from(self.service) as u64);
        put_varint_field(&mut out, 4, i64::from(self.method) as u64);
        for (key, value) in &self.headers {
            let mut header = Vec::new();
            put_bytes_field(&mut header, 1, key.as_bytes());
            put_bytes_field(&mut header, 2, value.as_bytes());
            put_bytes_field(&mut out, 5, &header);
        }
        if let Some(value) = &self.payload_encoding {
            put_bytes_field(&mut out, 6, value.as_bytes());
        }
        if let Some(value) = &self.payload_type {
            put_bytes_field(&mut out, 7, value.as_bytes());
        }
        if let Some(value) = &self.payload {
            put_bytes_field(&mut out, 8, value);
        }
        if let Some(value) = &self.log_id_new {
            put_bytes_field(&mut out, 9, value.as_bytes());
        }
        out
    }

    fn decode(mut bytes: &[u8]) -> Option<Self> {
        let mut frame = Self::default();
        while !bytes.is_empty() {
            let (field, value) = read_field(&mut bytes)?;
            match (field, value) {
                (1, Wire::Varint(v)) => frame.seq_id = v,
                (2, Wire::Varint(v)) => frame.log_id = v,
                (3, Wire::Varint(v)) => frame.service = v as i32,
                (4, Wire::Varint(v)) => frame.method = v as i32,
                (5, Wire::Bytes(mut header)) => {
                    let (mut key, mut value) = (String::new(), String::new());
                    while !header.is_empty() {
                        match read_field(&mut header)? {
                            (1, Wire::Bytes(b)) => key = String::from_utf8_lossy(b).into_owned(),
                            (2, Wire::Bytes(b)) => value = String::from_utf8_lossy(b).into_owned(),
                            _ => {}
                        }
                    }
                    frame.headers.push((key, value));
                }
                (6, Wire::Bytes(b)) => frame.payload_encoding = Some(String::from_utf8_lossy(b).into_owned()),
                (7, Wire::Bytes(b)) => frame.payload_type = Some(String::from_utf8_lossy(b).into_owned()),
                (8, Wire::Bytes(b)) => frame.payload = Some(b.to_vec()),
                (9, Wire::Bytes(b)) => frame.log_id_new = Some(String::from_utf8_lossy(b).into_owned()),
                _ => {}
            }
        }
        Some(frame)
    }
}

enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    put_varint(out, field << 3);
    put_varint(out, value);
}

fn put_bytes_field(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn read_varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_field<'a>(bytes: &mut &'a [u8]) -> Option<(u64, Wire<'a>)> {
    let tag = read_varint(bytes)?;
    let value = match tag & 7 {
        0 => Wire::Varint(read_varint(bytes)?),
        2 => {
            let len = usize::try_from(read_varint(bytes)?).ok()?;
            if bytes.len() < len {
                return None;
            }
            let (value, rest) = bytes.split_at(len);
            *bytes = rest;
            Wire::Bytes(value)
        }
        1 => {
            *bytes = bytes.get(8..)?;
            Wire::Other
        }
        5 => {
            *bytes = bytes.get(4..)?;
            Wire::Other
        }
        _ => return None,
    };
    Some((tag >> 3, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let frame = Frame {
            seq_id: 300,
            log_id: 1,
            service: 7,
            method: METHOD_DATA,
            headers: vec![("type".into(), "event".into()), ("sum".into(), "1".into())],
            payload: Some(br#"{"a":1}"#.to_vec()),
            log_id_new: Some("x".into()),
            ..Frame::default()
        };
        assert_eq!(Frame::decode(&frame.encode()), Some(frame));
    }

    #[test]
    fn the_ping_matches_the_protobuf_wire_format() {
        let ping = Frame {
            service: 1,
            method: METHOD_CONTROL,
            headers: vec![("type".into(), "ping".into())],
            ..Frame::default()
        };
        // SeqID=0, LogID=0, service=1, method=0, headers[{type: ping}]
        assert_eq!(
            ping.encode(),
            [8, 0, 16, 0, 24, 1, 32, 0, 42, 12, 10, 4, b't', b'y', b'p', b'e', 18, 4, b'p', b'i', b'n', b'g']
        );
    }

    #[test]
    fn a_frame_from_another_protobuf_implementation_decodes() {
        // Encoded by protobufjs from the pbbp2 schema.
        let bytes = [
            8, 172, 2, 16, 77, 24, 12, 32, 1, 42, 13, 10, 4, 116, 121, 112, 101, 18, 5, 101, 118, 101, 110, 116, 42, 16,
            10, 10, 109, 101, 115, 115, 97, 103, 101, 95, 105, 100, 18, 2, 109, 49, 42, 8, 10, 3, 115, 117, 109, 18, 1,
            49, 66, 14, 123, 34, 97, 34, 58, 34, 233, 163, 158, 228, 185, 166, 34, 125, 74, 2, 120, 49,
        ];
        let frame = Frame::decode(&bytes).unwrap();
        assert_eq!((frame.seq_id, frame.log_id, frame.service, frame.method), (300, 77, 12, METHOD_DATA));
        assert_eq!(frame.header("type"), Some("event"));
        assert_eq!(frame.header("message_id"), Some("m1"));
        assert_eq!(frame.payload.as_deref(), Some(r#"{"a":"飞书"}"#.as_bytes()));
        assert_eq!(frame.log_id_new.as_deref(), Some("x1"));
        // And encodes back to the same bytes.
        assert_eq!(frame.encode(), bytes);
    }

    #[test]
    fn split_payloads_are_joined() {
        let part = |seq: usize, bytes: &[u8]| Frame {
            method: METHOD_DATA,
            headers: vec![
                ("message_id".into(), "m1".into()),
                ("sum".into(), "2".into()),
                ("seq".into(), seq.to_string()),
            ],
            payload: Some(bytes.to_vec()),
            ..Frame::default()
        };
        let mut parts = HashMap::new();
        assert_eq!(assemble(&mut parts, &part(1, b"world")), None);
        assert_eq!(assemble(&mut parts, &part(0, b"hello ")).as_deref(), Some(&b"hello world"[..]));
        assert!(parts.is_empty());
    }

    #[test]
    fn user_messages_become_events_with_mentions_named() {
        let value = serde_json::json!({
            "schema": "2.0",
            "header": { "event_id": "ev1", "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_type": "user", "sender_id": { "open_id": "ou_1" } },
                "message": {
                    "message_id": "om_1", "chat_type": "group", "message_type": "text",
                    "content": "{\"text\":\"@_user_1 帮我总结一下\"}",
                    "mentions": [{ "key": "@_user_1", "name": "Shirley" }]
                }
            }
        });
        let event = event_for(&value).unwrap();
        assert_eq!(event.reply_to.as_deref(), Some("feishu:om_1"));
        assert!(event.details.contains("@Shirley 帮我总结一下"));
        assert_eq!(event.key, "feishu:ev1");

        let mut from_app = value.clone();
        from_app["event"]["sender"]["sender_type"] = "app".into();
        assert!(event_for(&from_app).is_none());
    }
}
