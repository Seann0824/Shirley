**Shirley 技术方案 · 多会话并行**

这份文档承接 `session.md`。`session.md` 第八节解决的是"**拥有并切换**多份会话"——
同一时刻只有一个会话在跑，切换即把 `Agent` 从旧会话挪到新会话（串行）。
本文档把这一步推进到"**多个会话同时进行**"：多个 `Agent` 各自 `run_stream`、
互不阻塞，前台只看一个、后台继续跑。

**范围声明**：本轮只定"多会话并行"的架构与接缝，不改 `Message` / `AgentEvent`
的语义（事件**字段追加**允许，变体语义不变）。持久化层只在"同一份会话被多个
驱动同时写"时才需要新锁；**不同会话并行不需要动持久化**。

---

**一、先把"多 session 同时进行"拆成三档（先定调）**

这个词有歧义，不同解释的改动量差一个数量级。先定边界，避免把三件事混成一件：

| 档 | 含义 | 是否本文档范围 |
| --- | --- | --- |
| **A. 多会话并行跑** | 同一时刻多个 `Agent` 各自 `run_stream`，互不阻塞 | **本文档主体** |
| **B. 同会话多驱动** | 同一份会话日志被多个驱动同时读写 | 本文档给出边界，**不实现** |
| **C. 后台挂起 + 前台切换** | 只有前台会话在跑，其它挂起 | 现状已接近，本文档顺带收编 |

**当前状态是 C 的雏形**：`/session` 能切换，但同一时刻只能有一个 `Agent` 在跑
（TUI 用 `take_agent` 把 `Agent` 移出，切换时会话操作直接报"agent 正在运行中"）。
本文档按 **A** 展开。

> **落地状态（本仓库当前代码）**：**P1 + P2 事件通道部分已落地**（决策 4 / 5 / 9，
> 以及决策 2 的"每会话一条通道"）。
> 应用层 `src/interface/session.rs` 已有 `SessionManager` + `Session`（每会话自持
> 独立 `Agent`）；`src/bootstrap.rs` 已从 `Bootstrap`（产单个 `Agent`）改为
> `AgentFactory`（`build_agent(messages)` 工厂）；SDK 侧 `Agent::switch_session` 已移除，
> TUI 与 desktop 均改为经 `SessionManager.active` 指针切换。
> **每会话一条通道已落地**：`Session` 自持 `events: broadcast::Sender<Result<AgentEvent, String>>`
> （决策 2 路线甲，用 `broadcast` 而非 `mpsc`——desktop 允许"重连订阅"，
> `broadcast` 天然支持多订阅者 + `Lagged` 后靠重发快照对齐）；累加逻辑收敛到
> `Session::apply_event`，**TUI 与 desktop 共享同一条事件处理路径**；`SessionManager`
> 提供按会话名的 `begin_turn` / `restore_agent` / `apply_event` / `subscribe` /
> `snapshot`，desktop 的 `agent_send(session)` 按名驱动、`agent_subscribe` 同锁内
> `subscribe` + `snapshot`（不丢不重）。**仍未落地**：决策 3 的 SDK 取消接缝、
> 决策 7/8 的最大并行上限与排队、决策 6 的 B 档持久化锁。

**决策 1：多会话 = 多个独立 `Agent`，不是"一个 Agent 分时"。**

理由在 SDK 的类型里已经写死：

- `Agent::run_stream(&'a mut self, ...) -> Stream + Send + 'a` **借用** `self`，
  单个 `Agent` 在类型层面就无法被并发驱动（借用检查器强制）。
- `todo` 在 `Agent::new` 内部 `Arc::new(...)`，**每个 `Agent` 各一份**——
  多 `Agent` 并行时任务账本天然隔离，不会串会话。
- 全仓无全局可变状态（`static` / `OnceLock` / `thread_local` 只命中测试锁）。

所以"多会话并行"的正确形态是**每会话一个 `Agent`**，而不是克隆或分时复用。
这也意味着 `Agent::switch_session`（SDK 侧那个串行切换接缝）**不该存在**：
"哪个会话活跃"是应用层编排，SDK 不该有"当前会话"这个概念（见第七节决策 9）。

---

**二、SDK 现状：基本就绪，只缺两处**

实地核对后的结论：**SDK 层几乎不用改**。

**已经就绪的**：

- `Agent` 可独立构造、无全局态、`Send`；
- `todo` 按实例隔离（`Agent::new` 内 `Arc::new`），并行不串；
- `trait SessionStore: Send + Sync`（**现已归应用层 `src/session.rs`**），
  `JsonlSessionStore` 内部 `Mutex<File>`，单实例 `append` / `load` / `truncate` 互斥；
- `ToolManager` / `ToolContext` 随 `Agent` 构造，`ToolContext` 是
  `Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>`，`Tool: Send + Sync`。

**唯一的两个缺口**：

| 缺口 | 位置 | 影响 |
| --- | --- | --- |
| **事件无来源标识** | `runtime/event.rs` 的 `AgentEvent` 每个变体都不带 session / run id | 多会话并行时，消费方无法区分"这个 `ContentDelta` 属于哪个会话" |
| **无取消接缝** | `run_stream` 没有 abort；`StopReason::Cancelled` 定义了但不产生 | 无法单独取消某一个会话；现在的"打断"靠调用方 drop 流，且只能作用于"当前跑的那个" |

**决策 2：事件来源标识由"每会话一条通道"解决——通道归 `Session` 自己持有，
不改 `AgentEvent` 变体。**

两条路线：

- **路线甲（采用）**：每个 `Session` 自持一条独立 `mpsc` 通道，`run_stream`
  的事件天然按通道隔离；消费方（TUI / desktop 主循环）从各 `Session` 的
  `rx` 收事件，无需在事件里塞来源标识。
- **路线乙（弃用）**：给 `run_stream` 传一个 `run_id`，每个事件携带它。

选甲的理由：`AgentEvent` 是对外契约（`docs/README.md` 原则 4：契约要保持小），
来源标识是**调用方的编排信息**，不是 SDK 的基础能力；用通道隔离还能顺带把
"谁在跑"的调度权留在应用层。只有当**同一个 `Agent` 需要被多路消费**时才需要乙，
而决策 1 已排除这种形态。

**通道归属（原待决 1，已定）：`Session` 自持 `tx`，`SessionManager` 不持有通道。**
理由：与"每个 `Session` 自己持有 `Agent`"对称——事件通道是会话的私有管道，
不该外泄给编排层；`SessionManager` 只存 `JoinHandle`（回收 / 取消用）。
"用一个 `select!` 同时收所有会话事件"的诉求，由主循环轮询各 `Session.rx`
（或 `FuturesUnordered`）满足，不必把通道上交。**路线乙不落地。**

**决策 3：取消接缝做成"`run_stream` 的一个可选取消信号"。**

现有 `StopReason::Cancelled` 是死代码，多会话并行几乎必然要用它（后台会话要能
被单独叫停）。最小侵入：给 `run_stream` 增一个重载 / 新方法，接受一个
`tokio_util::sync::CancellationToken`（或 `oneshot::Receiver<()>`），
在流的 `select!` 里响应取消、`drop` 掉内部请求、以 `Cancelled` 结束本轮。

- 这与 TUI 现状（`run_agent` 里 `select! { _ = &mut cancel => break }` + `drop(stream)`）
  **同一手法**，只是把它从"应用层的手工包装"提升为"SDK 的正式接缝"，使其可寻址。
- 不引入新变体：取消后 `Finished(RunResult { stop_reason: Cancelled, .. })`，
  现有消费方已能处理（`StopReason` 早就有这个值）。

> 若第一步只想做 MVP，可先不碰 SDK：应用层继续用"drop 流"取消，代价是
> **无法精确寻址到某个后台会话**。取消接缝作为第二步补。

---

**三、应用层现状：单例状态模型是真正的阻塞点**

两个界面都把"单个 `Agent` + 单份 UI 状态"写死了：

**TUI**（`src/interface/app.rs` 的 `App`）：

```
agent: Option<Agent>              // 单个 Agent
items: Vec<Item>                  // 单份消息列表
scroll / auto_scroll / max_scroll // 单份滚动状态
last_usage / total_usage          // 单份用量统计
context_usage: Option<(u64,u64)>  // 单份上下文占用
current_session: Option<String>   // 单个当前会话名
ui_turn_start / turn_prompt       // 单份"本轮"状态
```

`tui.rs` 主循环用 `take_agent()` → `tokio::spawn(run_agent(...))` → `restore_agent()`
的**串行**手法，`run_agent` 结果经**单条** `mpsc` 回传。

**Desktop**（`src/interface/desktop/shell.rs` 的 `DesktopState`）：

```
agent: Arc<Mutex<Option<Agent>>>
current_session: Arc<Mutex<Option<String>>>
```

`agent_send` 里 `slot.lock().await.take().ok_or("agent 正在运行中")?`——
**第二次并发调用直接拒绝**。事件走**单一全局** `EVENT_NAME = "agent://event"`，
前端无法区分来源。

**决策 4：应用层引入 `SessionManager`（全局唯一）+ `Session`（每会话一个），
每个 `Session` 自己持有 `Agent`。**

`SessionManager` 是应用层新增的**唯一编排入口**——它把"会话从哪来"
（`SessionCatalog`）、"每个会话跑到哪了"（`Session`）、"前台是谁"（`active`）
三件事收在一处：

```
SessionManager                          // 应用层，全局唯一
├── sessions: HashMap<SessionId, Session>
├── active: SessionId                   // 前台是谁（纯 UI 指针，不重建 Agent）
├── catalog: Arc<dyn SessionCatalog>    // 会话从哪来（已存在）
├── build_agent: fn(Vec<Message>) -> Agent   // 工厂（决策 5；先 load 再建 Agent）
└── max_parallel / 待运行队列            // 调度（决策 7 / 8）

Session                                 // 一个会话 = 一个运行时容器
├── id: SessionId                       // 稳定标识（复用 SessionEntry.name）
├── agent: Agent                        // **该会话独占**，别的会话碰不到
├── items: Vec<Item>                    // 该会话的消息条目
├── usage / context_usage               // 该会话的用量
├── scroll / ui_turn_start / turn_prompt
├── state: RuntimeState                 // Idle | Running
└── events: broadcast::Sender<...>      // 事件回传通道（决策 2 路线甲，实为 broadcast）
```

**为什么身份住在 `Session` 外层、`Agent` 保持匿名**：若 `Agent` 也持有一个 id，
就出现"`Agent` 里一个、`Session` 里一个"两个真相源。工厂 `build_agent` 造出
**匿名** `Agent`，塞进 `Session` 时贴一次 `id`——身份在容器上，不在里面。

`App` 的职责从"单份状态"上移为 **`SessionManager`**（`App` 退化为"当前 `Session` 的视图"）；所有现有方法
（`append_streaming_delta` / `record_usage` / `scroll_lines` / `take_agent` …）
要么带 `SessionId` 参数，要么改为"在 `manager.active` 会话上操作"。

> **落地做法（与原设计的偏差，均为实现细节）**：
> - `App` 保留 `sessions: SessionManager` 字段，并 `impl Deref/DerefMut<Target = Session>`，
>   把 `self.items` / `self.agent` / `self.waiting` … 透明解析到 `sessions.active()`——
>   于是既有的几十个方法**零改动**，语义正是"在 `manager.active` 会话上操作"。
>   真正的 per-session 字段（含 `name`）住在 `Session` 上。
> - **通道归属已按决策 2 落地**：`Session` 自持 `events: broadcast::Sender<...>`，
>   `SessionManager` 不持有通道。通道元素是 `Result<AgentEvent, String>`——把
>   `AgentError` 也当一条事件（`Err`）发出去，累加方（`apply_event`）据此置错。
>   容量 `EVENT_CHANNEL_CAPACITY = 256`；订阅者落后会 `Lagged`，订阅方（desktop
>   pump）跳过、靠重发快照重对齐。
> - `App` 的累加方法（`append_streaming_delta` / `record_usage` / `start_compression` …）
>   已**下移到 `Session`**，`App` 只留薄包装（`App::apply_event` 转发到
>   `sessions.active_mut().apply_event` 并失效 `message_cache`）。`message_cache`
>   是纯渲染缓存、留在 `App`（不进 `Session`）。
> - **`Agent` 的 take / restore 已改为按会话名**（`begin_turn(name, user_message)` /
>   `restore_agent(name, agent)`）：desktop 的 `agent_send(session)` 按名取，修复了
>   "后台会话运行时前台已切走、事件/Agent 错记到别的会话"的串会话 bug。
> - **`begin_turn` 同时乐观记入用户消息**（= 取 `Agent` + `push_message(Role::User)`，
>   原子）：`Session::apply_event` **有意忽略** `MessageAdded(User)`——用户消息由**驱动方**
>   记录（TUI 在 `App::submit` 里记，desktop 在 `agent_send` 里记），而非事件流。此前
>   desktop 漏记，`Session.items` 永不含用户消息，切走再切回按快照重建时就"消失"了；
>   回归测试 `session::tests::begin_turn_records_user_message_into_snapshot` 覆盖此点。
> - 仍未引入 `RuntimeState` 枚举——运行态用 `Session.running: bool` 表达（`is_running(name)`），
>   够用即可；`Idle↔Running` 的完整状态机留待决策 7/8 的并行调度一起做。

**这个形状顺手消掉两处现状 hack**：

- TUI 的 `take_agent` / `restore_agent` 串行手法 → 变成 `Session.state` 的
  `Idle ↔ Running` 转移，且**每个会话各 take / 各 restore**；
- Desktop 的 `Arc<Mutex<Option<Agent>>>` 全局单槽 → 每个 `Session` 一个槽，
  "agent 正在运行中"从**全局拒绝**变成**仅该会话拒绝**。

**决策 5：`Bootstrap` 从"产一个 Agent"改为"产一个 Agent 工厂"。**（**已落地**）

原 `Bootstrap::assemble` 只产单个 `Agent`（`src/bootstrap.rs`）；现已改名
`AgentFactory::assemble`，产物是**工厂 + 共享 `session_catalog`**，多会话
**按会话造多个 `Agent`**。`build_agent(messages) -> Agent` 工厂
复用同一份 `ModelConfig` / 工具注册 / 系统提示词 / 工作目录 / 压缩指令：

- `assemble` 返回 **工厂 + 共享的 `session_catalog`**（现在 `session_catalog` 已是
  `Arc` 共享，TUI / desktop 读同一份 `<root>/.shirley/sessions`）。
- 工厂必须**每次新建 `ToolManager` 并重新注册工具**（`ToolManager` 非 `Clone`），
  但 `on_register` 钩子（如 `web_search` 的凭据注入）会各自执行一次——可接受，
  因为状态本就按工具实例隔离。
- 注意 `reqwest::Client`：现在每次 `run_stream` 内部 `Client::new()`，
  多会话并行 = 多份连接池。可在工厂里共享一个 `Client`（属于后续优化，
  非阻塞项）。

---

**四、持久化：A 档零改动，B 档要加锁**

**A（不同会话并行）**：不同会话写**不同文件**，`JsonlSessionStore` 各自的
`Mutex<File>` 足够，**零改动**。

**B（同会话多驱动）**：两个 `JsonlSessionStore` 实例指向同一文件时，
**各自持有独立的 `File` 句柄和独立 `Mutex`，无跨实例互斥** → 交错写风险；
且 `truncate` 的"写 tmp → rename → 重开追加句柄"在并发下会互相覆盖
（`src/session.rs` 的 `rewrite_locked`）。

**决策 6：本期只保证 A 档安全；B 档记为后续，需要时给 `FileSessionCatalog`
加"按会话名的进程内锁表"（`Arc<Mutex<HashMap<name, Arc<Mutex<()>>>>>`），
必要时再加跨进程文件锁。**

理由：B 档（同一份会话被多个驱动同时写）在单机单进程的 TUI / desktop 里
**不是真实场景**——一个会话在同一时刻本就只该有一个活动 `Agent`（决策 1）。
真正要防的是"两个窗口打开同一份会话"，那是**多进程**问题，锁表也解决不了，
得上文件锁。所以现在不为 B 设计，只**明确声明不支持**。

---

**五、数据流（A 档）**

```
SessionCatalog.list()  ──►  [SessionEntry...]  （目录扫描，同步）
        │
        ├─ 新建 / 打开 ──►  Arc<dyn SessionStore>
        │                        │
        │                        ▼
        │            store.load() ──► Bootstrap::build_agent(messages) ──►  Agent
        │                        │
        │                        ▼
        │            Session { id, agent, items, ... }  ──►  sessions: HashMap
        │
        └─ 切换活动指针 ──►  SessionManager.active = id  （纯 UI 操作，不重建 Agent）

用户提交 ──►  active 会话的 Agent::run_stream(task)
                │
                ▼
        每会话一条 mpsc 通道（路线甲）
                │
                ▼
        SessionManager 按 session_id 把事件写回对应 Session
                │
                ▼
        渲染只画 active 会话；后台会话照常推进（进度可显示在标签上）
```

---

**六、并行调度**

**决策 7：每会话一个 `tokio::spawn` 任务，`JoinHandle` / 回收按会话存。**

把现在的 `take_agent` / `restore_agent` 串行模式，改为每个 `Session`
各自 spawn / 回收：

- 提交时把该会话的 `Agent` 移入后台任务（`RuntimeState::Running`），
  任务结束把 `Agent` 放回（`RuntimeState::Idle`）；
- 与现状一致：一轮运行期间该会话的 `Agent` 不在 `Session` 里（被移入后台任务），**同一会话不会并发驱动**；
- 不同会话的 `Agent` 互不共享，可真正并行。

**决策 8：必须有"最大并行会话数"上限。**

多会话并行 = 多份 `reqwest::Client`（多连接池）+ 多份并发工具执行（`bash`
会真起子进程）。不加限制会打爆连接数 / 进程数 / 上游限流。建议：

- 默认上限（如 4）可配置；
- 超限时**排队**而非拒绝：新提交的会话进入待运行队列，有空位再跑；
- 对 `ErrorKind::RateLimited`（429）保持既有重试语义（`SdkError::is_retryable`）。

---

**七、与既有接缝的关系**

**决策 9（移除 `switch_session`）：`Agent::switch_session` 从 SDK 删除；
"哪个会话活跃"是应用层编排，SDK 不该有"当前会话"这个概念。**

**为什么当初会有它**：它是单会话时代的产物——那时一个 `Agent` 只服务一个会话，
`/session` 要"换会话"最省事的做法就是给 `Agent` 换一份日志源（清空 `messages` /
`todo` → 从新日志恢复 → system 置顶）。它把"应用层的编排动作"
（切换）错放进了 SDK，因为当时没有 `SessionManager` 承载编排。

**边界要划清（避免误伤）**：

- **当时保留在 SDK**：`SessionStore` 契约、`Agent.session: Option<Arc<dyn SessionStore>>`
  字段、以及"从日志恢复工作集"的恢复逻辑——当时判断"持久化是合法的 SDK 能力"。
  **后续（`docs/session.md` 决策 2 的进一步演进）**：这一层也被移出 SDK——
  会话持久化整体归应用层，`Agent` 不再持有 `session` 字段，只 `yield MessageAdded`；
  恢复改由应用层 `store.load()` 后 `build_agent(log)` 完成，落库由应用层事件驱动。
- **移出 SDK**：`switch_session` 方法 + 本轮进一步移出的 `SessionStore` 契约 /
  `session` 字段 / 恢复 / 落库 / 截尾。`switch_session` 表达的是"**换掉当前会话**"，
  而"当前会话"在 SDK 里根本不存在——每个 `Agent` 对应一份固定日志源，
  切换由 `SessionManager.active` 指针完成（见决策 4）。

**迁移面**：

- SDK 侧：删 `runtime/agent.rs` 的 `switch_session` 方法及其文档注释、
  删对应单测（`agent.rs` 的 `switch_session_clears_ledger`、
  `tests/session_contract.rs` 的两个 `switch_session_*`）；
- 应用层：TUI 的 `/session`（`app.rs`）与 desktop 的 `agent_switch_session`
  改为**由 `SessionManager` 切 `active` 指针**——每个 `Session` 已持有各自就绪的
  `Agent`，切换**不重建 `Agent`**（重建会丢失该会话正在后台跑的上下文，
  也违背"多 `Agent` 隔离"）；
- 恢复语义（**应用层 `store.load()` → `build_agent(log)`** + `system_message` 置顶）
  **原样复用**——它现在是 `build_agent` 工厂调用方的事，不再是"切换"的动作。

---

**八、边界与不变量**

- **同一会话同一时刻只有一个活动 `Agent`**（决策 1）。这是并行安全的前提：
  `todo` / 会话日志都按会话隔离，不存在跨会话共享的可变状态。
- **`AgentEvent` 变体语义不变**。多会话不需要给事件加来源标识（决策 2 用
  `Session` 自持通道解决）；将来若确需追加字段，遵守"字段追加、旧消费方
  用 `..` 忽略"的既有约定（`ToolStarted` / `ToolFinished` 当初就是这么加的）。
- **前缀缓存不受影响**：多会话各自维护自己的前缀，互不干扰；但并发请求共享
  上游限流，注意 429。
- **`RunResult.messages` 的 `start_index`** 由每个 `Agent` 各自维护，只要不共享
  `Agent` 就安全（`docs/README.md` 原则 3 提到的切片越界前提是"重建底层容器"，
  多 `Agent` 不触发）。
- **持久化只保证 A 档**；B 档（同会话多驱动 / 多进程）**明确不支持**。

---

**九、落地顺序**

1. **P1 · 应用层状态模型重构**（工作量主体）：`SessionManager`（`Session` +
   `HashMap` + `active` 指针）；**每个 `Session` 自持一条事件通道**（决策 2）。
2. **P1 · `AgentFactory::build_agent` 工厂**（决策 5）——让多 `Agent` 可被造出来；
   会话恢复由调用方先 `store.load()` 再 `build_agent(log)`。
3. **P2 · 并行调度 + 上限**（决策 7 / 8）：每会话 spawn、排队、运行态展示。
4. **P2 · SDK 取消接缝**（决策 3）——做 P2 时几乎必然需要。
5. **P3 · 从 SDK 移除 `switch_session`**（决策 9）：删方法 + 单测，应用层
   改由 `SessionManager` 切 `active`。
6. **P3 · 持久化锁**（决策 6，B 档）——A 档不需要，留到最后。

**MVP 路径**：不动 SDK 事件类型 → "每会话一条 mpsc + 每会话一个 `Agent` +
`SessionManager`"，即可实现 A 档的"多会话并行跑"；
取消接缝作为第二步补。

---

**十、验收口径**

- 同时向两个不同会话提交任务，两者都能推进，事件互不串台（各写回自己的
  `Session`）；
- 后台会话完成时，其 `items` / `usage` 正确落库，切换过去即可看到完整对话；
- 同一会话在运行中再次提交被拒绝（不并发驱动同一 `Agent`），错误文案与现状一致；
- 关闭某个后台会话 / 切换活动指针，不影响其它会话的运行；
- 超过最大并行数时新任务排队而非失败；
- 不同会话并行时，各自的任务账本 / 会话日志**互不污染**；
- （若做决策 3）取消能精确作用于指定会话，其余会话不受影响，
  `StopReason::Cancelled` 被真实产生。

