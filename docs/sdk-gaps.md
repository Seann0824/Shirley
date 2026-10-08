**Shirley 技术方案 · SDK 能力缺口修复（P0）**

这份文档处理三条来自使用方反馈的 SDK 问题。三条都经代码核对，共同点是：**SDK 替应用做了本该由应用决定的事**——决定请求体形状、决定工具有没有状态、决定工具顺序。

结论：**gap-1（工具上下文）与 gap-4（请求体留口）已落地**；**gap-2（工具顺序保序）决定不做**（见第一节的"为什么不改"）。已落地的两条为破坏性变更，发 `0.0.2`（`Cargo.toml` × 3 同步，SDK 依赖 macros 的 `version` 一并改）。

| 缺口 | 定性 | 破坏性 | 落地顺序 |
| --- | --- | --- | --- |
| gap-2 `definitions()` 强制按名排序 | 设计取向（非缺陷） | —— | **决定不做** |
| gap-4 请求体写死、字段未接线 | 真缺陷，含一个 400 bug | 行为变更 | 2（**已落地**） |
| gap-1 工具拿不到运行时上下文 | 真缺陷，改动面最大 | 签名变更 | 3（**已落地**） |

---

**一、gap-2：`definitions()` 保序（决定不做）**

> **结论：不做。** 下面保留原始分析，但结论调整为"维持现状（按名排序）"。
> 原因见 1.6：这不是缺陷，是把一个影响很小的决策权交给应用的**取向**问题，
> 当前没有功能依赖它，改动的收益（迁移可复刻 + 模型 steering）不足以支撑
> 一次破坏性的 prefix 变更。

**1.1 现状**

`tool/mod.rs`：

```rust
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
}

pub fn definitions(&self) -> Vec<&ToolDefinition> {
    let mut definitions = self.tools.values().map(|t| t.definition()).collect();
    definitions.sort_by(|a, b| a.name.cmp(&b.name));   // 越俎代庖
    definitions
}
```

底层 `HashMap` 迭代本就无序，再按名排序 = **双重丢失应用意图**。

**1.2 为什么是缺陷**

原注释的理由是"排序为了 stable prompt prefix"。但**稳定只要求"每次一样"，不要求"按字母"**。注册顺序本身也是确定的（`main.rs` 是一串固定调用），保序同样满足 prefix 稳定，且把顺序决定权还给应用。

应用可能需要按优先级、按阶段（先导航工具后写入工具）排列。SDK 按字母排会破坏应用刻意设计的顺序，导致 prompt 前缀变化、缓存命中率骤降。

**1.3 方案**

底层补一条插入序，`definitions()` 按它取：

```rust
pub struct ToolManager {
    tools: HashMap<ToolName, Box<dyn Tool>>,
    order: Vec<ToolName>,          // 新增：注册顺序
}
```

- `register()` 成功时 `self.order.push(tool_name.into())`（查重已在前面，不会重复）。
- `definitions()` 改为按 `order` 取，不再排序。

不引入 `IndexMap` 依赖，保持 O(1) 查找。

**1.4 语义与影响**

- 稳定 = 每次一样；注册顺序确定 ⇒ prefix 仍稳定。
- 一次性 cache miss（顺序变了），之后稳定——正是应用预期。
- `definitions()` 返回类型不变，**调用方无感**。

**1.5 验收**

单测：注册 `c, a, b` → `definitions()` 名字序列必须是 `["c","a","b"]`，不得是字母序。

---

**1.6 为什么不改（最终决策）**

最初把这条定性为"真缺陷"，是**抬高了**。复盘如下：

- **现状不是 bug。** `sort_by(name)` 出来的是**确定**的字母序，每次请求都一样，prompt 前缀**本来就稳定**。评审说的"改成字母序 → prefix 变 → 线上掉缓存"这个因果**不成立**——保序同样稳定，两条路都不会持续掉缓存，差别只是切换那一次 miss。
- **保序站得住的理由只有两条**：① 迁移期复刻旧系统的既定前缀（一次性价值）；② 工具在数组里的位置对模型选择有**微弱** steering 作用。都不构成硬需求。
- **当前没有任何功能依赖顺序**，诉求本质是偏好。
- **改动并非零成本**：底层是 `HashMap`（迭代无序），保序必须额外存一份插入序（`order: Vec<ToolName>` 或换 `IndexMap`），且会**改变 prompt 前缀**（一次 breaking 的行为变更）。

结论：**维持按名排序**。若将来出现"必须控制工具顺序"的真实需求（例如多阶段工具集按阶段排序能显著提升选择准确率），再按 1.3 的方案做——届时它是**有依据的功能**，而不是现在的偏好。

---

**二、gap-4：请求体留口 + 接线未生效字段（已落地）**

> 落地实现：`ModelConfig` 新增 `tool_choice` / `extra_body`；`encode_request` 改为
> 仅在 `Some` 时写入 `temperature` / `max_tokens` / `reasoning_effort` / `tool_choice`，
> `thinking` 仅在开启时写 `{"type":"enabled"}`；最后 `apply_extra_body` 浅合并
> （`null` 删除键）。单测 `chat_completions::tests`（5 个）。
> 注意线上键名是 `max_tokens`（ChatCompletions），非 `max_output_tokens`。
> 下面保留原始设计推导。

**2.1 现状**

`chat_completions/mod.rs::encode_request`：

```rust
let thinking = match config.thinking { true => "enabled", false => "disabled" };

let body = serde_json::json!({
    "model": &config.model,
    "messages": encode_messages(input.messages),
    "tools": encode_tools(input.tools),
    "thinking": { "type": thinking },
    "reasoning_effort": &config.reasoning_effort,
    "stream": &config.stream,
});
```

`temperature` / `max_output_tokens` 已定义未进 body；`tool_choice` 完全没暴露。

**2.2 三处问题分级（纠正一处因果）**

| 项 | 定性 |
| --- | --- |
| `thinking` 硬编码 `{"type":"enabled"}` | **真问题，400 的根源**——这是某厂商私有约定，不是 OpenAI 标准。OpenAI 只用 `reasoning_effort`；严格校验端点会因该字段 400 |
| `tool_choice` 缺失 | **功能缺失，非 400**——OpenAI 中它可选，不传不报错；缺的是"强制/禁用工具调用"的能力 |
| `temperature` / `max_output_tokens` 未接线 | 已定义未使用 |

同时塞 `thinking` + `reasoning_effort` 是"广撒网"，在严格端点上不可靠。

**2.3 方案：加一处 escape hatch，而不是加 N 个厂商分支**

厂商私有字段（`enable_thinking` / `thinking_budget` / `{type:"enabled"}`…）差万别，逐个加分支是无底洞。改为**给应用一个覆盖请求体的口子**。

`ModelConfig` 新增：

```rust
#[builder(default)] pub tool_choice: Option<serde_json::Value>,
#[builder(into)]  pub extra_body: Option<serde_json::Value>,
```

`Value` 而非枚举：`tool_choice` 可为 `"auto"` / `"none"` / `"required"` 或对象，枚举会反复破 API。

`encode_request` 构造顺序：

1. 构造标准字段：`model` / `messages` / `tools` / `stream`；
2. `temperature` / `max_output_tokens` / `tool_choice` **仅在 `Some` 时**插入；
3. `thinking`：**仅当 `config.thinking == true`** 时输出 `{"type":"enabled"}`；`false` 时**完全省略**（现在会发 `disabled`，多数端点不接受该形状）；
4. 最后 `apply_extra_body(&mut body, extra)`：**浅合并**，应用键覆盖标准键；**值为 `null` 表示删除该键**——这样应用能彻底移除 SDK 默认的 `thinking`，换成自己的形状。

合并函数独立成 `fn apply_extra_body(body: &mut Value, extra: &Value)`，单测覆盖三种情形。

**2.4 语义与影响**

- 默认路径行为尽量不变（除 `thinking:false` 不再发 `disabled`）。
- 厂商私有字段一律走 `extra_body`，SDK 不做厂商判断。
- `ModelConfig` 加字段是 builder 兼容变更，调用方无感。

**2.5 验收**

单测：

1. `temperature` / `max_output_tokens` / `tool_choice` 为 `Some` 时出现在 body；
2. `extra_body` 覆盖 `reasoning_effort`；
3. `extra_body` 里 `"thinking": null` 删除该键；
4. `thinking=false` 时 body 无 `thinking` 键。

---

**三、gap-1：工具运行时上下文注入（已落地）**

> 落地实现：`ToolContext`（`tool/mod.rs`）+ `Tool::invoke` 新增 `ctx` 参数 +
> 宏识别 `ToolContext` / `&ToolContext` 参数（不进入 schema，按原位置传给原函数）。
> 回归测试 `tests/tool_context.rs`（5 个）。
>
> **后续演进**：原先的 `Agent` 的 `.tool_context()` builder **已被移除**——`ToolContext`
> 的所有权移入 `ToolManager`，并新增注册钩子 `on_register` / `on_unregister`（注入与
> 清理），见 `docs/tool-lifecycle.md`、`docs/tool-macro.md`。下面保留原始设计推导，
> 其中"方案 A"即最终采用形态；**注入途径以 lifecycle 文档为准**。

**3.1 现状**

```rust
fn invoke(&self, input: serde_json::Value) -> ToolFuture<'_>;   // 无上下文
```

宏展开（`macros/src/tool.rs`）只做 `反序列化参数 → super::#name(args).await`，把函数编译成**纯函数**。工具拿不到 session、状态或任何外部句柄。

有状态工具（`RecallTool` 持 `Arc<RecallStore>`、应用手工 `impl Tool + Arc<Mutex<Session>>`）**只能手写，用不了宏**。

**3.2 为什么框架层该管**

ReAct agent 的工具天然有状态——工具要读写会话状态。任何有状态 agent 都会遇到，不是某个应用的特殊业务。

**3.3 关键约束（决定形态）**

工具是**并发执行**的（`runtime/agent.rs` 用 `FuturesUnordered`）。所以上下文**不可能**是 `&mut Session`——多工具并发要 `&mut` 同一会话必然冲突。**唯一可行形态是内部可变性句柄**（`Arc<Mutex<Session>>`）。应用手工绕过用的正是这个形态，是对的。

**3.4 方案 A（推荐）：类型擦除上下文**

新增公开类型：

```rust
#[derive(Clone, Default)]
pub struct ToolContext {
    inner: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ToolContext {
    pub fn new() -> Self;
    pub fn with<T: Any + Send + Sync>(self, value: T) -> Self;   // 存入 Arc::new(value)
    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>>;   // Arc::downcast
}
```

签名变更（`Tool` trait）：

```rust
fn invoke(&self, input: Value, ctx: ToolContext) -> ToolFuture<'_>;   // 新增 ctx，owned
```

`ToolContext` 内部是 `Arc<map>`，clone = 引用计数 +1，per-call 开销可忽略；owned 传入避免生命周期纠缠。

`ToolManager::invoke` 透传 `ctx`；`Agent` 持有 `ToolContext`（builder 新增 `.tool_context(ToolContext)`），runtime 调用时 `self.context.clone()`。

宏识别**类型末段为 `ToolContext`** 的参数为上下文参数——不纳入 `Arguments` struct / schema，展开时把 `ctx` 传给原函数：

```rust
#[tool(description = "...")]
async fn save_section(
    ctx: ToolContext,
    #[param(description = "...")] input: SaveInput,
) -> Result<String, ToolError> { ... }
```

（`ToolContext` 或 `shirley_agent_sdk::ToolContext` 均可；一个函数最多一个 ctx 参数。）

应用侧：

```rust
let ctx = ToolContext::new().with(Mutex::new(my_session));
Agent::builder().tool_context(ctx)...
```

工具内：

```rust
let session = ctx.get::<Mutex<Session>>().expect("session not provided");
```

`RecallTool` 等手写实现只需加一个忽略的 `_ctx` 参数。

**3.5 方案 B（备选）：泛型 `Agent<T>` / `ToolManager<T>`**

`ctx: &T` 类型安全，但泛型参数**污染整条链**（`Agent` / `ToolManager` / `Tool` / runtime 全要带 `<T>`），并**杀死 `dyn Tool`**（当前 `ToolManager` 靠它擦除存储）。与"对外契约保持小"冲突。

**3.6 取舍**

**选 A。** 它保留 `dyn Tool`、改动面可控；代价是 `get::<T>()` 为运行期 downcast，取不到需显式处理。这比 B 的传染代价小得多。

**3.7 验收**

集成测试：注册一个从 ctx 取 `Arc<Mutex<Session>>` 的宏工具，**并发**调用两次，断言状态累加正确（同时验证"并发执行"与"状态可用"两个前提）。

---

**四、变更清单与破坏性**

| 变更 | 破坏性 | 影响的调用方 |
| --- | --- | --- |
| `ToolManager` 加 `order` 字段 | 否 | 无 |
| `definitions()` 返回顺序变 | 行为 | prefix 一次性 miss |
| `ModelConfig` 加 `tool_choice` / `extra_body` | 加字段（builder 兼容） | 无 |
| `encode_request` 输出变 | 行为 | 端点请求体 |
| `Tool::invoke` 加 `ctx` 参数 | **是** | 所有手写 `impl Tool`（含 `RecallTool`、应用手工工具） |
| `ToolManager::invoke` 加 `ctx` | **是** | runtime、测试 |
| 新增 `ToolContext` | 否（新增） | 无 |
| ~~`Agent` builder 加 `.tool_context()`~~ | —— | **已被移除**，改由 `ToolManager` 持有 `ToolContext`（见 `docs/tool-lifecycle.md`） |
| 宏支持 ctx 参数 | 否（向后兼容） | 无 |

→ 发 **`0.0.2`**。

---

**五、落地状态**

| 缺口 | 状态 | 落点 |
| --- | --- | --- |
| gap-1 工具上下文 | **已落地** | `ToolContext` + `Tool::invoke` 加 `ctx` + 宏识别 ctx 参数；测试 `tests/tool_context.rs`（5）。所有权后移入 `ToolManager`（`docs/tool-lifecycle.md`） |
| gap-4 请求体留口 | **已落地** | `ModelConfig::tool_choice` / `extra_body` + `encode_request` 重构；单测 `chat_completions::tests`（5） |
| gap-2 工具顺序保序 | **不做** | 见 1.6 |

已同步更新：`Agent.md`（工具系统、`#[tool]` 宏、已知缺口）、`docs/README.md`（索引）。

---

**六、版本**

gap-1 与 gap-4 均为破坏性变更（`Tool::invoke` 签名、`encode_request` 输出行为），
发 **`0.0.2`**（`Cargo.toml` × 3 同步）。
