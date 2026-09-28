**Shirley 技术方案 · 流式输出（P1）**

**一、现状（已落地，本文档转为"已实现能力的设计说明"）**

流式已经接通：`encode_request` 的 `"stream"` 读 `ModelConfig.stream`（`main.rs` 里配置为 `true`），响应侧由 `chat_completions::decode_stream_response` 做 SSE 增量解析，`AgentEvent` 会逐片产出 `ContentDelta` / `ReasoningDelta`，TUI 的 `App::append_streaming_delta` 负责追加。

```rust
// adapter/chat_completions/mod.rs（现状）
let body = serde_json::json!({
    "model": &config.model,
    "messages": encode_messages(&input.messages),
    "tools": encode_tools(&input.tools),
    "thinking": { "type": thinking },          // 读 config.thinking
    "reasoning_effort": &config.reasoning_effort,
    "stream": &config.stream,                  // 读 config.stream
});
```

事件名定为 `ContentDelta`（不是早期草稿里的 `TextDetal`），reasoning 用 `ReasoningDelta`。`adapter/mod.rs` 的 `invoke` 在 `config.stream` 上分流；`runtime` 把 `AdapterEvent::ContentDelta` / `ReasoningDelta` 直接转成 `AgentEvent`。

**仍然存在的空白**：`decode_stream_response` 里对 SSE `id:` 行仍是 `todo!()`；`Agent::run_stream` 没有测试覆盖流式/非流式一致性。

**二、为什么它不只是体验问题**

非流式下，一个长回答（或长 reasoning）期间 TUI 只能显示"AI 回复中 Ns"。用户无法判断是在思考、在生成、还是卡住了。对 Coding Agent 这种动辄几十秒的任务，这直接影响"敢不敢交给它"。同时，流式也是后面"边生成边执行"的前提。

**三、设计要点**

**3.1 协议层：SSE 解码（已实现）**

ChatCompletions 流式是 `text/event-stream`，每行 `data: {...}`，以 `data: [DONE]` 结束。当前实现没有单独的 `SseDecoder` 结构体，而是在 `decode_stream_response` 内用一个 `buffer: String` 按 `\n\n` 切事件：

```rust
// crates/agent-sdk/src/adapter/chat_completions/mod.rs（现状）
let mut buffer = String::new();
while let Some(chunk) = byte_stream.next().await {
    buffer.push_str(&String::from_utf8_lossy(&chunk));
    while let Some(pos) = buffer.find("\n\n") {
        // 取出一个完整事件，逐行解析 event:/data:/id:
    }
}
```

注意点：chunk 边界会切断 JSON，所以按事件边界（空行）而不是按 chunk 边界解析；`[DONE]` 被当成终止哨兵跳过，不做 JSON 解析。（`id:` 行仍留了一个 `todo!()`。）

**3.2 增量拼接**

流式的每个 delta 只带片段，需要累积成完整消息：

```rust
#[derive(Default)]
struct StreamAccumulator {
    content: String,
    reasoning: String,
    tool_calls: BTreeMap<usize, ToolCallAccumulator>, // 按 index 聚合
    finish_reason: Option<String>,
    usage: Option<Usage>,
}
```

工具调用的 delta 尤其要注意：`function.arguments` 是**分片下发的 JSON 字符串**，必须按 `index` 聚合后再解析，不能每片都尝试 `from_str`。

**3.3 事件产出（已实现，命名与草稿略有出入）**

`AgentEvent` 实际新增的是：

```rust
pub enum AgentEvent {
    ContentDelta(String),      // 文本增量（草稿里叫 TextDelta）
    ReasoningDelta(String),    // 思考增量
    // ... 原有事件
}
```

注意没有单独的 `ToolCallDelta`：工具调用增量在 `decode_stream_response` 内部聚合（`merge_tool_call_deltas` + `finish_tool_calls`），只在流结束时通过 `Finished(ModelResponse)` 交付完整 `tool_calls`。

**3.4 流结束后的一致性**

关键约束：**流式与非流式最终落库的 `Message` 必须完全一致**。当前实现的做法是 `decode_stream_response` 内部累积 `content` / `reasoning_content` / `tool_calls`，在收到 `finish_reason` 时产出一个 `AdapterEvent::Finished(ModelResponse)`，之后走和现在完全相同的落库逻辑。这样压缩、`active_messages`、`RunResult` 都不用区分两种模式。

（草稿里提出的独立 `StreamAccumulator` 结构体没有单独抽出，累积逻辑直接内联在流式解码函数里。）

**四、`run_stream` 的改造（已实现）**

`Agent` 的方法名已经叫 `run_stream`（指事件流），模型是否流式由 `ModelConfig.stream` 决定。`adapter::invoke` 在 `config.stream` 上分流（`Adapters/mod.rs`）：流式消费 `decode_stream_response` 的事件流并 `yield`，非流式解析完整响应后 `yield` 一个 `Finished`。

```rust
// adapter/mod.rs（现状）
if !config.stream {
    // 完整响应 → yield AdapterEvent::Finished(msg)
} else {
    // decode_stream_response → 逐事件 yield
}
```

（草稿里提到的 `GenerationConfig` / `ModelOutput` 枚举没有引入，开关直接放在 `ModelConfig` 上。）

**五、TUI 侧（已实现）**

`AgentUpdate` 那层手写翻译已经去掉，`tui::apply` 直接消费 `AgentEvent`：

- 收到 `ContentDelta` → `App::append_streaming_delta(delta, false)`，追加到"当前正在生成的 Assistant 消息"（`streaming_delta_start` 指向起点，不是新增一条）。
- 收到 `ReasoningDelta` → `App::append_streaming_delta(delta, true)`，追加到"当前思考块"。
- 收到 `MessageAdded(Assistant)` → `App::finish_streaming_deltas()` 结束流式块，转为最终消息。

`App` 已经有 `streaming_delta_start: Option<usize>` 来表达"进行中的消息"。

**六、性能与缓存注意**

- 流式**不影响** prefix 缓存：请求体的前缀（system + 历史消息 + 工具定义）不变，`stream: true` 只是响应侧差异。`thinking` / `reasoning_effort` 现在读 `ModelConfig`（不再是硬编码），改动这些配置会改变请求前缀，破坏缓存——需要留意。
- `ToolManager::definitions()` 的排序（按 name）必须保留，`Agent.md` 特别提醒过。
- 每帧重建 `MessageCache` 在流式下会变成高频操作，必须改成增量追加（见 `testing.md` 的性能一节）。

**七、验收**

1. 开启 `stream: true`，TUI 逐字显示回答，reasoning 可见（`Ctrl+T` 控制）。
2. 流式与非流式对同一 prompt，落库的 `Message::Assistant` 内容一致。（暂无测试守护）
3. 工具调用在流式下能正确聚合出完整 `arguments` JSON。（`merge_tool_call_deltas` 已实现）
4. 中途取消，已生成的部分保留，且不产生半条损坏消息。（取消通道尚未实现，见 `runtime-hardening.md` 第四节）
