**Shirley 技术方案索引**

这组文档是对当前仓库（`Agent.md` 描述的 v0.1 状态）的技术设计提案。目标不是重写，而是在现有三层结构（应用层 / SDK / 宏）上补齐"能不能放心用"的缺口，并把 `plan.md` 里已经写下方向、但还没落地的能力排出可执行的顺序。

**文档清单**

| 文档 | 解决什么 | 优先级 |
| --- | --- | --- |
| `architecture.md` | 架构图 + 每个功能落在哪些节点 | 先读这个 |
| `roadmap.md` | 总路线图：分期、依赖关系、验收口径 | 其次 |
| `security.md` | 权限层、bash 边界、密钥、工作区隔离 | P0 |
| `runtime-hardening.md` | max_steps、重试、超时、取消、压缩健壮性 | P0 |
| `streaming.md` | 流式输出与 reasoning 流式 | P1 |
| `adapter-layer.md` | 协议适配中间层、工具参数标准化、多协议 | P1 |
| `testing.md` | SDK 单测策略、缓存命中率基准 | P1 |

---

**贯穿全局的三条设计原则**

1. **边界不渗漏**。应用层与 SDK 的边界要清：`encode_messages` 这种"内部 Message → 协议格式"的映射属于 message 侧，不属于适配层；`AgentUpdate` 这种手写重复翻译要么去掉、要么明确它只做展示裁剪。
2. **失败要可分类**。`ModelError = String` 把 HTTP 状态码、是否可重试、是不是限流全丢了。任何"要不要重试 / 要不要中断"的决策都需要结构化错误，这是运行时健壮性的前置条件。
3. **默认安全**。当前 `bash` 用 `bash -c` 执行、黑名单靠 `split_whitespace` 字符串匹配，等于没有边界。工具是 Agent 的手，手没有边界，其他所有设计都是空的。

---

**当前状态基线（写方案时的实测）**

- 代码量：`src` + `crates` 共 2534 行
- 测试：3 个（全部在 `read` 工具），SDK 侧 0 个
- clippy：17 条警告（`agent-sdk` 14 条 + 应用层 3 条）
- 已知 panic 点：`adapter::codec` 的 `_ => todo!()`
- 未接线配置：`request_timeout`、`GenerationConfig.temperature`、`GenerationConfig.max_output_tokens`
- 未使用的 `StopReason`：`MaxStepsReached`、`Cancelled`
