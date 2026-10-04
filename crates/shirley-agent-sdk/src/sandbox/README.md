# sandbox

进程沙盒层。目标：把"执行外部命令"从"把 shell 交给模型"变成"在一个受限的假世界里跑"。

## 分层

| 文件 | 职责 |
|---|---|
| `spec.rs` | 平台无关的执行意图：跑什么、能碰什么、出网策略（bon builder） |
| `backend/mod.rs` | 后端抽象 `SandboxBackend` + 能力自描述 `Capabilities` |
| `backend/process.rs` | 裸进程后端（无隔离），仅用于开发/测试与降级基线 |
| `output.rs` | 统一结果：stdout/stderr/exit_code/timed_out/**degraded** |
| `mod.rs` | 门面 `Sandbox`：统一施加超时，组装结果 |

## 四条设计原则

1. **边界在"能碰什么"，不在命令字符串。** spec 用 `program + args`，不做 `bash -c` 那种无法静态判断的字符串。
2. **默认拒绝。** 网络默认断，环境变量默认不继承，写路径默认空。
3. **超时不可逃避。** 由 `Sandbox::run` 在最外层用 `tokio::time::timeout` 施加，后端赖着不停也会被 drop + kill。
4. **降级透明。** 后端做不到的约束必须写进 `output.degraded`，上层可据此拒绝结果，而不是被"看起来跑了"骗过去。

## 接真实隔离（下一步）

`ProcessBackend` 只是把链路跑通。生产按平台替换：

- **macOS**：新增 `backend::sandbox_exec`，把 spec 编译成 `sandbox-exec` 的 SBPL profile（`(deny default)` + 放行 read/write/network）。
- **Linux**：新增 `backend::bwrap`，映射为 `bwrap` 参数（`--ro-bind` / `--tmpfs` / `--unshare-net` / `--die-with-parent`）。
- **高保证**：新增 `backend::microvm`（Firecracker），每任务一个独立内核。

新增后端只需实现 `SandboxBackend`，`Sandbox` 门面与上层调用无需改动。

## 与权限层的关系

本层只负责"执行与隔离"。**"该不该执行"由 policy 层决定**（见 `docs/security.md` 第五节）。
正确顺序：`policy.check(spec) -> Allow/Deny/Ask` → 放行后再交给 `Sandbox::run`。
