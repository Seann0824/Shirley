**Shirley technical design · Runtime hardening (P0)**

**1. Current state and risks**

The main loop of `run_stream` currently has only one exit -- "this round has no tool_calls". Around it there are four concrete problems:

1. **No step cap**. If the model keeps returning tool calls, it keeps running. `StopReason::MaxStepsReached` / `Cancelled` are defined, but nothing in the code ever produces them.
2. **No retry**. `adapter::invoke` turns 429, 5xx, and timeouts all directly into `Err`; one hiccup wastes the entire long task.
3. **`request_timeout` is defined but never used**. `reqwest::Client::new()` is created fresh on every `run_stream`, the connection pool is not reused, and there is no timeout.
4. **`_ => todo!()` in `codec`**. Switching to `Responses` / Anthropic is not an error but a panic.
5. **Compaction is a single point of failure**. A `compress_context` failure aborts the whole stream and the UI becomes one error line; and after compaction, if the summary itself is still over the threshold, compaction is triggered again immediately (churn).

**2. Structured errors (do this first)**

Retry, degradation, and cancellation semantics all depend on "can this error be retried". Right now `ModelError = String` throws away the status code and semantics entirely.

```rust
// crates/shirley-agent-sdk/src/adapter/error.rs (new)
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("network request failed: {0}")]
    Transport(#[source] reqwest::Error),

    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("response parsing failed: {0}")]
    Decode(String),

    #[error("request encoding failed: {0}")]
    Encode(String),

    #[error("protocol {protocol} is not yet implemented")]
    UnsupportedProtocol { protocol: String },
}

impl ModelError {
    pub fn is_retryable(&self) -> bool {
        match self {
            ModelError::Transport(_) => true,
            ModelError::Http { status, .. } => matches!(status, 408 | 409 | 429 | 500..=599),
            _ => false,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> { /* parse the Retry-After header */ }
}
```

`AgentError` implements `std::error::Error` in step, and passes `is_retryable` through.

**3. Step cap and termination semantics**

```rust
#[bon::bon]
impl Agent {
    pub fn new(
        // ...
        #[builder(default = 32)] max_steps: usize,
    ) -> Self { /* ... */ }
}
```

The main loop becomes counted:

```rust
let mut steps = 0usize;
loop {
    if steps >= self.max_steps {
        yield AgentEvent::Finished(RunResult {
            messages: self.messages[start_index..].to_vec(),
            stop_reason: StopReason::MaxStepsReached,
            usage: total_usage,
        });
        break;
    }
    steps += 1;
    // ... existing logic
}
```

At the same time, consume `finish_reason` -- currently it is decoded but nobody uses it, so `length` (output truncated) is treated as a normal completion:

```rust
match response.finish_reason {
    ModelFinishReason::Length => { /* mark truncation, prompt the model to continue or compact if necessary */ }
    ModelFinishReason::Other(reason) => { /* record the unknown reason */ }
    _ => {}
}
```

**4. Cancellation**

`StopReason::Cancelled` needs a real cancellation channel; otherwise the TUI's `Esc` can only `abort` the whole task (losing the `Agent` and all history along with it).

Plan: use `tokio_util::sync::CancellationToken`.

```rust
pub fn run_stream_cancellable<'a>(
    &'a mut self,
    task: &'a str,
    cancel: CancellationToken,
) -> Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentError>> + Send + 'a>> {
    Box::pin(async_stream::try_stream! {
        // ...
        tokio::select! {
            _ = cancel.cancelled() => {
                yield AgentEvent::Finished(RunResult {
                    messages: self.messages[start_index..].to_vec(),
                    stop_reason: StopReason::Cancelled,
                    usage: total_usage,
                });
                break;
            }
            response = adapter::invoke_with_retry(&client, &self.model_config, model_request) => {
                // ...
            }
        }
    })
}
```

On the TUI side, map `Esc` to "cancel the current round" rather than "destroy the Agent". This way both the session history and the `Agent` are preserved.

**5. HTTP client and retry**

**5.1 Client reuse**

`Agent` holds a `reqwest::Client` rather than creating a new one every round:

```rust
pub struct Agent {
    // ...
    client: reqwest::Client,
}

// at construction
let client = reqwest::Client::builder()
    .timeout(model_config.request_timeout)
    .pool_max_idle_per_host(4)
    .build()?;
```

**5.2 Backoff retry**

```rust
pub async fn invoke_with_retry(
    client: &reqwest::Client,
    config: &ModelConfig,
    input: ModelRequest<'_>,
) -> Result<ModelResponse, ModelError> {
    const MAX_ATTEMPTS: u32 = 3;
    let mut attempt = 0;
    loop {
        match invoke(client, config, input.clone()).await {
            Ok(response) => return Ok(response),
            Err(error) if error.is_retryable() && attempt + 1 < MAX_ATTEMPTS => {
                attempt += 1;
                let base = error.retry_after()
                    .unwrap_or(Duration::from_millis(500 * 2u64.pow(attempt)));
                let jitter = /* random jitter in 0..base/4, to avoid a synchronized retry storm */;
                tokio::time::sleep(base + jitter).await;
            }
            Err(error) => return Err(error),
        }
    }
}
```

Note: `ModelRequest` is currently borrowed (`&'a [Message]`); putting it in a retry loop requires rebuilding it on each attempt, or changing it to an owned request struct. The latter is cleaner and also solves the streaming reuse problem along the way.

**6. Compaction robustness**

**6.1 Degrade on failure**

A compaction failure should not abort the task. Degradation strategy:

- Compaction failure -> emit a warning event, set `compression_pending` back to `false`, and continue this round's conversation.
- If it fails 2 times in a row, do not attempt compaction again within this round, and rely directly on provider-side truncation or an error.

```rust
match self.compress_context(&client).await {
    Ok(usage) => { /* normal */ }
    Err(error) => {
        self.compression_pending = false;
        self.compression_failures += 1;
        yield AgentEvent::CompressionFailed { reason: error.to_string() };
        // continue, do not Err
    }
}
```

**6.2 Suppress churn**

Immediately after compaction, judge whether the summary itself is still close to the threshold; if so, do not compact again, but treat the "summary + recent messages" as the new active window, and emit `ContextUsage` so the upper layer can see it.

**6.3 Clean up the original messages (optional but recommended)**

Currently `active_messages()` only does request-side slicing, and `self.messages` grows monotonically. After a successful compaction, the archived messages could be moved to `archived: Vec<Message>`, making both the memory and the semantics of `RunResult.messages` clearer.

**7. `todo!()` fix**

```rust
fn codec(protocol: &ModelProtocol) -> Result<(Encoder, Decoder), ModelError> {
    match protocol {
        ModelProtocol::ChatCompletions => Ok((
            chat_completions::encode_request,
            chat_completions::decode_response,
        )),
        other => Err(ModelError::UnsupportedProtocol {
            protocol: format!("{other:?}"),
        }),
    }
}
```

Any unimplemented path returns `Err`; never panic inside the SDK.

**8. Acceptance**

1. Construct a fake response that keeps returning tool_calls; the Agent ends with `MaxStepsReached` after `max_steps`.
2. Inject a 429 / 500; it backs off, retries, and continues.
3. The timeout takes effect (verifiable with a slow endpoint after `request_timeout` is wired up).
4. When a compaction instruction causes the model to return empty content, the task continues rather than aborting.
5. Switching to the `Responses` protocol returns `Err`, not a panic.
6. After `Esc` cancels, the `Agent` can still continue the next round.
