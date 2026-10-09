**Shirley —— 项目 Agent 指南**

这份文件写给下一个接手这个仓库的 Agent（或者人）。我尽量写得像一份能直接照着干活的手册，而不是一份宣传材料。如果你只想知道"我该从哪里开始改代码"，请直接跳到 **上手路径** 那一节。

> 这份文件描述的是**当前代码的真实状态**。如果你改了架构，请顺手把这里对齐——上一版就因为落后十几个 commit 而误导过人。

---

**一、这是什么**

Shirley 是一个用 Rust 写的 Coding Agent。名字来自《Code Geass》里的夏莉，`src/prompt.rs` 里的 `ROLE` 常量（system prompt）就是按她的设定写的——这既是人格测试用例，也是真实的默认人设。

整个仓库分成三层：

| 层 | 位置 | 职责 |
| --- | --- | --- |
| 应用层 | `src/` | 一个可跑的 TUI 聊天 Agent，注册工具、驱动界面 |
| SDK 层 | `crates/shirley-agent-sdk/` | 与业务无关的 Agent 基础能力：消息、工具、协议适配、ReAct 运行时、沙盒、工作区 |
| 宏层 | `crates/shirley-agent-sdk-macros/` | `#[tool]` 属性宏，把普通 async 函数变成可被模型调用的工具 |

核心设计目标写在根目录的 `plan.md` 里：SDK 只沉淀"下次做 Agent 还会用到"的能力，不掺业务。当前聚焦的场景是"Coding Agent"，也就是拿它去做 TS 后端往 Rust 迁移这类长任务。

**`docs/` 是一组独立的技术方案文档**（和根目录的 `plan.md` 不是一回事）：

| 文档 | 解决什么 |
| --- | --- |
| `docs/README.md` | 文档索引 + 三条全局设计原则 |
| `docs/architecture.md` | 架构图 + 每个功能落在哪些节点 |
| `docs/roadmap.md` | 总路线图：分期、依赖关系、验收口径 |
| `docs/security.md` | 权限层、bash 边界、密钥、工作区隔离 |
| `docs/sandbox.md` | 进程沙盒：spec / 后端 / degraded / 超时 |
| `docs/runtime-hardening.md` | max_steps、重试、超时、取消、压缩健壮性 |
| `docs/streaming.md` | 流式输出与 reasoning 流式 |
| `docs/adapter-layer.md` | 协议适配中间层、工具参数标准化、多协议 |
| `docs/responses-api.md` | Responses 协议适配：请求 item 展开 / 响应解码 / 流式事件（**已实现**） |
| `docs/anthropic-messages-api.md` | Anthropic Messages 协议适配：content block / 流式分片聚合 / usage 语义（**已实现**） |
| `docs/todo.md` | 任务账本 `todo`：模型自维护、跨压缩存活的任务状态 / 每轮末尾注入（**已实现**） |
| `docs/testing.md` | SDK 单测策略、缓存命中率基准 |
| `docs/plan.md` | 错误处理统一化：已完成状态 + 后续任务清单 |
| `docs/tool-lifecycle.md` | 工具生命周期：`ToolContext` 归 `ToolManager` / `on_register`（created）/ `on_unregister`（destroy）/ `unregister`（**已实现**） |
| `docs/tool-macro.md` | 工具宏表达力：函数宏扩展可选伴生钩子，lifecycle 由宏接线（**已实现**） |
| `docs/desktop-interface.md` | 桌面界面：Tauri 2 + React 迁移 shiwen 聊天 UI / 共享 LCA（`AgentEvent`）/ 剥离 shiwen 定制逻辑（**M0–M3 已落地**，含 `@` 引用重绑工作区文件、markdown 渲染对齐、聊天区固定宽度 `max-w-190`、连续工具调用收集成折叠区） |

---

**二、整体架构**

数据流向大致是这样：

```
用户输入
  → Agent::run_stream(task)
      → 组装 active_messages（必要时先压缩上下文）
      → adapter::invoke 返回 AdapterEvent 流
          → 流式：ReasoningDelta / ContentDelta 增量
          → 结束：Finished(ModelResponse)
      → 落库 Assistant 消息
      → 若有 tool_calls，并发执行工具（沙盒）
      → 落库 Tool 消息
      → 回到模型，直到没有工具调用
  → 产出 AgentEvent 流（UI 消费）
  → 结束时给出 RunResult
```

**SDK 对外只暴露这些**（见 `crates/shirley-agent-sdk/src/lib.rs`）：

- `Agent`、`AgentError`、`AgentEvent`
- `SystemPrompt`、`SystemPromptContext`（系统提示词：静态字符串或函数）
- `CutPlan`、`CompactParts`、`plan_cut`（压缩切点/重建，供测试与上层观测）
- `Message`、`ToolCall`、`Usage`
- `ModelConfig`、`ModelProtocol`、`AdapterError`
- `ErrorKind`、`SdkError`（统一错误契约）
- `tool`（宏）、`ToolManager`、`Tool`、`ToolDefinition`、`ToolError`、`ToolContext`
- `sandbox`（`Sandbox` / `SandboxSpec` / `SandboxOutput` / `SandboxBackend` / `ProcessBackend` / `SandboxError` / `NetworkPolicy` / `Capabilities`）
- `workspace`（`WorkSpace` / `WorkspaceError`）
- `recall`（`Retriever` / `RecallStore` / `RecallTool` / `Chunk` / `ScoredChunk` / `chunk_messages`）——压缩的配套召回库
- `todo`（`TodoStore` / `TodoTool` / `TodoUpdate` / `TodoStep` / `TASK_STATE_HEADER`）——压缩的配套任务账本
- `session`（`SessionStore` / `SessionError` / `InMemoryStore`）——会话持久化契约
- `token`（`TokenCounter` / `HeuristicCounter` / `count_text` / `count_message` / `count_messages`）——token 记账

真正的私有模块只有 `message` / `adapter` / `runtime`（以及 `tool` 的内部实现）——它们不直接 `pub mod`，只通过上面的 `pub use` re-export 必要类型。改动这些模块时要留意不要破坏对外契约。

---

**三、关键模块速查**

**1. 消息模型** — `crates/shirley-agent-sdk/src/message/mod.rs`

`Message` 是一个带 `#[serde(tag = "role")]` 的枚举，共五种：

- `System` / `User`：纯文本
- `Assistant`：`content` 可选、`reasoning_content` 可选、`tool_calls` 列表
- `Tool`：`tool_call_id` + 可选内容
- `ContextSummary`：**不展示给用户**，编码成请求时降级为 system 消息，是上下文压缩的产物

`Usage` 是重点，它刻意区分"上报了 0"和"没上报"：

- `cached_input_tokens: Option<u64>` —— `None` 表示供应商没上报
- `cache_reported_input_tokens: Option<u64>` —— 累计命中率的真实分母（覆盖率）
- `cache_hit_rate()` 在未上报时返回 `None`，**不要把测不到当成 0% 命中**
- `Add` 实现里对 `Option` 做的是"保留已知的一侧"，不是当 0 累加

**2. 统一错误契约** — `crates/shirley-agent-sdk/src/error.rs`

- `ErrorKind`：稳定的错误分类（`Transport` / `RateLimited` / `ServerError` / `BadRequest` / `Unsupported` / `ToolFailure` / `Internal`），用于日志、指标、重试决策
- `SdkError` trait：`kind()` + `is_retryable()`（默认由 `ErrorKind::is_retryable` 推导）
- 各层错误（`AdapterError` / `ToolError` / `SandboxError` / `WorkspaceError` / `AgentError`）**各自留在自己的模块**，但都实现 `SdkError`。`AgentError` 用 `#[from]` 把它们收敛进来
- 展示格式统一为 `[前缀]: 详情`，前缀留在变体旁，不从 `ErrorKind` 推导
- **决策看分类，不看文案**：要判断重试与否，只依赖 `SdkError::is_retryable`，绝不 `match` 错误字符串

**3. 协议适配层** — `crates/shirley-agent-sdk/src/adapter/`

- `ModelConfig` 用 `bon` 生成 builder：`protocol` / `base_url` / `model` / `api_key` / `stream` / `thinking` / `reasoning_effort` / `temperature` / `max_output_tokens` / `context_window_tokens` / `tool_choice` / `extra_body`
- `codec(protocol)` 返回三件套函数指针 `(Encoder, Decoder, StreamDecoder)`（请求编码 / 非流式解码 / 流式解码，流式也纳入 codec 抽象，`invoke` 里不再有按协议硬分支）。**三个协议均已实现**（`ChatCompletions` / `Responses` / `AnthropicMessages`），`UnsupportedProtocol` 变体保留供未来协议使用
- `invoke()` 返回 `Stream<Item = Result<AdapterEvent, AdapterError>>`：
  - 非流式：读完整 body、decode，产出单个 `Finished(ModelResponse)`
  - 流式：走 `decode_stream_response`，逐块产出 `ReasoningDelta` / `ContentDelta` / `Finished`
- `ensure_success` 单独抽出来做状态检查，保证错误体与状态码一起保留（重试决策的唯一依据）
- `chat_completions/mod.rs` 里 `encode_messages` / `encode_tools` 做的是"内部 Message → OpenAI 格式"的转换；`dto.rs` 只做反序列化结构定义
- `temperature` / `max_output_tokens` / `tool_choice` 已接线（`Some` 时才写入请求体，`max_output_tokens` 在 ChatCompletions 上的键名是 `max_tokens`）；`extra_body` 是覆盖请求体的逃生口（浅合并，`null` 删除键）；`thinking` 仅在开启时写 `{"type":"enabled"}`。见 `docs/sdk-gaps.md` gap-4
- Responses 解码按 OpenAI 官方 schema 对齐（DeepSeek 本质是 OpenAI 兼容实现）：reasoning 取 `summary`（`summary_text`，OpenAI 系默认只回这个）优先、`content`（`reasoning_text`，DeepSeek 走这个）回退；流式同时认 `response.reasoning_text.delta` 与 `response.reasoning_summary_text.delta`；**非流式 `status:"failed"` 与流式一样转 `AdapterError`**（`cancelled`/`queued` 归 `Other`，不误报 `Stop`）；历史 assistant 文本用**纯字符串** content（`output_text` 只属输出 item，输入侧不接受）
- Responses 的 `encode_input` 是**纯保序映射**：system 作为普通 `{role:"system"}` item 留在 `input` 原位，**不用顶层 `instructions`**（后者会被服务端插到 `input` 之前，把中段/末尾 system 搬到最前，破坏 KV 前缀缓存）。保序 = 追加消息不动前缀，是缓存命中的结构性保证；**别在这里加 sort / 分组 / 位置调整**

已知设计债：`encode_messages` 写在适配层，但作者自己注释说"这逻辑其实该在 message 侧"。`encode_request` 里 `thinking` 现在按 `config.thinking` 映射成 `enabled` / `disabled`（**流式已实现**，不再写死）。

**4. 工具系统** — `crates/shirley-agent-sdk/src/tool/mod.rs`

- `Tool` trait：`definition()` 拿元信息，`invoke(Value, ToolContext)` 返回 `ToolFuture`；另有**两个默认钩子** `on_register`（= created）/ `on_unregister`（= destroy），默认 `Ok(())`——无状态工具零改动。`executed` 就是 `invoke` 本身，不另设钩子。见 `docs/tool-lifecycle.md`
- `ToolManager`：持有 `ToolContext`（**不再是 `Agent` 的独立 builder 参数**）；注册时查重，`definitions()` **按名字排序**（这是为了让 prefix 稳定、提高缓存命中率，别随手删掉这个 sort）。新增 `unregister(name)`（先移出表 → 再调 `on_unregister`，best-effort，未注册返回 `NotFoundError`）、`context()` / `context_mut()`
- `ToolContext`：类型擦除的运行时上下文容器（`Arc<HashMap<TypeId, Arc<dyn Any>>>`）。工具并发执行，所以上下文只能是 `Arc<Mutex<T>>` 这类内部可变性句柄；工具用 `ctx.get::<T>()` 取回。写入的两种途径：**注册钩子 `ctx.insert(value)`**（推荐，注册与注入同一步，不会漂移）或应用层 `with(data)`；清理用 `ctx.remove::<T>()`（按 `TypeId`，**每个工具请用自己的 newtype 状态**，避免误删同类型）。runtime 每次调用前 `self.tools.context().clone()` 分发（`Arc`，只加引用计数）。见 `docs/sdk-gaps.md` gap-1、`docs/tool-lifecycle.md`
- `ToolError` 分四类：`ExecutionError` / `RepetitionError` / `NotFoundError` / `ArgumentsError`

关于 `ToolDefinition.parameters`：它直接就是 JSON Schema。原先记为"绑死 OpenAI"的债，做 Responses 适配时得到修正——**Responses 的 `tools[].parameters` 也是 JSON Schema**，只少一层 `function` 包装。所以 JSON Schema 本身就是跨协议中间表示，不必另造 `ParamType`；真正的协议差异在**消息 item 结构**与**流式事件语义**上（见 `docs/responses-api.md`、`docs/adapter-layer.md`）。

**5. `#[tool]` 宏** — `crates/shirley-agent-sdk-macros/src/tool.rs`

写法：

```rust
#[tool(description = "bash 用于执行命令")]
pub async fn bash(
    #[param(description = "要执行的命令")] command: String,
    #[param(description = "超时时间（秒）")] timeout: Option<u64>,
) -> Result<String, shirley_agent_sdk::ToolError> { ... }
```

有状态工具可声明一个 `ToolContext`（或 `&ToolContext`）参数——它**不进入参数 schema**，由 SDK 在调用时注入，按原位置传给函数（一个工具最多一个）：

```rust
#[tool(description = "把计数加一")]
async fn bump(ctx: &ToolContext, #[param(description = "增量")] by: u32) -> Result<u32, ToolError> {
    let session = ctx.get::<Arc<Mutex<Session>>>()
        .ok_or_else(|| ToolError::ExecutionError("session not provided".into()))?;
    let mut guard = session.lock().unwrap();
    guard.counter += by;
    Ok(guard.counter)
}
```

可选**伴生钩子**：`on_register` / `on_unregister` 接的是用户写的普通函数（签名固定
`fn(&mut ToolContext) -> Result<(), ToolError>`），宏把它们 wire 进生成的 `impl Tool`——
用户只写实现，不碰 `Tool` trait / schema（这是硬约束："宏是唯一入口"）。不写钩子 = 空实现：

```rust
#[tool(description = "联网搜索", on_register = ws_on_register, on_unregister = ws_on_unregister)]
async fn web_search(ctx: &ToolContext, #[param(description = "查询")] query: String)
    -> Result<String, ToolError> { ... }

fn ws_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.insert(WebSearchState::from_env()?);   // 注册即注入依赖
    Ok(())
}
fn ws_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<WebSearchState>();
    Ok(())
}
```

钩子定义仍在 `Tool` trait 上，宏只"引用"；详见 `docs/tool-lifecycle.md`、`docs/tool-macro.md`。

宏展开后会在同名 `mod` 里生成：`Arguments` 结构体（`Deserialize` + `JsonSchema` + `deny_unknown_fields`）、`GenerateTool`（实现 `Tool`，含钩子覆写）、`definition()`、`tool()`。调用方写 `tools::bash_tool::tool()` 注册即可。

宏的约束（踩过的坑）：

- 函数不能用 `self`，参数不能解构、不能 `ref` / `@` 绑定
- 参数描述必须写 `#[param(description = "...")]`，否则编译报错
- 生成的 `invoke` 里调用的是 `super::#name`，所以**被修饰的函数和宏生成的 mod 必须在同一层级**（钩子同理：`on_register = f` 要求 `f` 与工具函数同层，宏生成 `super::f` 引用）

**6. ReAct 运行时** — `crates/shirley-agent-sdk/src/runtime/`

这是整个项目的心脏。按职责拆成几个子模块，`mod.rs` 只负责接线与再导出：

- `agent.rs`：`Agent` 本体（构造、`run` / `run_stream` 主循环、压缩调度）
- `compaction.rs`：压缩切点与重建（`RETAIN_RATIO` / `CutPlan` / `CompactParts` / `plan_cut` / `background_len`）
- `event.rs`：对外事件与运行结果（`AgentEvent` / `RunResult` / `StopReason`）
- `error.rs`：顶层错误收敛（`AgentError` + `SdkError`）
- `prompt.rs`：系统提示词（`SystemPrompt` / `SystemPromptContext`）

`CompactParts` 的契约测试移到了 `crates/shirley-agent-sdk/tests/runtime_compaction.rs`（集成测试，只依赖公开 API）。

- `Agent::new` 是 `bon` builder：`model_config` / `system_prompt` / `working_dir` / `messages` / `tools` / `compression_instruction` / `session`
- `system_prompt` 类型是 `SystemPrompt`（不是 `String`）：既接受固定字符串（`From<String>` / `From<&str>`），也接受**函数** `Fn(&SystemPromptContext) -> String`。函数形式让提示词按运行时上下文动态生成——`SystemPromptContext` 目前携带 `working_dir`。`working_dir` 是独立 builder 参数，构造时会用它解析一次提示词
- `run()` 是 `run_stream()` 的薄封装，只等最后一个 `Finished`
- 运行参数热切换接缝（应用层指令落地用，均不重建 `Agent`）：`set_model(model)` 只换模型名；`set_provider(base_url, api_key)` 换端点与密钥（与前者对称，`/login` 用）；`unregister_tool(name)` 运行期注销工具（触发其 `on_unregister`，与 `ToolManager::register` 对称）。这几条是 SDK 为应用层指令新增的接缝，模型配置的其余字段原样保留。**`switch_session` 已从 SDK 移除**（多会话重构，`docs/multi-session.md` 决策 9）——"哪个会话活跃"是应用层编排，SDK 不再有"当前会话"这个概念；每个 `Agent` 对应一份固定日志源，切换由应用层 `SessionManager.active` 指针完成
- `run_stream()` 返回 `Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentError>> + Send + 'a>>`，用 `async_stream::try_stream!` 实现
- 主循环：压缩检查 → 组装请求 → 调模型（消费 `AdapterEvent`）→ 落库 → 有 tool_calls 就并发跑（`FuturesUnordered`）→ 没工具调用就 `Finished` 退出
- `AgentEvent` 有：`ContentDelta` / `ReasoningDelta`（流式增量）、`MessageAdded`、`CompressionStarted` / `CompressionFinished`、`ContextUsage`、`ToolStarted` / `ToolFinished`、`Usage`、`Finished(RunResult)`
- `RunResult`：`messages`（本轮新增，从 `start_index` 切出）/ `stop_reason` / `usage`；`StopReason` 有 `Completed` / `MaxStepsReached` / `Cancelled`，**目前只会产生 `Completed`**

**上下文压缩**（这块最容易改坏）：

- `should_schedule_compression` 在 `(input + output) * 100 >= limit * 80` 时置 `compression_pending`，用整数比较避免浮点精度问题
- `active_messages()` 找到**最后一个** `ContextSummary`，只保留开头的 system 消息 + 从 summary 开始的消息
- `compress_context()` 把压缩指令作为 system 追加，要求模型输出纯文本摘要，非空且无 tool_calls 才算成功，否则报 `CompressionError`
- 摘要内容有**结构契约**：`compaction.rs` 的 `COMPACTION_TEMPLATE` 定义了 XML 块（`current_goal` / `hard_constraints` / `decisions` / `progress` / `open_questions` / `compacted_range`），`compress_context` 会把它追加到调用方的 `compression_instruction` 之后。模板**不含** `next_step`、并显式禁止推演——修的就是"压缩器编造用户没提过的下一步"（`docs/compaction.md` 5.3）
- 压缩成功后用 `CompactParts::rebuild` 重建 `self.messages`：**系统提示词不保留、不复制**——它由 `Agent` 单独持有（`system_prompt` 字段），每次重建都 `system_message()` 重新生成一条再置顶（函数形式的提示词会被**重新解析**，因此工作目录 / 项目指南的最新状态会反映进来）。`rebuild` 只返回 `[新 ContextSummary] + current_task + remain`
- 压缩失败会中断整个 stream，UI 侧会显示成错误

---

**四、应用层（TUI 与 Desktop）**

- `src/main.rs`：读 `.env`（`dotenvy`），按 `--desktop` / `SHIRLEY_INTERFACE=desktop` 选 `Mode::Tui` / `Mode::Desktop`，然后交给 `bootstrap::AgentFactory::assemble` 装配。**装配逻辑已从 `main.rs` 抽到 `src/bootstrap.rs`**——`AgentFactory` 是 TUI 与桌面界面共享的 LCA（另一个 LCA 是 SDK 的 `Agent`），两个界面拿同一份装配产物、只是渲染方式不同。`AgentFactory::assemble` 做这些事：经 `settings::Settings::load_default()` 装载配置（**不再散读 env**，见下条），构造 `ModelConfig`（`stream(true)` / `thinking(true)` / `reasoning_effort("low")`），算出 `needs_login = !settings.is_configured()`（缺配置不阻断启动），并把模型配置 / 系统提示词 / 工作目录 / 会话目录（`src/session.rs` 的 `FileSessionCatalog`，多会话布局 `.shirley/sessions/<name>.jsonl`）连同该标志交给 `interface::run`。**工厂的产物是"能造 `Agent` 的能力"而非单个 `Agent`**（决策 5）：`build_agent(store)` 每次造一个匿名 `Agent`（工具按需重建，见下）——多会话就多造几个。启动时**惰性开一份空会话**（而非恢复最近修改的旧会话）：此刻只定会话名、**不落盘**，发首条消息才真正建文件（`create_lazy`，Codex 的 UX）；旧的单文件 `.shirley/session.jsonl` 会被 `adopt_legacy` 收编（会话持久化，见 `docs/session.md`）。工作目录由 `prompt::workspace_root()` 决定（`SHIRLEY_WORKSPACE` 优先，否则当前目录）——它**刻意不属于 `settings`**（是"在哪个项目跑"的会话级信息，不是"用户是谁"的配置）
- `src/settings.rs`：**配置装载（方案 A）**。把"配置从哪来"与"怎么用"解耦，纯应用层、不碰 SDK。优先级 `内置默认 < 全局 config.toml < 工作区 .shirley/config.toml < 环境变量`。配置文件为 TOML，`[provider]` 表含 `protocol` / `base_url` / `api_key` / `models_url` / `model` / `context_window_tokens`（全 `Option`，`deny_unknown_fields`）。全局路径 `<config_dir>/shirley/config.toml`，工作区路径 `<root>/.shirley/config.toml`。环境变量层（`LOCAL_*`）放在**最后**以兼容既有 `.env` 习惯——老用户零改动。`Settings::load` 的环境读取用注入闭包（测试不打全局 env），`load_default` 才用真实 `config_dir` + 进程环境。缺 `base_url` **不再报错、不阻断启动**：`finalize` 落成空串，`Settings::is_configured()` 返回 `false`，应用层据此在 TUI 里自动进入 `/login` 引导用户补齐（见下条）。**也可写**：`save_provider(path, provider)` 先读后改再写（只覆盖传入字段、不动其它键、自动建目录、写完 chmod 600 收紧权限），是 `/login` 的落盘入口
- `src/prompt.rs`：应用侧系统提示词构造。`build(working_dir)` 返回一个 `SystemPrompt` 函数，每次解析时读取当前工作目录与工作区里的 `Agent.md`，拼成"角色 + 工作区根 + 项目指南"。工作目录明确告诉模型（`plan.md`：AI 不知道工作区就会从根目录乱找），`Agent.md` 提供项目怎么跑、代码怎么组织
- `src/session.rs`：应用侧会话存储与**会话目录**（`docs/session.md`）。`JsonlSessionStore` 把**原始 Message 全量日志**逐行落成 JSONL，实现 SDK 的 `SessionStore`（`append` / `load` / `truncate`，同步签名）。**不含 system**（恢复时现生成）、不含 chunk / BM25 索引（召回库由日志派生）。`truncate` 先写临时文件再原子替换、并重开追加句柄（注意：内部 `rewrite_locked` 需在已持锁下调用，否则非重入锁会死锁）。多会话（`/session`）：`SessionCatalog` trait（`list` / `open` / `create` / `create_named` / `create_lazy` / `rename` / `delete`，与 `ModelCatalog` 对称；后四者带默认实现，不支持元数据的后端免改）+ `FileSessionCatalog`（一份会话 = `.shirley/sessions/<name>.jsonl`，目录扫描即列出，`adopt_legacy` 收编旧单文件）+ `SessionEntry { name, label, preview, modified_ms, turns }`（`preview` 取首条用户消息前 40 字，`modified_ms` = 文件 mtime 用于降序，`turns` = 用户轮数）+ `EmptySessionCatalog`（`App::new` 兜底）。**标题持久化为 sidecar `<name>.title`**（纯文本，不污染只存 Message 的 JSONL）；`rename` **只改标题、不改文件名 / 内容**（Codex `/rename` 语义，回避已打开句柄指向旧 inode 的问题），`delete` 删日志并 best-effort 删 sidecar。**惰性新建**：`create_lazy` 返回 `LazySessionStore`——构造时定名但**不落盘**，首次 `append`（首条消息）才物化（落 JSONL + 写标题 sidecar）；物化前 `load` / `truncate` 视为空 / 空操作。这样"打开应用没聊"不留空会话文件
- `src/interface/selection.rs`：**鼠标选区**（`docs` 无独立文档）。左键按下记锚点、拖动延伸、松开即把选中文本写入系统剪贴板（`arboard`）——**全程只用鼠标，不需按复制键**。选区用屏幕单元格坐标表示，`highlight` 给缓冲区叠反色（渲染在一切之上），`text` 按符号显示宽度前进提取文本（跳过宽字符的续格，避免 CJK 之间插入假空格）。`Tui::draw` 在有选区时留存当帧缓冲区快照，松开左键时据此提取
- `src/interface/tui.rs`：主循环用 `tokio::select!`，把**前台会话**的 `Agent` 通过 `SessionManager::take_agent(name)`（按会话名）移出去、`tokio::spawn` 到独立任务里跑，结果通过 `mpsc` 通道回传，完成后 `restore_agent(name, agent)` 放回来——每个会话各持一个 `Agent`，后台会话的上下文不因前台切换而丢。**直接消费 `AgentEvent`**（不再有 `AgentUpdate` 中间类型）。收到事件后统一走 `App::apply_event`（薄包装：转发到 `sessions.active_mut().apply_event` 并失效 `message_cache`）——**累加逻辑收敛到 `Session`，TUI 与 desktop 共享同一条事件处理路径**，UI 只读结果
- `src/interface/app.rs`：纯状态机，`Item::Message` / `Item::Tools` 两种条目；`Role` 有 User / Assistant / Summary / Error；usage、context 用量、输入历史（上/下键回放）都缓存在这里；`toggle_thinking` / `toggle_tool_args` 控制展示
- `src/interface/ui.rs`：`MessageCache` 做渲染缓存（按宽度失效），`footer_line` 展示上下文占用百分比和缓存命中率（单次 + 累计）。消息正文交给 `markdown::render` 转成带样式的 `Line`/`Span`。**滚动高度必须用 ratatui 自己的换行计数**（`Paragraph::line_count`，按词边界，需开启 `unstable-rendered-line-info` feature）来累计 `row_offsets`，不能用 `line.width().div_ceil(width)` 估算——后者按字符数取整会少算中英混排长行的行数，导致 `max_scroll` 偏小、最新消息被底部输入框遮挡（已修，回归测试 `cache_row_count_matches_paragraph_word_wrap`）
- `src/interface/markdown.rs`：Markdown 渲染层。`parse` 用 `pulldown-cmark` 把 raw markdown 收敛成块级 AST（`Block`：Heading / Paragraph / Code / List / Quote / Table / Rule / Html），`render` 再把 `Block` 变成 ratatui 的 `Line`/`Span`。支持标题、粗斜体、行内代码、代码块、有序/无序/任务列表、嵌套列表（悬挂缩进）、引用、表格、分割线。错误消息不走 markdown（避免报错里的符号被当语法吃掉）。有 13 个单测覆盖各语法
- `src/interface/event.rs`：终端事件在独立线程里 `poll` + `read`，通过无界通道送给异步侧。开启括号粘贴（`EnableBracketedPaste`），粘贴内容整体作为 `Event::Paste` 送达；`Drop` 时关闭两者。**开启鼠标捕获**（`EnableMouseCapture`）——拖动选中由程序自己实现（见 `selection.rs`），所以复制全程只用鼠标、不需再按复制键；捕获会接管终端原生选择，这是有意为之。`MouseEventKind::Moved`（无按键移动）在读循环里直接丢弃，否则鼠标一移动终端就狂发事件、主循环空转重绘
- `src/interface/update.rs`：按键映射。`Esc`（AI 回复中 → 打断本轮；回溯编辑态 → 取消本次改动；否则退出）/ `Ctrl+C` 退出，`Ctrl+T` 切换思考显示，`Ctrl+O` 切换工具参数展开，`Enter` 提交，`Ctrl+A` / `Ctrl+E` 行首/行尾，`↑` / `↓` 历史回放，`PageUp` / `PageDown` 上下滚动（与鼠标滚轮等价，见 `event.rs`）。模型选择器面板打开时按键优先导向面板；指令候选浮层打开时 `↑↓←→` / `Tab` / `Enter` / `Esc` 优先导向浮层（其余按键继续正常编辑）。`Event::Paste` 走 `App::insert_input` 整段插入（换行归一为 `\n` 当普通字符），不触发发送。**鼠标事件不走这里**——由 `tui.rs::handle_mouse` 直接处理（需要最近一帧缓冲区与系统剪贴板）
- `src/interface/command.rs`：快捷指令（slash commands）。仅作用于 TUI，**不接触 SDK**——指令要么展开成 prompt 发给 AI，要么作为本地系统消息，要么触发一个本地面板。当前有 `/init`（展开为生成 `Agent.md` 的 prompt）、`/model`（打开模型选择器）、`/rewind`（回退最后一条用户消息）、`/session`（打开会话选择器）、`/login`（分步登录：依次询问 base_url / api_key / model）。`CommandOutcome` 区分这几类；模糊匹配（fuzzy + Levenshtein）在输入 `/` 时给候选，浮层里 `↑↓` / `←→` 移动高亮、`Tab` / `Enter` 采纳当前高亮项、`Esc` 关闭浮层（不动输入）
- `src/models.rs`：模型目录。`ModelEntry { label, value, provider }` + `trait ModelCatalog`（异步 `list()`，仿 SDK `ToolFuture` 手法）+ `StaticCatalog`（兜底静态列表）+ `RemoteCatalog`（请求 OpenAI 兼容 `GET /v1/models`，失败回退兜底）。`bootstrap.rs` 从 `LOCAL_BASE_URL` 推导接口、`LOCAL_MODELS_URL` 可覆盖。`/model` 打开面板时由主循环把目录移入后台任务异步加载（不阻塞 UI）
- **历史回溯（`/rewind`，方案 A：只回退最新一条）**：直接取**最后一条**用户消息，把**原文填回输入框**（进入"编辑重发态"，输入框提示切换为「Enter 重新发送 · Esc 取消本次修改」）。只做最新一条（`docs/session.md` 一.决策 4），没有"选择"这个动作，故**没有选择面板**。进入编辑态时**输入框不再钉在底部，而是就地移动到该消息在会话中的渲染位置**——渲染层把该条消息替换成一个青色描边的输入框（上方是它之前的对话，下方是之后的对话，新消息只会把底部挤出视野、不挤压编辑框，见 `ui.rs` 的 `draw_rewind_edit`）。回车则先回退 Agent 与界面到该消息之前、再作为全新一轮发送，Esc 则直接取消本次改动（清空输入、退出编辑态，Agent/界面均不动）。被压缩掉的历史不在 `agent.messages` 里，天然不可回溯
- **多会话切换（`/session`）**：模态选择器（与 `/model` 同款），列表首项固定是「＋ 新建会话」哨兵（`SessionPicker::NEW_NAME` 为空串），其余为真实会话、每项第二行显示首条用户消息预览。确认时**编排交给 `SessionManager`**（`src/interface/session.rs`，`docs/multi-session.md` 决策 4）——哨兵走 `create_new(None)`（`create_lazy`，惰性）、真实会话走 `switch_to(name)`：**已加载的会话只挪 `active` 指针、直接复用其就绪 `Agent`**（不重建，保住后台上下文）；未加载的经工厂 `build_agent` 从日志恢复后插入再切前台。**`Agent` 匿名、身份住在容器上**（决策 4）：`Session.name` 是唯一真相源。切换后 `App::rebuild_items_from_agent` 整体重建界面条目并重置会话绑定统计（usage / 上下文占用）。会话目录是本地扫描（同步），由主循环直接取列表打开，无需后台任务。页脚会显示当前会话名。**TUI 与 desktop 共用同一个 `FileSessionCatalog` 描述的目录**（`docs/session.md` 八.决策 7）
- **分步登录（`/login`，方案 A：分步问答版）**：**未配置模型服务（缺 `base_url`）时启动会自动进入本流程**——`bootstrap.rs` 算出 `needs_login` 传入 `interface::run`，`Tui::new` 据此调 `App::start_login()`，把"缺配置"从启动错误变成 TUI 内的一次引导；用户可 Esc 取消，取消后仍可手动 `/login`。不做表单浮层，而是**复用现有输入框与 `submit` 链路**——进入后输入框被临时征用为字段编辑器，每步一次回车。顺序固定 `base_url → api_key → model`，每步以当前配置（`agent.model_config()`）预填，直接回车即"保留不变"；`base_url` 空则留在本步重问（空端点无意义），`api_key` 空表示"无鉴权"（本地服务常见，会**明确清空**旧 key，与"保留旧值"不同）。走完先**热更新 Agent**（`set_provider` + 可选 `set_model`），再**落盘**到工作区 `<root>/.shirley/config.toml`（`settings::save_provider`，先读后改再写，不动其它键，写完 chmod 600）。热更新成功但落盘失败会如实告知（不假装成功）。`Esc` 取消登录（只退登录态，不退出程序，也不改配置）。渲染层无改动——提示语走既有的 `command_hint`（`ui.rs::input_hint` 已消费），登录态下 `on_input_changed` 与回溯编辑态一样保留提示。登录流程是内存态（`App::login: Option<LoginFlow>`），未走完不落盘。测试用 `set_config_path` 注入临时路径，避免污染真实工作区
- `src/interface/app.rs`：输入编辑状态机。`insert_input` 支持粘贴多行文本（CRLF/CR 归一为 LF），光标始终落在字符边界
- `src/interface/ui.rs`：输入框按显示宽度软换行 + 保留硬换行（`wrap_input`），框高随内容增长（上限 `INPUT_MAX_LINES = 10`），超出后内部纵向滚动，保证粘贴长文本时光标可见

**已有工具**：

- `bash`（`src/tools/bash.rs`）：先做 `split_whitespace` 黑名单检查（`rm` / `shutdown` / `reboot`，这只是临时兜底），然后**走进程沙盒**执行（`SandboxSpec::new("bash").arg("-c").arg(&command)`），默认 60s 超时。沙盒结果渲染时带 stdout / stderr / 退出码 / 超时提示。**真正的边界在沙盒后端，不在黑名单**
- `read_file`（`src/tools/read.rs`）：**受控的文件读取**，用来替代裸 `cat`，从源头卡住上下文占用。关键约束：
  - **路径受工作区约束**：经 SDK 的 `WorkSpace::resolve` 校验，拒绝绝对路径与 `..` 越界（越界/非法路径归为 `ArgumentsError`，模型换个写法就能成功）——落地 `docs/roadmap.md` 验收第 4 条
  - **行数有上限**：默认 200 行、硬上限 `MAX_LINE_COUNT = 2000`（显式要求更多也只读到上限）
  - **字节有上限**：单次输出 `MAX_OUTPUT_BYTES = 64KB` 封顶，单行超长时按字符边界截断
  - **疑似密钥默认拒绝**：`.env` / `*.pem` / `*.key` / `id_rsa*` 等（见 `docs/security.md` 第三节）
  - **二进制探测**：开头 8KB 出现 NUL 即拒绝；目录引导用 `ls`
  - 返回**纯文本带行号**（非 JSON），截断时在末尾提示 `start_line=<下一行>` 续读——模型可直接读、可续读

- `web_search`（`src/tools/web_search.rs`）：**联网搜索**，迁移自拾文（`shiwen-open-source`）。不走主模型路由，而是对 DeepSeek 的 **Anthropic-compatible Messages API**（`POST {base_url}/messages`）单独发一次有界请求，用服务端工具 `web_search_20250305` 拿回结构化来源（`web_search_tool_result`），归一化后作为**不可信外部数据**交给主模型。要点见 `docs/web-search.md`：
  - **形态**：`#[tool]` 宏 + **注册钩子（`on_register`）**。工具函数无状态，运行所需的 client / 凭据 / 配置由 `WebSearchState` 承载，由 `web_search_on_register` 在**注册时**读 env 并 `ctx.insert(state)` 注入——"注册工具"与"注入依赖"合成同一步，不会漂移；`web_search_on_unregister` 注销时 `ctx.remove::<WebSearchState>()`。函数内 `ctx.get::<WebSearchState>()` 取回（只读 `Arc`，并发共享无需锁）；参数 schema 由宏从签名自动生成（`ToolContext` 不入 schema）。见 `docs/tool-lifecycle.md`
  - 参数只有 `query`（非空、≤300 字符）；返回 JSON `{summary, query, sources[], truncated}`
  - 归一化：按 URL 归并 citation snippet、去重、只留 http(s)、按 `max_results` 截断；**只认结构化 block，缺失即报错，绝不伪造来源**
  - 安全：**不跟随重定向**、响应体 2MB 上限、总超时（`tokio::time::timeout`）
  - 配置全走环境变量（`DEEPSEEK_API_KEY` 必填，其余有默认）；**未配置时不注册**，模型看不到该工具

> `read` 工具曾在 commit `aaf0081` 被移除（当时判断"一个 bash 就够"），但裸 `cat` 没有输出上限，容易把上下文灌满。现已以 `read_file` 的形式**重新引入**，并补上工作区约束、行/字节上限与密钥脱敏。后续计划加 `apply_patch`（见根目录 `plan.md`）。

**桌面界面（`docs/desktop-interface.md`，M0–M3 已落地）**：与 TUI **并存**的 Tauri 2 桌面界面，把 shiwen 的聊天 UI（React/TS + Tailwind 4）迁进来。两者是**兄弟界面、共享 LCA**——不是"TUI 上叠一层 web"：`interface/` 里全是 ratatui 专属逻辑，webview 是独立窗口，二者不能同时画同一份对话。共享层是 SDK 的 `Agent`（`run_stream()` → `AgentEvent` 流）+ `bootstrap::AgentFactory` 的应用装配 + `interface::session::SessionManager`（多会话编排）；desktop 侧用 Tauri command/event 消费同一份 `AgentEvent`，落点 `src/interface/desktop/`。

- **启动**：`cargo run`（默认 TUI）不变；桌面窗口用 `cargo desktop`（= `cargo run --features desktop -- --desktop`，别名见 `.cargo/config.toml`），或 `SHIRLEY_INTERFACE=desktop cargo run --features desktop`。**注意 `--features` 是 cargo 参数，程序参数必须放在 `--` 之后**，否则程序看不到 `--desktop` 会退回 TUI。Tauri 依赖是 **feature-gated + optional**（`desktop` feature），默认构建不触碰 webview 工具链。
- **结构**：`src/interface/desktop/`（Rust：`mod.rs` 门面 / `wire.rs` 事件映射 / `shell.rs` Tauri 壳）+ `src/interface/desktop/web/`（React+TS+Vite+Tailwind 4 前端，`dist` 由 `tauri.conf.json` 的 `frontendDist` 指向）。构建前端：`cd src/interface/desktop/web && npm install && npm run build`。
- **commands**：`agent_send`（发起一轮，事件经 `agent://event` 推回；可选 `references` 为 `@` 引用的工作区路径，由 `compose_prompt` 拼进本轮 prompt）/ `agent_model_name` / `agent_list_models` / `agent_set_model`（热切换模型，不重建 Agent）/ `agent_search_files`（`@` 引用的工作区文件检索，纯应用层 `workspace_search`，不碰 SDK）/ `agent_cancel`（预留）。模型切换选择器放在发送按钮旁（`ModelSelector.tsx`，走 `AiChatComposer` 的 `trailingAction` 槽位），目录与切换都转发 Rust 侧 `ModelCatalog` / `Agent::set_model`，与 TUI 的 `/model` 同语义。
- **会话管理（事实源 + 扇出）**：`DesktopState` 持有 `sessions: Arc<Mutex<SessionManager>>`（与 TUI **同一个编排器语义、同一份 `<root>/.shirley/sessions` 数据**，经同一个 `FileSessionCatalog`）与 `session_catalog`。commands：`agent_list_sessions` / `agent_current_session` / `agent_new_session { title? }` / `agent_switch_session { name }`（走 `SessionManager::create_new` / `switch_to`，与 TUI 同一接缝）/ `agent_rename_session { name, title }` / `agent_delete_session { name }` / `agent_load_history`（从 `active_agent().messages()` 取 User / Assistant 正文成 `HistoryMessageWire`）。**运行态按会话名隔离**：`agent_send { text, references?, session? }` 按名 `take_agent` / `restore_agent`（不依赖 `active` 指针——后台会话运行时前台可能已切走），后台会话各自独立、可并行；同一会话运行中拒绝再次发送。**事件按会话扇出**：每个 `Session` 自持一条 `broadcast` 通道（`events: Sender<Result<AgentEvent, String>>`，容量 256），`agent_send` 把每个事件喂回 `SessionManager::apply_event(name, update)`（累加进对应 `Session`），再经 per-session pump 任务推给前端；`agent_subscribe { session }` 在**同一把锁内先 `subscribe` 再 `snapshot`**（返回 `SessionSnapshotWire { session, items, running }`，不丢 / 不重），`agent_unsubscribe { session }` abort 该 pump。**`AgentEventWire` 每个变体带 `session: Option<String>`**（`with_session` 打标），前端据此把事件路由到对应会话视图。前端 `SessionSelector.tsx`（顶栏标题旁）提供列表 / 切换 / 新建（可命名）/ 重命名（内联编辑）/ 删除（两次点击确认）；切到某会话即 `openSession`（拿快照重建 transcript + 订阅其事件），切走退订。**本期范围**：list / new / switch / rename / delete；**不做** fork / archive / 启动 `--resume`。
- **`@` 引用**（`web/src/lib/file-mentions/`）：迁移自 shiwen 的 entity-mentions 交互骨架，对象**重绑到工作区文件/目录**（`useFileMentions` / `FileChips` / `FileMentionPopover`）；输入 `@` 触发候选浮层、选中生成 chip、发送时路径拼进本轮 prompt。检索落在应用层 `src/workspace_search.rs`（工作区遍历 + 关键词排序，跳过 `.git`/`node_modules`/`target` 等）。**引用不是 Agent 的对外契约**——`Message` / `AgentEvent` 未改，SDK 未新增对外类型。
- **事件桥**：`wire.rs` 把 `AgentEvent` → `AgentEventWire`（JSON，`web/src/types/wire.ts` 为契约）。为支撑工具卡，`ToolStarted` / `ToolFinished` 两个既有变体**追加**了字段（`arguments` / `ok` / `output` / `elapsed_ms`）——字段追加、TUI 用 `..` 忽略，零破坏；仍未派生 `Serialize`（协议差异收敛在边界）。
- **渲染对齐 shiwen**（见 `docs/desktop-interface.md` 4.3.1 / 4.3.2）：① markdown 与 shiwen 不一致**不在组件**（`AiMarkdown.tsx` 逐字节相同），在 `styles/index.css` 漏了 shiwen 的 `@source ".../node_modules/streamdown/dist/*.js"`——Tailwind 4 默认不扫 `node_modules`，streamdown 的 utility 类全没生成，补回即修复；② 聊天区固定宽度 `max-w-190`（760px）居中，`App.tsx` 用 `<section>` 包裹转写区 + 输入框；③ 连续工具调用收集进一个 `AiToolActivityDisclosure` 折叠区（「查看处理过程 · N 项」），不再每个调用铺一张卡。
- **前端桥**：`web/src/lib/bridge.ts` 双模式（Tauri `invoke`/`listen` vs 纯浏览器 mock），`npm run dev` 可独立调试 UI。
- **剥离决策（已确认）**：A2UI / entity / citation / space / share / OCR / SSE 线格式全删；**审批（approval）代码路径保留在 `AiToolExecutionCard.tsx`，但 `AssistantMessage.tsx` 第一期不渲染审批态执行项**；引入 Node/npm/Vite 构建链已获用户允许。

---

**5. 召回** — `crates/shirley-agent-sdk/src/recall/`（`docs/recall.md`）

压缩丢失信息的退路。定位：**compaction 的自然配套，SDK 内部能力，应用层无感**。

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | 门面：`Retriever` trait（可扩展点，BM25 现在实现、embedding 以后加）+ `RecallStore`（内存存储，持久化留空）+ `RecallTool`（手写 `Tool`，因为要持有 `Arc<RecallStore>`） |
| `tokenize.rs` | 分词：CJK 按字 / ASCII 按词。Unicode 范围与 `token` 模块**共享** `token::is_cjk_word_char`（唯一一处定义），但口径不同——记账含 CJK 标点（占体积），切词不含（标点是分隔符，入词元会污染打分） |
| `bm25.rs` | 倒排索引 + 经典 BM25 打分。`k1=1.2` / `b=0.5`（低于经典 0.75，缓解 chunk 长度差异极端的失真） |
| `chunk.rs` | 分块：**只有对话性内容入库**——`UserChunk`（一条 User）独立成块；无 `tool_calls` 的 `Assistant` 正文成块。带 `tool_calls` 的 `Assistant` 与全部 `Tool` 结果**不入库**（走重建路径，决策 1）。`reasoning_content` 也不入索引视图（过程性思维会污染 IDF） |

核心设计（`docs/recall.md` 一、两个核心决策）：

- **重建与召回二分，由 AI 判断**："能不能重建"不是工具属性是调用属性（`cat x` vs `git commit`），AI 看到具体调用后自己决定。工具类消息**不入召回库**；对话类（User / Assistant 文本）入库。
- **索引自动，检索 AI 触发**：`compress_context` 把被压段里的对话性内容分块入库；recall 作为工具自动注册进 `ToolManager`（`Agent::new` 里做的，应用层一行没改），AI 生成 query 自己决定何时调。不做每轮自动检索。
- **召回无损**：chunk 原文保存，返回原文 + 相关度元信息，**绝不二次摘要**（单条超 4000 字符时截断并显式标注，是防垄断的兜底，不是摘要）。
- **工具输出统一清空**：压缩时 Tool.content 替换为占位标记（`[工具结果已省略以节省上下文；如仍需要，请重新执行调用获取当前状态]`），AI 走重建路径。这是 v0 有意的技术债（见下）。
- `<compacted_range>` 模板措辞已更新：重建路径（重新读取/执行）与召回路径（recall 工具）显式分立——这是 AI"感知到自己忘了"的钩子。

**5.1 任务账本（todo）** — `crates/shirley-agent-sdk/src/todo/`（`docs/todo.md`）

压缩的第三个配套能力：recall 找回"用户说过什么"，todo 记录"我做到哪了"。定位：
**compaction 的自然配套，SDK 内部能力，应用层无感**——与 recall 一样在 `Agent::new`
里自动注册工具、自动注入。

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | `TodoStore`（`Arc<Mutex<TaskState>>`，与 `RecallStore` 同款）+ `TodoUpdate` / `TodoStep`（补丁入参）+ `TodoTool`（手写 `Tool`，持有 `Arc<TodoStore>`）+ `TASK_STATE_HEADER` + 单测 |

核心设计（`docs/todo.md` 二、三个核心决策）：

- **模型写，不做程序化推断**：暴露 `todo` 工具，模型自己决定何时更新（与 recall"检索 AI 触发"同一哲学——状态判断发生在 AI 层最准）。
- **不进 `self.messages`，每轮作为 system 追加在末尾注入**：账本单独持有，压缩碰不到它，天然跨压缩存活；注入位置选**末尾**是因为追加不动前面的前缀，账本稳定时前缀缓存照常命中（`responses-api.md` 里"system 原位保留"正是为这个）。代价是模型每次调 `todo` 会让"账本那条 system 之后"的前缀失效——这是"每轮可见"的固有代价，用"放末尾 + 只写变化"压到最小。三个适配器都能处理任意位置 system（ChatCompletions 原样输出 / Responses 原位保留 / Anthropic 摘到顶层 `system`）。
- **补丁语义**：`goal` 设置替换、`steps` **整体替换**（清单短，每轮重发全量天然自纠）、`add_findings` / `add_open_questions` **追加**、`clear` 重置；未提供的字段保持原样。
- **渲染成 XML**（`<task_state>` / `<goal>` / `<steps>` / `<findings>` / `<open_questions>`）：空账本不注入；`&` / `<` / `>` 转义；`MAX_RENDER_CHARS = 4000` 超限截断并显式标注（防账本自己垄断上下文）。
- **切换会话清空**：会话切换由应用层 `SessionManager` 完成（每个会话自持独立 `Agent`），旧会话的召回库 / 任务账本随其 `Agent` 一并搁置，**天然不残留**——无需（也再无）`Agent::switch_session` 里的显式清空。

实现位置：`todo/mod.rs`（数据 + 工具 + 单测）、`runtime/agent.rs`（持有 `Arc<TodoStore>`、注册、`active_messages()` 末尾注入）。对外 re-export `TASK_STATE_HEADER` / `TodoStep` / `TodoStore` / `TodoTool` / `TodoUpdate`。已知缺口见 `docs/todo.md` 第七节（无持久化 / 无自动清理 / 注入即失前缀缓存 / 模型可能不用）。

---

**五、沙盒与工作区（SDK 新增能力）**

**沙盒** — `crates/shirley-agent-sdk/src/sandbox/`

| 文件 | 职责 |
| --- | --- |
| `spec.rs` | 平台无关的执行意图：program + args、cwd、env、timeout、workspace_root、读写路径、网络策略（`SandboxSpec`） |
| `backend/mod.rs` | 后端抽象 `SandboxBackend` + 能力自描述 `Capabilities` + `SandboxError` |
| `backend/process.rs` | 裸进程后端（无隔离），仅用于开发/测试与降级基线 |
| `output.rs` | 统一结果：stdout / stderr / exit_code / timed_out / isolation / **degraded** |
| `mod.rs` | 门面 `Sandbox<B>`：统一施加超时，组装结果 |

四条设计原则：边界在"能碰什么"不在命令字符串；默认拒绝（网络断、env 不继承、写路径空）；超时由最外层 `tokio::time::timeout` 强制，后端赖着不停也会被 kill；**降级透明**——后端做不到的约束写进 `output.degraded`，上层可据此拒绝结果。

当前用 `ProcessBackend`，它会如实上报降级（隔离没生效）。真后端（macOS `sandbox-exec` / Linux `bwrap`）还没接。`SandboxBackend::execute` 返回 `impl Future`，trait 非 object-safe，暂时不能用 `Box<dyn>` 动态注入。

**工作区** — `crates/shirley-agent-sdk/src/workspace/mod.rs`

`WorkSpace`：`new(root)` + `resolve(input)`，把相对路径解析到工作区根目录下并做越界校验。`WorkspaceError` 区分 `OutsideRoot`（越界被拒绝）和 `InvalidPath`（路径本身没法用）——模型靠这个差别决定换路径还是换写法。

---

**六、上手路径**

想跑起来：

```sh
cp .env.example .env   # 按需修改；.env 已在 .gitignore 中
cargo run
```

配置来源（优先级从低到高）：内置默认 → 全局 `<config_dir>/shirley/config.toml` → 工作区 `<root>/.shirley/config.toml` → 环境变量。最省事的是在 `.env` 里写（兼容旧习惯）：`LOCAL_BASE_URL`（缺了不阻断启动，TUI 会自动进入 `/login` 引导补齐）、`LOCAL_API_KEY`（可选，本地服务常不需要）、`LOCAL_MODEL`、`LOCAL_PROTOCOL`、`LOCAL_MODELS_URL`、`LOCAL_CONTEXT_WINDOW_TOKENS`（可选，默认 52429）。也可以写 TOML 配置文件：`[provider]` 表 + `base_url` / `api_key` / `model` / `protocol` / `models_url` / `context_window_tokens`。`DEEPSEEK_*` 那组目前没被代码引用。`bash` 工具与系统提示词都会读 `SHIRLEY_WORKSPACE`（工作区根目录，缺省为当前目录）；系统提示词还会把工作区根目录下的 `Agent.md`（项目指南）内联进去。注意 `.env` 已在 `.gitignore` 里，**里面的 key 已经泄露过一次，别提交**。

常用命令：

```sh
cargo build                        # 构建
cargo test                         # 跑全部测试
cargo test -p shirley-agent-sdk    # 只跑 SDK 测试
cargo clippy --all-targets         # 静态检查
```

测试分布（应用层约 95 个）：`markdown.rs` 13 个、`app.rs` 28 个、`ui.rs` 11 个、`command.rs` 10 个、`bash.rs` 6 个（其中 `reports_sandbox_degradation` 是既有的红测试）、`read.rs` 10 个、`session.rs` 10 个、`models.rs` 4 个、`web_search.rs` 11 个；SDK 集成测试 `error_contract.rs` / `sandbox_smoke.rs` / `tool_contract.rs` 各 6 个、`session_contract.rs` 7 个、`recall_contract.rs` 5 个、`tool_lifecycle.rs` 5 个、`tool_context.rs` 5 个、`runtime_compaction.rs` 20 个、`system_prompt_contract.rs` 4 个。

---

**七、当前状态与已知缺口**

**已经能用的**：

- 完整 ReAct 循环（含流式）：`ChatCompletions` / `Responses` / `AnthropicMessages` 三协议均已适配
- 工具注册、参数 schema 生成、并发工具调用
- 上下文自动压缩（80% 阈值触发）
- usage 统计 + 缓存命中率（区分"未上报"）
- 能跑的 TUI：流式增量渲染、思考显示、工具参数展开、输入历史、滚动、压缩状态提示
- 进程沙盒框架（spec / 后端抽象 / degraded 上报 / 超时）
- 工作区路径越界校验
- 统一错误契约（`ErrorKind` / `SdkError`）
- 召回：压缩段入库 + BM25 检索 + recall 工具（AI 主动触发）
- 任务账本：`todo` 工具（模型自维护）+ 每轮末尾注入（跨压缩存活），`Agent::new` 自动注册；多会话下账本随各会话独立 `Agent` 天然隔离

**明确没做的**：

1. ~~**多协议**~~：`ChatCompletions` / `Responses` / `AnthropicMessages` **三协议均已实现**（含流式）。Anthropic 适配见 `docs/anthropic-messages-api.md`（`x-api-key` 头、`tool_result` 在 user 消息里、`input_tokens` 不含缓存需加回 `cache_read`、流式 `input_json_delta` 分片聚合；**协议差异全部收敛在适配层，`Message` 未改动**）
2. **工具参数中间层**：目前直接生成 OpenAI schema，跨协议复用不了
3. **真沙盒后端**：只有 `ProcessBackend`（无隔离），`sandbox-exec` / `bwrap` 未接
4. **记忆系统**：完全没做。`plan.md` 里给了方向——任务结束后不能直接总结入库，要先做"蒸馏验证"判断出最佳路径再沉淀
5. **权限控制 / 行为限制**：只有 bash 的硬编码黑名单 + 沙盒，没有通用的权限层（`docs/security.md` 里的 `PermissionPolicy` 还没落地）
6. **任务规划**：长任务怎么拆解、怎么跟踪进度，还没设计
7. **`apply_patch` 工具**：还没做（`plan.md` 里提到）
8. ~~未接线配置~~：`temperature` / `max_output_tokens` / `tool_choice` 已接线，并新增 `extra_body` 逃生口（见 `docs/sdk-gaps.md` gap-4）
9. **未使用的 `StopReason`**：`MaxStepsReached` / `Cancelled` 定义了但不会产生（没有 max_steps 和取消机制）
10. **压缩重试**：压缩失败直接中断，`plan.md` 提到"压缩失败重试有时能成功"
11. **recall 的技术债**（`docs/recall.md` 第八节）：无持久化（进程结束即失）；BM25 只做词面匹配（同义改写召回不了，embedding 混合召回未做，`fuse.rs`/RRF 留接口）；中文无分词器（单字切，有噪声）；工具结果统一清空丢弃了不可重建的调用（一次性快照重跑拿不到当时结果，等工具能力细分后回填 per-call 判定）。**（曾经的坑已修：初版误把工具类消息也入库 + 漏了防递归，导致召回内容雪球式膨胀——见 `docs/recall.md` 2.2 注）**

**顺手能修的**：

- `adapter::ModelfinishReaon` 拼写错误（少个 i，应为 `ModelFinishReason`）——**仍然存在**
- `chat_completions/mod.rs` 里 `encode_request` 上面那条 `todo: 这里目前没有处理工具注入逻辑，以及思考逻辑。` 注释已过时（工具和思考都处理了）
- `runtime` 里工具执行失败时把错误 `to_string()` 塞进 tool message（有 TODO 标注，说"感觉这里不太合理"）
- `bash.rs` 里 `render` 的降级提示目前被注释掉了，但单测 `reports_sandbox_degradation` 仍断言 `[沙盒降级]`——**这条测试应该是红的**，要么恢复渲染、要么改测试
- `bash.rs` 里工具错误消息格式没有统一约定（有 TODO）

---

**八、改代码前请记住**

1. **别破坏 prefix 稳定性**。工具定义排序、消息顺序、system prompt 位置，任何变动都会影响缓存命中率。UI 状态栏会显示这个数字，改完自己看一眼。
2. **`Option<u64>` 的语义是有意的**。"未上报"和"0"必须区分，别为了图省事用 `unwrap_or(0)`。
3. **压缩是不可逆的**。`ContextSummary` 一旦写入，之前的消息在 `active_messages()` 里就不参与请求了，但原始消息仍留在 `self.messages` 里（注：压缩会把 `self.messages` **整体重建**变短，原始消息只存活在召回库里）。改这块要小心 `start_index` 的语义（`RunResult.messages` 是从这里切出来的）——**循环内压缩后必须 `start_index = start_index.min(self.messages.len())` 钳制**，否则切片越界 panic（真实炸过：`range start index 10 out of range for slice of length 6`，被 async_stream 包成 `Other("task panicked...")`）。同理，任何"记录下标 + 中途重建底层容器"的模式都脆弱：`run_stream` 开头那个压缩分支不炸纯属它发生在 `start_index` 记录**之前**的顺序依赖，将来在记录之后插入任何重建 `messages` 的路径都会再踩。
4. **对外契约要保持小**。`lib.rs` 的 `pub use` 是 SDK 的门面，加东西之前先问自己：这是基础能力，还是业务逻辑？
5. **错误分类是稳定的**。`ErrorKind` 给日志/指标/重试决策用，文案可以改，kind 不能随便改；要判断重试只看 `is_retryable`，别 match 字符串。
6. **边界在沙盒，不在黑名单**。`bash` 的字符串黑名单只是临时兜底，别把它当成安全边界。
7. **`plan.md` 和 `docs/` 都是活文档**。`plan.md` 有大量"我为什么这么设计"的思考过程，比代码注释更能说明问题；`docs/` 是分期落地的方案。改架构前先读一遍。

---

最后一句，算是我的私心：

这个项目叫 Shirley，但真正驱动它的是 `plan.md` 里那句"先聚焦一个 Agent 应用场景去实现，在实现的过程中不断做决策，发现哪些能力下次还能用上"。**不要为了抽象而抽象**。我见过太多 SDK 死在"预判了所有需求"上。

有不确定的地方，去翻 `git log`。提交信息写得很清楚，从 `macro foundation` 到 `context compression` 再到 `sandbox execution framework`，每一步的意图都留下来了。
