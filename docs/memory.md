**Shirley 技术方案 · 记忆系统（memory）**

这份文档给"跨会话记住用户"落地一个方案。它是 `compaction.md` / `todo.md` 的
**跨会话延伸**：压缩解决"一轮对话装不下"，todo 解决"压缩后忘了做到哪"，
而记忆解决**"新会话从零开始、用户上次说过的事全忘了"**。

方案的外部依据是《AI Agents in Depth》第 3 章（用户记忆和知识库）的存储格式与
更新机制；反面参照是 AgentLab（`/Users/sean/Desktop/repo/AgentLab`）——它方向对
但实现偏重，只取它直观的部分、避开它踩过的坑（见第七节）。

**范围声明**：本轮只做方案与最小落地（V1），不上向量库 / 嵌入 / 图数据库。

---

**一、要解决什么**

Shirley 现在是"会话即孤岛"：`session.rs` 把每个会话的原始 Message 落成 JSONL，
恢复时重建 `Agent`，但**换一个会话、或开新会话，模型对用户一无所知**。
`plan.md` 第 224–272 行已经写下方向，痛点可以收敛成一句话：

> 用户不想每次开新会话都重新自我介绍、重新说一遍"我不喜欢 X、这个项目用 Y"。

围绕它有三条硬约束（都来自用户对上一版记忆设计的反思）：

1. **复杂度**：上一版（AgentLab 式）依赖 PG + pgvector + Neo4j + Ollama，
   还要程序化 LLM 抽取。**Shirley 要轻、零外部服务。**
2. **evidence（证据）**：记忆条目没有出处，改错了无法回查。**每条记忆要能回答
   "从哪条证据来"。**
3. **冲突处理**：靠"保留最新"或让模型猜，会不可逆地丢历史。**用户判断：冲突
   本质上就是"要加入时间去整理"。**

以及一条**体验**要求：

4. **没有异步 curator**：上一版全靠 AI 主动调记忆工具，调不调看模型心情，
   体验不稳定。**要后台自己整理，并且每次 query 自动附带相关 context。**

---

**二、核心决策（先定调）**

**决策 1：纯 Markdown + 零外部服务，先不上向量库。**

第 3 章把存储格式按"简单性递减 / 表达力递增"排成四档（Simple Notes →
Enhanced Notes → JSON Cards → Advanced JSON Cards），结论是**混合模式**：
关键少量数据用结构化卡片，大量非关键事实用简单笔记。Shirley 用 **Markdown 目录 +
YAML frontmatter** 一次覆盖两档——正文是简单笔记，frontmatter 是结构化字段。

选 Markdown 而非专用数据库，理由（第 3 章"文件系统范式"）：用户可直接读 / 改 /
删；可进 Git 版本控制、可回滚；Agent 有 `write_file` 就能自主组织；**零部署成本**。

检索 V1 用**关键词 / 主题匹配**（零依赖），V2 升级 BM25，V3 才考虑 embedding。
接口始终是 `query → 相关条目`，所以升级不动架构——**别为将来可能用不上的向量库
现在就付复杂度**（呼应 `plan.md` 的"不要为了抽象而抽象"）。

**决策 2：双层结构——"常驻概览 + 按需细节"。**

这是第 3 章的汇合点，也是"每次 query 自动附带 context"的唯一可行解：

- **常驻概览层（L0/L1）**：少量关键事实（画像 / 明确偏好 / 活跃项目）**每轮自动
  注入**，不依赖 AI 主动调用；
- **按需细节层（L2）**：条目全文与原始对话，AI 需要时用 `read_file`（或未来的
  memory 工具）取回。

为什么必须双层：第 3 章实验 3-11 的结论——**只靠常驻会因容量受限丢细节，只靠
检索会因缺全局视野发现不了跨会话关联**。上一版只做了下半层（AI 主动检索），
所以"不稳定"。

**决策 3：写入交给后台 curator，而不是等 AI 主动调工具。**

第 3 章的知识更新是**双路径**：事件触发的增量更新 + 周期触发的全量整理。
落成两个**不阻塞前台**的后台任务（决策 3 的"异步"是体验的关键）：

- **增量更新**：会话结束时，curator 读最近对话 + 现有相关记忆，产出尽可能小
  而完整的 diff；
- **定期整理**：按周期 / 阈值（新增条目数、冲突数、检索质量下降）触发，做去重
  合并 + 回原始证据核查 + 冲突场景限定。

**决策 4：冲突不删除，靠时间线表达。**

新事实用 `supersedes` 指向旧条目，旧条目标 `status: superseded`，**历史永久保留**。
这正是第 3 章 Mem0 从 v2（写入时 UPDATE/DELETE）演进到 v3（仅追加 ADD-only）的
教训：**错误的 UPDATE/DELETE 会不可逆地丢历史**。用户说"冲突处理本质是加入时间
去整理"——第 3 章对应的词是 **qualification（场景限定）**：矛盾双方若在不同时间 /
对象 / 条件下都成立，就把**各自的适用场景写进知识**，而不是二选一。

**决策 5：记忆归应用层，SDK 只留通用接缝。**

与 `todo.md` 完全同构：**"记什么、怎么注入、何时整理"是 coding agent 这个产品的
取舍**，不是通用基础能力。SDK 已有的 `ContextProvider` 接缝（`runtime/context.rs`）
足够承载注入，**不新增任何 SDK 对外类型**（符合 `Agent.md`"对外契约保持小"）。

---

**三、存储格式**

**3.1 目录布局**

```
<config_dir>/shirley/memory/        # 个人记忆（跨项目、全局）
├── core.md                         # ★ 常驻层：画像 + 明确偏好 + 活跃项目（≤ 500 tok）
├── index.md                        # 程序维护的索引页（每条一行：id / 主题 / 摘要 / 路径）
├── episodic/                       # 情景记忆：具体事件（带时间戳）
│   └── 2026-05-12-ts-migration.md
├── people/                         # 人（"张三是我的同事"）
├── preferences/                    # 偏好 / 习惯（"以后都这样"）
├── procedures/                     # 程序记忆：行为流程（"先 X 再 Y"）
└── projects/                       # 项目级记忆

<root>/.shirley/memory/             # 工作区级记忆（项目相关，随仓库走）
<root>/.shirley/sessions/*.jsonl    # 原始证据层（已有，只增不改）
```

目录分类沿用 `plan.md` 的 `episodic / people / projects / preferences / procedures`，
对齐第 3 章的认知科学三类记忆（情景 / 语义 / 程序）——`episodic` 是情景，
`preferences` / `people` 偏语义，`procedures` 是程序。

**3.2 条目 schema（frontmatter）**

```markdown
---
id: pref-rust-error-style
type: preference            # preference | fact | event | procedure
subject: rust-error-handling
created_at: 2026-05-10
valid_from: 2026-05-10      # 生效时间（时间线的关键）
supersedes: pref-rust-error-style-v1   # 取代了哪条（不删旧条目）
status: active              # active | superseded | unconfirmed
confidence: high            # high | medium | low（证据不足时 low）
scope: rust / 领域错误处理   # ★ 适用场景（qualification）：冲突时各自限定，不二选一
utility: 0.8                # 可选：历史有用度（整理 / 排序参考，V2）
usage_count: 0              # 可选：被检索命中次数（V2 重要性评分）
source:                     # ★ 证据引用，可回溯
  - session: 2026-05-10-abc.jsonl#turn:14
---

用户偏好用 thiserror 而非 anyhow 定义领域错误。
```

字段说明：

| 字段 | 作用 | 来源 |
| --- | --- | --- |
| `type` / `subject` | 分类与主题，供索引与检索 | `plan.md` |
| `created_at` / `valid_from` | **时间线**：何时写入、何时生效 | `plan.md` + 决策 4 |
| `supersedes` / `status` | 版本化：取代关系，不删历史 | 决策 4（Mem0 v3 教训） |
| `confidence` | 证据不足时降级标注 | `plan.md` 的"蒸馏验证" |
| `scope` | **适用场景**：冲突双方在不同条件下并存 | 决策 4（qualification） |
| `utility` / `usage_count` | 有用度 / 命中次数，供整理与排序 | 论文记录的 `utility` + `usage` |
| `source` | **证据引用**：指向 `sessions/*.jsonl#turn:N` | 痛点 ② evidence |

**3.3 索引页 `index.md`（横向关联）**

第 3 章明确警告：**纯文本平铺会退化成"孤岛"**——知识越多越难找。解法是
像 Wikipedia 一样建入口页与交叉链接。`index.md` 由程序维护，每条一行：

```markdown
- [pref-rust-error-style](preferences/rust-error-style.md) · 偏好 · thiserror 而非 anyhow · 2026-05-10
- [event-ts-migration-kickoff](episodic/2026-05-12-ts-migration.md) · 事件 · TS 后端迁移启动 · 2026-05-12
```

> 第 3 章还提醒：**多数模型不会主动建链接**。所以写入提示词必须显式要求
> "新增条目前先检索并链接相关条目、更新 `index.md`"——不能指望模型自发做。

---

**四、双层注入（"每次 query 自动附带 context"）**

复用 `ContextProvider` 接缝，注入位置与 `todo.md` 决策 2 **完全一致**：作为一条
`System` 消息**追加在每轮请求末尾**。理由同样是不动前缀、缓存友好，且压缩碰不到它。

**4.1 注入两级**

1. **永远注入**：`core.md` 全量（画像 / 明确偏好 / 活跃项目）。小、稳定，
   前缀缓存友好——这是"稳定可见"的来源。
2. **相关注入**：程序按**当前 query + 工作目录**从 `index.md` 匹配 top-k 条目的
   **摘要**（不是全文），注入。AI 要全文时用 `read_file` 取。

```
┌─ 常驻概览层（L0/L1）──────────────────────────────┐
│  core.md + 匹配到的 top-k 条目摘要                  │
│  → 每轮自动注入（ContextProvider 末尾追加）          │
├─ 按需细节层（L2）──────────────────────────────────┤
│  条目全文 / 原始对话 JSONL / 证据                   │
│  → AI 用 read_file / 未来的 memory 工具取           │
└────────────────────────────────────────────────────┘
```

**4.2 渲染成 XML**

与 `todo.md` 的 `<task_state>` 同风格（`plan.md`：标签形式提升模型对结构信息的
注意力），便于模型区分"这是长期记忆"而非当前对话：

```xml
<memory_context>
<core>
用户是 Sean，偏好 Rust、讨厌过度抽象。
</core>
<relevant>
- [preference] 用户偏好用 thiserror 而非 anyhow（2026-05-10，active）
- [event] 2026-05-12 TS 后端迁移启动，当前在迁移用户模块
</relevant>
</memory_context>
```

**4.3 预算护栏**

- `core.md` 硬上限（如 500 tok），超限由 curator 提炼，不无限膨胀；
- 相关注入 top-k 上限（如 3 条）+ 总注入字符上限（如 2000 字符），超限截断并
  标注——防止记忆自己垄断上下文（与 `todo.rs` 的 `MAX_RENDER_CHARS` 同一思路）；
- **空记忆不注入**（`render()` 返回 `None`）——避免每轮多一条无意义 system。

---

**五、写入路径：后台 curator**

**5.1 增量更新（会话结束时触发）**

```
会话结束 / 每 N 轮
  → Curator(Proposer)：读最近对话 + 现有相关记忆
      先检索已有条目（不粗暴追加到文件尾）
      → 产出尽可能小而完整的 diff：增 / 删 / 改
        + 维护链接 / index.md / source / 时间字段
  → 探测（可选，见 5.4）：用只读工作区工具核实候选
       （反例 / 未触及场景 / 过时映射 / 前置条件 / 更短路径）
       无安全只读面则跳过，退化为纯轨迹 curation
  → 自检：核对每个断言是否有证据、是否遗漏限定、是否与其它条目冲突
  → 合入 memory/（写 markdown）+ 更新 index.md
```

**5.2 定期整理（周期 / 阈值触发，即第 3 章的"睡眠学习"）**

三项工作逐条落地：

1. **去重 / 去旧 / 合并**：全量扫描 `memory/`，识别语义重复、已被取代、过碎的
   条目 → 合并 / 重写 / 重链，重建 `index.md`。（删的是知识表达，**不动原始证据**）
2. **回原始数据核查**：对照 `sessions/*.jsonl` 逐条检查旧条目是否遗漏关键事实、
   丢否定词 / 时间条件、把推测当事实。
3. **冲突解决与场景限定**：见第六节。

**5.3 审核：诚实降级，不假装双模型**

第 3 章建议 Proposer / Reviewer 用**不同家族模型**（异源互审）。本地 CLI 成本高，
Shirley **V1 降级**为"同模型独立 prompt + 确定性检查"：

- 确定性检查（不靠 LLM）：frontmatter schema 合法、`source` 文件确实存在、
  `supersedes` 指向的 id 存在、时间字段合法、链接可达；
- **不通过就不合入**，绝不默认放行；
- V2 允许配置独立的 curator 模型做异源审核。

**5.4 环境探测式 curation（`plan.md` 引用的论文）**

论文：*Grounding Agent Memory: Environment-Probing Curation for Enterprise Agents*
（arXiv 2609.11060）。它针对的正是本文的痛点 ④——**异步 curator 已经存在，但只
能看"已完成轨迹"**，于是会保留错误、过度泛化局部证据、留着过时知识（论文举了三
例：一条记下错误聚合及其答案却没给替代过程；一条只给宽泛领域图却漏掉关键 relation；
一条在 schema drift 后仍留着已删除的字段名）。根因：**一次轨迹只是对环境的单次、
局部、可能有错的观察**——正是 `plan.md` 说的"不是最佳逻辑"。

论文的解法（不改 task agent / retriever / 记忆表示 / 写权限）：任务关闭后，给已有
的异步 curator **一组最小权限、只读的环境工具**，让它按 **propose–probe–commit**
走——先提候选记忆，再针对具体不确定性做定向只读探测，然后用观测决定 create /
revise / narrow / delete / skip。它能：区分偶然答案与可复用 relation、对比更短路径、
在另一 slice 上验证声称的关系、检查过程的前置条件、检查轨迹遗漏的状态、怀疑漂移时
重查当前环境。**探测只用于评估候选记忆，不为解决未来任务，也不能改环境 / 进轨迹 /
耗 task agent 预算 / 暴露未来任务**；**若没有安全的只读面，就回退纯轨迹 curation**。

落成 Shirley（V1 简化）：

- **"环境"就是工作区**：只读探测 = 现有的 `read_file` / 目录列举（SDK 的 `WorkSpace`
  已保证越界拒绝），不需要新工具、不需要写权限；
- **探测目标是 `procedure` / 事实类候选**：这类记忆必须引用**验证过的证据**；拿不到
  证据就 `confidence: low` 或不写，挡在记忆库之前，防止把偶然路径沉淀成"经验"；
- **前置蒸馏（可选）**：论文在 curator 之前还有一个**不写入**的 distiller，把原始轨迹
  压成固定标签段的"证据包"（`overview / history / work_done / technical_details /
  important_files / next_steps / checkpoint_title`）。关键边界值得照抄：**distiller
  看不到 terminal feedback、不能写记忆、把 rollout 当作 partial evidence 而非
  ground truth**——一次通过不自动验证每个中间假设。Shirley V1 可先不做独立 distiller，
  但 curator 的输入提示词要带上同样的边界声明。

效果（论文实测，作方向性参考）：CLBench 上 probing 把 pass rate 39%→73%、reward
8.60→22.60，单题查询 8.8→4.7；相对纯轨迹记忆，probing 把记录从"别用 X（因为算错）"
这种 answer-anchored warning，升级成"join 哪张表、on 什么 key、filter 什么、取什么
grain"这种 **executable procedure**。**增益取决于轨迹残留的证据缺口**：轨迹已足以
支撑可执行记录时探测增益很小，留下未解决的 join / 位置 / 过程时增益最大（论文自称
这是机理解释而非已确立的子群效应）。

---

**六、冲突处理 = 加入时间去整理**

- **永不删除历史**：新事实 `supersedes` 旧条目，旧条目 `status: superseded`。
- **每条带时间线**：`created_at` / `valid_from`，检索按时间排序、过滤已失效。
- **冲突时追溯来源**：定期整理遇到矛盾，回到各自 `source` 证据，检查它们是否在
  **不同时间 / 对象 / 任务 / 前置条件**下分别成立：
  - 都成立 → 把适用场景写进条目（qualification），**不是二选一**；
  - 证据不足 → 保留冲突 + `status: unconfirmed`，**不得强行收敛**。
- 这与第 3 章实验 3-11 一致：给矛盾条目补上"时间 + 人物 + 意图"前缀，矛盾块才
  可判优先级。

> 一句话：**冲突不是要消灭的 bug，是要用时间轴和适用场景表达的常态。**

---

**七、与 AgentLab 的取舍（批判性）**

**值得借鉴**：

- 分层记忆类型（working / episodic / semantic / perceptual）→ 采纳为目录分类；
- `MemoryItem` 软删除状态机（`active | invalidated | merged`）→ 采纳为
  `status: active | superseded | unconfirmed`；
- `add_batch` 批处理省 LLM、`candidate → filter → score → take` 检索管线、
  `forget` 三策略 → 作为 curator 与检索的设计参考。

**必须避开**（AgentLab "实现得不好"的部分）：

- ❌ 重后端（PG + pgvector + Neo4j + Ollama）——违背 Shirley"零依赖、轻"；
- ❌ **程序化 LLM 抽取**（每次 add 都调 LLM 做冲突裁决）——成本高、把记忆绑死在
  模型调用上；
- ❌ 硬编码魔数做语义路由 / 重要性、`unwrap_or_default()` 破坏 `Option` 语义、
  match 错误字符串、生产代码留 `eprintln!`、无界增长无清理；
- ❌ **只注册工具 + system_prompt 引导、不做自动注入**——这正是"体验不稳定"的来源。

**Shirley 反过来做**：轻、纯 markdown、模型驱动（AI 主动写 + 后台 curator 自动
整理）、**自动注入常驻层**。

---

**八、与现有件的关系**

| 能力 | 记什么 | 谁触发 | 恢复方式 |
| --- | --- | --- | --- |
| compaction | 对话摘要 | 自动（阈值） | 摘要本身 |
| todo | 本轮任务进度 | AI 主动更新 | 每轮末尾注入 |
| **memory** | **跨会话的用户事实 / 偏好 / 事件** | **AI 主动写 + 后台 curator 整理** | **常驻层每轮末尾注入 + 按需 read** |

- **与 todo 同构、职责不同**：todo 管"这一轮做到哪"，memory 管"这个用户是谁"。
  两者都走 `ContextProvider` 末尾注入，可在同一次请求里各追加一条 system。
- **与 session 的关系**：`sessions/*.jsonl` 是记忆的**原始证据层**（已有，只增
  不改），记忆条目通过 `source` 引用它——**不用新建证据层**。
- **与压缩的关系**：记忆不进 `self.messages`，单独持有，压缩碰不到它，天然跨压缩、
  跨会话存活。

---

**九、实现位置**

```
src/memory/
├── format.rs    # 条目 schema（frontmatter 解析 / 序列化）
├── store.rs     # MemoryStore：markdown 目录读写
├── index.rs     # index.md 维护 + 轻量检索（query → 相关条目）
├── provider.rs  # MemoryContextProvider: impl ContextProvider（每轮注入）
└── curator.rs   # 后台 curator（增量 + 定期整理）
```

| 文件 | 职责 |
| --- | --- |
| `src/memory/`（**应用层**） | 上表五个子模块 |
| `src/bootstrap.rs`（应用层） | `build_agent` 构造 `MemoryContextProvider` 并作为 `context_provider` 注入（与 `TodoContextProvider` 并列） |
| `crates/shirley-agent-sdk/src/runtime/context.rs`（SDK） | 复用既有通用接缝 `ContextProvider`，**不改** |

- 可选的 `remember` / `recall` 记忆工具走应用层 `Tool`（手写 `impl Tool`），
  **不进 SDK**。
- **SDK 不新增任何对外类型**；`lib.rs` 的 `pub use` 不动。

---

**十、分期落地**

| 阶段 | 内容 | 解决 |
| --- | --- | --- |
| **V1（稳）** | `core.md` 常驻注入 + `index.md` 关键词检索注入 + 会话结束增量 curator（同模型 + 确定性自检）+ 时间化冲突字段 | 自动附带、异步 curator、冲突不删历史 |
| **V2** | 定期整理（睡眠学习）+ 回原始证据核查 + 检索升级 BM25 + 可配独立 curator 模型 | 全量去重 / 合并、证据回查、异源审核 |
| **V3** | 可选 embedding 语义检索、跨工作区记忆、多模态（第 3 章 3.3.7） | 大规模召回、主动服务 |

**V1 验收口径**：

1. 开新会话时 `core.md` **自动出现在请求里**（不靠 AI 调工具）；
2. curator 能在会话结束后产出一条带 `source` 的条目；
3. 两条冲突偏好能以 `supersedes` 时间线共存，检索按时间排序；
4. 空记忆时**不注入**、不劣化现状。

---

**十一、已知缺口**

1. **无 Git 集成**：方案建议记忆目录进 Git 以获得版本 / 回滚，V1 未接。
2. **审核能力有限**：V1 同模型自审 + 确定性检查，弱于第 3 章的异源 Reviewer；
   复杂证据仍可能误合。
3. **检索精度**：V1 关键词匹配对同义改写不敏感，召回率有限，靠 `index.md` 摘要
   补偿。
4. **curator 触发时机**：会话"结束"在 TUI / desktop 的判定尚需明确（退出？
   空闲超时？切换会话？）。
5. **隐私**：第 3 章建议 PII 脱敏后再入库，V1 未做（记忆在本地，风险低但存在）。
6. **注入即失前缀缓存**：与 `todo.md` 同一代价——`core.md` 变化时其后前缀失效，
   用"放末尾 + 稳定内容"压到最小。
7. **环境探测的覆盖面有限**（论文 2609.11060）：V1 的"环境"只有工作区，探测 = 只读
   `read_file` / 目录列举；无法探测外部系统、运行中状态或未来任务。因此 probing 的
   收益**取决于轨迹残留的证据缺口**——轨迹已足以支撑可执行记录时增益很小，留下未解决
   的 join / 位置 / 过程时增益最大（论文亦强调这是机理解释、非已确立的子群效应）。
   没有安全只读面时应**退化为纯轨迹 curation**，不硬凑。
