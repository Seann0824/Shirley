//! 应用层工具的公共辅助。
//!
//! `workspace_root()` 原本在 `bash.rs` 与 `read.rs` 各抄一份，`is_secret_file()` /
//! `clip_to_budget()` / `BINARY_SNIFF_BYTES` 只在 `read.rs`。`apply_patch` 需要同一
//! 口径的护栏，故抽到一处，避免三处漂移（见 `docs/apply-patch.md` 第六节）。

use std::path::PathBuf;

/// 跨模块共享的测试锁：`read.rs` / `apply_patch.rs` 的测试都会改进程级环境变量
/// `SHIRLEY_WORKSPACE`，必须串行，否则互相污染（同一个锁，不是各模块一把）。
#[cfg(test)]
pub static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 二进制探测窗口：开头这么多字节里出现 NUL 就当成二进制文件。
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// 工作区根目录：优先读环境变量 `SHIRLEY_WORKSPACE`，否则退回当前目录。
///
/// 沙盒会把它作为 cwd，读取 / 编辑工具用它做路径越界校验。
pub fn workspace_root() -> Option<PathBuf> {
    std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// 疑似密钥文件：默认拒绝读写，避免把 `.env` / 私钥带进上下文或写进仓库
/// （见 `docs/security.md` 第三节）。
pub fn is_secret_file(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == ".netrc"
        || name == "credentials"
        || name.starts_with("id_rsa")
        || name.starts_with("id_dsa")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_ed25519")
}

/// 按字符边界把 `text` 截到不超过 `budget` 字节。
pub fn clip_to_budget(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut end = 0;
    for (index, ch) in text.char_indices() {
        if index + ch.len_utf8() > budget {
            break;
        }
        end = index + ch.len_utf8();
    }
    &text[..end]
}

/// 测试用 RAII 守卫：持有 [`ENV_LOCK`]、把 `SHIRLEY_WORKSPACE` 指向 `path`，
/// **drop 时还原**为进入前的值（原本没有则移除）。
///
/// 关键在"还原"——只 `set_var` 不还原会让后续测试（尤其 `bash` 的沙盒 cwd）
/// 读到已删除的临时目录，触发顺序敏感的偶发失败（曾经真实发生过）。
#[cfg(test)]
pub struct WorkspaceGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
}

#[cfg(test)]
impl WorkspaceGuard {
    pub async fn set(path: &std::path::Path) -> Self {
        let lock = ENV_LOCK.lock().await;
        let previous = std::env::var("SHIRLEY_WORKSPACE").ok();
        // SAFETY: 持锁期间独占，改 env 的测试彼此串行。
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", path) };
        Self {
            _lock: lock,
            previous,
        }
    }
}

#[cfg(test)]
impl Drop for WorkspaceGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var("SHIRLEY_WORKSPACE", value) },
            None => unsafe { std::env::remove_var("SHIRLEY_WORKSPACE") },
        }
    }
}
