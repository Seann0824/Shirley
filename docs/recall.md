**Shirley 技术方案 · 召回**

这份文档把压缩丢失信息的"退路"落地成方案。它承接 `compaction.md` 第五节
（`<compacted_range>` 钩子）与 5.5 节的定位——论文（Tirmazi et al., *Context Compaction Theory*,
arXiv:2608.01326v1）把"精确信息在摘要中必然丢失"证明为信息论结论，因此 recall 不是
锦上添花，而是压缩方案闭环的下半场。

**范围声明**：本轮只做内存实现，持久化层留空接口。检索算法只做 BM25，
但架构上必须保证后续能无痛引入 embedding 做混合召回。

---

**一、两个核心决策（先定调）**

**决策 1：恢复路径二分——"重建"与"召回"分离，由 AI 判断走哪条。**

压缩丢的信息分两类，恢复方式完全不同：

| 信息类型 | 例子 | 恢复方式 | 谁判断 |
| --- | --- | --- | --- |
| 世界可再生的 | 文件内容、命令输出、代码结构 | **重建**：AI 重新调 bash / 未来的 read | AI 看到具体调用后判断 |
| 对话性的 | 用户说过的话（"我叫什么"）、已确认的决策、约定 | **召回**：AI 调 recall 工具检索历史 | AI |

关键认知：**"能不能重建"不是工具属性，是调用属性**。`bash -c "cat x"` 可重建，
`bash -c "git commit"` 不可重建——同一个工具，不同调用。因此不做 per-tool 的
`replayable` 静态标志，让 AI 在上下文里看到具体调用后自己决定。
这个判断天然发生在 AI 层，比工具自报准确。

由此推出存储侧的分工：
- **工具类消息（Assistant{tool_calls} + Tool 结果）→ 不入召回库**。压缩时统一清空
  Tool.content（见决策 3），AI 走重建路径。
- **对话类消息（User / Assistant 文本）→ 入召回库**。这些是重建不出来的。

**决策 2：索引自动，检索由 AI 触发。**

- **索引是自动的**：`compress_context()` 把被压掉的对话段分块后送进召回库，外部无感。
- **检索是 AI 主动的**：recall 作为一个工具暴露给模型，AI 生成 query、自己决定何时调用。
  不做每轮自动检索——自动检索会召回一堆无关内容污染上下文，而 AI 触发天然带"相关性
  判断 + 可迭代重搜"（结果不对就换 query 再搜），这是比任何阈值都强的确认机制。

**AI 怎么"感知到自己忘了"**：靠 `<compacted_range>` 钩子。AI 不需要知道具体忘了什么，
只需要知道"有一段被压缩过、对话性信息可召回"。当用户问"我早上吃的什么"而 summary 里没有，
AI 读到这个标记就会想到调 recall。因此这个标记必须随 `ContextSummary` 每轮都在上下文里。
模板措辞需更新（见第六节）。

---

**二、分块策略（chunking）**

**2.1 为什么不能以单条消息为文档单位**

`Message` 模型里，"reason → act" 不是两条消息，而是同一条 `Assistant` 的两个字段
（`reasoning_content` = reason，`tool_calls` = act），"observation" 是独立的 `Tool` 消息，
靠 `tool_call_id` 关联。以单条消息为块的问题：

- 召回孤立 `Tool` 消息 → 孤儿 tool_call，注入回上下文时 API 会 400
  （与 `plan_cut` 的 tool 配对修正同类问题）；
- `Assistant` 内部横跨 reason/act 两个语义，拆开就丢上下文。

**2.2 chunk 定义**

```
Chunk = UserChunk        一条 User 消息，独立成块
      | StepChunk        一个完整 ReAct 步：
                          Assistant{reasoning_content, content, tool_calls}
                          + 其配对全部 Tool 结果（按 tool_call_id 聚合，支持并发多工具）
```

配对规则：`StepChunk` 的 observations 必须包含父 `Assistant.tool_calls` 的全部 id。
**注入时 chunk 是原子单位**，不拆开。

**`ContextSummary` 不入库**（它是压缩产物，且被 `background_len` 当背景前缀）。

**2.3 字段级索引视图（chunk 的可检索投影）**

注意本轮的存储分工（决策 1）：StepChunk **不入库**，因此字段级视图只对 UserChunk 与
Assistant 文本有意义。但规则先定下来，未来若回填工具结果入库时直接复用：

| 字段 | 入索引 | 理由 |
| --- | --- | --- |
| `User.content` | 是 | 任务锚点，主要检索对象 |
| `Assistant.content` | 是（中权重） | 正式回复与结论 |
| `Assistant.tool_calls[].name + arguments` | 是 | "做了什么"的锚点（未来工具入库时） |
| `Tool.content` | 截断后索引（未来） | 长输出（build 日志几万 token）会垄断 BM25 长度归一化 |
| `Assistant.reasoning_content` | **否** | 过程性思维，"我需要/让我看看"类低信息措辞会污染 IDF |

**索引视图 ≠ 注入内容**：索引前可截断（长输出保留头尾 + 报错行），注入时按需取原文。
召回结果**绝不二次摘要**——重新摘要等于二次损失，等于白召回。

---

**三、BM25 检索**

**3.1 BM25 用在消息召回上合适吗？—— 合适，前提是正确重述**

BM25 天生是"一个 query 对 N 个文档排序"的跨文档算法：IDF 依赖语料集合，
长度归一化（参数 b）依赖平均文档长度，词频饱和（参数 k1）作用于单文档内部。
把它用于消息召回，就是把场景重述为标准检索问题：
**query = AI 生成的检索词，documents = chunk 集合，输出 = top-k**。

这个重述在本场景格外自然：

1. **消息天然是离散文档**，边界清晰（chunk 化后更是如此）；
2. **IDF 正是想要的信号**：coding agent 历史里 `error`/`file` 到处都是（低 IDF），
   具体符号名、报错码、文件路径才是高 IDF 的定位信号；
3. **零依赖、纯内存、可解释**，与"本轮不持久化"的约束吻合。

**3.2 已知短板（承认并记录，不假装解决）**

- **只做词面匹配**：历史写"我叫夏莉"、query"我的名字"能命中；历史写
  "call me Shirley"就命中不了。这是 embedding 要补的洞（第八节）。
- **长度归一化失真**：chunk 长度差异极端（5 token 的 User vs 几万 token 的 tool 输出）。
  缓解：长内容索引前截断；`b` 参数取偏低值（如 0.5）减弱长度惩罚。
- **中文**：无分词器，CJK 按单字切（unigram），ASCII 按词切——与 `token` 模块
  的字符分档思路一致。够用，有噪声，记为技术债。

**3.3 参数候选**

| 参数 | 候选值 | 说明 |
| --- | --- | --- |
| `k1` | 1.2 | 经典默认，TF 饱和适中 |
| `b` | 0.5 | 低于经典 0.75，缓解 chunk 长度差异过大的失真 |
| `k`（top-k） | 5 | 返回给 AI 的候选数，AI 自己筛 |
| 截断阈值 | 2000 字符 | 索引视图对单字段的截断上限 |

参数应可配置（builder），上表是默认值。

---

**四、架构与模块**

**4.1 模块布局**

```
crates/agent-sdk/src/recall/
├── mod.rs        门面：RecallStore + Retriever trait（持久化层留空）
├── chunk.rs      分块策略（UserChunk / StepChunk、配对聚合、索引视图）
├── bm25.rs       BM25 实现（实现 Retriever）
├── tokenize.rs   分词（CJK 按字符、ASCII 按词）
└── fuse.rs       多 retriever 融合（RRF）—— 未来 BM25 + embedding 用，本轮只留接口
```

**4.2 核心契约（进 SDK 公开面）**

```rust
/// 检索器抽象：BM25 现在实现，embedding 以后实现。
/// 门面只依赖此 trait，上层完全不知道底下是什么算法。
pub trait Retriever: Send + Sync {
    fn retrieve(&self, query: &str, k: usize) -> Vec<ScoredChunk>;
}

/// 召回存储：索引（压缩时自动调用）+ 检索（recall 工具调用）。
/// 持久化实现本轮不写；trait 预留 `flush` / `load` 的位置。
pub struct RecallStore { /* chunks + Vec<Box<dyn Retriever>> */ }
```

`RecallStore` 由 `Agent` 内部持有（`Arc` 共享给 recall 工具），构造 `Agent` 时自动注册
`RecallTool` 进 `ToolManager`——**应用层完全无感**，符合"召回是 compaction 的自然配套
能力"的定位。

**4.3 recall 工具签名**

```rust
#[tool(description = "检索被压缩掉的历史对话。当当前上下文中缺少用户之前提过的
  信息（说过的话、约定、决策）且无法通过重新执行工具重建时调用。
  query 用你自己的措辞描述要找的内容。")]
pub async fn recall(
    #[param(description = "检索词")] query: String,
    #[param(description = "返回条数，默认 5")] k: Option<usize>,
) -> Result<String, ToolError>
```

要点：
- **query 由 AI 生成**（决策：不替 AI 构造 query，少一个不确定性来源）；
- **返回无损**：chunk 原文 + 元信息（类型 / 序号 / 相对时间），不二次摘要；
- **防递归**：recall 自己产生的 `Tool` 消息，将来被压缩时**不入召回库**（自我索引会循环）；
- 副作用：recall 会以 `ToolStarted/ToolFinished` 出现在 UI——用户能看到"AI 在回忆"，
  这是可观测性上的好事，接受。

**4.4 数据流（与现有 runtime 的接合点）**

```
compress_context():
    plan_cut → split 出 to_compress
    → 分块：
        UserChunk / Assistant 文本 → recall.index() → 丢弃原文
        StepChunk → 保留骨架，Tool.content 清空为占位标记（见第五节）
    → rebuild（现状逻辑不变）

run 循环：不变。AI 想召回时自己调 recall 工具，走正常工具执行路径。
```

---

**五、工具输出的统一清空（v0 有意的技术债）**

v0 极端化决策：**压缩时对所有 Tool.content 统一替换为占位标记**，不管可不可重建。

占位标记（写进 SDK 契约，措辞要稳定）：

```
[工具结果已省略以节省上下文；如仍需要，请重新执行调用获取当前状态]
```

注意**不能留空字符串**——模型会把空 content 误读为"执行成功但无输出"，而不是
"结果被省略了"。占位标记是给模型的显式信号。

**为什么这是债，不是方案**：
- 不可重建的调用（`git log` 快照、一次性输出、外部副作用）重跑拿不到当时的结果；
- 重跑 ≠ 还原：重跑 `cargo build` 拿到的是**另一个时间点的世界**。对 coding agent
  常常是优点（要的就是最新状态），但必须清楚它和"还原当时的结果"是两回事；
- 副作用风险：被清空的是 `git commit` 时，模型重跑可能提交两次。这不是召回层能解决的，
  依赖未来落地的 `PermissionPolicy`（`security.md`）。

**回填路径**：等工具能力细分（`read`/`grep` 类纯读工具回归）后，把"可重放判定"从
"统一清空"细化为 per-call 的 AI 判断或 per-tool 声明，不可重建的工具结果再考虑入库。

---

**六、改动清单**

| 改动 | 位置 | 说明 |
| --- | --- | --- |
| 新增 `recall` 模块 | `crates/agent-sdk/src/recall/` | 5 个文件，见 4.1 |
| `Retriever` / `RecallStore` 导出 | `lib.rs` | 检索器抽象进公开面（基础能力） |
| `Agent` 持有 `recall` + 自动注册工具 | `runtime/agent.rs` | 构造时 `Arc<RecallStore>` + `RecallTool` |
| 压缩时分块入库 | `runtime/agent.rs` `compress_context` | to_compress → chunk → index |
| 工具输出统一清空 | `runtime/agent.rs` `compress_context` | Tool.content → 占位标记 |
| `COMPACTION_TEMPLATE` 措辞更新 | `runtime/compaction.rs` | `<compacted_range>` 加 recall 引导（见下） |
| `tokenizer` 复用 | `recall/tokenize.rs` | 与 `token` 模块的 CJK/ASCII 分档对齐 |

`<compacted_range>` 新措辞：

```
<compacted_range>此前对话已被压缩，精确细节不在本摘要中。
需要时：文件内容、命令结果可重新读取或重新执行；
用户曾说过的话、约定与决策请调用 recall 工具检索。</compacted_range>
```

注意配套修改 `compaction.rs` 的模板单测（`template_marks_compacted_range_as_lossy`
等断言依赖具体措辞）。

**缓存影响**：recall 作为新工具进 definitions 会让 prefix 缓存 miss 一次，之后稳定
（`definitions()` 按名排序保证 prefix 稳定）。可接受，验收时核对。

---

**七、验收**

1. 构造长对话触发压缩后，用户问"我之前说过我叫什么"，AI 调用 recall 工具并正确回答。
2. recall 返回的 chunk 内容与压缩前原文**逐字一致**（无损，不二次摘要）。
3. 压缩后的请求体中，每个 `Tool` 消息的 content 都是占位标记，且 tool_call 配对完整（无孤儿）。
4. 压缩后 `self.messages` 里不存在任何未清空的旧 Tool 输出。
5. StepChunk 的 chunk 化正确处理并发多工具：一条 Assistant + N 条 Tool 聚为一个块。
6. BM25 冒烟：索引 10 个 chunk，中英混合 query 均能命中预期 top-1。
7. recall 产生的 Tool 消息不进入召回库（防递归）。
8. 压缩后首轮缓存 miss，后续轮命中率恢复（footer 核对）。
9. `Retriever` trait 可被第二个实现替换（测试里写个 stub retriever 注入）——验证可扩展性。

---

**八、不做的事 / 开放问题**

- **不做持久化**：内存 `RecallStore`，进程结束即失。trait 预留位置，方案另设计。
- **不做 embedding / 向量检索**：`Retriever` trait + `fuse.rs`（RRF）留好接口，
  引入时加一个实现 + 一个融合器，不动契约。BM25 的词面短板（同义改写召回不了）
  在此之前持续存在。
- **不做自动检索**（每轮请求前自动跑 BM25）：AI 触发是本轮决策；若将来发现
  AI"意识不到该召回"的案例频发，再评估。
- **不做 AI 生成 query 的质量优化**：v0 直接用模型的原始措辞当 query，
  query 改写 / 扩写留给未来。
- **不做工具结果入库**：统一清空是有意决策（第五节），回填依赖工具能力细分。
- **不做 rerank**：阈值过滤、元数据过滤都不做——AI 自己看结果筛选，比检索器猜更可靠。
