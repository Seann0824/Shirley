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

> **进度（本轮）**：一次性 `cargo fmt --all` 已全量落地（591 处 diff → 0），fmt 检查已改为阻塞；`redundant_imports` / `missing_debug_implementations` 两条 rustc lint 与 clippy `correctness` / `suspicious` 两组已清零并翻 `deny`。详见第 6 节。

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

clippy 各推荐组与 restriction 子集，**`correctness` / `suspicious` 已翻 `deny`**（各仅 2 处真实命中：`duplicated_attributes` 与 `await_holding_lock`，均已修复），其余组仍 `warn`（MS 默认也是 warn）：

```toml
[workspace.lints.clippy]
cargo       = { level = "warn", priority = -1 }
complexity  = { level = "warn", priority = -1 }
correctness = { level = "deny", priority = -1 }   # 已清零并翻 deny
pedantic    = { level = "warn", priority = -1 }
perf        = { level = "warn", priority = -1 }
style       = { level = "warn", priority = -1 }
suspicious  = { level = "deny", priority = -1 }   # 已清零并翻 deny
# MS 点名的 restriction 子集
allow_attributes_without_reason = "warn"
clone_on_ref_ptr                = "warn"
map_err_ignore                  = "warn"
undocumented_unsafe_blocks      = "warn"
unused_result_ok                = "warn"
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

> 注：`undocumented_unsafe_blocks` 会命中测试里的 `std::env::set_var/remove_var`（`src/interface/tui/command.rs`、`src/tools/read.rs`）。这些属于「测试间环境隔离」的必要 unsafe，等棘轮推进时补 `// SAFETY:` 注释即可，现在先 warn 不阻塞。

## 4. 落实位置

- **workspace 级**：root `Cargo.toml` 的 `[workspace.lints.rust]` / `[workspace.lints.clippy]`。
- **各 crate 级**：`Cargo.toml` 里写 `[lints] workspace = true` 才继承（三个 crate 都已加：root bin、`shirley-agent-sdk`、`shirley-agent-sdk-macros`）。**新增 crate 必须记得加这一行**，否则不继承 workspace 规则。
- **`#[expect]` 优先于 `#[allow]`**（MS `M-LINT-OVERRIDE-EXPECT`）：需要局部豁免时用 `#[expect(lint, reason = "...")]`，这样 lint 以后不再触发时编译器会提醒你删掉过期豁免，避免 `#[allow]` 越积越多。

## 5. CI（`.github/workflows/ci.yml`）

CI 只把 **Tier 1 当硬门禁**，Tier 2 只展示不阻塞：

- `cargo build --workspace`（默认 feature）
- `cargo build --workspace --features server` + `cargo test --features server`（**固定带 `server` feature**：默认 feature 下 `src/model_providers.rs` 的 `crate::interface::runtime` 被 `cfg(any(desktop, server))` 门控，无 feature 会编译失败——这是既有问题，非本次引入）
- `cargo clippy --workspace --all-targets --features server`：Tier 1 已 `deny`，任何 Tier 1 回归都会让 clippy 非零退出 → 门禁生效；Tier 2 的 warning 只是打印。
- `cargo fmt --all --check`：**阻塞**。一次性 `cargo fmt --all` 已全量落地（591 处 diff → 0），fmt 现在是硬门禁。

## 6. 棘轮路线图（后续）

1. ✅ 一次性 `cargo fmt --all` 全量格式化（591 处 diff → 0），fmt 检查已改为阻塞。
2. ✅ 清零 `redundant_imports`（13）与 `missing_debug_implementations`（8），两条 rustc lint 已翻 `deny`。
3. ✅ 清理 clippy `correctness`/`suspicious`（真实命中各 2 处）→ 已翻 `deny`。**下一步**：清 `style`/`complexity`/`perf` → 再 `pedantic`（`pedantic` 存量仍 ~392，是最大的一块）。
4. ⬜ 给测试里的 `set_var/remove_var` 补 `// SAFETY:`，让 `undocumented_unsafe_blocks` 清零。
5. ⬜ 接入 MS 推荐的其它工具链：`cargo-audit`（依赖漏洞，稳定可接入）、`cargo-hack`（feature 组合矩阵）、`cargo-udeps` / `miri`（需 nightly，暂缓）。

## 7. 与其它规范的取舍

- 我们**没有**引入 `clippy::nursery`（不稳定、噪声大），MS 也把它列为可选。
- 命名类建议（`M-WEASEL-WORDS`：避免 `Manager`/`Factory` 这类含糊词）无法用 lint 自动检查，属于 code review 约定，暂不强制；现有 `SessionManager` / `AgentFactory` 等命名保留（改名会破坏外部契约且收益低）。
