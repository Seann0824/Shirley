//! A generic webhook: anything that can POST JSON (CI, monitoring, a script) hands a bot work.
//!
//! The URL carries a random key made when connecting (`/api/hooks/webhook/<key>`); that key is
//! the only credential, so the route sits outside the page's token check. The body becomes the
//! event as it is (JSON pretty-printed, other text verbatim). There is no way to answer.

use super::{Event, Kind, one_line};

const MAX_BODY_CHARS: usize = 8000;

/// A fresh, unguessable key for the webhook URL.
pub fn new_key() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Constant-time comparison, so the key cannot be found by timing.
pub fn key_matches(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// The event for one call.
pub fn event_for(body: &[u8]) -> Event {
    let text = String::from_utf8_lossy(body);
    let pretty = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.into_owned());
    let shown: String = pretty.chars().take(MAX_BODY_CHARS).collect();
    let cut = if pretty.chars().count() > MAX_BODY_CHARS { "\n… (truncated)" } else { "" };
    Event {
        kind: Kind::Webhook,
        title: one_line(&shown, 40),
        details: format!("A webhook call delivered:\n\n{shown}{cut}"),
        reply_to: None,
        key: format!("webhook:{}", uuid::Uuid::new_v4()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_long_and_compared_exactly() {
        let key = new_key();
        assert_eq!(key.len(), 64);
        assert!(key_matches(&key, &key.clone()));
        assert!(!key_matches(&key, &key[..63]));
        assert!(!key_matches(&key, &new_key()));
    }

    #[test]
    fn json_bodies_are_pretty_printed() {
        let event = event_for(br#"{"alert":"disk full","host":"web-1"}"#);
        assert!(event.details.contains("\"alert\": \"disk full\""));
        assert!(event.reply_to.is_none());
        let plain = event_for(b"deploy finished");
        assert!(plain.details.ends_with("deploy finished"));
    }
}
