**Shirley —— 项目 Agent 指南**

这份文件写给下一个接手这个仓库的 Agent（或者人）。我尽量写得像一份能直接照着干活的手册，而不是一份宣传材料。如果你只想知道"我该从哪里开始改代码"，请直接跳到 **上手路径** 那一节。

> 这份文件描述的是**当前代码的真实状态**。如果你改了架构，请顺手把这里对齐——上一版就因为落后十几个 commit 而误导过人。

---

**一、这是什么**

Shirley 是一个用 Rust 写的 Coding Agent。名字来自《Code Geass》里的夏莉，`main.rs` 里的 system prompt 就是按她的设定写的——这既是人格测试用例，也是真实的默认人设。

整个仓库分成三层：

| 层 | 位置 | 职责 |
| --- | --- | --- |
| 应用层 | `src/` | 一个可跑的 TUI 聊天 Agent，注册工具、驱动界面 |
| SDK 层 | `crates/agent-sdk/` | 与业务无关的 Agent 基础能力：消息、工具、协议适配、ReAct 运行时、沙盒、工作区 |
| 宏层 | `crates/agent-sdk-macros/` | `#[tool]` 属性宏，把普通 async 函数变成可被模型调用的工具 |

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
| `docs/testing.md` | SDK 单测策略、缓存命中率基准 |
| `docs/plan.md` | 错误处理统一化：已完成状态 + 后续任务清单 |

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

**SDK 对外只暴露这些**（见 `crates/agent-sdk/src/lib.rs`）：

- `Agent`、`AgentError`、`AgentEvent`
- `SystemPrompt`、`SystemPromptContext`（系统提示词：静态字符串或函数）
- `CutPlan`、`CompactParts`、`plan_cut`（压缩切点/重建，供测试与上层观测）
- `Message`、`ToolCall`、`Usage`
- `ModelConfig`、`ModelProtocol`、`AdapterError`
- `ErrorKind`、`SdkError`（统一错误契约）
- `tool`（宏）、`ToolManager`、`Tool`、`ToolDefinition`、`ToolError`
- `sandbox`（`Sandbox` / `SandboxSpec` / `SandboxOutput` / `SandboxBackend` / `ProcessBackend` / `SandboxError` / `NetworkPolicy` / `Capabilities`）
- `workspace`（`WorkSpace` / `WorkspaceError`）

其余模块（`message` / `adapter` 内部 / `runtime`）是私有模块，改动时要留意不要破坏这个对外契约。

---

**三、关键模块速查**

**1. 消息模型** — `crates/agent-sdk/src/message/mod.rs`

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

**2. 统一错误契约** — `crates/agent-sdk/src/error.rs`

- `ErrorKind`：稳定的错误分类（`Transport` / `RateLimited` / `ServerError` / `BadRequest` / `Unsupported` / `ToolFailure` / `Internal`），用于日志、指标、重试决策
- `SdkError` trait：`kind()` + `is_retryable()`（默认由 `ErrorKind::is_retryable` 推导）
- 各层错误（`AdapterError` / `ToolError` / `SandboxError` / `WorkspaceError` / `AgentError`）**各自留在自己的模块**，但都实现 `SdkError`。`AgentError` 用 `#[from]` 把它们收敛进来
- 展示格式统一为 `[前缀]: 详情`，前缀留在变体旁，不从 `ErrorKind` 推导
- **决策看分类，不看文案**：要判断重试与否，只依赖 `SdkError::is_retryable`，绝不 `match` 错误字符串

**3. 协议适配层** — `crates/agent-sdk/src/adapter/`

- `ModelConfig` 用 `bon` 生成 builder：`protocol` / `base_url` / `model` / `api_key` / `request_timeout` / `stream` / `thinking` / `reasoning_effort` / `temperature` / `max_output_tokens` / `context_window_tokens`
- `codec(protocol)` 返回一对函数指针 `(Encoder, Decoder)`，目前只有 `ChatCompletions` 有实现；`Responses` 和 `AnyhtopicMessages` 返回 `Err(AdapterError::UnsupportedProtocol)`（**不再 panic**）
- `invoke()` 返回 `Stream<Item = Result<AdapterEvent, AdapterError>>`：
  - 非流式：读完整 body、decode，产出单个 `Finished(ModelResponse)`
  - 流式：走 `decode_stream_response`，逐块产出 `ReasoningDelta` / `ContentDelta` / `Finished`
- `ensure_success` 单独抽出来做状态检查，保证错误体与状态码一起保留（重试决策的唯一依据）
- `chat_completions/mod.rs` 里 `encode_messages` / `encode_tools` 做的是"内部 Message → OpenAI 格式"的转换；`dto.rs` 只做反序列化结构定义
- `request_timeout` / `temperature` / `max_output_tokens` 目前**已定义但没进请求体**（`encode_request` 只写了 model / messages / tools / thinking / reasoning_effort / stream）

已知设计债：`encode_messages` 写在适配层，但作者自己注释说"这逻辑其实该在 message 侧"。`encode_request` 里 `thinking` 现在按 `config.thinking` 映射成 `enabled` / `disabled`（**流式已实现**，不再写死）。

**4. 工具系统** — `crates/agent-sdk/src/tool/mod.rs`

- `Tool` trait：`definition()` 拿元信息，`invoke(Value)` 返回 `ToolFuture`
- `ToolManager`：注册时查重，`definitions()` **按名字排序**（这是为了让 prefix 稳定、提高缓存命中率，别随手删掉这个 sort）
- `ToolError` 分四类：`ExecutionError` / `RepetitionError` / `NotFoundError` / `ArgumentsError`

已知设计债：`ToolDefinition.parameters` 直接就是 OpenAI 格式的 JSON Schema，所以一旦要兼容 Anthropic，参数结构没法复用。`docs/adapter-layer.md` 里说得很清楚，正确做法是在中间加一层标准化的参数模型再往外转换。

**5. `#[tool]` 宏** — `crates/agent-sdk-macros/src/tool.rs`

写法：

```rust
#[tool(description = "bash 用于执行命令")]
pub async fn bash(
    #[param(description = "要执行的命令")] command: String,
    #[param(description = "超时时间（秒）")] timeout: Option<u64>,
) -> Result<String, agent_sdk::ToolError> { ... }
```

宏展开后会在同名 `mod` 里生成：`Arguments` 结构体（`Deserialize` + `JsonSchema` + `deny_unknown_fields`）、`GenerateTool`（实现 `Tool`）、`definition()`、`tool()`。调用方写 `tools::bash_tool::tool()` 注册即可。

宏的约束（踩过的坑）：

- 函数不能用 `self`，参数不能解构、不能 `ref` / `@` 绑定
- 参数描述必须写 `#[param(description = "...")]`，否则编译报错
- 生成的 `invoke` 里调用的是 `super::#name`，所以**被修饰的函数和宏生成的 mod 必须在同一层级**

**6. ReAct 运行时** — `crates/agent-sdk/src/runtime/`

这是整个项目的心脏。按职责拆成几个子模块，`mod.rs` 只负责接线与再导出：

- `agent.rs`：`Agent` 本体（构造、`run` / `run_stream` 主循环、压缩调度）
- `compaction.rs`：压缩切点与重建（`RETAIN_RATIO` / `CutPlan` / `CompactParts` / `plan_cut` / `background_len`）
- `event.rs`：对外事件与运行结果（`AgentEvent` / `RunResult` / `StopReason`）
- `error.rs`：顶层错误收敛（`AgentError` + `SdkError`）
- `prompt.rs`：系统提示词（`SystemPrompt` / `SystemPromptContext`）

`CompactParts` 的契约测试移到了 `crates/agent-sdk/tests/runtime_compaction.rs`（集成测试，只依赖公开 API）。

- `Agent::new` 是 `bon` builder：`model_config` / `system_prompt` / `working_dir` / `messages` / `tools` / `compression_instruction`
- `system_prompt` 类型是 `SystemPrompt`（不是 `String`）：既接受固定字符串（`From<String>` / `From<&str>`），也接受**函数** `Fn(&SystemPromptContext) -> String`。函数形式让提示词按运行时上下文动态生成——`SystemPromptContext` 目前携带 `working_dir`。`working_dir` 是独立 builder 参数，构造时会用它解析一次提示词
- `run()` 是 `run_stream()` 的薄封装，只等最后一个 `Finished`
- 运行参数热切换接缝（应用层指令落地用，均不重建 `Agent`）：`set_model(model)` 只换模型名；`set_provider(base_url, api_key)` 换端点与密钥（与前者对称，`/login` 用）；`switch_session(store)` 换当前会话（`/session` 用）。后两者是 SDK 为应用层指令新增的接缝，模型配置的其余字段原样保留
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

**四、应用层（TUI）**

- `src/main.rs`：读 `.env`（`dotenvy`），经 `settings::Settings::load_default()` 装载配置（**不再散读 env**，见下条），构造 `ModelConfig`（`stream(true)` / `thinking(true)` / `reasoning_effort("low")`），注册 `bash` 工具，算出 `needs_login = !settings.is_configured()`（缺配置不阻断启动），并把会话目录（`src/session.rs` 的 `FileSessionCatalog`，多会话布局 `.shirley/sessions/<name>.jsonl`）连同该标志交给 `interface::run`——启动时打开"最近修改"的会话（无则新建），旧的单文件 `.shirley/session.jsonl` 会被 `adopt_legacy` 收编（会话持久化，见 `docs/session.md`）。工作目录由 `prompt::workspace_root()` 决定（`SHIRLEY_WORKSPACE` 优先，否则当前目录）——它**刻意不属于 `settings`**（是"在哪个项目跑"的会话级信息，不是"用户是谁"的配置）
- `src/settings.rs`：**配置装载（方案 A）**。把"配置从哪来"与"怎么用"解耦，纯应用层、不碰 SDK。优先级 `内置默认 < 全局 config.toml < 工作区 .shirley/config.toml < 环境变量`。配置文件为 TOML，`[provider]` 表含 `protocol` / `base_url` / `api_key` / `models_url` / `model` / `context_window_tokens`（全 `Option`，`deny_unknown_fields`）。全局路径 `<config_dir>/shirley/config.toml`，工作区路径 `<root>/.shirley/config.toml`。环境变量层（`LOCAL_*`）放在**最后**以兼容既有 `.env` 习惯——老用户零改动。`Settings::load` 的环境读取用注入闭包（测试不打全局 env），`load_default` 才用真实 `config_dir` + 进程环境。缺 `base_url` **不再报错、不阻断启动**：`finalize` 落成空串，`Settings::is_configured()` 返回 `false`，应用层据此在 TUI 里自动进入 `/login` 引导用户补齐（见下条）。**也可写**：`save_provider(path, provider)` 先读后改再写（只覆盖传入字段、不动其它键、自动建目录、写完 chmod 600 收紧权限），是 `/login` 的落盘入口
- `src/prompt.rs`：应用侧系统提示词构造。`build(working_dir)` 返回一个 `SystemPrompt` 函数，每次解析时读取当前工作目录与工作区里的 `Agent.md`，拼成"角色 + 工作区根 + 项目指南"。工作目录明确告诉模型（`plan.md`：AI 不知道工作区就会从根目录乱找），`Agent.md` 提供项目怎么跑、代码怎么组织
- `src/session.rs`：应用侧会话存储与**会话目录**（`docs/session.md`）。`JsonlSessionStore` 把**原始 Message 全量日志**逐行落成 JSONL，实现 SDK 的 `SessionStore`（`append` / `load` / `truncate`，同步签名）。**不含 system**（恢复时现生成）、不含 chunk / BM25 索引（召回库由日志派生）。`truncate` 先写临时文件再原子替换、并重开追加句柄（注意：内部 `rewrite_locked` 需在已持锁下调用，否则非重入锁会死锁）。多会话（`/session`）：`SessionCatalog` trait（`list` / `open` / `create` / `latest` / `entry`，与 `ModelCatalog` 对称）+ `FileSessionCatalog`（一份会话 = `.shirley/sessions/<name>.jsonl`，目录扫描即列出，`adopt_legacy` 收编旧单文件）+ `SessionEntry { name, label, preview }`（`preview` 取首条用户消息）+ `EmptySessionCatalog`（`App::new` 兜底）
- `src/interface/tui.rs`：主循环用 `tokio::select!`，把 `Agent` 通过 `take_agent()` 移出去、`tokio::spawn` 到独立任务里跑，结果通过 `mpsc` 通道回传，完成后 `restore_agent()` 放回来。**直接消费 `AgentEvent`**（不再有 `AgentUpdate` 中间类型）
- `src/interface/app.rs`：纯状态机，`Item::Message` / `Item::Tools` 两种条目；`Role` 有 User / Assistant / Summary / Error；usage、context 用量、输入历史（上/下键回放）都缓存在这里；`toggle_thinking` / `toggle_tool_args` 控制展示
- `src/interface/ui.rs`：`MessageCache` 做渲染缓存（按宽度失效），`footer_line` 展示上下文占用百分比和缓存命中率（单次 + 累计）。消息正文交给 `markdown::render` 转成带样式的 `Line`/`Span`。**滚动高度必须用 ratatui 自己的换行计数**（`Paragraph::line_count`，按词边界，需开启 `unstable-rendered-line-info` feature）来累计 `row_offsets`，不能用 `line.width().div_ceil(width)` 估算——后者按字符数取整会少算中英混排长行的行数，导致 `max_scroll` 偏小、最新消息被底部输入框遮挡（已修，回归测试 `cache_row_count_matches_paragraph_word_wrap`）
- `src/interface/markdown.rs`：Markdown 渲染层。`parse` 用 `pulldown-cmark` 把 raw markdown 收敛成块级 AST（`Block`：Heading / Paragraph / Code / List / Quote / Table / Rule / Html），`render` 再把 `Block` 变成 ratatui 的 `Line`/`Span`。支持标题、粗斜体、行内代码、代码块、有序/无序/任务列表、嵌套列表（悬挂缩进）、引用、表格、分割线。错误消息不走 markdown（避免报错里的符号被当语法吃掉）。有 13 个单测覆盖各语法
- `src/interface/event.rs`：终端事件在独立线程里 `poll` + `read`，通过无界通道送给异步侧。开启括号粘贴（`EnableBracketedPaste`），粘贴内容整体作为 `Event::Paste` 送达；`Drop` 时关鼠标捕获与括号粘贴
- `src/interface/update.rs`：按键映射。`Esc`（AI 回复中 → 打断本轮；回溯编辑态 → 取消本次改动；否则退出）/ `Ctrl+C` 退出，`Ctrl+T` 切换思考显示，`Ctrl+O` 切换工具参数展开，`Enter` 提交，`Ctrl+A` / `Ctrl+E` 行首/行尾，`↑` / `↓` 历史回放，滚轮上下滚动。模型选择器面板打开时按键优先导向面板；指令候选浮层打开时 `↑↓←→` / `Tab` / `Enter` / `Esc` 优先导向浮层（其余按键继续正常编辑）。`Event::Paste` 走 `App::insert_input` 整段插入（换行归一为 `\n` 当普通字符），不触发发送
- `src/interface/command.rs`：快捷指令（slash commands）。仅作用于 TUI，**不接触 SDK**——指令要么展开成 prompt 发给 AI，要么作为本地系统消息，要么触发一个本地面板。当前有 `/init`（展开为生成 `Agent.md` 的 prompt）、`/model`（打开模型选择器）、`/rewind`（回退最后一条用户消息）、`/session`（打开会话选择器）、`/login`（分步登录：依次询问 base_url / api_key / model）。`CommandOutcome` 区分这几类；模糊匹配（fuzzy + Levenshtein）在输入 `/` 时给候选，浮层里 `↑↓` / `←→` 移动高亮、`Tab` / `Enter` 采纳当前高亮项、`Esc` 关闭浮层（不动输入）
- `src/models.rs`：模型目录。`ModelEntry { label, value, provider }` + `trait ModelCatalog`（异步 `list()`，仿 SDK `ToolFuture` 手法）+ `StaticCatalog`（兜底静态列表）+ `RemoteCatalog`（请求 OpenAI 兼容 `GET /v1/models`，失败回退兜底）。`main.rs` 从 `LOCAL_BASE_URL` 推导接口、`LOCAL_MODELS_URL` 可覆盖。`/model` 打开面板时由主循环把目录移入后台任务异步加载（不阻塞 UI）
- **历史回溯（`/rewind`，方案 A：只回退最新一条）**：直接取**最后一条**用户消息，把**原文填回输入框**（进入"编辑重发态"，输入框提示切换为「Enter 重新发送 · Esc 取消本次修改」）。只做最新一条（`docs/session.md` 一.决策 4），没有"选择"这个动作，故**没有选择面板**。进入编辑态时**输入框不再钉在底部，而是就地移动到该消息在会话中的渲染位置**——渲染层把该条消息替换成一个青色描边的输入框（上方是它之前的对话，下方是之后的对话，新消息只会把底部挤出视野、不挤压编辑框，见 `ui.rs` 的 `draw_rewind_edit`）。回车则先回退 Agent 与界面到该消息之前、再作为全新一轮发送，Esc 则直接取消本次改动（清空输入、退出编辑态，Agent/界面均不动）。被压缩掉的历史不在 `agent.messages` 里，天然不可回溯
- **多会话切换（`/session`）**：模态选择器（与 `/model` 同款），列表首项固定是「＋ 新建会话」哨兵（`SessionPicker::NEW_NAME` 为空串），其余为真实会话、每项第二行显示首条用户消息预览。确认时按哨兵分流到 `SessionCatalog::create` 或 `open`，拿到 `Arc<dyn SessionStore>` 后调 `Agent::switch_session`（SDK 接缝，与 `set_model` / `set_provider` 对称）——模型配置 / 系统提示词 / 工作目录 / 工具 / 压缩指令原样保留，只换"当前会话"。切换语义与 `Agent::new` 的恢复路径**共用 `restore_from_session`**（清空工作集与召回库 → 从新日志重建 → 现生成 system 置顶），因此切换出的工作集必然与冷启动恢复一致；配套 `RecallStore::clear()` 防旧语料污染。切换后 `App::rebuild_items_from_agent` 整体重建界面条目并重置会话绑定统计（usage / 上下文占用）。会话目录是本地扫描（同步），由主循环直接取列表打开，无需后台任务。页脚会显示当前会话名
- **分步登录（`/login`，方案 A：分步问答版）**：**未配置模型服务（缺 `base_url`）时启动会自动进入本流程**——`main.rs` 算出 `needs_login` 传入 `interface::run`，`Tui::new` 据此调 `App::start_login()`，把"缺配置"从启动错误变成 TUI 内的一次引导；用户可 Esc 取消，取消后仍可手动 `/login`。不做表单浮层，而是**复用现有输入框与 `submit` 链路**——进入后输入框被临时征用为字段编辑器，每步一次回车。顺序固定 `base_url → api_key → model`，每步以当前配置（`agent.model_config()`）预填，直接回车即"保留不变"；`base_url` 空则留在本步重问（空端点无意义），`api_key` 空表示"无鉴权"（本地服务常见，会**明确清空**旧 key，与"保留旧值"不同）。走完先**热更新 Agent**（`set_provider` + 可选 `set_model`），再**落盘**到工作区 `<root>/.shirley/config.toml`（`settings::save_provider`，先读后改再写，不动其它键，写完 chmod 600）。热更新成功但落盘失败会如实告知（不假装成功）。`Esc` 取消登录（只退登录态，不退出程序，也不改配置）。渲染层无改动——提示语走既有的 `command_hint`（`ui.rs::input_hint` 已消费），登录态下 `on_input_changed` 与回溯编辑态一样保留提示。登录流程是内存态（`App::login: Option<LoginFlow>`），未走完不落盘。测试用 `set_config_path` 注入临时路径，避免污染真实工作区
- `src/interface/app.rs`：输入编辑状态机。`insert_input` 支持粘贴多行文本（CRLF/CR 归一为 LF），光标始终落在字符边界
- `src/interface/ui.rs`：输入框按显示宽度软换行 + 保留硬换行（`wrap_input`），框高随内容增长（上限 `INPUT_MAX_LINES = 10`），超出后内部纵向滚动，保证粘贴长文本时光标可见

**已有工具**：

- `bash`（`src/tools/bash.rs`）：先做 `split_whitespace` 黑名单检查（`rm` / `shutdown` / `reboot`，这只是临时兜底），然后**走进程沙盒**执行（`SandboxSpec::new("bash").arg("-c").arg(&command)`），默认 60s 超时。沙盒结果渲染时带 stdout / stderr / 退出码 / 超时提示。**真正的边界在沙盒后端，不在黑名单**

> `read` 工具已被移除（commit `aaf0081`）。`src/tools/` 现在只有 `bash.rs`。后续计划加 `apply_patch`（见根目录 `plan.md`）。

---

**5. 召回** — `crates/agent-sdk/src/recall/`（`docs/recall.md`）

压缩丢失信息的退路。定位：**compaction 的自然配套，SDK 内部能力，应用层无感**。

| 文件 | 职责 |
| --- | --- |
| `mod.rs` | 门面：`Retriever` trait（可扩展点，BM25 现在实现、embedding 以后加）+ `RecallStore`（内存存储，持久化留空）+ `RecallTool`（手写 `Tool`，因为要持有 `Arc<RecallStore>`） |
| `tokenize.rs` | 分词：CJK 按字 / ASCII 按词。Unicode 范围与 `token` 模块**共享** `token::is_cjk_word_char`（唯一一处定义），但口径不同——记账含 CJK 标点（占体积），切词不含（标点是分隔符，入词元会污染打分） |
| `bm25.rs` | 倒排索引 + 经典 BM25 打分。`k1=1.2` / `b=0.5`（低于经典 0.75，缓解 chunk 长度差异极端的失真） |
| `chunk.rs` | 分块：`UserChunk` 独立成块；`StepChunk` = Assistant{tool_calls} + 按 id 配对的全部 Tool（原子组，支持并发多工具）。`reasoning_content` 不入索引视图（过程性思维会污染 IDF） |

核心设计（`docs/recall.md` 一、两个核心决策）：

- **重建与召回二分，由 AI 判断**："能不能重建"不是工具属性是调用属性（`cat x` vs `git commit`），AI 看到具体调用后自己决定。工具类消息**不入召回库**；对话类（User / Assistant 文本）入库。
- **索引自动，检索 AI 触发**：`compress_context` 把被压段分块入库（先索引后清空——索引视图需要原始 Tool 输出）；recall 作为工具自动注册进 `ToolManager`（`Agent::new` 里做的，`main.rs` 一行没改），AI 生成 query 自己决定何时调。不做每轮自动检索。
- **召回无损**：chunk 原文保存，返回原文 + 相关度元信息，**绝不二次摘要**。
- **工具输出统一清空**：压缩时 Tool.content 替换为占位标记（`[工具结果已省略以节省上下文；如仍需要，请重新执行调用获取当前状态]`），AI 走重建路径。这是 v0 有意的技术债（见下）。
- `<compacted_range>` 模板措辞已更新：重建路径（重新读取/执行）与召回路径（recall 工具）显式分立——这是 AI"感知到自己忘了"的钩子。

---

**五、沙盒与工作区（SDK 新增能力）**

**沙盒** — `crates/agent-sdk/src/sandbox/`

| 文件 | 职责 |
| --- | --- |
| `spec.rs` | 平台无关的执行意图：program + args、cwd、env、timeout、workspace_root、读写路径、网络策略（`SandboxSpec`） |
| `backend/mod.rs` | 后端抽象 `SandboxBackend` + 能力自描述 `Capabilities` + `SandboxError` |
| `backend/process.rs` | 裸进程后端（无隔离），仅用于开发/测试与降级基线 |
| `output.rs` | 统一结果：stdout / stderr / exit_code / timed_out / isolation / **degraded** |
| `mod.rs` | 门面 `Sandbox<B>`：统一施加超时，组装结果 |

四条设计原则：边界在"能碰什么"不在命令字符串；默认拒绝（网络断、env 不继承、写路径空）；超时由最外层 `tokio::time::timeout` 强制，后端赖着不停也会被 kill；**降级透明**——后端做不到的约束写进 `output.degraded`，上层可据此拒绝结果。

当前用 `ProcessBackend`，它会如实上报降级（隔离没生效）。真后端（macOS `sandbox-exec` / Linux `bwrap`）还没接。`SandboxBackend::execute` 返回 `impl Future`，trait 非 object-safe，暂时不能用 `Box<dyn>` 动态注入。

**工作区** — `crates/agent-sdk/src/workspace/mod.rs`

`WorkSpace`：`new(root)` + `resolve(input)`，把相对路径解析到工作区根目录下并做越界校验。`WorkspaceError` 区分 `OutsideRoot`（越界被拒绝）和 `InvalidPath`（路径本身没法用）——模型靠这个差别决定换路径还是换写法。

---

**六、上手路径**

想跑起来：

```sh
cp .env.example .env   # 其实没有 example，照 .env 的键名自己写
cargo run
```

配置来源（优先级从低到高）：内置默认 → 全局 `<config_dir>/shirley/config.toml` → 工作区 `<root>/.shirley/config.toml` → 环境变量。最省事的是在 `.env` 里写（兼容旧习惯）：`LOCAL_BASE_URL`（**必填**，缺失时报错并提示去哪配）、`LOCAL_API_KEY`（可选，本地服务常不需要）、`LOCAL_MODEL`、`LOCAL_PROTOCOL`、`LOCAL_MODELS_URL`、`LOCAL_CONTEXT_WINDOW_TOKENS`（可选，默认 52429）。也可以写 TOML 配置文件：`[provider]` 表 + `base_url` / `api_key` / `model` / `protocol` / `models_url` / `context_window_tokens`。`DEEPSEEK_*` 那组目前没被代码引用。`bash` 工具与系统提示词都会读 `SHIRLEY_WORKSPACE`（工作区根目录，缺省为当前目录）；系统提示词还会把工作区根目录下的 `Agent.md`（项目指南）内联进去。注意 `.env` 已在 `.gitignore` 里，**里面的 key 已经泄露过一次，别提交**。

常用命令：

```sh
cargo build                      # 构建
cargo test                       # 跑全部测试
cargo test -p agent-sdk          # 只跑 SDK 测试
cargo clippy --all-targets       # 静态检查
```

测试分布（应用层约 85 个）：`markdown.rs` 13 个、`app.rs` 28 个、`ui.rs` 11 个、`command.rs` 10 个、`bash.rs` 6 个（其中 `reports_sandbox_degradation` 是既有的红测试）、`session.rs` 10 个、`models.rs` 4 个；SDK 集成测试 `error_contract.rs` / `sandbox_smoke.rs` / `tool_contract.rs` 各 6 个、`session_contract.rs` 7 个。

---

**七、当前状态与已知缺口**

**已经能用的**：

- ChatCompletions 协议的完整 ReAct 循环（含流式）
- 工具注册、参数 schema 生成、并发工具调用
- 上下文自动压缩（80% 阈值触发）
- usage 统计 + 缓存命中率（区分"未上报"）
- 能跑的 TUI：流式增量渲染、思考显示、工具参数展开、输入历史、滚动、压缩状态提示
- 进程沙盒框架（spec / 后端抽象 / degraded 上报 / 超时）
- 工作区路径越界校验
- 统一错误契约（`ErrorKind` / `SdkError`）
- 召回：压缩段入库 + BM25 检索 + recall 工具（AI 主动触发）

**明确没做的**：

1. **多协议**：`Responses` 和 `AnyhtopicMessages` 返回 `UnsupportedProtocol`（不再是 `todo!()` panic，但也没实现）
2. **工具参数中间层**：目前直接生成 OpenAI schema，跨协议复用不了
3. **真沙盒后端**：只有 `ProcessBackend`（无隔离），`sandbox-exec` / `bwrap` 未接
4. **记忆系统**：完全没做。`plan.md` 里给了方向——任务结束后不能直接总结入库，要先做"蒸馏验证"判断出最佳路径再沉淀
5. **权限控制 / 行为限制**：只有 bash 的硬编码黑名单 + 沙盒，没有通用的权限层（`docs/security.md` 里的 `PermissionPolicy` 还没落地）
6. **任务规划**：长任务怎么拆解、怎么跟踪进度，还没设计
7. **`apply_patch` 工具**：还没做（`plan.md` 里提到）
8. **未接线配置**：`request_timeout` / `temperature` / `max_output_tokens` 定义了但没进请求体
9. **未使用的 `StopReason`**：`MaxStepsReached` / `Cancelled` 定义了但不会产生（没有 max_steps 和取消机制）
10. **压缩重试**：压缩失败直接中断，`plan.md` 提到"压缩失败重试有时能成功"
11. **recall 的技术债**（`docs/recall.md` 第八节）：无持久化（进程结束即失）；BM25 只做词面匹配（同义改写召回不了，embedding 混合召回未做，`fuse.rs`/RRF 留接口）；中文无分词器（单字切，有噪声）；工具结果统一清空丢弃了不可重建的调用（一次性快照重跑拿不到当时结果，等工具能力细分后回填 per-call 判定）

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
