//! Connectors: outside services that hand bots work (and let bots answer there).
//!
//! A connector is three optional parts on top of the platform that already exists:
//!
//! - **a receiver** that turns what happens there (a Slack mention, a Feishu message, a GitHub
//!   notification, a webhook call) into an [`Event`];
//! - **a handler bot** chosen when connecting: every event becomes a task in that bot's pool
//!   (`prompts/event.md`), run in the background under the bot's own persona and skills. Event
//!   tasks finish without asking the user to review them; only a failure shows up;
//! - **a way back**: an event carries a `reply_to` handle, and the `reply_to_event` tool posts the
//!   bot's answer where the event came from.
//!
//! Connections live in `~/.shirley/connections.json` (0600, secrets never leave the server).
//! The [`Hub`] runs one receiver per connection, reconnecting with backoff, and reports status.

pub mod feishu;
pub mod github;
pub mod slack;
pub mod webhook;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::prompt_template::render;

const FILE: &str = "connections.json";
const EVENT_TEMPLATE: &str = include_str!("../../prompts/event.md");
/// Recent event keys, so a redelivered event (Feishu and Slack retry) runs once.
const SEEN_LIMIT: usize = 2000;
const BACKOFF_MIN: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The services Shirley can connect to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Github,
    Feishu,
    Slack,
    Webhook,
}

impl Kind {
    pub const ALL: [Self; 4] = [Self::Github, Self::Feishu, Self::Slack, Self::Webhook];

    pub fn id(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Feishu => "feishu",
            Self::Slack => "slack",
            Self::Webhook => "webhook",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }

    /// What the connect form asks for.
    pub fn fields(self) -> &'static [Field] {
        match self {
            Self::Github => &[Field {
                key: "token",
                label: "Personal access token",
                help: "GitHub → Settings → Developer settings → Personal access tokens。需要 notifications 和 repo 权限。",
                secret: true,
            }],
            Self::Feishu => &[
                Field {
                    key: "app_id",
                    label: "App ID",
                    help: "飞书开放平台 → 企业自建应用 → 凭证与基础信息。",
                    secret: false,
                },
                Field {
                    key: "app_secret",
                    label: "App Secret",
                    help: "同一页。应用需开启机器人能力，事件订阅选「使用长连接接收事件」并添加 im.message.receive_v1。",
                    secret: true,
                },
            ],
            Self::Slack => &[
                Field {
                    key: "app_token",
                    label: "App-level token (xapp-…)",
                    help: "Slack app → Basic Information → App-Level Tokens，权限 connections:write；并打开 Socket Mode。",
                    secret: true,
                },
                Field {
                    key: "bot_token",
                    label: "Bot token (xoxb-…)",
                    help: "OAuth & Permissions。权限 app_mentions:read、chat:write、im:history；事件订阅 app_mention、message.im。",
                    secret: true,
                },
            ],
            Self::Webhook => &[],
        }
    }

    /// Whether a bot can answer where this connector's events come from.
    pub fn replies(self) -> bool {
        !matches!(self, Self::Webhook)
    }
}

/// One input of a connect form.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// Never sent back to the page once saved.
    pub secret: bool,
}

/// A saved connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub kind: Kind,
    /// The bot that handles this connection's events.
    pub bot: String,
    #[serde(default)]
    pub settings: HashMap<String, String>,
    #[serde(default)]
    pub created_ms: u64,
}

impl Connection {
    pub fn setting(&self, key: &str) -> Result<&str, String> {
        self.settings
            .get(key)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("missing {key}"))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    connections: Vec<Connection>,
}

/// All saved connections.
pub fn load(global: &Path) -> Vec<Connection> {
    std::fs::read(global.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok())
        .map(|store| store.connections)
        .unwrap_or_default()
}

/// The saved connection of `kind`, if any.
pub fn find(global: &Path, kind: Kind) -> Option<Connection> {
    load(global).into_iter().find(|connection| connection.kind == kind)
}

fn save_all(global: &Path, connections: Vec<Connection>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&Store { connections }).map_err(|error| error.to_string())?;
    let path = global.join(FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|error| error.to_string())
}

/// Save `connection`, replacing the one of the same kind.
pub fn upsert(global: &Path, connection: Connection) -> Result<(), String> {
    let mut all: Vec<Connection> = load(global).into_iter().filter(|c| c.kind != connection.kind).collect();
    all.push(connection);
    save_all(global, all)
}

pub fn remove(global: &Path, kind: Kind) -> Result<(), String> {
    save_all(global, load(global).into_iter().filter(|c| c.kind != kind).collect())
}

/// Something that happened in a connected service, for a bot to handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    /// Short label for the task list ("Slack · @alice: can you …").
    pub title: String,
    /// What happened, for the bot: who, where, the message itself, links.
    pub details: String,
    /// Where an answer goes (`reply_to_event`); `None` when there is no way back.
    pub reply_to: Option<String>,
    /// Identifies the event across redeliveries.
    pub key: String,
}

impl Event {
    /// The task brief the handler bot receives.
    pub fn brief(&self) -> String {
        let reply = self.reply_to.as_deref().map_or_else(
            || "There is no way to answer where it came from: just do what it needs.".to_owned(),
            |handle| {
                format!(
                    "To answer where it came from, call `reply_to_event` with reply_to = \"{handle}\". \
                     If it needs no answer, finish without replying."
                )
            },
        );
        render(
            EVENT_TEMPLATE,
            &[("source", label(self.kind)), ("details", self.details.trim()), ("reply", &reply)],
        )
    }
}

/// The service's name as the user knows it.
pub fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Github => "GitHub",
        Kind::Feishu => "飞书",
        Kind::Slack => "Slack",
        Kind::Webhook => "Webhook",
    }
}

/// Where a connection stands, for the settings page.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// `connecting`, `connected`, `error`, or `idle` (nothing running).
    pub state: String,
    pub detail: Option<String>,
    pub events: u64,
    pub last_event_ms: Option<u64>,
}

/// What a receiver reports back through.
#[derive(Clone)]
pub struct Sink {
    kind: Kind,
    hub: Arc<Hub>,
}

impl Sink {
    /// Hand an event to the connection's bot.
    pub async fn event(&self, event: Event) {
        self.hub.handle(event).await;
    }

    pub async fn connected(&self, detail: Option<String>) {
        self.hub.set_state(self.kind, "connected", detail).await;
    }
}

/// Hands events to bots; implemented by the runtime manager (kept as a trait so this module
/// does not depend on the server).
pub trait Dispatch: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        bot: &'a str,
        title: &'a str,
        brief: &'a str,
        source: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>;
}

/// Runs the receivers.
pub struct Hub {
    global: PathBuf,
    dispatch: Arc<dyn Dispatch>,
    running: Mutex<HashMap<Kind, JoinHandle<()>>>,
    status: Mutex<HashMap<Kind, Status>>,
    seen: Mutex<VecDeque<String>>,
}

impl Hub {
    pub fn new(global: PathBuf, dispatch: Arc<dyn Dispatch>) -> Arc<Self> {
        Arc::new(Self {
            global,
            dispatch,
            running: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
            seen: Mutex::new(VecDeque::new()),
        })
    }

    pub fn global(&self) -> &Path {
        &self.global
    }

    /// Start a receiver for every saved connection.
    pub async fn start_all(self: &Arc<Self>) {
        for connection in load(&self.global) {
            self.restart(connection.kind).await;
        }
    }

    /// (Re)start the receiver of `kind` from its saved settings; stop it when there are none.
    pub async fn restart(self: &Arc<Self>, kind: Kind) {
        if let Some(handle) = self.running.lock().await.remove(&kind) {
            handle.abort();
        }
        let Some(connection) = find(&self.global, kind) else {
            self.status.lock().await.remove(&kind);
            return;
        };
        if kind == Kind::Webhook {
            // Nothing to run: calls arrive at the HTTP route.
            self.set_state(kind, "connected", None).await;
            return;
        }
        let hub = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let sink = Sink { kind, hub: Arc::clone(&hub) };
            let mut backoff = BACKOFF_MIN;
            loop {
                hub.set_state(kind, "connecting", None).await;
                let started = std::time::Instant::now();
                let outcome = match kind {
                    Kind::Github => github::run(&connection, &sink).await,
                    Kind::Feishu => feishu::run(&connection, &sink).await,
                    Kind::Slack => slack::run(&connection, &sink).await,
                    Kind::Webhook => Ok(()),
                };
                let reason = outcome.err().unwrap_or_else(|| "connection closed".to_owned());
                eprintln!("[connectors] {}: {reason}", kind.id());
                hub.set_state(kind, "error", Some(reason)).await;
                // A connection that held for a while was healthy: reconnect promptly.
                if started.elapsed() > Duration::from_secs(120) {
                    backoff = BACKOFF_MIN;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        });
        self.running.lock().await.insert(kind, handle);
    }

    pub async fn stop(&self, kind: Kind) {
        if let Some(handle) = self.running.lock().await.remove(&kind) {
            handle.abort();
        }
        self.status.lock().await.remove(&kind);
    }

    pub async fn status(&self, kind: Kind) -> Status {
        self.status.lock().await.get(&kind).cloned().unwrap_or_else(|| Status {
            state: "idle".to_owned(),
            ..Status::default()
        })
    }

    async fn set_state(&self, kind: Kind, state: &str, detail: Option<String>) {
        let mut all = self.status.lock().await;
        let status = all.entry(kind).or_default();
        state.clone_into(&mut status.state);
        status.detail = detail;
    }

    /// Run `event` once: skip a redelivery, then queue it for the connection's bot.
    pub async fn handle(&self, event: Event) {
        {
            let mut seen = self.seen.lock().await;
            if seen.contains(&event.key) {
                return;
            }
            seen.push_back(event.key.clone());
            if seen.len() > SEEN_LIMIT {
                seen.pop_front();
            }
        }
        {
            let mut all = self.status.lock().await;
            let status = all.entry(event.kind).or_default();
            status.events += 1;
            status.last_event_ms = Some(crate::pool::now_ms());
        }
        let Some(connection) = find(&self.global, event.kind) else {
            return;
        };
        let title = format!("{} · {}", label(event.kind), event.title);
        if let Err(error) = self
            .dispatch
            .dispatch(&connection.bot, &title, &event.brief(), event.kind.id())
            .await
        {
            eprintln!("[connectors] {}: could not queue an event for {}: {error}", event.kind.id(), connection.bot);
        }
    }
}

/// Shorten to `max` characters, on one line.
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// How long an identical answer to the same place counts as a repeat.
const REPEAT_WINDOW: Duration = Duration::from_secs(600);

/// Whether `text` already went to `handle` just now. A model stuck in a loop must not flood a
/// channel with the same message.
fn is_repeat(handle: &str, text: &str) -> bool {
    use std::hash::{Hash, Hasher};
    static SENT: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, std::time::Instant>>> =
        std::sync::OnceLock::new();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (handle, text.trim()).hash(&mut hasher);
    let key = hasher.finish();
    let mut sent = SENT
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = std::time::Instant::now();
    sent.retain(|_, at| now.duration_since(*at) < REPEAT_WINDOW);
    sent.insert(key, now).is_some()
}

/// Post a bot's answer where an event came from. `handle` is an event's `reply_to`.
pub async fn reply(global: &Path, handle: &str, text: &str) -> Result<String, String> {
    if is_repeat(handle, text) {
        return Ok("already sent this exact answer there; not sent again".to_owned());
    }
    let (kind, target) = handle
        .split_once(':')
        .ok_or_else(|| format!("not a reply handle: {handle}"))?;
    let kind = Kind::parse(kind).ok_or_else(|| format!("unknown source in reply handle: {handle}"))?;
    let connection = find(global, kind).ok_or_else(|| format!("{} is not connected", label(kind)))?;
    match kind {
        Kind::Github => github::reply(&connection, target, text).await,
        Kind::Feishu => feishu::reply(&connection, target, text).await,
        Kind::Slack => slack::reply(&connection, target, text).await,
        Kind::Webhook => Err("a webhook has no way to answer".to_owned()),
    }
}

/// Check credentials before saving them; returns who they belong to.
pub async fn verify(connection: &Connection) -> Result<String, String> {
    match connection.kind {
        Kind::Github => github::verify(connection).await,
        Kind::Feishu => feishu::verify(connection).await,
        Kind::Slack => slack::verify(connection).await,
        Kind::Webhook => Ok(String::new()),
    }
}

/// An HTTP client for the connectors.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("shirley/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_forms_ask_for_secrets() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.id()), Some(kind));
        }
        assert!(Kind::Slack.fields().iter().all(|field| field.secret));
        assert!(Kind::Webhook.fields().is_empty());
        assert!(!Kind::Webhook.replies());
    }

    #[test]
    fn the_brief_says_how_to_answer() {
        let event = Event {
            kind: Kind::Slack,
            title: "alice: hi".into(),
            details: "alice in #general: can you summarise the thread?".into(),
            reply_to: Some("slack:C1:1700.1".into()),
            key: "k".into(),
        };
        let brief = event.brief();
        assert!(brief.contains("Slack") && brief.contains("summarise the thread"));
        assert!(brief.contains(r#"reply_to = "slack:C1:1700.1""#));
        assert!(!brief.contains("{{"));
        let silent = Event { reply_to: None, ..event }.brief();
        assert!(silent.contains("no way to answer"));
    }

    #[test]
    fn connections_are_saved_privately_and_replaced_by_kind() {
        let dir = std::env::temp_dir().join(format!("shirley_connections_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let connection = |bot: &str| Connection {
            kind: Kind::Github,
            bot: bot.into(),
            settings: HashMap::from([("token".to_owned(), "t".to_owned())]),
            created_ms: 1,
        };
        upsert(&dir, connection("nono")).unwrap();
        upsert(&dir, connection("patch")).unwrap();
        let all = load(&dir);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].bot, "patch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        remove(&dir, Kind::Github).unwrap();
        assert!(load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_answer_to_the_same_place_is_sent_once() {
        assert!(!is_repeat("slack:C1:1", "hello"));
        assert!(is_repeat("slack:C1:1", "hello "));
        assert!(!is_repeat("slack:C1:2", "hello"));
        assert!(!is_repeat("slack:C1:1", "something else"));
    }

    #[test]
    fn long_text_becomes_one_short_line() {
        assert_eq!(one_line("a\n b   c", 10), "a b c");
        assert_eq!(one_line("一二三四五六", 4), "一二三…");
    }
}

#[cfg(test)]
mod network_tests {
    /// Run by hand (`--ignored`): a real `wss://` handshake through the TLS stack the
    /// connectors use.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn secure_websockets_connect() {
        use futures::{SinkExt, StreamExt};
        let (mut socket, _) = tokio_tungstenite::connect_async("wss://echo.websocket.org").await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text("ping".into()))
            .await
            .unwrap();
        assert!(socket.next().await.is_some());
    }
}
