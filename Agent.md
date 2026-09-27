**Shirley —— 项目 Agent 指南**

这份文件写给下一个接手这个仓库的 Agent（或者人）。我尽量写得像一份能直接照着干活的手册，而不是一份宣传材料。如果你只想知道"我该从哪里开始改代码"，请直接跳到 **上手路径** 那一节。

---

**一、这是什么**

Shirley 是一个用 Rust 写的 Coding Agent。名字来自《Code Geass》里的夏莉，`main.rs` 里的 system prompt 就是按她的设定写的——这既是人格测试用例，也是真实的默认人设。

整个仓库分成三层：

| 层 | 位置 | 职责 |
| --- | --- | --- |
| 应用层 | `src/` | 一个可跑的 TUI 聊天 Agent，注册工具、驱动界面 |
| SDK 层 | `crates/agent-sdk/` | 与业务无关的 Agent 基础能力：消息、工具、协议适配、ReAct 运行时 |
| 宏层 | `crates/agent-sdk-macros/` | `#[tool]` 属性宏，把普通 async 函数变成可被模型调用的工具 |

核心设计目标写在 `plan.md` 里：SDK 只沉淀"下次做 Agent 还会用到"的能力，不掺业务。当前聚焦的场景是"Coding Agent"，也就是拿它去做 TS 后端往 Rust 迁移这类长任务。

---

**二、整体架构**

数据流向大致是这样：

```
用户输入
  → Agent::run_stream(task)
      → 组装 active_messages（必要时先压缩上下文）
      → adapter::invoke 编码请求 / 解码响应
          → chat_completions 协议实现
      → 落库 Assistant 消息
      → 若有 tool_calls，并发执行工具
      → 落库 Tool 消息
      → 回到模型，直到没有工具调用
  → 产出 AgentEvent 流（UI 消费）
  → 结束时给出 RunResult
```

**SDK 对外只暴露这些**（见 `crates/agent-sdk/src/lib.rs`）：

- `Agent`、`AgentError`、`AgentEvent`
- `Message`、`Usage`
- `ModelConfig`、`ModelProtocol`
- `tool`（宏）、`ToolManager`、`Tool`、`ToolDefinition`、`ToolError`

其余模块（`message` / `adapter` / `runtime` / `tool`）都是私有模块，改动时要留意不要破坏这个对外契约。

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
- `cache_hit_rate()` 在未上报时返回 `None`，**不要把测不到当成 0% 命中**
- `Add` 实现里对 `Option` 做的是"保留已知的一侧"，不是当 0 累加
- `cache_reported_input_tokens` 用来算累计命中率的真实分母（覆盖率）

**2. 协议适配层** — `crates/agent-sdk/src/adapter/`

- `ModelConfig` 用 `bon` 生成 builder：`protocol` / `base_url` / `model` / `api_key` / `request_timeout` / `generation` / `context_window_tokens`
- `codec(protocol)` 返回一对函数指针 `(Encoder, Decoder)`，目前只有 `ChatCompletions` 有实现，`Responses` 和 `AnyhtopicMessages` 是 `todo!()`
- `invoke()` 负责发请求、查 HTTP 状态、把 body 解析成 `Value` 再交给 decoder
- `chat_completions/mod.rs` 里 `encode_messages` / `encode_tools` 做的是"内部 Message → OpenAI 格式"的转换
- `dto.rs` 只做反序列化结构定义，`openapi.yaml` 是协议参考文档

已知设计债：`encode_messages` 写在适配层，但作者自己注释说"这逻辑其实该在 message 侧"。另外 `encode_request` 里 `thinking` 写死 `disabled`、`reasoning_effort` 写死 `medium`、`stream` 写死 `false`——**流式还没实现**。

**3. 工具系统** — `crates/agent-sdk/src/tool/mod.rs`

- `Tool` trait：`definition()` 拿元信息，`invoke(Value)` 返回 `ToolFuture`
- `ToolManager`：注册时查重，`definitions()` **按名字排序**（这是为了让 prefix 稳定、提高缓存命中率，别随手删掉这个 sort）
- `ToolError` 分四类：`ExecutionError` / `RepetitionError` / `NotFoundError` / `ArgumentsError`

已知设计债：`ToolDefinition.parameters` 直接就是 OpenAI 格式的 JSON Schema，所以一旦要兼容 Anthropic，参数结构没法复用。`plan.md` 里说得很清楚，正确做法是在中间加一层标准化的参数模型再往外转换。

**4. `#[tool]` 宏** — `crates/agent-sdk-macros/src/tool.rs`

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

**5. ReAct 运行时** — `crates/agent-sdk/src/runtime/mod.rs`

这是整个项目的心脏。

- `Agent::new` 是 `bon` builder：`model_config` / `system_prompt` / `messages` / `tools` / `compression_instruction`
- `run()` 是 `run_stream()` 的薄封装，只等最后一个 `Finished`
- `run_stream()` 返回 `Pin<Box<dyn Stream<Item = Result<AgentEvent, AgentError>> + Send + 'a>>`，用 `async_stream::try_stream!` 实现
- 主循环：压缩检查 → 组装请求 → 调模型 → 落库 → 有 tool_calls 就并发跑（`FuturesUnordered`）→ 没工具调用就 `Finished` 退出
- `AgentEvent` 有：`TextDetal`（拼写错误，应为 `TextDelta`）、`MessageAdded`、`CompressionStarted/Finished`、`ContextUsage`、`ToolStarted/Finished`、`Usage`、`Finished`

**上下文压缩**（这块最容易改坏）：

- `should_schedule_compression` 在 `(input + output) * 100 >= limit * 80` 时置 `compression_pending`，用整数比较避免浮点精度问题
- `active_messages()` 找到**最后一个** `ContextSummary`，只保留开头的 system 消息 + 从 summary 开始的消息
- `compress_context()` 把压缩指令作为 system 追加，要求模型输出纯文本摘要，非空且无 tool_calls 才算成功，否则报 `CompressionError`
- 压缩失败会中断整个 stream，UI 侧会显示成错误

---

**四、应用层（TUI）**

- `src/main.rs`：读 `.env`（`dotenvy`），构造 `ModelConfig`（默认 `LOCAL_*`，模型名写死 `deepseek-v4.1-flash`），注册 `bash` 和 `read` 两个工具，交给 `interface::run`
- `src/interface/tui.rs`：主循环用 `tokio::select!`，把 `Agent` 通过 `take_agent()` 移出去、`tokio::spawn` 到独立任务里跑，结果通过 `mpsc` 通道回传，完成后 `restore_agent()` 放回来。`AgentUpdate` 是 UI 内部的精简事件类型
- `src/interface/app.rs`：纯状态机，`Item::Message` / `Item::Tools` 两种条目；`Role` 有 User / Assistant / Error；usage 和 context 用量都缓存在这里
- `src/interface/ui.rs`：`MessageCache` 做渲染缓存（按宽度失效），`status_line` 展示上下文占用百分比和缓存命中率
- `src/interface/event.rs`：终端事件在独立线程里 `poll` + `read`，通过无界通道送给异步侧，`Drop` 时关鼠标捕获
- `src/interface/update.rs`：按键映射。`Esc` / `Ctrl+C` 退出，`Ctrl+T` 切换思考显示，`Enter` 提交，滚轮上下滚动

**已有工具**：

- `bash`：有 `rm` / `shutdown` / `reboot` 黑名单，默认 60s 超时，`kill_on_drop(true)`。注意它用的是 `Command::new("bash").arg("-c").arg(&command)`，前面的 `split_whitespace` 只用于黑名单检查
- `read`：按行列读文件，返回带行号的文本 + 字节范围 + 总行数，`MAX_OUTPUT_BYTES` 是 64KB。实现上先用 `scan_lines` 扫全文拿总行数（用 `memchr` 找换行），再 seek 回起点只读需要的区间。有 3 个单测覆盖边界情况

---

**五、上手路径**

想跑起来：

```sh
cp .env.example .env   # 其实没有 example，照 .env 的键名自己写
cargo run
```

`.env` 需要的键：`LOCAL_API_KEY`、`LOCAL_BASE_URL`、`LOCAL_CONTEXT_WINDOW_TOKENS`（可选，默认 104858）。`DEEPSEEK_*` 那组目前没被代码引用。注意 `.env` 已在 `.gitignore` 里，**里面的 key 已经泄露过一次，别提交**。

常用命令：

```sh
cargo build                      # 构建
cargo test                       # 跑测试（目前只有 read 工具的 3 个）
cargo test -p agent-sdk          # 只跑 SDK 测试
cargo clippy --all-targets       # 静态检查
```

---

**六、当前状态与已知缺口**

**已经能用的**：

- ChatCompletions 协议的完整 ReAct 循环
- 工具注册、参数 schema 生成、并发工具调用
- 上下文自动压缩（80% 阈值触发）
- usage 统计 + 缓存命中率（区分"未上报"）
- 能跑的 TUI，含思考显示、滚动、压缩状态提示

**明确没做的**：

1. **流式输出**：`encode_request` 里 `stream: false` 写死。`AgentEvent::TextDetal` 定义了但没地方产生
2. **reasoning 流式**：同上，`ReasonDetail` 压根没定义
3. **多协议**：`Responses` 和 `AnyhtopicMessages` 是 `todo!()`，调进去直接 panic
4. **工具参数中间层**：目前直接生成 OpenAI schema，跨协议复用不了
5. **记忆系统**：完全没做。`plan.md` 里给了方向——任务结束后不能直接总结入库，要先做"蒸馏验证"判断出最佳路径再沉淀
6. **权限控制 / 行为限制**：只有 bash 的硬编码黑名单，没有通用的权限层
7. **任务规划**：长任务怎么拆解、怎么跟踪进度，还没设计

**顺手能修的**：

- `AgentEvent::TextDetal` 拼写错误
- `adapter::ModelfinishReaon` 拼写错误（少个 i）
- `adapter/mod.rs` 里 `use std::{time::Duration, todo}` 的 `todo` 是无用导入
- `tool/mod.rs` 里 `use crate::{Agent, AgentError, message}` 的 `Agent` 没用到
- `chat_completions/mod.rs` 里 `use crate::ModelConfig` 和下面 `adapter::` 前缀风格不统一
- `read` 工具把"文件打不开"这种错误塞进 `Ok` 的 `error` 字段返回，而不是返回 `Err`。这是有意的（让模型能看到错误并重试），但和 `bash` 的错误处理风格不一致

---

**七、改代码前请记住**

1. **别破坏 prefix 稳定性**。工具定义排序、消息顺序、system prompt 位置，任何变动都会影响缓存命中率。UI 状态栏会显示这个数字，改完自己看一眼。
2. **`Option<u64>` 的语义是有意的**。"未上报"和"0"必须区分，别为了图省事用 `unwrap_or(0)`。
3. **压缩是不可逆的**。`ContextSummary` 一旦写入，之前的消息在 `active_messages()` 里就不参与请求了，但原始消息仍留在 `self.messages` 里。改这块要小心 `start_index` 的语义（`RunResult.messages` 是从这里切出来的）。
4. **对外契约要保持小**。`lib.rs` 的 `pub use` 是 SDK 的门面，加东西之前先问自己：这是基础能力，还是业务逻辑？
5. **`plan.md` 是活文档**。里面有大量"我为什么这么设计"的思考过程，比代码注释更能说明问题。改架构前先读一遍。

---

最后一句，算是我的私心：

这个项目叫 Shirley，但真正驱动它的是 `plan.md` 里那句"先聚焦一个 Agent 应用场景去实现，在实现的过程中不断做决策，发现哪些能力下次还能用上"。**不要为了抽象而抽象**。我见过太多 SDK 死在"预判了所有需求"上。

有不确定的地方，去翻 `git log`。提交信息写得很清楚，从 `macro foundation` 到 `context compression`，每一步的意图都留下来了。
