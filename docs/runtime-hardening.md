**Shirley 技术方案 · 运行时加固（P0）**

**一、现状与风险**

`run_stream` 的主循环目前只有一个出口——"本轮没有 tool_calls"。围绕它有四个具体问题：

1. **没有步数上限**。模型持续返回工具调用就会一直跑。`StopReason::MaxStepsReached` / `Cancelled` 已定义，但代码里没有任何地方产生它们。
2. **没有重试**。`adapter::invoke` 把 429、5xx、超时全部直接变成 `Err`，一次抖动废掉整轮长任务。
3. **`request_timeout` 定义了但从未使用**。`reqwest::Client::new()` 每次 `run_stream` 新建，连接池不复用，也没有超时。
4. **`codec` 里 `_ => todo!()`**。切到 `Responses` / Anthropic 不是报错而是 panic。
5. **压缩是单点故障**。`compress_context` 失败即中断整个 stream，UI 变成一条错误；且压缩后若摘要本身仍超阈值，会立刻再次触发压缩（抖动）。

**二、结构化错误（先做这个）**

重试、降级、取消语义都依赖"这个错误能不能重试"。现在 `ModelError = String` 把状态码和语义全丢了。

```rust
// crates/shirley-agent-sdk/src/adapter/error.rs（新增）
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("网络请求失败: {0}")]
    Transport(#[source] reqwest::Error),

    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("响应解析失败: {0}")]
    Decode(String),

    #[error("请求编码失败: {0}")]
    Encode(String),

    #[error("协议 {protocol} 尚未实现")]
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

    pub fn retry_after(&self) -> Option<Duration> { /* 解析 Retry-After 头 */ }
}
```

`AgentError` 同步实现 `std::error::Error`，并给 `is_retryable` 透传。

**三、步数上限与终止语义**

```rust
#[bon::bon]
impl Agent {
    pub fn new(
        // ...
        #[builder(default = 32)] max_steps: usize,
    ) -> Self { /* ... */ }
}
```

主循环改为带计数：

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
    // ... 原有逻辑
}
```

同时消费 `finish_reason`——目前它被解码但没人用，导致 `length`（输出被截断）被当成正常完成：

```rust
match response.finish_reason {
    ModelfinishReaon::Length => { /* 标记截断，必要时提示模型续写或压缩 */ }
    ModelfinishReaon::Other(reason) => { /* 记录未知原因 */ }
    _ => {}
}
```

**四、取消**

`StopReason::Cancelled` 需要真正的取消通道，否则 TUI 的 `Esc` 只能 `abort` 掉整个任务（连带丢掉 `Agent` 和全部历史）。

方案：用 `tokio_util::sync::CancellationToken`。

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

TUI 侧把 `Esc` 映射成"取消当前轮"，而不是"销毁 Agent"。这样会话历史与 `Agent` 都能保留。

**五、HTTP 客户端与重试**

**5.1 客户端复用**

`Agent` 持有 `reqwest::Client` 而不是每轮新建：

```rust
pub struct Agent {
    // ...
    client: reqwest::Client,
}

// 构造时
let client = reqwest::Client::builder()
    .timeout(model_config.request_timeout)
    .pool_max_idle_per_host(4)
    .build()?;
```

**5.2 退避重试**

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
                let jitter = /* 0..base/4 随机抖动，避免同步重试风暴 */;
                tokio::time::sleep(base + jitter).await;
            }
            Err(error) => return Err(error),
        }
    }
}
```

注意：`ModelRequest` 目前是借用（`&'a [Message]`），放进重试循环需要在每次尝试时重建，或改为拥有所有权的请求结构。后者更干净，也顺带解决流式的复用问题。

**六、压缩健壮性**

**6.1 失败降级**

压缩失败不应中断任务。降级策略：

- 压缩失败 → 记一条警告事件，把 `compression_pending` 置回 `false`，继续本轮对话。
- 若连续失败 2 次，本轮内不再尝试压缩，直接依赖供应商侧截断或报错。

```rust
match self.compress_context(&client).await {
    Ok(usage) => { /* 正常 */ }
    Err(error) => {
        self.compression_pending = false;
        self.compression_failures += 1;
        yield AgentEvent::CompressionFailed { reason: error.to_string() };
        // 继续，不 Err
    }
}
```

**6.2 抑制抖动**

压缩后立刻判断摘要本身是否仍接近阈值；若是，不再重复压缩，而是把"摘要 + 近期消息"作为新的活跃窗口，并给出 `ContextUsage` 让上层可见。

**6.3 清理原始消息（可选但推荐）**

当前 `active_messages()` 只做请求侧切片，`self.messages` 单调增长。可以在压缩成功后把已归档的消息移到 `archived: Vec<Message>`，让内存与 `RunResult.messages` 的语义都更清楚。

**七、`todo!()` 修复**

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

任何未实现路径都返回 `Err`，绝不在 SDK 里 panic。

**八、验收**

1. 构造一个持续返回 tool_calls 的假响应，Agent 在 `max_steps` 后以 `MaxStepsReached` 结束。
2. 注入 429 / 500，自动退避重试并继续。
3. 超时生效（`request_timeout` 接线后可用慢接口验证）。
4. 压缩指令导致模型返回空内容时，任务继续而非中断。
5. 切换到 `Responses` 协议返回 `Err`，不 panic。
6. `Esc` 取消后 `Agent` 仍可继续下一轮。
