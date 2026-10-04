**Shirley 技术方案 · Responses 协议适配**

本文把 DeepSeek 的 Responses API（`POST /responses`）沉淀为可落地的适配方案。
协议依据：

- 指南：<https://api-docs.deepseek.com/guides/responses_api/>
- 接口：<https://api-docs.deepseek.com/zh-cn/api/create-response>

Responses 与 ChatCompletions 是**两套 wire 协议，不是同一个协议的别名**。本文只描述
"多一份协议实现"，抽象层本身的重构见 `docs/adapter-layer.md`。

> **状态：已实现**（`crates/shirley-agent-sdk/src/adapter/responses/`）。
> 落地时的两点取舍：
> 1. `codec` 从 `(Encoder, Decoder)` 扩成 `(Encoder, Decoder, StreamDecoder)`，
>    流式解码也纳入抽象，`invoke` 里不再按协议硬分支；
> 2. 流式**只认终态事件**（`response.completed` / `incomplete` / `failed`），
>    delta 仅用于实时渲染，不做 `function_call_arguments` 分片聚合——因为终态对象
>    已携带完整 `output`，比靠 delta 拼更可靠（本文四.4 的"双轨、以终态为准"）。

---

**一、与 ChatCompletions 的差异总览**

先把"为什么不能复用现有 codec"讲清楚。差异集中在四处：

| 维度 | ChatCompletions | Responses |
| --- | --- | --- |
| 端点 | `POST /v1/chat/completions` | `POST /responses` |
| 消息 | `messages: [{role, content}]` | `input: string \| item[]`，system 走顶层 `instructions` |
| 工具调用 | assistant 上的 `tool_calls[]`；结果走 `role:"tool"` | 顶层 `function_call` / `function_call_output` item；结果按 `call_id` 配对 |
| 工具参数 | `tools[].function.parameters` | `tools[].parameters`（少一层 `function` 包装） |
| 流式结束 | `data: [DONE]` | **没有 `[DONE]`**，以 `response.completed/incomplete/failed` 收尾 |
| token 用量 | `prompt_tokens` / `completion_tokens` | `input_tokens` / `output_tokens` |

**关键结论**：`input` 是一个 **item 列表**，而不是"消息列表"。一轮对话在这里被
摊平成扁平的 item 序列——assistant 的文本、它发出的 `function_call`、以及工具回传的
`function_call_output` 是**并列的兄弟 item**，而不是嵌套在一条 assistant 消息里。
这决定了编码器不能沿用"Message → 一条 wire 消息"的一对一映射。

---

**二、请求编码**

**2.1 顶层参数**

| 我们的字段 | Responses 键 | 说明 |
| --- | --- | --- |
| `model` | `model` | 必填 |
| system prompt | `instructions` | **不是** system 消息，是顶层字符串；作为首条 system 消息注入 |
| `stream` | `stream` | 布尔 |
| `temperature` | `temperature` | 范围 `[0, 2]`；思考模式下不生效 |
| `max_output_tokens` | `max_output_tokens` | 含可见输出 + 思维链 token |
| `reasoning_effort` | `reasoning.effort` | **嵌套对象**，取值 `none`/`low`/`high`/`max`；`minimal→low`、`medium`/`xhigh→high` 由服务端兼容映射 |
| `tool_choice` | `tool_choice` | `none`/`auto`/`required` 或 `{type:"function", name}` |
| `extra_body` | 浅合并 | 逃生口，语义同 gap-4 |

注意两个易错点：

1. `reasoning_effort` 在 ChatCompletions 是**顶层字符串**，在 Responses 要包成
   `{"reasoning": {"effort": "..."}}`。同一个 `ModelConfig` 字段，两个协议两种形状。
2. `max_output_tokens` 键名在这里**就是** `max_output_tokens`（ChatCompletions 是
   `max_tokens`）——正好与我们的内部字段名一致，别被 gap-4 的映射经验带偏。

**2.2 消息 → input items**

`encode_messages` 在本协议下要产出 item 序列。映射规则：

| 内部 Message | 产出 item |
| --- | --- |
| `System` | 摘出来 → 顶层 `instructions`（多个则拼接）；**不**放进 `input` |
| `ContextSummary` | 同 `System`（我们内部用它承载压缩摘要，语义上就是 system 级指令） |
| `User` | `{type:"message", role:"user", content: <string>}` |
| `Assistant`（有文本） | `{type:"message", role:"assistant", content:[{type:"output_text", text}]}` |
| `Assistant`（有 tool_calls） | 每个调用产出一个 `{type:"function_call", call_id, name, arguments}` item |
| `Assistant`（既有文本又有 tool_calls） | 文本 item + 若干 `function_call` item（兄弟关系） |
| `Tool` | `{type:"function_call_output", call_id, output: <string>}` |

**关键点**：

- 文本内容块类型是 `output_text`（assistant 侧）或 `input_text`（user 侧）。DeepSeek 对
  两者都接受，但我们按角色区分更贴合 OpenAI 语义。
- `function_call` 的 `arguments` 是**字符串形式的 JSON**，与 ChatCompletions 一致，直接透传。
- `call_id` 必须非空、唯一，且**每个 `function_call` 必须有配对的 `function_call_output`**。
  我们的 `Message::Tool` 已经带 `tool_call_id`，直接当 `call_id` 用。
- 一条 assistant 同时有文本和工具调用时，会被**拆成多个 item**——这是与
  ChatCompletions 最本质的结构差异，编码器要能一对多展开。

**2.3 工具定义**

```json
// ChatCompletions
{ "type": "function", "function": { "name", "description", "parameters" } }
// Responses
{ "type": "function", "name", "description", "parameters" }
```

少了 `function` 这一层包装，`name`/`description`/`parameters` 直接平铺。
`parameters` 仍是 JSON Schema 对象——**印证 `docs/adapter-layer.md` 的判断：
JSON Schema 本身就是跨协议的中间表示，差异只在包装层**。因此工具 schema 的
编码只需一个 `encode_tool_schema` 变体，无需重建参数模型。

约束：函数名非空、≤128 字符、匹配 `^[a-zA-Z0-9_-]+$`、全局唯一。

---

**三、非流式响应解码**

响应对象（`object: "response"`）关键字段：

```json
{
  "id": "...", "object": "response", "created_at": 0,
  "status": "completed",
  "output": [
    { "type": "reasoning", "id": "rs_1",
      "content": [{ "type": "reasoning_text", "text": "..." }] },
    { "type": "message", "id": "msg_1", "role": "assistant",
      "content": [{ "type": "output_text", "text": "...", "annotations": [] }] },
    { "type": "function_call", "call_id": "fc1", "name": "take_screenshot", "arguments": "{}" }
  ],
  "usage": {
    "input_tokens": 22,
    "input_tokens_details": { "cached_tokens": 0 },
    "output_tokens": 29,
    "output_tokens_details": { "reasoning_tokens": 27 },
    "total_tokens": 51
  }
}
```

**解码规则**：遍历 `output[]`，按 `type` 分派：

- `reasoning` → 拼接其 `content[].text`（`reasoning_text` 块）为 `reasoning_content`；
  `summary` 字段被接受但不会生成内容，忽略。
- `message` → 拼接 `content[].text`（`output_text` 块）为 `content`。
- `function_call` → 收集为 `tool_calls[]`，`call_id`/`name`/`arguments` 直接映射。

三者可同时出现，最终合成**一条** `Message::Assistant { content, reasoning_content, tool_calls }`。
这正是内部模型"一条 assistant 消息可同时承载文本 + 思考 + 工具调用"能自然容纳的形状——
解码方向是**多对一**，比编码方向的**一对多**简单。

**finish_reason 映射**（`status` + `incomplete_details`）：

| 响应状态 | `ModelFinishReason` |
| --- | --- |
| `completed` 且 `output` 含 `function_call` | `ToolCalls` |
| `completed` 否则 | `Stop` |
| `incomplete` 且 `incomplete_details.reason == "max_output_tokens"` | `Length` |
| `failed` | 建议映射为 `AdapterError`（携带 `error`），而非 `Other` |

注意：Responses **没有 `finish_reason` 字段**，判定工具调用要**看 output 里有没有
`function_call` item**，不能像 ChatCompletions 那样读一个字符串。

**usage 映射**：

| 内部 `Usage` 字段 | 来源 |
| --- | --- |
| `input_tokens` | `usage.input_tokens` |
| `output_tokens` | `usage.output_tokens` |
| `cached_input_tokens` | `usage.input_tokens_details.cached_tokens` |
| `cache_reported_input_tokens` | 同上命中时 = `input_tokens`（沿用"上报了才算分母"的契约） |
| `reasoning_tokens` | `usage.output_tokens_details.reasoning_tokens` |

**与既有契约一致**：`cached_tokens` 缺失时 `cached_input_tokens = None`（不是 0），
`cache_hit_rate()` 相应返回 `None`。别为了省事 `unwrap_or(0)`。

---

**四、流式解码**

**4.1 SSE 形态**

每个事件同时带 `event:` 行与 `data:` 行，`data` 里也含 `type`（冗余但可自描述）：

```
event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":11,
       "item_id":"msg_1","output_index":1,"content_index":0,"delta":"Hello"}
```

`sequence_number` 单调递增。**流以 `response.completed` / `response.incomplete` /
`response.failed` 结束，没有 `data: [DONE]`**——现有 ChatCompletions 的
`payload == "[DONE]"` 跳过逻辑在这里永远不会命中，收尾必须靠**事件类型**驱动。

**4.2 事件表**

| 事件 | 我们的处理 |
| --- | --- |
| `response.created` | 忽略（或记 id） |
| `response.in_progress` | 忽略 |
| `response.output_item.added` / `.done` | 记录 item 边界（见下） |
| `response.content_part.added` / `.done` | 忽略 |
| `response.reasoning_text.delta` | → `AdapterEvent::ReasoningDelta(delta)` |
| `response.reasoning_text.done` | 忽略（delta 已累计） |
| `response.output_text.delta` | → `AdapterEvent::ContentDelta(delta)` |
| `response.output_text.done` | 忽略 |
| `response.function_call_arguments.delta` | 累积到对应 `call_id` 的 arguments |
| `response.function_call_arguments.done` | 可用其完整 arguments 兜底校验 |
| `response.completed` | → `AdapterEvent::Finished(ModelResponse)`（含 usage） |
| `response.incomplete` | → `Finished`，`finish_reason = Length` |
| `response.failed` | → `Err(AdapterError)`，携带 `response.error` |

**4.3 工具调用的流式拼装**

ChatCompletions 用 `delta.tool_calls[].index` 做分片聚合；Responses 改用
**`call_id` + `output_index`**：

- `response.output_item.added`（item.type == `function_call`）先给出
  `call_id` / `name`，此时 arguments 为空；
- 后续 `response.function_call_arguments.delta` 按 `call_id`（或 `item_id`/`output_index`）
  累积 arguments 分片；
- `response.output_item.done` 时该调用完整。

所以聚合容器要从 `BTreeMap<usize, ToolCall>` 换成按 `call_id` 索引。
**不依赖到达顺序**（虽然 `sequence_number` 保证有序，但别把正确性押在它上面）。

**4.4 收尾的差异（必须处理）**

现有 `decode_stream_response` 的收尾逻辑是：

```rust
if let Some(finish_reason) = finish_reason { /* yield Finished */ }
```

它**依赖 ChatCompletions 每轮都会给 `finish_reason`**。Responses 没有这个字段，
如果照搬，`Finished` 永远不会发出。正确做法是：

- 遇到 `response.completed/incomplete` 时，**直接用该事件携带的完整 `response` 对象**
  构造 `Finished`——`response.output` 是权威结果，比"靠 delta 拼"更可靠；
- delta 只用于**实时渲染**，最终态以 `response.completed` 里的 `output` 为准。
  这与 `plan.md` 第 101 行的既有设计意图一致："当 finish_reason 出现时，将本轮
  完整消息通过 Finished 抛出"。

> 建议：delta 累计与终态对象**双轨**。若终态 `output` 与累计值不一致，**以终态为准**，
> 并在测试里锁死这个行为。

---

**五、落地方式**

**5.1 接入点**

现有 `codec(protocol)` 返回 `(Encoder, Decoder)` 函数对。Responses 需要**三处**：

```rust
fn codec(protocol) -> Result<(Encoder, Decoder), AdapterError> {
    match protocol {
        ChatCompletions => Ok((chat_completions::encode_request, chat_completions::decode_response)),
        Responses => Ok((responses::encode_request, responses::decode_response)),
        ...
    }
}
```

但流式解码当前是 `invoke()` 里 `match config.protocol` 硬分支：

```rust
let mut stream = match &config.protocol {
    ModelProtocol::ChatCompletions => chat_completions::decode_stream_response(response).await,
    other => Err(UnsupportedProtocol)?,
};
```

**这里要一并改造**：把流式解码也纳入 codec 抽象，否则每加一个协议就多一个硬分支。
建议扩成三元组或一个小 trait：

```rust
type StreamDecoder = fn(reqwest::Response)
    -> Pin<Box<dyn Stream<Item = Result<AdapterEvent, AdapterError>> + Send>>;
```

**5.2 新文件**

`crates/shirley-agent-sdk/src/adapter/responses/{mod.rs, dto.rs}`，结构与
`chat_completions/` 对称：`mod.rs` 放 `encode_request` / `decode_response` /
`decode_stream_response`，`dto.rs` 放反序列化结构。

**5.3 复用与不复用**

| 复用 | 不复用 |
| --- | --- |
| `message::Message` / `ToolCall` / `Usage` | 消息编码（input item 结构不同） |
| `PreparedRequest` / `ensure_success` / `apply_extra_body` | 工具定义编码（少一层 `function`） |
| SSE 分帧逻辑（按 `\n\n` 切段） | 事件分派与聚合键（`call_id` vs `index`） |
| `ModelFinishReason` / `AdapterEvent` | finish 判定（看 output，不读字符串） |

SSE 分帧可以抽成公共工具函数，但**事件语义必须各写各的**——两协议的 payload 形状
完全不同，硬套一个泛型解码器只会更绕。

**5.4 配置接线**

- `ModelProtocol::Responses` 已存在，只需让 `codec` 认它。
- `settings.rs` 解析 `protocol` 时补 `"responses"` 分支（现只有 chat 与 anyhtopic）。
- `main.rs` 构造 `ModelConfig` 时若协议为 Responses，`base_url` 指向 `/responses`。
- `extra_body` 逃生口照常生效，用于厂商私有字段。

---

**六、验证清单**

1. **编码**：一条含 system + user + assistant(文本+tool_calls) + tool 的对话，
   `input` 展开为正确的 item 序列，`instructions` 独立、不在 `input` 里。
2. **编码**：`reasoning_effort` 包成 `{"reasoning":{"effort":...}}`；
   `max_output_tokens` 键名正确。
3. **解码（非流式）**：`output` 含 reasoning + message + function_call 时，
   合成一条 assistant，三部分齐全；usage 各字段（含 cached/reasoning）正确。
4. **解码（非流式）**：`incomplete` → `Length`；`failed` → 错误而非 `Other`。
5. **流式**：`output_text.delta` 序列 → 一串 `ContentDelta`；
   `reasoning_text.delta` → `ReasoningDelta`。
6. **流式**：`function_call` 分片按 `call_id` 聚合正确；
   `response.completed` 产出的 `Finished` 与 `output` 一致。
7. **流式收尾**：**没有 `[DONE]`** 也能正确 `Finished`；`failed` 事件转为错误。
8. **契约**：`cached_tokens` 缺失时 `cached_input_tokens == None`，
   `cache_hit_rate()` 返回 `None`（不是 0）。
9. **回归**：ChatCompletions 路径行为完全不变。

**验证状态**：1–9 条均已由单测覆盖（`adapter::responses::tests`，12 条 + `adapter::sse` 3 条）。
此外有 **3 条真实网络测试**（默认 `#[ignore]`），已对 `https://api.deepseek.com` 跑通：

```sh
export DEEPSEEK_API_KEY=...
# 可选：SHIRLEY_RESPONSES_BASE_URL / SHIRLEY_RESPONSES_MODEL
cargo test -p shirley-agent-sdk --lib responses::tests::live -- --ignored --nocapture
```

| 测试 | 验证内容 | 结果 |
| --- | --- | --- |
| `live_responses_round_trip` | 流式纯文本往返 | `content="Hi"`，`finish=Stop`，usage 正确 |
| `live_responses_tool_round_trip` | 非流式工具往返（发起 → 回传 output → 收尾） | 两轮均正确，`call_id` 配对被服务端接受 |
| `live_responses_streaming_tool_call` | 流式工具调用 | 终态含完整 `function_call`，`finish=ToolCalls` |

真实抓包确认的两个事实（都写进了实现）：

1. **流式终态 `response.completed` 的 `output` 携带完整 item 列表**（`reasoning` +
   `function_call`，含完整 `call_id` / `name` / `arguments`）——所以**无需**聚合
   `function_call_arguments.delta`，本文四.4 的"以终态为准"成立。
2. 服务端**会上报 `cached_tokens: 0`**（而非省略），因此 `cached_input_tokens`
   会是 `Some(0)`；这符合契约——`Some(0)` 表示"确实命中 0"，`None` 才表示"未上报"。

---

**七、与其它文档的关系**

- `docs/adapter-layer.md`：本文是其"五、多协议落地"里 **Responses 优先** 那一步的展开。
  其中"债一（encode_messages 归位）"在本协议下更紧迫——因为 Responses 的
  item 展开逻辑注定不能和 ChatCompletions 共用。
- `docs/sdk-gaps.md`：gap-4 的 `extra_body` 逃生口在本文里继续沿用。
- `plan.md` 第 36 行"先将适配层做了"：本文是这条路线上的具体一步。
- `docs/anthropic-messages-api.md`：第三个协议的方案，与本文结构对称。
