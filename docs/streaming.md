**Shirley 技术方案 · 流式输出（P1）**

**一、现状**

`encode_request` 里 `"stream": false` 是写死的：

```rust
let body = serde_json::json!({
    "model": &config.model,
    "messages": encode_messages(&input.messages),
    "tools": encode_tools(&input.tools),
    "thinking": { "type": "disabled" },
    "reasoning_effort": "medium",
    "stream": false,          // <- 写死
});
```

`AgentEvent::TextDetal`（拼写应为 `TextDelta`）定义了但没有任何地方产生它；`ReasonDetail` 压根没定义。所以 `plan.md` 里排第一的流式，一行都还没落。

**二、为什么它不只是体验问题**

非流式下，一个长回答（或长 reasoning）期间 TUI 只能显示"AI 回复中 Ns"。用户无法判断是在思考、在生成、还是卡住了。对 Coding Agent 这种动辄几十秒的任务，这直接影响"敢不敢交给它"。同时，流式也是后面"边生成边执行"的前提。

**三、设计要点**

**3.1 协议层：SSE 解码**

ChatCompletions 流式是 `text/event-stream`，每行 `data: {...}`，以 `data: [DONE]` 结束。需要一个增量的 SSE 解析器，而不是"收完再解析"：

```rust
// crates/agent-sdk/src/adapter/sse.rs（新增）
pub struct SseDecoder {
    buffer: String,
}

impl SseDecoder {
    // 传入新收到的字节片段，返回已完整的 data 载荷
    pub fn push(&mut self, chunk: &str) -> Vec<String> { /* 按 \n\n 切分事件 */ }
}
```

注意点：chunk 边界会切断 JSON，必须按事件边界（空行）而不是按 chunk 边界解析；`[DONE]` 是终止哨兵，不是 JSON。

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

**3.3 事件产出**

`AgentEvent` 增补：

```rust
pub enum AgentEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta { index: usize, name: Option<String>, arguments_delta: String },
    // ... 原有事件
}
```

`TextDetal` 顺手改成 `TextDelta`（这是 `Agent.md` 里记的拼写债）。

**3.4 流结束后的一致性**

关键约束：**流式与非流式最终落库的 `Message` 必须完全一致**。做法是让流式路径把 delta 交给 `StreamAccumulator`，结束时产出一个 `Message::Assistant`，走和现在完全相同的落库逻辑。这样压缩、`active_messages`、`RunResult` 都不用区分两种模式。

**四、`run_stream` 的改造**

`Agent` 的方法名已经叫 `run_stream`（指事件流），现在需要区分"模型是否流式"。建议在 `GenerationConfig` 增加开关，并让 `invoke` 有两条路径：

```rust
pub struct GenerationConfig {
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub stream: bool,        // 新增
}
```

```rust
// adapter 侧
pub enum ModelOutput {
    Complete(ModelResponse),
    Streaming(Pin<Box<dyn Stream<Item = Result<StreamEvent, ModelError>> + Send>>),
}
```

运行时按 `generation.stream` 分支：流式则消费 `StreamEvent` 并 yield `TextDelta` / `ReasoningDelta`，同时喂给 accumulator；非流式走原路径。

**五、TUI 侧**

当前 `AgentUpdate` 是 `AgentEvent` 的手写翻译层，还丢信息。建议直接消费 `AgentEvent`，让 `App` 自己决定怎么渲染：

- 收到 `TextDelta` → 追加到"当前正在生成的 Assistant 消息"（而不是新增一条）。
- 收到 `ReasoningDelta` → 追加到"当前思考块"。
- 收到 `MessageAdded(Assistant)` → 结束当前流式块，转为最终消息。

这要求 `App` 有"进行中的消息"概念，现在只有已完成的 `items`。

**六、性能与缓存注意**

- 流式**不影响** prefix 缓存：请求体的前缀（system + 历史消息 + 工具定义）不变，`stream: true` 只是响应侧差异。但 `encode_request` 里 `thinking` / `reasoning_effort` 的硬编码要挪进 `GenerationConfig`，否则一旦改成可变，不同配置会破坏前缀稳定性。
- `ToolManager::definitions()` 的排序（按 name）必须保留，`Agent.md` 特别提醒过。
- 每帧重建 `MessageCache` 在流式下会变成高频操作，必须改成增量追加（见 `testing.md` 的性能一节）。

**七、验收**

1. 开启 `stream: true`，TUI 逐字显示回答，reasoning 可见（`Ctrl+T` 控制）。
2. 流式与非流式对同一 prompt，落库的 `Message::Assistant` 内容一致。
3. 工具调用在流式下能正确聚合出完整 `arguments` JSON。
4. 中途取消，已生成的部分保留，且不产生半条损坏消息。
