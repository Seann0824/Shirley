**Shirley 技术方案索引**

这组文档是对当前仓库（`Agent.md` 描述的 v0.1 状态）的技术设计提案。目标不是重写，而是在现有三层结构（应用层 / SDK / 宏）上补齐"能不能放心用"的缺口，并把 `plan.md` 里已经写下方向、但还没落地的能力排出可执行的顺序。

**文档清单**

| 文档 | 解决什么 | 优先级 |
| --- | --- | --- |
| `architecture.md` | 架构图 + 每个功能落在哪些节点 | 先读这个 |
| `roadmap.md` | 总路线图：分期、依赖关系、验收口径 | 其次 |
| `security.md` | 权限层、bash 边界、密钥、工作区隔离 | P0 |
| `sandbox.md` | 进程沙盒：spec / 后端 / degraded / 超时 / 执行策略归属 | P0 |
| `runtime-hardening.md` | max_steps、重试、超时、取消、压缩健壮性 | P0 |
| `compaction.md` | 上下文压缩优化：切点压缩 + 分层保留 + recall 依据 | P0 |
| `recall.md` | 压缩后召回：分块 / BM25 / Retriever 抽象 / 工具输出清空 | P0 |
| `session.md` | 会话持久化与恢复：SessionStore 契约 / 日志派生 recall / rewind 语义 | P0 |
| `streaming.md` | 流式输出与 reasoning 流式 | P1 |
| `adapter-layer.md` | 协议适配中间层、工具参数标准化、多协议 | P1 |
| `responses-api.md` | Responses 协议适配：请求 item 展开 / 响应解码 / 流式事件 / 落地方式 | P1 |
| `sdk-gaps.md` | SDK 能力缺口修复：工具上下文注入（已落地）/ 请求体留口（已落地）/ 工具顺序保序（不做） | P0 |
| `testing.md` | SDK 单测策略、缓存命中率基准 | P1 |
| `plan.md` | 错误处理统一化：已完成状态 + 后续任务清单 | P0 |

---

**贯穿全局的设计原则**

1. **边界不渗漏**。应用层与 SDK 的边界要清：`encode_messages` 这种"内部 Message → 协议格式"的映射属于 message 侧，不属于适配层；`AgentUpdate` 这种手写重复翻译要么去掉、要么明确它只做展示裁剪。
2. **失败要可分类**。`ModelError = String` 把 HTTP 状态码、是否可重试、是不是限流全丢了。任何"要不要重试 / 要不要中断"的决策都需要结构化错误，这是运行时健壮性的前置条件。
3. **默认安全**。当前 `bash` 用 `bash -c` 执行、黑名单靠 `split_whitespace` 字符串匹配，等于没有边界。工具是 Agent 的手，手没有边界，其他所有设计都是空的。

4. **图用对工具**。文档里的图按表达对象选载体，不要为了统一而强转：目录 / 文件布局这种**层级结构用 ASCII tree**（`├──` / `└──`），mermaid 没有原生树语法，硬转成 `graph TD` 反而啰嗦且失真；**流程、依赖、数据流、时序**这类关系用 **mermaid**（架构图统一在 `architecture.md` 开头放一份图例，说明方框 / 实线 / 虚线 / 菱形 / `subgraph` 的语义）。判断标准：读者是要"看结构"还是要"看流向"——前者 ASCII，后者 mermaid。

沙盒层的落地设计见 `sandbox.md`：它把"默认安全"从"静态拒绝危险命令"推进到"物理上跑不出假世界"，是 `bash` 边界的下半场。

---

**当前状态基线（核对代码后的实测）**

- 代码量：`src` + `crates` 共 2809 行
- 测试：3 个（全部在 `read` 工具），SDK 侧 0 个
- clippy：36 条警告（`shirley-agent-sdk` 32 条 + 应用层 4 条）
- 已知 panic 点：**已清除**。`codec` / `invoke` 的未实现协议改为返回 `Err(UnsupportedProtocol)`（见 `docs/runtime-hardening.md`）
- 未接线配置：**已接线**。`temperature` / `max_output_tokens` / `tool_choice` 进入请求体，并新增 `extra_body` 逃生口（见 `docs/sdk-gaps.md` gap-4；`request_timeout` 字段已移除）
- 未使用的 `StopReason`：`MaxStepsReached`、`Cancelled`（`Completed` 是当前唯一会被产生的值）
- 已落地（写方案时尚未做）：流式输出（`stream` 配置 + `ContentDelta` / `ReasoningDelta` 事件 + SSE 增量解析）、TUI 直接消费 `AgentEvent`
