**Shirley 技术方案 · 任务账本（todo）**

这份文档把"让模型维护自己的任务状态"落地成方案。它是 `compaction.md`
的配套能力：压缩解决了"上下文装不下"，而 todo 解决的是
**"压缩后模型忘了自己做到哪、于是重新探索"**这个正反馈回路。

**范围声明**：本轮只做内存实现，持久化层留空。

**归属修正（后续决策）**：账本**已从 SDK 移到应用层**（`src/todo.rs`）。
"记什么、怎么催、注入什么文案"是 coding agent 这个产品的取舍，不是通用基础
能力。SDK 只保留一个与业务无关的通用接缝——[`ContextProvider`]：`Agent` 组装
每轮请求时调它，把返回文本作为一条 system **追加在末尾**。账本只是该接缝的
一个应用层实现（`TodoContextProvider`）。

---

**一、要解决什么**

压缩是**有损**的（`compaction.md` 引用 Tirmazi et al. 的信息论结论：精确信息在摘要中
必然丢失）。丢的不只是"用户说过的话"，还有**模型自己的进度状态**：

- 我这一轮的目标是什么、拆成了哪几步、哪几步做完了；
- 我已经验证过什么事实（读过哪个文件、跑过什么命令、得到什么结论）；
- 还有哪些问题悬而未决。

这些信息原本散落在 assistant 正文、reasoning、tool 调用里。压缩把工具输出清空、
把对话压成摘要后，模型看不到"我已经做过 X 了"，于是：

```
压缩 → 忘了做过 X → 重新探索 X → 上下文又涨 → 再压缩 → ……
```

账本换一条路：把"进度状态"从对话里**抽出来、单独持有**，
每轮无条件注入，模型不需要"想起来去查"，它一直在眼前。

---

**二、三个核心决策（先定调）**

**决策 1：账本由模型写，不做程序化推断。**

"任务拆成了哪几步""什么算已完成的结论"没有通用判据，写死程序只会猜错。所以账本是
**模型自己维护**的：SDK 暴露一个 `todo` 工具，模型自己决定何时更新——**状态判断发生在 AI 层，比任何程序规则准**。

**决策 2：账本不进 `self.messages`，每轮作为 system 追加在末尾注入。**

- **不进工作集**：`self.messages` 是压缩重建的对象，账本放进去就会被压掉——那正是要避免的。
  账本单独持有，压缩碰不到它，天然跨压缩存活。
- **注入位置选末尾**：`active_messages()` 在组装请求时把账本渲染成一条 `System` 消息
  **追加在最后**。理由：
  - 追加在尾部**不动前面的前缀**——账本内容稳定时，KV 前缀缓存照常命中
    （`responses-api.md` 里"system 原位保留、不上提到 `instructions`"就是为了这个）；
  - 账本只在模型调用 `todo` 时变化，届时前缀才失效。这是"必须每轮可见"的固有代价，
    无法避免，只能把变化点压到最小。
  - 位置在末尾也符合"最近可见"：模型读到最新的任务状态。

> **注意**：账本注入的是一条 **`System`** 消息，不是 `ContextSummary`。它不参与
> `active_messages()` 的"最后一条 summary 之后"裁剪——因为它是函数**末尾追加**的，
> 不来自 `self.messages`。三个适配器（ChatCompletions / Responses / Anthropic）都已能
> 处理任意位置的 system：ChatCompletions 原样输出，Responses 原位保留为 system item，
> Anthropic 会把它摘到顶层 `system`（Anthropic 协议本身没有 mid-list system）。

**决策 3：补丁语义，只动显式提供的字段。**

`todo` 工具的入参是**补丁**，不是全量覆盖：

| 字段 | 语义 | 理由 |
| --- | --- | --- |
| `goal` | 设置 / 替换目标 | 目标通常稳定，偶尔改 |
| `steps` | **整体替换**清单 | 清单短，每轮重发全量，模型天然自纠（漏了哪步下一轮补） |
| `add_findings` | **追加**结论 | "我又发现了 X"更自然，追加不会丢历史 |
| `add_open_questions` | **追加**问题 | 同上 |
| `clear` | 重置整个账本 | 任务切换时清场 |

未提供的字段保持原样。这样模型可以只更新变化的部分（"这一步做完了"= 重发 `steps`），
不必每轮重发整个账本。

---

**三、数据结构与渲染**

**3.1 `TodoStore`**

```rust
pub struct TodoStore { inner: Mutex<TaskState> }

struct TaskState {
    goal: Option<String>,
    steps: Vec<Step>,          // Step { text, done }
    findings: Vec<String>,
    open_questions: Vec<String>,
}
```

`Arc` 共享（runtime 与 `todo` 工具各持一份 `Arc`），内部可变性走
`Mutex`，`&self` 即可写（`apply` / `clear` / `render`）。持久化留空。

**3.2 渲染成 XML**

`render()` 把账本拼成 XML 块（`plan.md`：标签形式提升模型对结构信息的注意力）：

```xml
<task_state>
<goal>把 TS 后端迁移到 Rust</goal>
<steps>
- [x] 梳理现有路由
- [ ] 迁移用户模块
</steps>
<findings>
- 鉴权逻辑在 src/auth.rs，用 JWT
</findings>
<open_questions>
- 数据库连接池用哪个 crate？
</open_questions>
</task_state>
```

要点：

- **空账本不注入**（`render()` 返回 `None`）——避免每轮多一条无意义的 system。
- **XML 转义**：`&` / `<` / `>` 转义，防止模型写入的内容破坏标签结构。
- **字符上限** `MAX_RENDER_CHARS = 4000`：超限截断并显式标注
  `... (task ledger truncated; keep it concise)`——账本自己也会占上下文，防它垄断。
- 注入时前面加一行 `TASK_STATE_HEADER` 说明（"这是你的任务账本，跨压缩存活，用 `todo`
  工具更新，别重复已完成的工作"）——与 `render()` 分开，工具结果里不需要重复这段说明。

**3.3 `todo` 工具**

手写 `impl Tool`：状态 `Arc<TodoStore>` 由工具自己持有，
构造时绑定，不走 `ToolContext` 注入。`invoke` 反序列化 `TodoUpdate` → `store.apply()`
→ 返回**更新后的完整账本**（让模型确认更新生效、据此续接）。参数 schema 手写 JSON Schema。

---

**四、与压缩的关系**

两者职责不重叠：

| 能力 | 记什么 | 谁触发 | 恢复方式 |
| --- | --- | --- | --- |
| compaction | 对话摘要 | 自动（80% 阈值） | 摘要本身 |
| **todo** | **任务进度状态（目标 / 步骤 / 结论 / 待决）** | **AI 主动更新** | **每轮无条件注入** |

- **账本不是"再摘要一遍"**：摘要是有损的（那是二次损失），而账本里写的是模型**自己
  确认过的事实**，是"提炼"不是"压缩"。
- **账本不入 `self.messages`**：它单独持有，跨压缩存活。
- **切换会话天然隔离**：多会话落地后每个会话自持独立 `Agent`（`SessionManager`），
  账本随 `Agent` 一并搁置——旧会话的任务状态不会残留到新会话。
  （单会话时代曾有 `Agent::switch_session` 里的显式 `todo.clear()`，该接缝已移除。）

---

**五、生命周期**

- **注册**：`Agent::new` 里 `tools.register(TodoTool::new(todo.clone()))`。
  应用层 `main.rs` 一行不用改。
- **注入**：`active_messages()` 末尾追加（见决策 2）。
- **清空**：随会话切换（每会话独立 `Agent`）天然隔离；无跨会话残留。
- **持久化**：留空（进程结束即失）。若将来要做持久化，
  `TodoStore` 的接口（`apply` / `render` / `clear`）已足够挂载后端。

---

**六、实现位置**

| 文件 | 职责 |
| --- | --- |
| `src/todo.rs`（**应用层**） | `TodoStore` / `TodoStatus` / `TodoUpdate` / `TodoStep` / `TodoTool` / `TodoContextProvider` / `TASK_STATE_HEADER` / `TODO_NAG_REMINDER` / 单测 |
| `src/bootstrap.rs`（应用层） | `build_agent` 每个 `Agent` 新建一份 `Arc<TodoStore>`，注册 `TodoTool` 并作为 `context_provider` 注入 `TodoContextProvider`（两者共享同一 `Arc`） |
| `crates/shirley-agent-sdk/src/runtime/context.rs`（SDK） | 通用接缝 `ContextProvider`（`context() -> Option<String>`）；`agent.rs` 在 `active_messages()` 末尾调用它 |
| `crates/shirley-agent-sdk/src/runtime/agent.rs`（SDK） | 持有 `Option<Arc<dyn ContextProvider>>`、`active_messages()` 末尾注入返回文本 |

**不再对外导出**：`TodoStore` / `TodoTool` 等已随模块移出 SDK（`lib.rs` 不再 re-export）。
SDK 只导出通用接缝 `ContextProvider`。

---

**七、已知缺口**

1. **无持久化**：进程结束即失。恢复会话时账本不重建——冷启动的
   会话没有任务状态，模型需要重新 `todo` 一遍（若有会话日志，未来可从最后一段重建）。
2. **无自动清理**：账本只在模型调 `todo {clear: true}` 或切换会话时清空。长任务里
   若模型忘记清理，账本会一直增长（`MAX_RENDER_CHARS` 兜底截断）。
3. **注入即失前缀缓存**：模型每次调 `todo` 都会让"账本那条 system 之后的"前缀失效。
   这是"每轮可见"的固有代价，目前用"账本放末尾 + 只写变化"把影响压到最小。
4. **模型可能不用**：是否更新账本完全由模型决定。若模型不调用 `todo`，账本为空、
   不注入，行为退回现状（不劣化）。
