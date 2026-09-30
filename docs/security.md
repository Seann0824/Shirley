**Shirley 技术方案 · 安全与权限（P0）**

**一、问题定性**

当前 `bash` 工具等价于"把 shell 交给模型"。黑名单是装饰性的：

```rust
// src/tools/bash.rs 现状
let args: Vec<&str> = command.split_whitespace().collect();
let is_blacklisted = args.iter().any(|arg| BLACKLIST.contains(arg));
// ...
cmd.arg("-c").arg(&command);
```

`split_whitespace` 的结果只用于检查，真正执行的是 `bash -c <原字符串>`。因此以下全部能绕过：

- `/bin/rm -rf /`（`rm` 不是独立 token）
- `sh -c "rm -rf /"`（`rm` 在引号内被拆分后才出现，但检查针对的是 `sh`、`-c`）
- `echo cm0gLXJm | base64 -d | bash`
- `r''m -rf /`（shell 会拼接，字符串匹配看不到 `rm`）

同时 `read` 没有任何路径限制，可以读 `~/.ssh/id_rsa`，也可以读 `.env` 把 API key 读进上下文再发给模型。

**二、设计目标**

1. 默认拒绝，显式放行。
2. 边界在"工作区根目录"这一层，而不是在命令字符串里做模式匹配。
3. 权限策略是 SDK 能力，不是应用层硬编码。
4. 工具调用可审计：谁调了什么、是否被拦、拦的原因。

**三、密钥处理（立即执行，不需要写代码）**

1. 轮换 `LOCAL_API_KEY` 与 `DEEPSEEK_API_KEY`（`.env` 中曾以明文存在，`Agent.md` 也记录了泄露历史）。
2. 新增 `.env.example`，只留键名与占位符：

```ini
LOCAL_API_KEY=your-key-here
LOCAL_BASE_URL=http://127.0.0.1:8788/v1/chat/completions
LOCAL_CONTEXT_WINDOW_TOKENS=104858
```

3. 在 `read` 工具侧增加默认脱敏：命中 `.env`、`*.pem`、`id_rsa*`、`*.key` 等模式时拒绝或遮蔽内容。
4. 长期：密钥不落盘，改为启动时从环境或系统钥匙串注入。

**四、工作区隔离**

在 SDK 侧引入工作区概念，所有文件类工具与 `bash` 的 cwd 都受其约束：

```rust
// crates/agent-sdk/src/workspace.rs（新增）
pub struct Workspace {
    root: PathBuf,          // 规范化后的绝对路径
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> { /* canonicalize */ }

    // 解析并校验路径，越界即 Err
    pub fn resolve(&self, input: &str) -> Result<PathBuf, WorkspaceError> {
        let joined = self.root.join(input);
        let canonical = canonicalize_allow_missing(&joined)?; // 处理不存在的文件
        if !canonical.starts_with(&self.root) {
            return Err(WorkspaceError::OutsideRoot { requested: input.into() });
        }
        Ok(canonical)
    }
}
```

要点：

- 用 `canonicalize` 解析 `..` 与符号链接后再比较，不能靠字符串前缀。
- 对"目标文件尚不存在"的写场景，`canonicalize` 会失败，需要先规范化其父目录。
- 拒绝路径时返回结构化错误，让模型知道"被拒绝"而不是"文件不存在"。

**五、权限层**

把硬编码黑名单升级为 SDK 内的策略对象：

```rust
// crates/agent-sdk/src/permission.rs（新增）
pub enum Decision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },   // 预留：交给上层交互确认
}

pub trait PermissionPolicy: Send + Sync {
    fn check(&self, request: &PermissionRequest) -> Decision;
}

pub struct PermissionRequest<'a> {
    pub tool: &'a str,
    pub arguments: &'a serde_json::Value,
}
```

内置实现 `WorkspacePolicy`：

- 文件类工具：`workspace.resolve(path)` 通过则 Allow，否则 Deny。
- `bash`：先做静态拒绝（危险命令、重定向到工作区外、`sudo`），其余 Allow 并记录审计。

**六、`bash` 工具改造**

**6.1 返回 stderr 与退出码**

现状只返回 `stdout`，编译失败时模型看到空字符串，会陷入重试同一个错误命令的循环。改为：

```rust
#[derive(serde::Serialize)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}
```

模型需要看到失败信息才能自我纠正，这是能力问题，不只是体验问题。

**6.2 执行方式**

保留 `bash -c`（否则会破坏管道、重定向等正常用法），但边界靠两层补：

- **静态拒绝**：在 `-c` 之前，用解析后的 token 判断危险模式；对无法静态判断的（如 `base64 | bash`），进入 `Ask` 或直接 Deny。
- **运行时约束**：cwd 锁定工作区；可选 `ulimit`；后续可接入 OS 级沙箱（macOS `sandbox-exec` / Linux `bubblewrap`）。

  其中"OS 级沙箱"已单独成层并落地骨架，详见 `sandbox.md`。

**6.3 审计**

每次工具调用记录 `(timestamp, tool, arguments, decision, exit_code)` 到 `~/.shirley/audit.log`。长任务出问题时这是唯一的复盘依据。

**七、验收**

1. `/bin/rm -rf /`、`echo ... | base64 -d | bash` 被 Deny 且有原因。
2. `read ~/.ssh/id_rsa`、`read ../../etc/passwd` 被 Deny。
3. `cargo build` 失败时模型能收到 stderr 与退出码。
4. 审计日志包含上述全部被拦截记录。
