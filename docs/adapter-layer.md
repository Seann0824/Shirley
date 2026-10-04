**Shirley 技术方案 · 适配中间层（P1）**

**一、现状与两处设计债**

`plan.md` 自己点出了两个问题，都还在：

**债一：`encode_messages` 的位置**

`chat_completions/mod.rs`（`encode_messages` 定义上一行）的注释：

> 说实话我感觉这个应该是个 message 侧做的转换逻辑，而不是我们写在适配层对于这个消息处理。

现在"内部 Message → 协议格式"的映射写在适配层。每加一个协议就要复制一遍这个映射，且各协议的实现容易漂移。

**债二：工具参数没有中间层**

`#[tool]` 宏直接生成 OpenAI 格式的 JSON Schema：

```rust
parameters: ::serde_json::json!(::schemars::schema_for!(Arguments)),
```

而 `ToolDefinition.parameters` 就是个裸 `serde_json::Value`。所以参数结构绑死在 OpenAI 上，换成 Anthropic 的 `input_schema` 无法复用。

**二、目标**

引入一个"协议无关的中间表示"，各协议只负责双向转换。判断标准：**新增一个协议时，只写一份转换代码，不改 Message 和 Tool 的定义。**

**三、消息侧：统一内部表示**

`Message` 枚举本身已经是一个不错的中间表示，问题只在于"编码成协议格式"这一步放错了位置。方案：

```rust
// crates/shirley-agent-sdk/src/message/codec.rs（新增）
pub trait MessageCodec {
    type Wire;
    fn encode(&self, message: &Message) -> Result<Self::Wire, CodecError>;
    fn decode(&self, wire: Self::Wire) -> Result<Message, CodecError>;
}
```

`chat_completions` 实现 `MessageCodec`，`encode_messages` 从适配层的自由函数变成它的一个方法。适配层只做"拿到 `Message` → 调用 codec → 发请求"。

这样 `chat_completions/mod.rs` 瘦身成纯协议细节（URL、header、body 组装、SSE 解析），消息映射归位。

**四、工具参数：标准化中间模型**

这是更关键的一层。核心是把 `#[tool]` 生成的 schema 从"OpenAI JSON Schema"降级为"标准参数模型"，再由各协议转换。

**4.1 中间模型**

```rust
// crates/shirley-agent-sdk/src/tool/schema.rs（新增）
#[derive(Debug, Clone)]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Array { items: Box<ParamType> },
    Object { fields: Vec<ParamField> },
    Optional(Box<ParamType>),
}

pub struct ParamField {
    pub name: String,
    pub description: String,
    pub ty: ParamType,
    pub required: bool,
    pub default: Option<serde_json::Value>,
}
```

**4.2 宏侧改动**

`#[tool]` 宏生成 `ParamType` 而不是直接生成 schema。这一步可以渐进：先让宏同时产出两者（`schemars` schema 保留用于 OpenAI，`ParamType` 用于其他协议），确认无误后再切主路径。

类型映射规则：

| Rust 类型 | ParamType |
| --- | --- |
| `String` | `String` |
| `i32` / `i64` / `u64` | `Integer` |
| `f64` | `Number` |
| `bool` | `Boolean` |
| `Option<T>` | `Optional(T)`，且 `required = false` |
| `Vec<T>` | `Array { items: T }` |
| `struct`（派生 `JsonSchema`） | `Object { fields }` |

注意 `Option<T>` 的语义：`#[tool]` 现在用 `Option<u64>` 表示"参数可省略"，宏需要把 `Option` 识别成 `required = false` 而不是生成 `nullable`。

**4.3 协议转换**

```rust
// chat_completions
fn encode_tool_schema(field: &ParamField) -> Value { /* -> JSON Schema */ }

// anthropic
fn encode_tool_schema(field: &ParamField) -> Value { /* -> input_schema */ }
```

**五、多协议落地**

`codec` 的 `todo!()` 修复见 `runtime-hardening.md`。落地顺序建议：

1. **Responses**：与 ChatCompletions 同源，差异最小，先做。**已完成**（`adapter/responses/`，含流式；方案见 `docs/responses-api.md`）。
   **详细方案见 `docs/responses-api.md`**——它把本节的"工具参数标准化"结论推进了一步：
   Responses 的 `tools[].parameters` 与 ChatCompletions 的
   `tools[].function.parameters` **都接受 JSON Schema**，只差一层包装。这印证了
   JSON Schema 本身就是跨协议中间表示，不必另造 `ParamType`；真正的差异在**消息
   item 结构**与**流式事件语义**上。
2. **Anthropic Messages**：需要处理 `system` 独立字段、`tool_use` / `tool_result` 的内容块结构、`thinking` 块。差异最大，放最后。
   **已实现**（`docs/anthropic-messages-api.md`，基于真实抓包）。三条最要命的差异：
   工具结果在 **user 消息**里（Anthropic 无 tool role）、`input_tokens` **不含缓存**
   （与 OpenAI 相反，映射时要加回 `cache_read`）、流式工具参数靠 **`input_json_delta`
   分片聚合**（与 Responses 的"终态自带完整"相反）。**协议差异全部收敛在适配层，
   `Message` 未改动**——分层的目的正在于此。

**六、顺手清理**

- `ModelProtocol::AnyhtopicMessages` → `AnthropicMessages`（拼写）。**已修**（`adapter/mod.rs`，同步 `settings.rs` 别名与 `error_contract.rs`）。
- `ModelfinishReaon` → `ModelFinishReason`（少个 i）。**已修**（现为 `ModelFinishReason`）。
- `use serde_json::{Value, map}` 的 `map` 未使用。**已清**（现在只剩 `use serde_json::Value;`）。

**七、验收**

1. `ToolDefinition.parameters` 的语义明确为"协议无关的 JSON Schema"（它本就是标准
   JSON Schema，不是 OpenAI 私有），协议差异收敛在 `encode_tool_schema` 一处。
2. 新增一个协议只需实现 `MessageCodec` + `encode_tool_schema`，不改 `Message` / `Tool` 定义。
3. 现有 `bash` / `read` 工具在改造后行为不变（回归测试保护）。
