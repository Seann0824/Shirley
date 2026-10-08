**Shirley 技术方案 · 工具宏的表达力（Tool Macro）**

> **状态：已实现。** `#[tool]` 函数宏现支持可选伴生钩子
> `on_register = fn_name` / `on_unregister = fn_name`，宏把钩子 wire 进生成的 `impl Tool`。
> 契约测试见 `crates/shirley-agent-sdk/tests/tool_lifecycle.rs`（SDK 侧）与
> `src/tools/web_search.rs`（应用侧）。trait 侧机制见 `docs/tool-lifecycle.md`。
>
> 文档正文保留了方案期的推理与**被否掉的备选**，**以下方"最终方案"一节的措辞为准**。

**一、这份文档要回答什么**

`#[tool]` 宏原先只能生成**无状态**工具——它产出的 `GenerateTool` 只持有 `definition`：

```rust
struct GenerateTool {
    definition: ::shirley_agent_sdk::ToolDefinition,
}
pub fn tool() -> impl ::shirley_agent_sdk::Tool + 'static {
    GenerateTool { definition: definition() }
}
```

有状态工具无法用它表达：`recall` 手写 `impl Tool`（持有 `Arc<RecallStore>`），
`web_search` 走 `ToolContext` 注入（工具本身仍无状态，但需要"注册时注入依赖"）。

用户问题：**宏该细化还是新增？有没有更好的表达方式？** 用户的硬约束是：
**"宏本质上就是让用户只关注自己的实现，而不需要去关注工具细节。"**

---

**二、先划一条线：宏只该消除「样板」**

判断标准——**每个工具都要重复写、且内容完全由签名决定的东西，才该交给宏生成**：

| 东西 | 是样板吗 | 该进宏吗 |
| --- | --- | --- |
| 参数 JSON Schema | 是（由参数类型决定，`schemars` 可推导） | ✅ 是 |
| 参数反序列化 | 是（由参数类型决定） | ✅ 是 |
| `ToolDefinition` 组装 | 是 | ✅ 是 |
| 状态字段（`client` / `store`） | **否**（每个工具不同，宏猜不出类型） | ❌ 否 |

`on_register` / `on_unregister` 介于两者之间——**它是"引用"而非"生成"**：宏不生成钩子
语义，只把用户写好的普通函数接到 `impl Tool` 上。用户写的仍是自己的实现（一个普通
`fn(&mut ToolContext) -> Result<(), ToolError>`），不碰 `Tool` trait。

---

**三、根因：宏把两件事捆在了一起**

`#[tool]` 同时做两件事：

1. **Schema 生成**：从类型化参数 → JSON Schema（+ 反序列化）。
2. **`Tool` impl 生成**：产出 `GenerateTool` 结构体 + `impl Tool`。

对无状态工具，两件事绑一起没问题。对有状态工具：

- #1 **仍然有价值**（手写 `impl Tool` 时，`ToolDefinition` 的 schema JSON 是最烦的样板——`recall` 手写了几十行 `serde_json::json!`）；
- #2 **强制无状态**（`GenerateTool` 的字段形状写死）。

---

**四、备选方案与最终选择**

**方案 A：宏完全不动，有状态工具手写 `impl Tool`**

`recall` 已是此形态。

- **优点**：宏零改动；无新概念。
- **缺点**：`definition` 的 schema JSON 要手写；**背叛"用户不碰工具细节"**——用户被迫写
  `impl Tool`、组装 `ToolDefinition`。

**方案 B：拆出 schema 生成派生宏（`#[derive(ToolArgs)]`），让手写工具复用**

新增一个**只生成 schema + 反序列化**的派生宏，不生成 `Tool` impl；有状态工具手写
`impl Tool` 但不再手写 JSON。

- **优点**：schema 样板复用；`Tool` impl 完全自由。
- **缺点**：引入一个新宏（派生宏），且**用户仍要手写 `impl Tool`**——同样背叛硬约束。
  **已被撤回。**

**方案 C：struct 级宏，一把梭**

把 `#[tool]` 升级成同时接受函数和结构体/impl，生成状态字段 + `impl Tool` + lifecycle。

- **优点**：一个宏统一所有工具。
- **缺点**：宏复杂度暴涨（解析 struct、impl、方法属性、区分 state 字段 vs 参数）。
  **未采用。**

**✅ 最终方案（已实现）：扩展函数宏 + 可选伴生钩子**

保留**一个函数宏**，给它加两个**可选**属性：

```rust
#[tool(
    description = "搜索公开互联网中的最新信息……",
    on_register = web_search_on_register,
    on_unregister = web_search_on_unregister,
)]
pub async fn web_search(
    ctx: &ToolContext,
    #[param(description = "查询语句")] query: String,
) -> Result<String, ToolError> { /* ... */ }

/// 伴生函数：普通 fn，用户写实现，不碰 Tool trait。
fn web_search_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    let state = WebSearchState::from_env()
        .map_err(ToolError::ExecutionError)?
        .ok_or_else(|| ToolError::ExecutionError("未配置 DEEPSEEK_API_KEY".into()))?;
    ctx.insert(state);
    Ok(())
}

fn web_search_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<WebSearchState>();
    Ok(())
}
```

宏在生成的 `impl Tool` 里插入覆写：

```rust
fn on_register(&mut self, ctx: &mut ::shirley_agent_sdk::ToolContext) -> Result<(), ::shirley_agent_sdk::ToolError> {
    super::web_search_on_register(ctx)
}
```

- **钩子定义仍在 `Tool` trait 上**（默认空实现），宏只"引用"它们——没有把 lifecycle 语义
  搬进宏。
- **不写钩子 = 空实现**：无状态工具（`bash` / `read_file`）**零改动**。
- **用户只写**：工具函数 + 可选的普通 `fn`；不碰 `impl Tool`、不碰 schema、不碰 trait。
- 钩子签名固定为 `fn(&mut ToolContext) -> Result<(), ToolError>`——由 `Tool` trait 定义决定，
  宏只按名引用。

---

**五、为什么不选 A / B / C**

- **A / B 都要求用户手写 `impl Tool`**，直接违反"宏是唯一入口、用户不碰工具细节"的硬约束。
- **C 的宏复杂度与当前需求严重不匹配**（当前只有 2 个有状态工具）。
- 最终方案把"接线"这件事留给宏（纯机械、无语义），把"实现"留给用户（普通函数），
  恰好落在第二节那条线上：**宏只消除样板，不预判语义。**

---

**六、函数宏的形态与分工**

| | `#[tool]`（函数，含可选钩子） |
| --- | --- |
| 输入 | 一个 async 函数 + 可选的伴生钩子函数名 |
| 产出 | 完整 `Tool` impl（`GenerateTool`） |
| 状态 | 无（可经注册钩子注入 `ToolContext`） |
| lifecycle | 由 `on_register` / `on_unregister` 属性接线；不写即空实现 |
| 适用 | 所有工具（`bash` / `read_file` / `web_search`） |

只有一个宏。`recall` 因需持有 `Arc<RecallStore>` 且属 SDK 内部，仍手写 `impl Tool`——
它是唯一例外，不是范式。

---

**七、钩子的执行语义（与 trait 侧一致）**

- `on_register`：`ToolManager::register` 查重通过后、插入前调用；返回 `Err` 即注册失败、
  工具不入表（不留半注册状态）。
- `on_unregister`：`ToolManager::unregister` 先移出表、再回调；**best-effort**，`Err` 只告知
  失败，工具不复活。未注册的名字返回 `ToolError::NotFoundError`。
- 详见 `docs/tool-lifecycle.md` 第四、五节。

---

**八、改动面**

| 项 | 影响 | 破坏性 |
| --- | --- | --- |
| `#[tool]`（函数宏） | 加**可选**属性 `on_register` / `on_unregister` | 否 |
| `macros` crate | `ToolConfig` 增两个 `Option<Ident>`；生成可选覆写 | 否 |
| `Tool` trait | 两个钩子为默认空方法 | 否 |
| `ToolManager` | 持有 `ToolContext` + `unregister` | 否 |
| `web_search` | 迁到 `#[tool]` + 注册钩子 | 否 |
| `recall` | 不变（唯一手写 `impl Tool` 的内部工具） | 否 |
| 文档 | 本文件 + `docs/tool-lifecycle.md` + `docs/web-search.md` + `Agent.md` | 否 |

---

**九、已解决的决策点**

1. **lifecycle 是否进宏**：~~不进~~ → **进，但以"引用"方式**——宏只接线用户写的普通函数，
   语义仍在 `Tool` trait。这是"宏是唯一入口"与"宏不预判语义"两条约束的交点。
2. **是否需要派生宏**：**否**。撤回 `#[derive(ToolArgs)]` 方案——它要求用户手写 `impl Tool`。
3. **`#[tool]` 生成 `mod #name`、函数须与 mod 同层**：**未改**，本次不处理（既有约束）。

---

**十、与既有文档的关系**

- 本文是 `docs/tool-lifecycle.md` 的**宏侧配套**：那边定义 lifecycle 钩子（trait 侧），这边定义
  宏如何把用户写的伴生函数接到钩子上。
- 边界判定遵循 `Agent.md` 第八节第 4 条：schema 生成 / 钩子接线是**通用机制**（SDK/宏），
  具体工具的参数与状态是**应用层**。
