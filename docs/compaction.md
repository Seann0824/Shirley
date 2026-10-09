**Shirley 技术方案 · 上下文压缩优化**

这份文档把 `plan.md` 里关于上下文压缩的零散判断，收敛成一个可执行的方案。
它承接 `runtime-hardening.md` 第六节（压缩健壮性）与 `docs/plan.md` 的 T6，并引入一篇形式化论文
（Tirmazi et al., *Context Compaction Theory*, arXiv:2608.01326v1）作为理论依据，
回答三个问题：**为什么现在的压缩会丢任务、压缩在理论上能做到什么、我们这一轮改什么**。

---

**一、为什么单独做这件事**

触发点是一次真实故障：压缩完成后，AI 忘记了用户压缩前下达的任务，
而压缩摘要里居然给出了"下一步步骤"——但那一步并不是用户提过的。

这不是调 prompt 能修好的小问题。它暴露了当前实现的两个结构性缺陷（见第二节），
而论文（见第三节）进一步说明：**压缩的信息损失在理论上无法完全避免，
LLM 摘要在"精确事实"上必然退化**。所以压缩不是"写句更好的提示词"，
而是要重新设计**切法、保留策略、以及信息丢失后的退路**。

---

**二、现状解剖**

**2.1 当前链路**

```
should_schedule_compression  →  (input + output) * 100 >= limit * 80
        ↓ 置 compression_pending
compress_context             →  active_messages() + 压缩指令（追加为新 system）
        ↓ 取模型输出的纯文本摘要
push 到 self.messages 末尾     →  Message::ContextSummary
        ↓
active_messages()            →  开头的 system + 从最后一个 ContextSummary 开始的消息
```

对应 `crates/shirley-agent-sdk/src/runtime/mod.rs`。

**2.2 两个根因（同一个结构问题）**

**根因 A —— 摘要追加在末尾，等于把尾巴也吃掉了。**

`compress_context` 把 `ContextSummary` push 到 `self.messages` 的**最末端**。
下一轮 `active_messages()` 找"最后一个 summary"，切出来就是 `[system, summary]`：

- 本轮正在进行的用户任务（在 summary 之前）被排除在请求之外；
- 压缩前后的一段近期对话也全部不再参与请求。

模型只剩一个摘要，自然只能靠摘要活着。**这是"忘记原任务"的直接原因。**

**根因 B —— 压缩指令要求模型编"下一步"。**

`src/main.rs` 的 `compression_instruction` 明确写了"……未解决的问题、下一步"。
你让它输出下一步，它就会输出，哪怕用户没提过。
同时指令没有强调"忠实保留用户的原始指令"，摘要因此有越界倾向。

**结论**：切法本身要改，不是调 prompt。

---

**三、理论依据（论文能拿走什么）**

论文把压缩形式化为两个游戏：

| | Context Selection Game（`S`） | Context Generation Game（`G`） |
| --- | --- | --- |
| 做法 | 选一个子集保留 | 生成有界长度的新消息（摘要） |
| 例子 | 截断、丢老消息、Aider repo map、LLMLingua | LLM summarization（Codex / Claude Code / Gemini CLI） |
| 关系 | `S ⊆ G`，生成更强（可造新 token、可重排、可用更低编码） | |

**核心定理（Theorem 1）**：Context Generation Game **等价于单向通信复杂度**。
压缩器 = Alice（condenser），未来读摘要的 LLM = Bob（interpreter），摘要 = Alice 发的消息。

> 为在目标错误率 ε 内回答一组查询，所需的最小压缩预算 = 该诱导通信问题在 ε 下的单向随机通信复杂度。

由此得出四条对我们有直接约束力的结论：

1. **压缩的最优策略无法独立于"未来会问什么"决定。**
   这从形式上支持了 `plan.md` 的直觉——"压缩价值由未来查询决定，而非原文长什么样"。
   因此固定程序化压缩策略注定有损，压缩决策必须交给 LLM（`G` 类），
   这解释了为什么我们的 `ContextSummary` 方向是对的。

2. **摘要保不住"精确事实"。**
   附录 A 的实验：Anthropic 的 compaction endpoint + Opus 4.8 做集合成员查询，
   压缩后错误率 ≈ 随机猜（false negative 率 0.97 / 0.79 / 0.63），
   同尺寸 Bloom filter 约 1/3；不压缩的对照组错误率仅 0.04。
   摘要原文里模型自己写："我无法无损存储这个集合，只能靠看起来像不像来猜。"
   → **叙述性信息（目标、决策、进度）能保住；精确条目（集合、列表、错误码、文件名、
   完整文件内容）保不住。** 后者必须靠摘要之外的机制兜。

3. **下界对所有算法成立，上界不保证 LLM 算得出。**
   论文的 caveat 1：这是信息论结论。下界无懈可击；上界只说明"存在某算法能达到"，
   不保证 LLM summarizer 能算出来。→ 我们只能优化"够用"，不能追求"最优"。

4. **多次压缩只会更差。**
   论文只建模单次压缩，且指出单次是"最有利的情况"；
   每次压缩只能在上一次结果上再丢信息。Codex 自己也警告"多次压缩会降低质量"。
   → 我们的设计必须**避免摘要叠摘要**（见 5.2）。

**论文给出的两条出路**（附录 A 结尾）：① 把 sketch 维护在 LLM 上下文之外（工具/记忆）；
② 把 sketch 原始状态放进上下文 + 解码指令。
→ ①是我们任务账本（`todo.md`）的方向。

---

**四、Token 记账（切点预算的前置）**

**4.1 为什么需要**

保留尾部的预算口径定为 **上下文窗口的 20%**：

```
budget = context_window_tokens * 0.20
```

要判断"从尾部往前累加到哪一条"，必须知道每条 message 的 token 占用。

关键事实：**`input_tokens` 无法精确分解到每条 message。**
它是整个请求的真实总量，包含 system、tool schemas、role 包装、协议特殊 token。
要精确分摊，除非复刻 chat template——代价过高，不值得。

因此目标是**够用的分摊 + 校准**，而不是绝对精确。压缩选切点只需要**相对大小准确**：
我们知道"砍到哪里能落进预算"就够了。

**4.2 三层方案**

| 层 | 内容 | 本轮 |
| --- | --- | --- |
| L1 启发式估算 | CJK 感知的字符 → token 估算 + 消息包装开销 | **做** |
| L2 API usage 校准 | 用最近一次真实 `input_tokens` 求比例，回乘到估算 | **做** |
| L3 精确计数接口 | `TokenCounter` trait，未来接 `/count_tokens` 或本地词表 | 留口子，不实现 |

**4.3 估算规则（L1）**

CJK 感知的字符级估算，比 `chars/4` 准很多：

```
count_text(s):
  total = Σ 每个字符:
    CJK   (U+4E00–U+9FFF / U+3040–U+30FF / U+AC00–U+D7AF / U+3000–U+303F) → 1.0
    ASCII (0x00–0x7F)                                                     → 0.25
    其他（emoji / 符号 / 其他 Unicode）                                     → 0.5
  return ceil(total)

count_message(m):
  return count_text(该消息的全部文本内容) + 包装开销
    包装开销:
      role 包装          ≈ 4
      每个 ToolCall      ≈ count_text(name) + count_text(arguments) + 8
      Tool 消息          ≈ count_text(tool_call_id) + 4
```

**4.3.1 系数依据（文献支撑）**

三个系数不是拍脑袋，有实测文献支撑：

- **CJK = 1.0 token/字**：中文 PLM 的 tokenizer 通常把每个汉字当作不可分的 token
  （Sub-Character Tokenization for Chinese PLMs, arXiv:2106.00400）。
  《Vowel Signs Are Not Letters》(arXiv:2608.26449) 测出 **Han 脚本的 fertility 因子恰为 1.00x**，
  汉字在 byte-level BPE 下相对稳定。→ 1.0 是**保守上界**（见下）。
- **ASCII = 0.25 token/字符**：英文 ≈ 1.2 tokens/word（Tokenizer Tax Across 25 European
  Languages, arXiv:2605.24718），英文平均词长约 4.7 字符，得 ≈ 0.25。
- **其他 Unicode = 0.5 token/字符**：失败 BPE merge 会让文本碎片化成单字节 token
  （Tokenizer Tax for Indian Languages, arXiv:2607.24276）。取 0.5 偏保守。

**为什么必须校准（4.4）而不是靠固定系数**：
《The Invisible Language Tax》(arXiv:2609.39001) 测了 7 个 2026 主流 tokenizer（含 DeepSeek V3/V4），
结论是 token 溢价**因模型而异、无统一系数**——简体中文相对英文从 **-5% 到 +40%**。
现代 BPE（o200k / DeepSeek 新版）能把**两个汉字合成一个 token**，所以 CJK 真实值可能是 0.5~1.0。
我们取 1.0 是**宁可高估**（早触发压缩，安全侧）；L2 校准会把它拉回真实值。

度量方式沿用学界通行的 **tokens per character / characters per token**
（Is Sanskrit the most token-efficient?, arXiv:2601.06142）。

**4.4 校准（L2）**

每次响应拿到 `real = usage.input_tokens` 后，本地也算一个本次请求的总估计：

```
est   = overhead_est(system + tools) + Σ count_message(m)     # 本次请求的 message
ratio = real / est
factor = 0.7 * factor + 0.3 * ratio                           # EMA 平滑，初值 1.0
```

`factor` 在**聚合时统一回乘**：对每条 message 的原始估算乘同一个系数。
因为系数一致，累加序不变，切点结果稳定。
启发式即使偏差 30%，校准后通常能压到 10% 以内。

**4.5 切点算法（含 tool 配对约束）**

```text
fn compute_cut() -> Option<usize>:
    window = context_window_tokens                 # 未配置 → 返回 None（不压缩）
    budget = window / 5                            # 20%

    head = 开头连续 System 消息的条数                # 永远保留
    acc = 0; cut = messages.len()

    for i in (head..messages.len()).rev():         # 从尾部往前累加
        c = factor * count_message(messages[i])
        if acc > 0 and acc + c > budget:
            break                                  # 再加就超预算
        acc += c
        cut = i                                    # cut 是保留段起点
    # 至少保留一条（最后一条用户消息），即使它本身超预算

    if cut <= head: return None                    # 没有可压内容

    # ---- tool 配对修正 ----
    # 保留段的第一条不能是 Tool 结果，否则会出现孤儿 tool_call。
    while cut > head and messages[cut] 是 Tool:
        cut -= 1                                   # 回退到产出它的 Assistant{tool_calls}
    # 允许因此略微超预算：正确性优先于预算（孤儿 tool_call 会被 API 400 拒绝）

    if cut <= head: return None
    return Some(cut)
```

要点：

- **预算边界浮动**，所以配对修正必须显式做，不能靠"大概对齐"。
- 一条 `Assistant{tool_calls}` 可能对应多条连续 `Tool` 结果；
  `while` 回退会把这些一起吞进保留段，配对自然完整。
- 压缩区域 = `messages[head..cut]`；重建为
  `[新 ContextSummary] + messages[cut..]`，系统提示词由 `Agent` 另行重新生成置顶。

---

**五、压缩设计**

**5.1 心智模型：切点压缩 + 保留尾部**

压缩不是"压全部"，而是**选一个切点（见 4.5），把切点之前压成一条，切点之后原样保留**：

```
[system prompt]              ← 永远保留
[ContextSummary 结构化块]     ← 切点之前的段落被压成这一条
[近 20% window 的对话 verbatim] ← 保留尾巴，不压
[current user message]       ← 本轮任务，永远在请求里
```

这对应论文 Table 1 里的 hybrid 策略（Claude Code / OpenCode：先便宜地 inline 压缩大 tool 输出，
再 fallback 到 summarization），也是我们该学的分层。

**5.2 分层保留，禁止摘要叠摘要**

现状 `active_messages()` 找"最后一个 summary"、新 summary 又覆盖它，正是"摘要叠摘要"的退化路径。
新设计：压缩时**重建** `self.messages`，让已压缩段**冻结**：

```
self.messages = [system（Agent 重新生成）] + [新 ContextSummary] + messages[cut..]
```

即 `ContextSummary` 不再是"追加在末尾"，而是**插到切点位置**，替换掉切点之前的段落。
下一次压缩时，新 summary 覆盖的是"上一个 summary + 其后的对话"，语义清楚，
且避免同一段被反复压缩（论文 caveat 4：多次压缩只会更差）。

**系统提示词不参与重建**：它由 `Agent` 单独持有（`system_prompt` 字段），
`CompactParts::rebuild` 直接返回 `[新 ContextSummary] + current_task + remain`，
既不接收也不复制原消息里的任何 `System`；`compress_context` 随后调用
`Agent::system_message()` 重新生成一条置顶（`SystemPrompt` 是函数时会被**重新解析**，
工作目录 / 项目指南的最新状态会随之更新）。这样系统提示词永远不会被压缩产物污染，
也不会因为"保留开头 system"而在重建里意外带上旧摘要。

**5.3 摘要模板（XML 标签 + 区分可叙述与精确）**

`ContextSummary` 的 content 从自由文本改为结构化块。
依据：`plan.md` 提到 XML 标签能提升 LLM 注意力；论文说明要区分信息类型。

```text
<current_goal>忠实复述用户当前真正下达的任务，不得添加</current_goal>
<hard_constraints>用户明确提出的约束</hard_constraints>
<decisions>已做的关键决策及原因</decisions>
<progress>已完成 / 当前状态</progress>
<open_questions>尚未解决的问题</open_questions>
<compacted_range>此前对话已压缩，精确细节（文件内容/命令输出/错误/符号列表）不在摘要中，需要时请重新读取或执行</compacted_range>
```

要点：

- **删掉"下一步"**。要不要有 next step 由用户说了算，不由压缩器编。
- 强调"只提炼已发生的内容，不得推演、不得补充未出现的计划"。
- `<compacted_range>` 提示模型"被压段不再可见、需要时自行重建"——因为论文证明精确信息在 `G` 下必然丢失，
  不是 prompt 能补救的。
- 摘要用 **system 角色**（当前 `encode_messages` 已把 `ContextSummary` 降级为 system），保持"背景信息"语义。

**5.4 重试与降级**

依据 `runtime-hardening.md` 6.1 与 `plan.md`"压缩失败重试有时能成功"：

- 仅在**可恢复失败**上重试（空内容 / 带 tool_calls 的响应），重试 2~3 次。
- 仍失败 → **不中断**：记 `AgentEvent::CompressionFailed`，把 `compression_pending` 置回 `false`，
  保留原消息继续本轮任务。
- 连续失败 2 次 → 本轮内不再尝试压缩，依赖供应商侧截断并上报 `ContextUsage`。
- **抑制抖动**：压缩后若摘要本身仍接近阈值，不再立刻重复压缩。

---

**六、落点与改动清单**

主要落在 `crates/shirley-agent-sdk/src/runtime/mod.rs`，新增一个 `token` 模块，外加 `src/main.rs` 的压缩指令文案。

| 改动 | 位置 | 说明 |
| --- | --- | --- |
| 新增 `token` 模块 | `crates/shirley-agent-sdk/src/token/mod.rs` | `TokenCounter` trait + `HeuristicCounter`（带 `factor`） |
| `compact()` 取代 `compress_context` | `runtime/mod.rs` | 先算切点 → 只对 `messages[..cut]` 生成摘要 → 重建 `self.messages` |
| 切点计算 `compute_cut()` | `runtime/mod.rs` | 见 4.5：20% 预算 + tool 配对修正 |
| 校准状态 | `runtime/mod.rs` | 每次响应后用 `input_tokens` 更新 `factor` |
| `active_messages()` 简化/移除 | `runtime/mod.rs` | 压缩时已重建消息，请求侧不再需要切片 |
| 摘要模板 | `runtime/mod.rs`（构造压缩指令处） | XML 标签结构 |
| 压缩指令文案 | `src/main.rs` | 删"下一步"，加"忠实保留用户指令""不得推演" |
| 压缩重试 + 降级 | `runtime/mod.rs` | 见 5.4 |
| `AgentEvent::CompressionFailed` | `runtime/mod.rs` | 新增事件，UI 消费 |
| `retain_ratio` builder 参数 | `runtime/mod.rs` | 默认 0.20，与 `compression_instruction` 同级 |

---

**七、验收**

1. 构造一段长对话（含工具调用），触发压缩后，模型仍能回答"我最初让你做什么"。
2. 压缩摘要中**不出现**用户未下达的"下一步"。
3. 切点两侧不存在孤儿 tool_call（对生成的请求体断言：每个 `Tool` 消息都有前置 `Assistant.tool_calls`）。
4. 保留段 token 估算不超过 `window * 20%`（允许配对修正导致的少量超出）。
5. 注入"压缩返回空内容"，任务继续而非中断，UI 收到 `CompressionFailed`。
6. 连续两轮压缩，`self.messages` 中 `ContextSummary` 数量为 1（不叠加）。
7. 缓存命中率不明显下降（压缩后首轮允许 miss，后续轮应恢复）。

---

**八、不做的事 / 开放问题**

- **不做程序化 selection 策略**（论文证明 `G` 严格强于 `S`，且最优依赖查询分布）。
- **不引入向量库 / 外部记忆存储**（与 `roadmap.md` 第五节一致）。
- **不做 adaptive adversary 下的最优性**（论文本身是 open problem）。
- **多次压缩的质量衰减**（论文 open problem）：我们只能做到"不叠摘要 + 分层冻结"，无法给保证。
- **L3 精确计数不实现**：只留 `TokenCounter` trait 接口，等有明确需求（如供应商提供 `/count_tokens`）再落地。
- **不做本地 tokenizer**：词表与具体模型绑定，依赖重且和"不为多协议提前抽象"的克制原则冲突。
