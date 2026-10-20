//! One user-owned cloud connection for remote chats and personal-memory sync.
//! Credentials stay in the native process; the webview only sees connection metadata.
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub sync_personal_memory: bool,
}

#[derive(Default, Serialize)]
pub struct RemoteInfo {
    pub url: String,
    pub has_token: bool,
    pub sync_personal_memory: bool,
    pub status: SyncStatus,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncStatus {
    pub last_attempt_ms: Option<i64>,
    pub last_success_ms: Option<i64>,
    pub error: Option<String>,
    pub uploaded: usize,
    pub downloaded: usize,
    pub conflicts: usize,
    pub excluded: usize,
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.to_string()),
    }
}

impl RemoteConfig {
    pub fn load(global: &Path) -> Result<Self, String> {
        let config: Self = read_json(&global.join("remote.json"))?;
        if !config.url.is_empty() {
            config.validate()?;
        }
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        let url = url::Url::parse(&self.url).map_err(|_e| "云端地址无效")?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("云端地址不能含账号、密码、查询参数或片段".into());
        }
        let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err("远端连接必须使用 HTTPS；仅本机允许 HTTP".into());
        }
        if !loopback && self.token.trim().is_empty() {
            return Err("远端连接必须填写访问令牌".into());
        }
        Ok(())
    }

    #[cfg(any(feature = "desktop", test))]
    pub fn save(&self, global: &Path) -> Result<(), String> {
        if !self.url.is_empty() {
            self.validate()?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::memory::store::atomic_write(&global.join("remote.json"), &bytes)
            .map_err(|e| e.to_string())
    }

    pub fn client(&self, streaming: bool) -> Result<reqwest::Client, String> {
        self.validate()?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5));
        if !streaming {
            builder = builder.timeout(Duration::from_secs(20));
        }
        builder.build().map_err(|e| e.to_string())
    }

    pub fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.url.trim_end_matches('/'))
    }

    pub fn authorize(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.token.is_empty() {
            request
        } else {
            request.bearer_auth(&self.token)
        }
    }

    pub async fn post(&self, path: &str, body: &impl Serialize) -> Result<Value, String> {
        let request = self.client(false)?.post(self.endpoint(path)).json(body);
        let response = self
            .authorize(request)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| e.without_url().to_string())?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("云端响应超过 16 MiB".into());
        }
        if !status.is_success() {
            return Err(format!(
                "云端请求失败 ({status}): {}",
                String::from_utf8_lossy(&bytes)
            ));
        }
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        }
    }

    #[cfg(any(feature = "desktop", test))]
    pub async fn command(&self, command: &str, args: Value) -> Result<Value, String> {
        // Restrict the proxy to this application's command namespace.
        if !command.starts_with("agent_")
            || !command.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
        {
            return Err("unsupported cloud command".into());
        }
        self.post(&format!("api/cmd/{command}"), &args).await
    }
}

pub fn info(global: &Path) -> Result<RemoteInfo, String> {
    let config = RemoteConfig::load(global)?;
    Ok(RemoteInfo {
        url: config.url,
        has_token: !config.token.is_empty(),
        sync_personal_memory: config.sync_personal_memory,
        status: read_json(&global.join("remote-sync-status.json"))?,
    })
}

pub async fn sync_once(global: &Path) -> Result<SyncStatus, String> {
    let remote = RemoteConfig::load(global)?;
    if remote.url.is_empty() || !remote.sync_personal_memory {
        return Ok(info(global)?.status);
    }
    let previous: SyncStatus = read_json(&global.join("remote-sync-status.json"))?;
    let mut status = SyncStatus {
        last_attempt_ms: Some(chrono::Utc::now().timestamp_millis()),
        ..previous
    };
    match crate::memory::sync::synchronize(global, &remote).await {
        Ok(report) => {
            status.last_success_ms = status.last_attempt_ms;
            status.error = None;
            status.uploaded = report.uploaded;
            status.downloaded = report.downloaded;
            // Cloud is the single source of truth: sync is last-write-wins, so there are no
            // forked conflict copies to count anymore.
            status.conflicts = report.conflicts;
            status.excluded = report.excluded;
        }
        Err(e) => {
            status.error = Some(e);
        }
    }
    crate::memory::store::atomic_write(
        &global.join("remote-sync-status.json"),
        &serde_json::to_vec_pretty(&status).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if let Some(error) = &status.error {
        return Err(error.clone());
    }
    Ok(status)
}

pub fn start_sync_worker(global: std::path::PathBuf) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = sync_once(&global).await {
                eprintln!("[memory:sync] {error}");
            }
        }
    });
}

/// Incremental SSE decoder. Byte buffering preserves UTF-8 split across network chunks.
#[cfg(any(feature = "desktop", test))]
#[derive(Default)]
pub struct EventDecoder {
    buffer: Vec<u8>,
}

#[cfg(any(feature = "desktop", test))]
impl EventDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 4 * 1024 * 1024 {
            return Err("cloud event exceeds 4 MiB".into());
        }
        let mut events = Vec::new();
        loop {
            let boundary = [
                self.buffer
                    .windows(2)
                    .position(|s| s == b"\n\n")
                    .map(|i| (i, 2)),
                self.buffer
                    .windows(4)
                    .position(|s| s == b"\r\n\r\n")
                    .map(|i| (i, 4)),
            ]
            .into_iter()
            .flatten()
            .min_by_key(|(end, _)| *end);
            let Some((end, separator)) = boundary else {
                break;
            };
            let frame = String::from_utf8(self.buffer.drain(..end + separator).collect())
                .map_err(|e| e.to_string())?;
            let kind = frame
                .lines()
                .find_map(|s| s.strip_prefix("event:"))
                .map(str::trim);
            if kind == Some("resync") {
                events.push(serde_json::json!({"type": "transport_resync"}));
                continue;
            }
            if kind != Some("agent_event") {
                continue;
            }
            let data = frame
                .lines()
                .filter_map(|s| s.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            events.push(serde_json::from_str(&data).map_err(|e| e.to_string())?);
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cloud_url_requires_https_and_credentials_without_url_secrets() {
        let config = |url: &str, token: &str| RemoteConfig {
            url: url.into(),
            token: token.into(),
            sync_personal_memory: true,
        };
        assert!(config("http://127.0.0.1:8787", "").validate().is_ok());
        assert!(
            config("https://cloud.example/shirley", "secret")
                .validate()
                .is_ok()
        );
        assert!(config("http://cloud.example", "secret").validate().is_err());
        assert!(config("https://cloud.example", "").validate().is_err());
        assert!(
            config("https://user:secret@cloud.example", "secret")
                .validate()
                .is_err()
        );
        assert!(
            config("https://cloud.example?token=secret", "secret")
                .validate()
                .is_err()
        );
        assert!(config("file:///etc/passwd", "secret").validate().is_err());
    }

    #[test]
    fn event_decoder_preserves_utf8_across_chunks_and_handles_crlf_and_resync() {
        let json = serde_json::json!({"type":"content_delta", "text":"中文", "project":"p", "session":"s", "sequence":4});
        let frame = format!("event: agent_event\r\ndata: {json}\r\n\r\n");
        let bytes = frame.as_bytes();
        let first_chinese = bytes.iter().position(|b| *b >= 128).unwrap();
        let mut decoder = EventDecoder::default();
        assert!(decoder.push(&bytes[..=first_chinese]).unwrap().is_empty());
        assert_eq!(
            decoder.push(&bytes[first_chinese + 1..]).unwrap(),
            vec![json]
        );
        assert_eq!(decoder.push(b"event: agent_event\r\ndata: {\"type\":\"one\"}\r\n\r\nevent: agent_event\ndata: {\"type\":\"two\"}\n\n").unwrap().len(), 2);
        let events = decoder
            .push(b": keepalive\n\nevent: resync\ndata: {}\n\n")
            .unwrap();
        assert_eq!(events, vec![serde_json::json!({"type":"transport_resync"})]);
    }

    #[test]
    fn credentials_are_private_and_not_exposed_in_info() {
        let dir = std::env::temp_dir().join(format!(
            "shirley-remote-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let config = RemoteConfig {
            url: "https://cloud.example".into(),
            token: "private-token".into(),
            sync_personal_memory: false,
        };
        config.save(&dir).unwrap();
        let info = serde_json::to_string(&info(&dir).unwrap()).unwrap();
        assert!(!info.contains("private-token"));
        assert!(info.contains("has_token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.join("remote.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(feature = "server")]
    #[tokio::test]
    async fn command_proxy_keeps_remote_project_paths_and_execution_destination() {
        use axum::{Json, Router, extract::Path as ApiPath, routing::post};
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
        let captured = Arc::clone(&seen);
        let app = Router::new().route("/api/cmd/{command}", post(move |ApiPath(command): ApiPath<String>, Json(args): Json<Value>| {
            let seen = Arc::clone(&captured);
            async move {
                seen.lock().unwrap().push((command.clone(), args.clone()));
                Json(match command.as_str() {
                    "agent_list_projects" => serde_json::json!([{ "id": "/cloud/repo", "path": "/cloud/repo", "name": "Cloud project" }]),
                    "agent_load_history" => serde_json::json!([{ "role": "assistant", "content": "Remote history" }]),
                    "agent_send" => serde_json::json!({ "executed_on": "cloud", "project": args["project"] }),
                    _ => Value::Null,
                })
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let remote = RemoteConfig {
            url,
            token: String::new(),
            sync_personal_memory: false,
        };
        let projects = remote
            .command("agent_list_projects", serde_json::json!({}))
            .await
            .unwrap();
        let project = projects[0]["id"].as_str().unwrap();
        let history = remote
            .command(
                "agent_load_history",
                serde_json::json!({"project": project}),
            )
            .await
            .unwrap();
        assert_eq!(history[0]["content"], "Remote history");
        let sent = remote.command("agent_send", serde_json::json!({"project": project, "session": "remote-chat", "text": "continue"})).await.unwrap();
        assert_eq!(sent["executed_on"], "cloud");
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen[2].1["project"], "/cloud/repo");
            assert_eq!(seen[2].1["session"], "remote-chat");
        }
        assert!(
            remote
                .command("../api/memory/sync", serde_json::json!({}))
                .await
                .is_err()
        );
        task.abort();
    }
}
