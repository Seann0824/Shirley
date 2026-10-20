# 引入 Microsoft Rust Guidelines（落地说明）

> 来源：Microsoft **Pragmatic Rust Guidelines** — <https://microsoft.github.io/rust-guidelines/>
> 机器可读汇总（89 条 `M-*`）：<https://microsoft.github.io/rust-guidelines/agents/all.txt>
> 本文只记录「我们怎么落地」，不复述原文条目。

## 1. 结论先行：分级（graduated）引入，不一次性全量 deny

MS 指南本身把绝大多数 lint 标为 **warn** 而非 deny。我们照做，并按「当前代码库能不能立刻变绿」把约束分成三层：

| 层级 | 内容 | 处理 | 是否阻塞 CI |
| --- | --- | --- | --- |
| **Tier 1 · 硬门禁** | 已清零的 rustc lint + clippy `correctness`/`suspicious` | `deny` | ✅ 阻塞 |
| **Tier 2 · 棘轮（ratchet）** | 有存量告警、但方向正确的 lint | `warn`，逐步清零后翻 `deny` | ❌ 不阻塞（advisory） |
| **Tier 3 · 工具链** | rustfmt / cargo-audit / cargo-hack 等 | 逐步接入 | 视情况 |

> **进度（本轮）**：一次性 `cargo fmt --all` 已全量落地（591 处 diff → 0），fmt 检查已改为阻塞；`redundant_imports` / `missing_debug_implementations` 两条 rustc lint、clippy `correctness` / `suspicious` / `style` / `complexity` / `perf` 五组、以及 pedantic 的批次 A（16 条）+ 批次 B（7 条）子集均已清零并翻 `deny`。详见第 3、6 节。

为什么不能全量 deny：引入时实测——

- 默认 `cargo clippy`：**58** 条 warning
- `clippy::pedantic` 全开：**457** 条（去掉 numeric-cast 家族仍有 **392**）
- 各 clippy 组单独统计：correctness 21 / suspicious 23 / style 32 / complexity 23 / perf 21
- `cargo fmt --all --check`：**591** 处 `Diff in`（代码库从未整体 rustfmt 过）

一次性把 MS 全量设成 `deny` 会立刻全红，CI 永远红等于没有 CI。所以先把「已经干净、且回归即 bug」的一小撮锁成硬门禁，其余用 warn 记录技术债、按棘轮逐步收紧。

## 2. Tier 1 · 硬门禁（`[workspace.lints.rust]`，deny）

这 5 条在当前 `--workspace --all-targets` 下实测 **0 告警**，直接 `deny`——它们回归时基本都意味着真问题（无用生命周期、无意义数值 cast、`unsafe fn` 里没包 `unsafe {}` 等）。此后 `missing_debug_implementations` / `redundant_imports` 也清零并翻 `deny`（见第 3 节），一并归入硬门禁：

```toml
[workspace.lints.rust]
ambiguous_negative_literals = "deny"
redundant_lifetimes = "deny"
trivial_numeric_casts = "deny"
unsafe_op_in_unsafe_fn = "deny"
unused_lifetimes = "deny"
```

## 3. Tier 2 · 棘轮（`warn`，逐步清零）

MS 同样推荐的这两条 rustc lint，**存量已清零并翻 `deny`**（棘轮第 2 步已完成）：

```toml
missing_debug_implementations = "deny"   # 8 处：4 处 #[derive(Debug)]，4 处手写 impl
redundant_imports = "deny"               # 13 处（测试里 use super::* 之后的重复 import）
```

> `missing_debug_implementations` 的 4 处手写 `impl Debug`：`Agent`（含闭包 / trait object）、`Sandbox<B>`（backend 无 Debug bound）、`ToolContext`（`Arc<dyn Any>`）、`ToolManager`（`Box<dyn Tool>`）——分别用 `finish_non_exhaustive()` / 只打印 `entries` 数量 / 只打印工具名集合。

clippy 各推荐组与 restriction 子集：**`correctness` / `suspicious` / `style` / `complexity` / `perf` 五组存量均已清零并翻 `deny`**（`correctness` / `suspicious` 各 2 处真实命中；`style` 11 / `complexity` 3 / `perf` 1，全部机械清零）。restriction 子集中 **`allow_attributes_without_reason`（48 处）与 `cargo_common_metadata`（6 处）也已清零并翻 `deny`**。`cargo` / `pedantic` 组仍整体 `warn`，但其中**已清零并逐条翻 `deny` 的子集**见下方批次 A / 批次 B（pedantic 剩余仍是最主要的一块）：

```toml
[workspace.lints.clippy]
cargo       = { level = "warn", priority = -1 }
complexity  = { level = "deny", priority = -1 }   # 已清零并翻 deny
correctness = { level = "deny", priority = -1 }   # 已清零并翻 deny
pedantic    = { level = "warn", priority = -1 }
perf        = { level = "deny", priority = -1 }   # 已清零并翻 deny
style       = { level = "deny", priority = -1 }   # 已清零并翻 deny
suspicious  = { level = "deny", priority = -1 }   # 已清零并翻 deny
# MS 点名的 restriction 子集
allow_attributes_without_reason = "deny"          # 48 处已补 reason / 转 expect 并翻 deny
cargo_common_metadata            = "deny"          # 三个 crate 元数据已补齐并翻 deny
clone_on_ref_ptr                = "deny"          # 71 处已改 Arc::clone(&x) ���翻 deny
map_err_ignore                  = "deny"          # 10 处已具名 |_e| 并翻 deny
undocumented_unsafe_blocks      = "deny"          # 15 处已补 // SAFETY: 并翻 deny
unused_result_ok                = "deny"          # 20 处已改 `let _ = ...;` 并翻 deny
too_long_first_doc_paragraph    = "warn"
literal_string_with_formatting_args = "allow"
# numeric-cast 家族在存量代码里太吵，暂时显式 allow，留待棘轮
cast_possible_truncation = "allow"
cast_sign_loss = "allow"
cast_precision_loss = "allow"
cast_lossless = "allow"
cast_possible_wrap = "allow"
cast_ptr_alignment = "allow"
```

### 3.1 批次 A：pedantic 机械型子集（16 条，已翻 `deny`）

这些子集存量可机械清零（多为 `clippy --fix` + 少量手改），清零后逐条翻 `deny`：

```toml
uninlined_format_args = "deny"
manual_string_new = "deny"
range_plus_one = "deny"
checked_conversions = "deny"
single_char_pattern = "deny"
semicolon_if_nothing_returned = "deny"
unnested_or_patterns = "deny"
unnecessary_join = "deny"
if_not_else = "deny"
unreadable_literal = "deny"
default_trait_access = "deny"
format_collect = "deny"
needless_continue = "deny"
items_after_statements = "deny"
ref_option = "deny"
too_long_first_doc_paragraph = "deny"
```

### 3.2 批次 B：pedantic / restriction 结构型子集（7 条，已翻 `deny`）

```toml
assigning_clones = "deny"
map_unwrap_or = "deny"
float_cmp = "deny"
match_same_arms = "deny"
trivially_copy_pass_by_ref = "deny"
unnecessary_wraps = "deny"
redundant_closure_for_method_calls = "deny"
```

- `assigning_clones`（9 处）：`x = y.clone()` 改 `x.clone_from(&y)` 或 `trimmed.clone_into(&mut s)`。
- `match_same_arms`（16 处）：合并 body 完全相同的臂（如 `System | ContextSummary`、`Retrying | ImagesDropped`）。
- `map_unwrap_or`（1 处）：`Option::map(...).unwrap_or(...)` 改 `map_or_else`。
- `float_cmp`（6 处）：测试里比较浮点加 `#[expect(clippy::float_cmp, reason = "...")]`。
- `trivially_copy_pass_by_ref`（7 处）：`Copy` 类型（`ModelProtocol` / `Selection` / `LoginStep`）的 `&self` / `&T` 改值参。
- `unnecessary_wraps`（12 处）：能去掉 `Result` 的去掉（`codec` / `responses_content` / `draw_rewind_edit` / `encode_input`）；handler 约定必须返回 `Result` 的用 `#[expect(..., reason = "...")]`；`#[tool]` 宏注册钩子（`on_register` / `on_unregister`）签名固定，统一用 `#[expect(clippy::unnecessary_wraps, reason = "hook signature is fixed by the #[tool] macro contract ...")]`。
- `redundant_closure_for_method_calls`：`|x| x.method()` 改 `Type::method`。

### 3.3 批次 C：pedantic / restriction 明细清零（多批，均已翻 `deny`）

这一批把 pedantic 与 restriction 里剩下的、可机械或可局部改写的 lint 逐条清零后翻 `deny`：

- `missing_errors_doc`（12 处）：SDK 公开 API 一律补 `# Errors` 文档段（不做 expect 豁免）。
- `missing_panics_doc`（1 处）：补 `# Panics`。
- `needless_pass_by_value`（15 处）：能改签名的改（`&ParseNestedMeta` / `&ItemFn` / `&ToolConfig` / `&serde_json::Value` / `&Path` 等 8 类）；契约或 move 语义要求 owned 的用 `#[expect(..., reason = "...")]`（如 codec `Decoder` 函数指针契约、`spawn_blocking` 需 owned、测试 helper 构造 owned 字段）。
- `format_push_string`（44 处）+ `write_with_newline`（38 处）：统一改 `let _ = write!/writeln!(recv, ...)`（`use std::fmt::Write as _;`），输出字节完全一致；以单个 `\n` 结尾的用 `writeln!` 并去掉字面量末尾 `\n`，以 `\n\n` 结尾的保留 `write!`。
- `unused_result_ok`（20 处）：`.ok();` → `let _ = ...;`。
- `map_err_ignore`（10 处）：`.map_err(|_| ...)` → `.map_err(|_e| ...)`。
- `clone_on_ref_ptr`（71 处）：`.clone()` → `Arc::clone(&x)`（代码库无 `Rc`，全为 `Arc`；entry 链式先 `let entry = ...` 再 `Arc::clone(entry)`）；顺带修 5 处 `needless_borrow`（receiver 已是引用时去掉 `&`）。
- 其余小项：`return_self_not_must_use`（8）/ `unused_async`（1）/ `manual_let_else`（4）/ `ignore_without_reason`（3，改 `#[ignore = "..."]`）/ `doc_markdown`（2）/ `similar_names`（1）/ `struct_field_names`（5）/ `struct_excessive_bools`（2）/ `unused_self`（4）/ `case_sensitive_file_extension_comparisons`（2）。

> `large_futures`（19）与 `too_many_lines`（15）**保留 `warn`（advisory），未翻 `deny`**：前者全部落在 examples / tests / `bash.rs` 的 `#[tokio::test]` 上（测试里的大 future 属正常），后者是若干结构性大函数（`runtime/agent.rs`、`interface/runtime/mod.rs`、`tui/app.rs`、`memory/curator.rs` 等），清零需要高风险拆分、收益低于风险，按分级策略留作技术债。

> `undocumented_unsafe_blocks` 命中的 15 处全部是测试里的 `std::env::set_var/remove_var`（`crates/shirley/src/tools/read.rs` 9 处，均在 `ENV_LOCK` 串行守卫下；`crates/shirley/src/interface/tui/command.rs` 4 处；`crates/shirley/src/tools/web_search.rs` 1 处；`crates/shirley-agent-sdk/tests/sandbox_smoke.rs` 1 处）。这些属于「测试间环境隔离」的必要 unsafe，已逐处补 `// SAFETY:` 说明并翻 `deny`。

## 4. 落实位置

- **workspace 级**：root `Cargo.toml` 的 `[workspace.lints.rust]` / `[workspace.lints.clippy]`。
- **各 crate 级**：`Cargo.toml` 里写 `[lints] workspace = true` 才继承（三个 crate 都已加：root bin、`shirley-agent-sdk`、`shirley-agent-sdk-macros`）。**新增 crate 必须记得加这一行**，否则不继承 workspace 规则。
- **`#[expect]` 优先于 `#[allow]`**（MS `M-LINT-OVERRIDE-EXPECT`）：需要局部豁免时用 `#[expect(lint, reason = "...")]`，这样 lint 以后不再触发时编译器会提醒你删掉过期豁免，避免 `#[allow]` 越积越多。

## 5. CI（`.github/workflows/ci.yml`）

CI 只把 **Tier 1 当硬门禁**，Tier 2 只展示不阻塞：

- `cargo build --workspace`（默认 feature）
- `cargo build --workspace --features server` + `cargo test --features server`（**固定带 `server` feature**：默认 feature 下 `crates/shirley/src/model_providers.rs` 的 `crate::interface::runtime` 被 `cfg(any(desktop, server))` 门控，无 feature 会编译失败——这是既有问题，非本次引入）
- `cargo clippy --workspace --all-targets --features server`：Tier 1 已 `deny`，任何 Tier 1 回归都会让 clippy 非零退出 → 门禁生效；Tier 2 的 warning 只是打印。
- `cargo fmt --all --check`：**阻塞**。一次性 `cargo fmt --all` 已全量落地（591 处 diff → 0），fmt 现在是硬门禁。
- `cargo-audit` job：扫 `Cargo.lock` 对照 RustSec 漏洞库，发现已知漏洞即失败（用 `taiki-e/install-action` 装预编译二进制）。
- `cargo-hack` job：`cargo hack check --workspace --all-targets --feature-powerset --exclude-features desktop` 验证各 feature 组合能编译（排除需要 webview 工具链的 `desktop`）。
- `cargo-udeps` / `miri` 需要 nightly，暂缓接入。

## 6. 棘轮路线图（后续）

1. ✅ 一次性 `cargo fmt --all` 全量格式化（591 处 diff → 0），fmt 检查已改为阻塞。
2. ✅ 清零 `redundant_imports`（13）与 `missing_debug_implementations`（8），两条 rustc lint 已翻 `deny`。
3. ✅ 清理 clippy `correctness`/`suspicious`（真实命中各 2 处）→ 已翻 `deny`；再清 `style`（11）/`complexity`（3）/`perf`（1）→ 均已翻 `deny`。✅ 清 `allow_attributes_without_reason`（48 处：`dead_code`/`unused_imports` 补 `reason` 保留 `allow`，无条件触发的 `too_many_arguments` 转 `#[expect(..., reason=...)]`）与 `cargo_common_metadata`（6 处：补齐三个 crate 的 description/license/repository/keywords/categories）→ 均翻 `deny`。
4. ✅ 给测试里的 15 处 `set_var/remove_var` 补 `// SAFETY:`，`undocumented_unsafe_blocks` 已清零并翻 `deny`。
5. ✅ 接入 MS 推荐的其它工具链：`cargo-audit`（依赖漏洞）、`cargo-hack`（feature 组合矩阵）已加进 CI；`cargo-udeps` / `miri`（需 nightly，暂缓）。
6. ✅ 清 `pedantic` / `restriction` 的结构型子集：批次 A（16 条机械型：`uninlined_format_args` … `too_long_first_doc_paragraph`）+ 批次 B（7 条：`assigning_clones` / `match_same_arms` / `map_unwrap_or` / `float_cmp` / `trivially_copy_pass_by_ref` / `unnecessary_wraps` / `redundant_closure_for_method_calls`）已全部清零并逐条翻 `deny`。**下一步**：继续清 `pedantic` 剩余存量（`missing_errors_doc` / `needless_pass_by_value` / `too_many_lines` 等）与 restriction 剩余（`clone_on_ref_ptr` / `unused_result_ok` / `map_err_ignore`）。

## 7. 与其它规范的取舍

- 我们**没有**引入 `clippy::nursery`（不稳定、噪声大），MS 也把它列为可选。
- 命名类建议（`M-WEASEL-WORDS`：避免 `Manager`/`Factory` 这类含糊词）无法用 lint 自动检查，属于 code review 约定，暂不强制；现有 `SessionManager` / `AgentFactory` 等命名保留（改名会破坏外部契约且收益低）。
