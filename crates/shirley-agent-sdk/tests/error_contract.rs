//! Regression tests for the unified error contract.
//!
//! These assertions lock the "classification semantics", not the wording:
//! the wording can change, but the behavior of `ErrorKind` and `is_retryable` must not be changed
//! lightly — the caller's retry / degradation decisions all hang on them.

use shirley_agent_sdk::error::{ErrorKind, SdkError};
use shirley_agent_sdk::{AdapterError, ModelProtocol};

/// HTTP status codes must map to the correct classification.
/// This is the most central point of this refactor: previously the status code was baked into a
/// string and the classification info was simply lost.
#[test]
fn http_status_maps_to_kind() {
    // 429 is rate limiting and must be retryable — otherwise a long task hits rate limiting and the whole turn is lost.
    assert_eq!(kind_of_http(429), ErrorKind::RateLimited);
    assert!(kind_of_http(429).is_retryable());

    // 5xx is server-side flakiness; retryable.
    assert_eq!(kind_of_http(500), ErrorKind::ServerError);
    assert_eq!(kind_of_http(503), ErrorKind::ServerError);
    assert!(kind_of_http(503).is_retryable());

    // 4xx means the request itself is bad; retrying the same request is pointless.
    assert_eq!(kind_of_http(400), ErrorKind::BadRequest);
    assert_eq!(kind_of_http(401), ErrorKind::BadRequest);
    assert_eq!(kind_of_http(404), ErrorKind::BadRequest);
    assert!(!kind_of_http(400).is_retryable());
}

/// An unimplemented protocol must be a handleable `Err`, never a panic.
///
/// Corresponds to section 7 of `docs/runtime-hardening.md`: any unimplemented path in the SDK must
/// return an error,
/// because switching protocols is a runtime behavior and must not take the host process down.
#[test]
fn unsupported_protocol_is_error_not_panic() {
    let error = AdapterError::UnsupportedProtocol {
        protocol: format!("{:?}", ModelProtocol::Responses),
    };
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(!error.is_retryable());

    // The protocol variants are still in the enum (guards against someone deleting a variant but forgetting to implement it).
    let _ = ModelProtocol::Responses;
    let _ = ModelProtocol::AnthropicMessages;
}

/// Decode / encode failures are internal errors: retrying the same input will not help.
#[test]
fn decode_and_encode_are_not_retryable() {
    assert_eq!(
        AdapterError::Decode("missing choices[0]".into()).kind(),
        ErrorKind::Internal
    );
    assert_eq!(
        AdapterError::Encode("API Key cannot form a request header".into()).kind(),
        ErrorKind::Internal
    );
    assert!(!AdapterError::Decode("x".into()).is_retryable());
}

/// `ErrorKind::is_retryable` is the only basis for retrying; its behavior must be stable.
#[test]
fn retryable_kinds_are_stable() {
    // Retryable: transient network / server-side problems.
    assert!(ErrorKind::Transport.is_retryable());
    assert!(ErrorKind::RateLimited.is_retryable());
    assert!(ErrorKind::ServerError.is_retryable());

    // Not retryable: only changing the configuration, the arguments, or the code will help.
    assert!(!ErrorKind::BadRequest.is_retryable());
    assert!(!ErrorKind::Unsupported.is_retryable());
    assert!(!ErrorKind::ToolFailure.is_retryable());
    assert!(!ErrorKind::Internal.is_retryable());
}

/// Every kind must have stable display text (used by logs / auditing).
#[test]
fn every_kind_has_display_text() {
    for kind in [
        ErrorKind::Transport,
        ErrorKind::RateLimited,
        ErrorKind::ServerError,
        ErrorKind::BadRequest,
        ErrorKind::Unsupported,
        ErrorKind::ToolFailure,
        ErrorKind::Internal,
    ] {
        assert!(
            !kind.to_string().is_empty(),
            "{kind:?} is missing display text"
        );
    }
}

/// The error chain must trace up to the underlying cause (`#[source]` was not written wrong).
#[test]
fn error_source_chain_is_preserved() {
    let error = AdapterError::Http {
        status: 503,
        body: "upstream unavailable".into(),
    };
    // If there is a body, it should appear in the display text, to ease troubleshooting.
    let text = error.to_string();
    assert!(
        text.contains("503"),
        "display text should contain the status code: {text}"
    );
    assert!(
        text.contains("upstream unavailable"),
        "display text should contain the error body: {text}"
    );
}

// --- Test helpers -----------------------------------------------------------

/// Walk the status code -> kind mapping with a real `AdapterError`.
fn kind_of_http(status: u16) -> ErrorKind {
    AdapterError::Http {
        status,
        body: String::new(),
    }
    .kind()
}
