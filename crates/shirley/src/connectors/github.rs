//! GitHub: your notifications as events, a comment as the answer.
//!
//! Polls the notifications API (participating only: mentions, assignments, review requests,
//! threads you are in), so it needs no public webhook URL, only a personal access token. Each
//! new notification is fetched in full (the latest comment, or the issue / PR itself), handed to
//! the bot, and marked read. Only notifications from after the connection started are handled:
//! connecting must not replay a backlog of old threads.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::{Connection, Event, Kind, Sink, client, one_line};

const API: &str = "https://api.github.com";
const MIN_POLL: Duration = Duration::from_secs(60);
const MAX_BODY_CHARS: usize = 4000;

fn token(connection: &Connection) -> Result<&str, String> {
    connection.setting("token")
}

async fn get(client: &reqwest::Client, token: &str, url: &str) -> Result<reqwest::Response, String> {
    client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|error| error.to_string())
}

/// The login the token belongs to.
pub async fn verify(connection: &Connection) -> Result<String, String> {
    let response = get(&client()?, token(connection)?, &format!("{API}/user")).await?;
    if !response.status().is_success() {
        return Err(format!("GitHub 拒绝了这个令牌（{}）", response.status()));
    }
    let user: Value = response.json().await.map_err(|error| error.to_string())?;
    Ok(format!("@{}", user["login"].as_str().unwrap_or("?")))
}

#[derive(Debug, Deserialize)]
struct Notification {
    id: String,
    reason: String,
    updated_at: String,
    subject: Subject,
    repository: Repository,
}

#[derive(Debug, Deserialize)]
struct Subject {
    title: String,
    #[serde(rename = "type")]
    kind: String,
    url: Option<String>,
    latest_comment_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    full_name: String,
}

pub async fn run(connection: &Connection, sink: &Sink) -> Result<(), String> {
    let client = client()?;
    let token = token(connection)?.to_owned();
    let who = verify(connection).await?;
    sink.connected(Some(who)).await;
    // ISO-8601 timestamps compare correctly as strings.
    let mut since = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    loop {
        let url = format!("{API}/notifications?participating=true&since={since}");
        let response = get(&client, &token, &url).await?;
        let poll = response
            .headers()
            .get("x-poll-interval")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .map_or(MIN_POLL, |seconds| Duration::from_secs(seconds).max(MIN_POLL));
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err("GitHub 令牌失效了，请重新连接".to_owned());
        }
        if !response.status().is_success() {
            return Err(format!("GitHub notifications: {}", response.status()));
        }
        let mut notifications: Vec<Notification> = response.json().await.map_err(|error| error.to_string())?;
        notifications.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        for notification in notifications {
            if notification.updated_at <= since {
                continue;
            }
            since.clone_from(&notification.updated_at);
            if let Some(event) = event_for(&client, &token, &notification).await {
                sink.event(event).await;
            }
            // Read: GitHub stops listing it, and the user sees it was taken care of.
            let _ = client
                .patch(format!("{API}/notifications/threads/{}", notification.id))
                .bearer_auth(&token)
                .send()
                .await;
        }
        tokio::time::sleep(poll).await;
    }
}

/// The full event: the latest comment when there is one, else the issue or PR itself.
async fn event_for(client: &reqwest::Client, token: &str, notification: &Notification) -> Option<Event> {
    let subject = &notification.subject;
    let source = subject.latest_comment_url.as_deref().or(subject.url.as_deref());
    let item: Value = match source {
        Some(url) => get(client, token, url).await.ok()?.json().await.ok()?,
        None => Value::Null,
    };
    let author = item["user"]["login"].as_str().unwrap_or("someone");
    let body = item["body"].as_str().unwrap_or("");
    let link = item["html_url"].as_str().unwrap_or("");
    let repo = &notification.repository.full_name;
    let number = subject.url.as_deref().and_then(issue_number);
    let reply_to = number.map(|number| format!("github:{repo}#{number}"));
    let details = format!(
        "{kind} in {repo}{number}: \"{title}\"\nWhy you got it: {reason}\nLatest from @{author}: {link}\n\n{body}",
        kind = subject.kind,
        number = number.map(|n| format!(" #{n}")).unwrap_or_default(),
        title = subject.title,
        reason = reason_text(&notification.reason),
        body = truncate(body, MAX_BODY_CHARS),
    );
    Some(Event {
        kind: Kind::Github,
        title: format!("{repo}: {}", one_line(&subject.title, 40)),
        details,
        reply_to,
        key: format!("github:{}:{}", notification.id, notification.updated_at),
    })
}

/// `…/issues/12` or `…/pulls/12` -> 12.
fn issue_number(api_url: &str) -> Option<u64> {
    let mut parts = api_url.rsplit('/');
    let number = parts.next()?.parse().ok()?;
    matches!(parts.next(), Some("issues" | "pulls")).then_some(number)
}

fn reason_text(reason: &str) -> &str {
    match reason {
        "mention" => "you were @mentioned",
        "team_mention" => "your team was @mentioned",
        "assign" => "you were assigned",
        "review_requested" => "your review was requested",
        "author" => "you opened it",
        "comment" => "you commented on it",
        "state_change" => "you changed its state",
        other => other,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        format!("{}\n… (truncated)", text.chars().take(max).collect::<String>())
    } else {
        text.to_owned()
    }
}

/// Comment on the issue or PR `target` (`owner/repo#12`).
pub async fn reply(connection: &Connection, target: &str, text: &str) -> Result<String, String> {
    let (repo, number) = target
        .rsplit_once('#')
        .ok_or_else(|| format!("not a GitHub reply handle: {target}"))?;
    let response = client()?
        .post(format!("{API}/repos/{repo}/issues/{number}/comments"))
        .bearer_auth(token(connection)?)
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({ "body": text }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub refused the comment ({})", response.status()));
    }
    let comment: Value = response.json().await.map_err(|error| error.to_string())?;
    Ok(comment["html_url"].as_str().unwrap_or("commented").to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_pr_numbers_are_read_from_api_urls() {
        assert_eq!(issue_number("https://api.github.com/repos/a/b/issues/12"), Some(12));
        assert_eq!(issue_number("https://api.github.com/repos/a/b/pulls/7"), Some(7));
        assert_eq!(issue_number("https://api.github.com/repos/a/b/releases/7"), None);
    }

    #[test]
    fn notifications_parse() {
        let json = r#"[{"id":"1","reason":"mention","updated_at":"2026-11-17T10:00:00Z","unread":true,
            "subject":{"title":"Crash on start","type":"Issue","url":"https://api.github.com/repos/a/b/issues/3",
            "latest_comment_url":"https://api.github.com/repos/a/b/issues/comments/9"},
            "repository":{"full_name":"a/b","private":false}}]"#;
        let parsed: Vec<Notification> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed[0].subject.kind, "Issue");
        assert_eq!(parsed[0].repository.full_name, "a/b");
    }
}
