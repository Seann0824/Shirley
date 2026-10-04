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

1. **Responses**：与 ChatCompletions 同源，差异最小，先做。
2. **Anthropic Messages**：需要处理 `system` 独立字段、`tool_use` / `tool_result` 的内容块结构、`thinking` 块。差异最大，放最后。

Anthropic 的关键差异（提前记录，避免以后返工）：

- `system` 不在 `messages` 里，是顶层字段 → `encode_messages` 要把它摘出来。
- 工具调用是 content block（`tool_use`），不是独立的 `tool_calls` 字段。
- `max_tokens` 必填。
- reasoning 通过 `thinking` 块返回，与 `reasoning_content` 语义不同，需要映射。

**六、顺手清理**

- `ModelProtocol::AnyhtopicMessages` → `AnthropicMessages`（拼写）。**仍在**（`adapter/mod.rs`）。
- `ModelfinishReaon` → `ModelFinishReason`（少个 i）。**仍在**（`adapter/mod.rs` 定义，`chat_completions/mod.rs` 多处使用）。
- `use serde_json::{Value, map}` 的 `map` 未使用。**已清**（现在只剩 `use serde_json::Value;`）。

**七、验收**

1. `ToolDefinition` 不再暴露裸 OpenAI schema，而是标准模型。
2. 新增一个协议只需实现 `MessageCodec` + `encode_tool_schema`，不改 `Message` / `Tool` 定义。
3. 现有 `bash` / `read` 工具在改造后行为不变（回归测试保护）。
