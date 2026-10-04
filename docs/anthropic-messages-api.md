# Anthropic Messages 协议适配

> 目标：让 `ModelProtocol::AnthropicMessages` 从 `UnsupportedProtocol` 变成可用实现。
>
> 与 Responses 的关系：`docs/responses-api.md` 讲的是"同一份 item 列表的两种展开"，
> 本文讲的是"三个协议里结构差异最大的一个"。抽象层本身见 `docs/adapter-layer.md`。

**端点**（DeepSeek 兼容）：`https://api.deepseek.com/anthropic/v1/messages`。
DeepSeek 也接受 Claude 模型名（`claude-opus*` → `deepseek-v4-pro`，
`claude-haiku*`/`claude-sonnet*` → `deepseek-flash`）。

**本文的事实全部来自真实抓包**（`deepseek-flash`，2025-10），不是照抄规范——
几处与"教科书版 Anthropic"不同，下面逐条标注。

---

**一、差异总览（vs ChatCompletions / Responses）**

| 维度 | ChatCompletions | Responses | **Anthropic Messages** |
| --- | --- | --- | --- |
| system | `messages[0]` | 顶层 `instructions` | 顶层 `system`（字符串或块数组） |
| 用户消息 | `content: "..."` | `content:[{input_text}]` | `content:[{text}]` |
| 工具调用 | assistant 的 `tool_calls[]` | 兄弟 item `function_call` | **content block** `tool_use` |
| 工具结果 | role=tool 消息 | `function_call_output` item | **user 消息里的** `tool_result` block |
| 工具参数 | JSON **字符串** | JSON **字符串** | JSON **对象** |
| 工具 schema | `function.parameters` | `parameters` | `input_schema` |
| 思考 | `reasoning_content` | `reasoning` item | `thinking` block（**带 signature**） |
| 思考回传 | 可选 | 不回传 | **必须回传**（否则 400） |
| 结束原因 | `finish_reason` | 无字段，看 output | `stop_reason` |
| 流式收尾 | `data: [DONE]` | `response.completed` | `message_stop`（无 `[DONE]`） |
| 流式工具参数 | `index` 聚合 | 终态自带完整 | `input_json_delta` **分片聚合** |
| 缓存语义 | `prompt_tokens` **含** cached | `input_tokens` 含 cached | `input_tokens` **不含** cached |

三条最要命的：
1. **工具结果放在 `user` 消息里**（不是独立 role），且是 content block；
2. **thinking 必须回传**（带 signature），否则下一轮直接 400；
3. **`input_tokens` 不含缓存**，命中率算法与 OpenAI 相反。

---

**二、请求编码**

**2.1 顶层字段**

```jsonc
{
  "model": "deepseek-flash",
  "max_tokens": 4096,          // 建议必填（见 2.5）
  "system": "…",               // system + ContextSummary 拼在这里
  "messages": [ /* 见 2.2 */ ],
  "tools": [ /* 见 2.3 */ ],
  "stream": true,
  "temperature": 0.3,          // 可选
  "thinking": { "type": "enabled", "budget_tokens": 2048 },  // 可选
  "tool_choice": { "type": "auto" }  // 形状与 OpenAI 不同
}
```

- `system` 可接受字符串，也可接受块数组。我们用字符串（多条 system/ContextSummary
  用 `\n\n` 拼接），与 Responses 的 `instructions` 处理一致。
- `thinking` 在 Anthropic 里**是对象**（`{type, budget_tokens}`），不是字符串；
  这与 Responses 的 `{"reasoning":{"effort":...}}`、ChatCompletions 的顶层字符串
  **三者形状各不相同**。我们 `ModelConfig.thinking: bool` → `{"type":"enabled"}`，
  `budget_tokens` 走 `extra_body`（需要精调时）。
- 注意 `reasoning_effort` 在 Anthropic 协议下**没有对应字段**（那是 Responses/OpenAI
  的概念）。若用户设了 `reasoning_effort`，Anthropic 编码应**忽略它**而不是硬塞。

**2.2 消息 → content block**

| 内部 Message | Anthropic 编码 |
| --- | --- |
| `System` / `ContextSummary` | 摘到顶层 `system` |
| `User{content}` | `{role:"user", content:[{type:"text", text}]}` |
| `Assistant{content, reasoning_content, tool_calls}` | `{role:"assistant", content:[thinking?, text?, tool_use…]}` |
| `Tool{tool_call_id, content}` | **`{role:"user", content:[{type:"tool_result", tool_use_id, content}]}`** |

**assistant 块顺序**：`thinking`（若 `reasoning_content` 非空）→ `text`（若 content 非空）
→ 每个 `tool_use`。顺序重要：thinking 必须在前。

**thinking 回传**：`reasoning_content` 非空时必须产出
`{"type":"thinking","thinking": <reasoning_content>, "signature": <…>}`。
signature 从哪来见 2.4——**这是本协议最大的结构性代价**。

**工具结果**：`Tool` 消息编码成 **`role:"user"`** 的消息（Anthropic 没有 tool role）。
多个连续 Tool 消息可以合并进同一条 user 消息的多个 `tool_result` block，
也可以各发一条 user 消息——两者服务端都接受；我们**逐条发**（实现简单，顺序天然正确）。

**2.3 工具定义**

```json
{ "name": "bash", "description": "…", "input_schema": { /* JSON Schema */ } }
```

`input_schema` 就是 `ToolDefinition.parameters`（JSON Schema），**只改键名**。
又一次印证：JSON Schema 是跨协议中间表示，三协议都直接吃它。

**2.4 thinking signature（关键设计点）**

真实抓包：
- 非流式：`thinking` block 直接带 `signature` 字段（DeepSeek 给的是个 UUID 样字符串）。
- 流式：`thinking_delta` 逐片给 thinking 文本，**最后一个 `signature_delta`** 给签名。
- 第二轮**必须**把 thinking block（含 signature）原样回传，否则：
  `400 The content[].thinking in the thinking mode must be passed back to the API.`

问题：内部 `Message::Assistant.reasoning_content: Option<String>` **装不下 signature**。

**方案（v0，最小改动）**：给 `reasoning_content` 附加一个**私有包装**，把 signature
随文本一起序列化进那个字符串，Anthropic 编码时再拆出来。缺点是污染了 `reasoning_content`
的语义（它本该是纯文本）。**不采用**——理由见下。

**方案（推荐，v1）**：新增 `ToolCall` 之外的旁路字段：
`Message::Assistant` 增加 `thinking_signature: Option<String>`
（`#[serde(default, skip_serializing_if = "Option::is_none")]`）。
- ChatCompletions / Responses 编码**忽略**它（零影响）。
- Anthropic 编码：`reasoning_content` + `thinking_signature` 一起产出 thinking block。
- Anthropic 解码：把 signature 写回该字段。
- 持久化：`session.rs` 的 JSONL 会自然带上它（`Message` 的 serde）。

这个字段是**协议中立**的（"思考块的完整性凭据"），不是 Anthropic 私货，
所以放进 `Message` 是合理的，不违反 SDK 边界原则。

> 若不想动 `Message`：退一步只做"无 signature 回传"，
> 代价是**带思考的多轮工具调用在 Anthropic 下会 400**。
> 因为工具调用几乎总伴随思考，这个退路实际不可用——**建议做 v1**。

**2.5 max_tokens**

Anthropic 规范里 `max_tokens` **必填**。真实抓包：DeepSeek 的兼容端点**缺省也接受**
（缺省时它自己给个上限）。但为了跨供应商可移植，我们：
- `max_output_tokens` 有值 → 写 `max_tokens`；
- 无值 → **给一个默认值**（如 4096），而不是省略。
这样对真 Anthropic 也能用。默认值取多少见"待定项"。

---

**三、非流式响应解码**

```json
{
  "id": "…", "type": "message", "role": "assistant", "model": "deepseek-flash",
  "content": [
    { "type": "thinking", "thinking": "…", "signature": "…" },
    { "type": "text", "text": "…" },
    { "type": "tool_use", "id": "call_00_…", "name": "get_weather", "input": {"city":"北京"} }
  ],
  "stop_reason": "tool_use",
  "stop_sequence": null,
  "usage": {
    "input_tokens": 168,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 128,
    "output_tokens": 75,
    "service_tier": "standard"
  }
}
```

**3.1 合成一条 assistant**：遍历 `content[]`，按 `type` 分派——
`thinking`→`reasoning_content`（+signature）、`text`→`content`（拼接）、
`tool_use`→`ToolCall`。`input` 是 **JSON 对象**，需 `to_string()` 成
`ToolCall.arguments`（内部契约是字符串）。多块 `text` 直接拼接。

**3.2 stop_reason → ModelFinishReason**

| stop_reason | 内部 | 说明 |
| --- | --- | --- |
| `end_turn` | `Stop` | 正常结束 |
| `tool_use` | `ToolCalls` | **由 content 里有无 tool_use 决定更稳**，但 stop_reason 也够 |
| `max_tokens` | `Length` | 截断 |
| `stop_sequence` | `Stop` | 命中自定义停止串 |
| `refusal` | `Other("refusal")` | 拒答 |
| `pause_turn` | `Other("pause_turn")` | 长任务暂停（服务端工具循环） |
| `null` | `Stop` | 兜底 |

建议**优先看 content 里有没有 `tool_use`**（与 Responses 一致的"不读字符串"原则），
`stop_reason` 作为辅助。

**3.3 usage 映射（反直觉，重点）**

真实抓包决定性证据：

```
call1: input=2435 cache_read=0                    （首次，无缓存）
call2: input=134  cache_read=2301                 （命中；134 + 2301 = 2435）
```

结论：**Anthropic 的 `input_tokens` 不含缓存读取**（与 OpenAI 的
`prompt_tokens` 含 cached **相反**）。所以：

```
usage.input_tokens   = input_tokens + cache_read_input_tokens   // 归一成"总输入"
usage.cached_input_tokens     = Some(cache_read_input_tokens)    // 上报了才 Some
usage.cache_reported_input_tokens = Some(总输入)                  // 命中率分母 = 总输入
```

**这是三个协议里唯一需要"加回缓存"的**。若照搬 OpenAI 的映射（直接取 `input_tokens`
当总量），命中率会算出 >100% 或严重偏低。**必须在注释里写清楚**。

`cache_creation_input_tokens`（写缓存）**不计入** `cached_input_tokens`
（那是"命中"，写入不是命中），但可考虑并入 `cache_reported_input_tokens` 的分母——
取决于我们想衡量"读命中率"还是"缓存有效性"。v0 取前者（只算 `cache_read`）。

---

**四、流式解码**

**4.1 事件表（真实抓包）**

| 事件 | 载荷 | 处理 |
| --- | --- | --- |
| `message_start` | `message{usage…}` | 记下初始 usage（input 侧） |
| `content_block_start` | `index`, `content_block{type,…}` | 按 type 建块：thinking / text / tool_use |
| `content_block_delta` | `index`, `delta{…}` | 见 4.2 |
| `content_block_stop` | `index` | 块收尾 |
| `message_delta` | `delta{stop_reason}`, `usage{output_tokens}` | **收尾的 stop_reason 与 output usage 在这里** |
| `message_stop` | — | 流结束 |
| `ping` | — | 忽略 |

**没有 `data: [DONE]`**；以 `message_stop` 结束。

**4.2 delta 类型**

| delta.type | 字段 | 映射 |
| --- | --- | --- |
| `text_delta` | `text` | → `ContentDelta` |
| `thinking_delta` | `thinking` | → `ReasoningDelta` |
| `signature_delta` | `signature` | 记入当前 thinking 块的 signature |
| `input_json_delta` | `partial_json` | **追加到当前 tool_use 块的 arguments 字符串** |

**4.3 工具参数分片聚合（与 Responses 的关键差异）**

Responses 的终态对象自带完整 `output`，我们**不用**聚合 delta。
**Anthropic 不行**：`tool_use` 的 `input` 只能靠 `input_json_delta.partial_json`
逐片拼出来，拼完才是完整 JSON 字符串。

聚合键：`content_block_start` 的 `index`（→ 一个 `index → (id, name, args_buf)` 表）。
收尾时（`content_block_stop` 或 `message_stop`）把每个未完成的 tool_use 转成
`ToolCall`。**按 index 聚合**（Responses 用的是 `call_id`/`output_index`，但这里
`call_id` 只在 `content_block_start` 出现一次，index 才是流内稳定键）。

**4.4 收尾**

`message_delta` 给 `stop_reason` 与最终 `output_tokens`；
`message_start` 给 `input_tokens` / `cache_read`。两者要**合并**成最终 `Usage`
（流式 usage 是分两次给的，非流式是一次给全）。收尾产出单个 `Finished(ModelResponse)`。

---

**五、落地方式**

**5.1 接入点**：`adapter::codec` 增加 `ModelProtocol::AnthropicMessages` 分支，
返回 `(encode_request, decode_response, decode_stream_response)`。
`invoke()` 无需改动（流式已纳入 codec，见 `docs/responses-api.md`）。

**5.2 新文件**：`crates/shirley-agent-sdk/src/adapter/anthropic_messages/{mod.rs, dto.rs}`。

**5.3 复用**：`adapter::sse`（分帧 + 取 `data:`）直接复用，与 Responses 共享。

**5.4 需要改的共享代码**：
- `message/mod.rs`：`Message::Assistant` 增加 `thinking_signature: Option<String>`
  （见 2.4）。**这是唯一需要动内部模型的点**，且对另两个协议零影响。
- `settings.rs`：别名已就绪（`anthropic_messages` / `anthropic` / `messages`）。

**5.5 请求头**：Anthropic 用 **`x-api-key`**（不是 `Authorization: Bearer`）
+ `anthropic-version: 2023-06-01`。这是编码层的事，不影响其它协议。

---

**六、验证清单**

1. **编码**：system / ContextSummary → 顶层 `system`，不进 `messages`。
2. **编码**：`Tool` → `role:"user"` 的 `tool_result` block（**不是** tool role）。
3. **编码**：assistant 块顺序 = thinking → text → tool_use；thinking 带 signature。
4. **编码**：工具 schema 用 `input_schema` 键名，值与 `parameters` 相同。
5. **编码**：`thinking` 对象形状；`reasoning_effort` 被忽略而非硬塞。
6. **解码**：`content[]` 三类块合成一条 assistant；`input` 对象 → `arguments` 字符串。
7. **解码**：`stop_reason` 全表映射；优先看有无 `tool_use`。
8. **解码（usage）**：`input_tokens` **加上** `cache_read`；命中率分母 = 总输入。
9. **解码（usage）**：`cache_read` 缺失 → `cached_input_tokens == None`（不是 0）。
10. **流式**：`text_delta`→ContentDelta、`thinking_delta`→ReasoningDelta、
    `signature_delta` 被记录。
11. **流式**：`input_json_delta` 按 index 聚合出完整 arguments。
12. **流式**：`message_start` + `message_delta` 两处 usage 合并；`message_stop` 收尾。
13. **流式**：**没有 `[DONE]`** 也能正确 `Finished`。
14. **契约**：ChatCompletions / Responses 路径行为完全不变（回归）。

真实网络测试（`#[ignore]`，需 `DEEPSEEK_API_KEY`）：
`live_anthropic_round_trip`（纯文本）、`live_anthropic_tool_round_trip`
（**含 thinking 回传**——这是最关键的，验证 signature 链路）、
`live_anthropic_streaming_tool_call`。

---

**七、待定项（实现前需要拍板）**

1. **`thinking_signature` 是否进 `Message`**（2.4）。建议进。
2. **`max_tokens` 默认值**（2.5）。建议 4096，可配置。
3. **`cache_creation_input_tokens` 是否计入分母**（3.3）。建议 v0 不计。
4. **tool_result 合并 vs 逐条**（2.2）。建议逐条。
5. **`reasoning_effort` 在 Anthropic 下的去处**（2.1）。建议忽略。

---

**八、与其它文档的关系**

- `docs/adapter-layer.md`：本文是其"五、多协议落地"第 2 步（Anthropic）的展开，
  也是"工具参数标准化"结论的第三次印证——三协议都吃 JSON Schema。
- `docs/responses-api.md`：同为协议适配，结构对称；两文可对照阅读。
- `docs/security.md`：`x-api-key` 头的敏感标记（`set_sensitive(true)`）同 Bearer。
