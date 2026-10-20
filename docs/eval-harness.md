# Shirley 技术设计 · 开发阶段评估能力（Eval Harness）

> 定位：这是一份**技术方案**（设计文档），不是已落地实现。目标是把 harness2 里"真正能落地"的部分抽出来，
> 去掉论文级重装备（4k rollout、离线递归自优化、benchmark 权重），只保留**开发阶段评估**必需的能力：
> 用固定任务集反复跑 Shirley，产出可复现、可对比、可回归的分数。
>
> 关联文档：`docs/testing.md`（单元/契约/集成测试）与本文互补——前者测"代码是否正确"，
> 本文测"agent 作为整体在真实任务上表现如何"。

---

## 1. 目标与非目标

### 1.1 目标（开发阶段评估）

1. **回归**：改 prompt / skills / tools / 模型后，跑固定任务集，看分数是否退化。
2. **A/B 对比**：同一任务集，不同 harness 变体（prompt v1 vs v2、skills 开/关、模型 A vs B），
   产出可比的分数与证据。
3. **可复现**：每个 rollout 的输入（任务、harness 指纹、模型配置）与输出（轨迹、产物、评分）
   全部落盘，任何一次结果都能事后复查、能 resume。
4. **证据落盘 + 渐进披露**：轨迹/产物写到磁盘，评分与人工复查时按需读，不把证据塞进上下文。
5. **打分与执行解耦**：runner 只负责"跑"，评分是独立的一趟（见 §6），judge 失败 = `None` 而不是 `0`。

### 1.2 非目标（明确不做，或留作可选进阶）

- **不做 harness2 的递归自优化主回路**（Propose–Probe–Compose、4k rollouts、离线树搜索）。
  本方案只做"固定 harness 的评估"；盲选 / 行为对比 / 经验沉淀作为 §10 的可选进阶。
- **不做产品化在线评测**（用户每次会话都打分）。这是开发/CI 期能力。
- **不引入新协议 / 不改 SDK 公共契约**。eval 是应用层能力（同 `todo` / `memory` / `skills`）。

---

## 2. 从 harness2 借什么、丢什么

| harness2 的做法 | 本方案取舍 | 理由 |
| --- | --- | --- |
| **三轴解耦**：core（方法）/ substrate（执行器）/ benchmarks（任务集 + adapter） | **保留**，映射到 Shirley（§3） | 换任务集、换被测对象都不改调度逻辑 |
| **BenchmarkAdapter 协议** | **保留**（简化） | 任务集是"数据 + 适配器"，不是硬编码 |
| **证据物化到磁盘、按需 grep** | **保留** | 与 Shirley skills 渐进披露同思路；省上下文 |
| **打分与选择解耦、answer-free** | **保留**（作为"评分不参与运行"的硬约束） | 防泄漏、防过拟合 |
| **judge 失败 = None 不是 0** | **保留** | 区分"跑得差"与"没测成"，分母不污染 |
| **preflight（花钱前体检）** | **保留** | 避免跑完才发现 judge 没配 |
| **c1..c8 可编辑树 / SurfaceSpec** | **简化**为 `HarnessSpec`（§5） | Shirley 的可编辑面比 harness2 窄 |
| **Propose–Probe–Compose 递归、library 经验沉淀** | **丢**（留 §10 可选） | 开发期评估不需要自动改 harness |
| **4k rollouts + 3k stages 预算** | **丢** | 成本不可接受；改为可配置小预算 |

---

## 3. 架构：三轴映射到 Shirley

harness2 的"被测对象"是 opencode/codex；**在 Shirley 里，被测对象就是 Shirley 自己**（及其 harness 变体）。
所以 substrate 轴 = Shirley agent 运行时；benchmark 轴 = 任务集 + adapter；method 轴 = 调度/评分/报告。

```
┌──────────────────────────────────────────────────────────────┐
│ Method 层（crates/shirley/src/eval/runner.rs, grade.rs, report.rs）           │
│   与任务集、被测对象无关：排期、并发、超时、预算、落盘、报告    │
└───────────────┬──────────────────────────────┬────────────────┘
                │                              │
   ┌────────────▼───────────┐      ┌───────────▼───────────────┐
   │ Substrate 层            │      │ Benchmark 层               │
   │ crates/shirley/src/eval/substrate.rs   │      │ crates/shirley/src/eval/bench/*.rs        │
   │ ShirleySubstrate：      │      │ BenchmarkAdapter：         │
   │  surface() → HarnessSpec│      │  tasks() / task_prompt() / │
   │  materialise() → Agent  │      │  task_files() / validate() │
   │  execute() → Trajectory │      │  deliverables() / score()  │
   │  read_trajectory()      │      │                           │
   └─────────────────────────┘      └───────────────────────────┘
```

**为什么这样切**：开发期最常换的是"任务集"（换一套 coding task）和"被测配置"（换 prompt/模型），
方法层（怎么跑、怎么评分、怎么出报告）应保持稳定。这与 `docs/testing.md` 的分层精神一致。

---

## 4. 目录与入口

Shirley 目前是**单 bin、无 `lib.rs`**，模块在 `main.rs` 声明，`Mode` 枚举分派
`Tui / Desktop / Server`（`--desktop` / `--server` / `SHIRLEY_INTERFACE`）。因此 eval **不能做独立 `[[bin]]`**
（无法 import `bootstrap` 等模块），必须作为**新增 `Mode::Eval` 分支**接入，复用 `bootstrap::AgentFactory`。

```
crates/shirley/src/
  eval/
    mod.rs           # facade + Mode::Eval 入口（run_eval(args)）
    task.rs          # Task / TaskSuite / TaskFile / Deliverable
    harness.rs       # HarnessSpec（可编辑面）+ 解析成 build_agent 的 override
    substrate.rs     # ShirleySubstrate：materialise / execute / read_trajectory
    trajectory.rs    # Trajectory / Step + 统一渲染语法
    runner.rs        # run_suite：并发/超时/预算/resume/落盘
    grade.rs         # Grader trait + DeterministicGrader + JudgeGrader
    report.rs        # Report 聚合 + markdown/json 输出
    record.rs        # RunRecord + 磁盘布局 + resume
    bench/
      mod.rs         # BenchmarkAdapter trait
      local.rs       # 本地 coding 任务集 adapter（默认）
      jobbench.rs    # （可选）jobbench 适配
    selection.rs     # （可选 P3）盲选 / 行为对比
```

`main.rs` 改动（最小侵入）：

```rust
enum Mode { Tui, Desktop, Server, Eval }   // 新增 Eval
// from_env_and_args(): 识别 --eval / SHIRLEY_INTERFACE=eval
// main(): Mode::Eval => eval::run_eval(std::env::args().collect())?;
```

CLI 形态：

```sh
shirley --eval --suite local/coding-v1 --k 3 --harness base --out runs/
shirley --eval --suite local/coding-v1 --harness prompt-v2 --compare runs/<base-run-id>
```

---

## 5. Harness 注入 seam（本方案最关键的一块）

**约束（已核实）**：
- system prompt 是 `crates/shirley/src/prompt.rs` 里 `include_str!` **编译期嵌入**的（函数形式每次解析，但模板固定）。
- skills 是**运行时扫描目录**（`SkillCatalog`），可直接指向不同目录。
- tools / model / max_steps 已能通过 `AgentFactory::build_agent_with(messages, tool_filter, max_steps)` 部分覆盖。

所以需要一个 `HarnessSpec`（对应 harness2 的 `SurfaceSpec` + `Tree`），描述"这次评测用什么 harness"：

```rust
pub struct HarnessSpec {
    pub name: String,                       // 变体名（base / prompt-v2 / skills-on ...）
    /// system prompt 覆盖：None = 用默认编译期模板；Some = 运行时替换正文
    pub system_prompt_override: Option<String>,
    /// skills 目录覆盖：None = 用默认；Some = 指向另一套 skill 目录
    pub skills_dir: Option<PathBuf>,
    /// 工具允许/拒绝（复用现有 ToolSelection 语义）
    pub tool_filter: Option<ToolSelection>,
    /// 模型 / 压缩配置覆盖
    pub model_config: Option<ModelConfig>,
    pub compression: Option<CompressionConfig>,
    /// 指纹：以上内容的稳定 hash，用于 run 记录与 A/B 对齐
    pub fingerprint: String,
}
```

**需要的 bootstrap 小改动**（唯一侵入 SDK 之上的地方，且默认行为不变）：

新增 `AgentFactory::build_agent_with_harness(messages, &HarnessSpec) -> BuiltAgent`：
- `system_prompt_override` → 用 `SystemPrompt::from(override)` 替换默认函数式 prompt；
- `skills_dir` → 构造对应 `SkillCatalog`；
- 其余透传给现有 `build_agent_with`。

> 之所以要这个 seam：如果只能在编译期换 prompt，就无法做"A/B 对比 prompt v1/v2"，也无法跑 harness2 式
> "改 harness 再评"的进阶。这是把 eval 从"只能测一个固定 Shirley"升级成"能测 harness 变体"的最小代价。

`HarnessSpec::fingerprint` 用于：
- 记录到 `meta.json`，A/B 时确认"除 harness 外其他一致"；
- 缓存：同一 (task, harness fingerprint, model) 可复用（对应 harness2 的 `_call_fingerprint`）。

---

## 6. 数据模型

```rust
// task.rs
pub struct Task {
    pub id: String,
    pub domain: String,           // 用于分组/统计
    pub prompt: String,           // 给 agent 的任务原文（answer-free，不含评分标准）
    pub files: Vec<TaskFile>,     // 初始素材（写入 rollout 的临时 workspace）
    pub setup: Vec<String>,       // 可选：准备命令
}
pub struct TaskSuite { pub name: String, pub tasks: Vec<Task> }

// trajectory.rs
pub struct Trajectory {
    pub task_id: String,
    pub steps: Vec<Step>,         // 从 AgentEvent 采集
    pub usage: Usage,
    pub stop_reason: StopReason,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}
pub enum Step {                 // 统一渲染语法（对应 harness2 的 [ACTION]/[OBSERVATION]/[ERROR]）
    Action { tool: String, args: serde_json::Value },
    Observation { output: String },
    Error { message: String },
    Assistant { text: String },
}

// record.rs —— 磁盘上的唯一事实
pub struct RunRecord {
    pub run_id: String,          // 稳定 id
    pub task_id: String,
    pub harness: HarnessSpec,    // 含 fingerprint
    pub model: String,
    pub status: RunStatus,       // Pending/Running/Done/Failed
    pub trajectory_path: PathBuf,
    pub workspace_path: PathBuf, // 产物快照（deliverables 来源）
    pub grade: Option<GradeResult>,  // 打分是独立一趟（§7）
    pub created_ms: u64, pub modified_ms: u64,
}

// grade.rs
pub struct GradeResult {
    pub grader_id: String,       // "deterministic:<check>" / "judge:<model>"
    pub score: Option<f64>,      // ★ None = 没测成（judge 失败/产物不可读），不是 0
    pub passed: Option<bool>,
    pub detail: String,          // 判词 / 命中项
    pub graded_at_ms: u64,
}
```

**磁盘布局（证据落盘，harness2 风格）**：

```
<state>/shirley/evals/<suite>/<run_id>/
  meta.json          # RunRecord（不含 score，见下）
  task.md            # 任务原文（answer-free）
  trajectory.md      # 统一语法渲染的完整轨迹（不裁剪）
  workspace/         # agent 产出的文件快照（deliverables 从这取）
  grade.json         # GradeResult（独立一趟写入）
```

> **answer-free 硬约束**：`task.md` 与 `meta.json` 里**绝不写评分标准 / 期望答案**。评分标准只存在于
> grader 代码/配置里，且 grader 在**选择之后**才跑（§7）。这是 harness2 "selection 不带 score"的等价物。

---

## 7. Runner 与打分解耦

### 7.1 Runner（只跑不打分）

`runner.rs::run_suite(suite, harness, k, budget)`：
1. **preflight**（花钱前体检，对应 harness2 `require_ready`）：
   - 校验 `HarnessSpec`（skills 目录存在、prompt 非空、tool_filter 合法）；
   - 校验 model config 可构造（`AgentFactory::assemble` 成功、`needs_login == false`）；
   - 校验 grader 已配置（judge 可达）——**否则直接拒绝启动**，避免跑完才发现评分跑不了。
2. 对每个 task × k 次重复：
   - 建临时 `workspace/`（`WorkSpace::new`），写入 `task.files`；
   - `build_agent_with_harness(vec![], &harness)`；
   - `agent.run_stream(task.prompt)` 消费 `AgentEvent`：
     - `ToolStarted/ToolFinished` → `Step::Action/Observation`；
     - `ContentDelta` → 累积 `Step::Assistant`；
     - `Usage` → 汇总；`Finished(RunResult)` → `stop_reason`；
   - 落盘 `trajectory.md` + `workspace/` 快照 + `meta.json`（**status=Done，grade=None**）。
3. **并发**：`Semaphore` 限制并发 rollout（默认 2–4）；每 task 有 `per_task_timeout`（外层 `tokio::time::timeout`）。
4. **预算护栏**：`EvalBudget { max_rollouts, max_cost_usd, max_wall_secs }`；超限即停止并标记剩余为 `Skipped`。
5. **resume**：`run_id` 稳定（task_id + harness.fingerprint + model + 第 k 次）；已 `Done` 的跳过。

> Runner **完全不调用 grader**。这是硬性架构约束——保证"跑"这一趟 answer-free，
> 也为将来"盲选 / 行为对比"（§10）保留可能：那套流程需要"跑"和"打分"彻底分离。

### 7.2 Grader（独立一趟）

`grade.rs`，两实现：

```rust
pub trait Grader {
    fn id(&self) -> String;
    fn grade(&self, task: &Task, record: &RunRecord, workspace: &Path) -> GradeResult;
}
```

- **DeterministicGrader**：跑检查脚本 / 断言（文件存在、内容匹配、编译通过、测试通过……）。
  产物直接来自 `workspace/`。失败 → `score=Some(0.0)`（**这是真"跑得差"**）。
- **JudgeGrader**（LLM judge）：把 `task.md` + `workspace/` 产物（用**与评分同款 reader** 渲染成文本）
  交给 judge 模型打分。**judge 调用失败 / 产物不可读 → `score=None`**（"没测成"，排除出分母，不是 0）。

`grade_suite(records, grader)` 遍历已 `Done` 的 `RunRecord`，写回 `grade.json`。

**分母语义**（照抄 harness2 的 `Scored.graded`）：
```
平均分 = sum(score where score.is_some()) / count(score.is_some())
```
`None` 的样本单列"未测成数"，不拉低平均分，也不当成 0。

---

## 8. Report

`report.rs::Report`：按 task / domain / harness 聚合，输出：

```markdown
# Eval: local/coding-v1 · harness=prompt-v2 · model=xxx
- Rollouts: 30  (Done 28 / Failed 2 / 未测成 3)
- 平均分: 0.72   (分母 25，5 个 None 已排除)
- 通过率: 20/25
- 平均耗时: 41s   总 usage: in=..., out=..., 缓存命中率=...
| task | score | passed | 未测成 | 备注 |
```

同时写 `report.json`（机器可读，供 CI 阈值比较）。
**A/B 模式**：`--compare <base-run>` 输出逐 task 的分数差与轨迹差异摘要。

---

## 9. 安全与成本护栏

| 风险 | 措施 |
| --- | --- |
| 命令执行越界 | rollout 走现有 `Sandbox`（process backend，**诚实报告 degraded**）+ 临时 workspace，deny 网络、不继承 env |
| 产物路径逃逸 | `WorkSpace::resolve` 校验；deliverables 只从 `workspace/` 内取 |
| 泄题 | `task.md`/`meta.json` 不写评分标准；grader 在跑完之后独立执行（§7） |
| 成本失控 | `EvalBudget`（rollout 数 / 美元 / 墙钟）+ `per_task_timeout` + 并发上限；preflight 先校验再花钱 |
| 密钥泄漏 | eval 目录不落 api_key；`meta.json` 只存 model 名与 endpoint 指纹 |
| judge 假阳性 | judge 失败 = None；A/B 报告同时展示"未测成数"，不掩盖 |

---

## 10. 可选进阶（明确留白，不在 MVP）

只在**离线可回放**场景启用，对应 harness2 的 selection / recursion，但**砍到最小**：

1. **盲选（P3）**：一个 task 有多个 harness 变体产出时，把候选产物匿名渲染成 A/B/C（种子稳定打乱），
   让 selector 只读"任务 + 源码 + 产物"选一个，**score 不参与**；失��回退到已交付的 base，不 raise。
2. **行为对比（P3）**：不是"哪个分高"，而是"哪个改动指到了轨迹里的具体某一刻"。
3. **经验沉淀（P4）**：采纳后产出结构化 `<experience>` 入 `library/`，下个同域任务带上。
   —— 这一步与 Shirley 现有 `memory` curator 同构，未来可复用。

> 这些是 harness2 的"真东西"，但都需要"可重放 + 多次 rollout"，只有离线 eval 集能满足。
> 故列为可选，不进 MVP。

---

## 11. 里程碑与验收标准

| 里程碑 | 交付 | 验收标准 |
| --- | --- | --- |
| **M0 骨架** | `Mode::Eval` + `task/suite` + `ShirleySubstrate`（materialise/execute/read_trajectory）+ `runner` 单任务 + 轨迹落盘 | 能对 1 个本地 task 跑通 Shirley，产出 `trajectory.md` + `workspace/` + `meta.json`；不调用任何 grader |
| **M1 确定性评分** | `DeterministicGrader` + `grade_suite` + `report`（markdown/json） | 一个含 5 个 task 的本地 suite 能跑完并出报告；分数可复现（同输入两次结果一致） |
| **M2 A/B + judge** | `JudgeGrader`（None-on-failure）+ `HarnessSpec` 变体 + `--compare` + `EvalBudget` + preflight | 能对比 prompt-v1/v2 或 skills 开/关；judge 挂掉时报告显示"未测成"而非 0；超预算自动停 |
| **M3（可选）盲选/对比** | `selection.rs` | 多 harness 候选能匿名盲选并给出理由；score 不参与选择 |

**测试要求**（并入 `docs/testing.md` 的测试面）：
- `HarnessSpec::fingerprint` 稳定性单测（同配置同 hash）。
- `RunRecord` 序列化/反序列化 round-trip。
- `JudgeGrader` 失败 → `score=None` 单测（用假 judge）。
- runner 超时 / 预算截断单测。
- `report` 分母语义单测（None 不计入分母）。

---

## 12. 风险与开放问题

1. **prompt 编译期嵌入**：`HarnessSpec.system_prompt_override` 是唯一可行 seam；若未来 prompt 需大改，
   考虑把 prompt 也改成运行时加载（但会动 `crates/shirley/src/prompt.rs`，需单独评估对缓存前缀的影响）。
2. **非确定性**：模型温度、工具并发顺序 → 单次结果不可靠。方案用 **k 次重复 + 分布**而非单值；报告给均值 + 通过率。
3. **judge 可靠性**：LLM judge 有偏差。MVP 以确定性 grader 为主，judge 只做"软指标"，且 None-on-failure。
4. **沙箱降级**：`ProcessBackend` 无真隔离，eval 里如实标记 degraded；真隔离（sandbox-exec/bwrap）未接入前，
   只跑可信任务集。
5. **任务集从哪来**：MVP 先做本地 coding task（基于 `Agent.md` 场景），jobbench 适配作为可选。
6. **与 `docs/testing.md` 边界**：本文测"能力"，`testing.md` 测"代码正确"；两者不重叠，CI 可都跑。

---

## 13. 一句话总结

把 Shirley 自己当成 harness2 的 "substrate"，把任务集当成 "benchmark"，方法层只做"跑 + 落盘 + 独立评分 + 报告"。
**MVP 只做固定 harness 的评估**（回归 + A/B），打分与执行彻底解耦、judge 失败为 None、证据全落盘；
盲选 / 行为对比 / 经验沉淀作为可选进阶，等离线 eval 集成熟再上。
