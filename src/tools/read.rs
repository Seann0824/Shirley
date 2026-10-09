use std::path::PathBuf;

use shirley_agent_sdk::workspace::{WorkSpace, WorkspaceError};
use shirley_agent_sdk::{ToolError, tool};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};

/// 默认读取行数。模型不指定时用这个值，避免"顺手读整个文件"。
const DEFAULT_LINE_COUNT: usize = 200;

/// 单次读取行数硬上限。即便模型显式要求更多，也不会一次灌满上下文。
const MAX_LINE_COUNT: usize = 2000;

/// 单次输出字节上限。行数达标但内容过长（如超长压缩文件、单行巨大）时，
/// 由这一层兜底截断，保证上下文占用可控。
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// 二进制探测窗口：开头这么多字节里出现 NUL 就当成二进制文件拒绝。
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

/// 工作区根目录：优先读环境变量 `SHIRLEY_WORKSPACE`，否则退回当前目录。
/// 与 `bash` 工具保持同一口径，避免两处漂移。
fn workspace_root() -> Option<PathBuf> {
    std::env::var("SHIRLEY_WORKSPACE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// 疑似密钥文件：默认拒绝，避免把 `.env` / 私钥读进上下文再发给模型
/// （见 `docs/security.md` 第三节）。
fn is_secret_file(name: &str) -> bool {
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
fn clip_to_budget(text: &str, budget: usize) -> &str {
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

/// 按行读取工作区内的文本文件，返回带行号的片段。
///
/// 存在的意义是**把读取能力变得可控**：路径受工作区约束、行数与字节数都有
/// 硬上限，模型不再需要（也不该）用 `cat` 裸读整个文件把上下文灌满。
///
/// 返回纯文本而非结构化 JSON，是刻意为之——模型直接可读，且带行号便于
/// 后续用 `start_line` 续读。
#[tool(
    description = "按行读取工作区内文本文件的内容，返回带行号的文本。读取有行数与字节上限；内容被截断时会在末尾提示如何续读。读取大文件请先用 rg/grep 定位，再用本工具读取相关区间，不要用 cat 读整个文件。"
)]
pub async fn read_file(
    #[param(description = "文件路径，相对于工作区根目录；不接受绝对路径或越出工作区的路径")]
    path: String,
    #[param(description = "起始行，从 1 开始；省略时为第 1 行")]
    start_line: Option<usize>,
    #[param(description = "最多读取多少行，省略时 200 行，上限 2000 行")]
    line_count: Option<usize>,
) -> Result<String, ToolError> {
    let start_line = start_line.unwrap_or(1);
    if start_line == 0 {
        return Err(ToolError::ArgumentsError("起始行必须大于 0".into()));
    }
    let requested = line_count.unwrap_or(DEFAULT_LINE_COUNT);
    if requested == 0 {
        return Err(ToolError::ArgumentsError("读取行数必须大于 0".into()));
    }
    let count = requested.min(MAX_LINE_COUNT);

    // 1. 路径先过工作区校验：绝对路径与 `..` 越界都在这里被拒绝。
    let root = workspace_root()
        .ok_or_else(|| ToolError::ExecutionError("无法确定工作区根目录".into()))?;
    let workspace = WorkSpace::new(root)
        .map_err(|error| ToolError::ExecutionError(format!("工作区初始化失败: {error}")))?;
    let resolved = workspace.resolve(&path).map_err(|error| match error {
        // 越界 / 非法路径都是请求侧问题：模型换个路径或写法就能成功，
        // 所以归为参数错误，而不是执行失败。
        WorkspaceError::OutsideRoot(_) | WorkspaceError::InvalidPath(_) => {
            ToolError::ArgumentsError(error.to_string())
        }
        WorkspaceError::Io(_) => ToolError::ExecutionError(error.to_string()),
    })?;

    // 2. 密钥文件默认拒绝。
    if let Some(name) = resolved.file_name().and_then(|name| name.to_str())
        && is_secret_file(name)
    {
        return Err(ToolError::ArgumentsError(format!(
            "拒绝读取疑似密钥文件: {path}"
        )));
    }

    // 3. 目录 / 特殊文件给出明确引导，而不是让读取莫名其妙地失败。
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("读取 {path} 元信息失败: {error}")))?;
    if metadata.is_dir() {
        return Err(ToolError::ExecutionError(format!(
            "{path} 是目录；查看目录请用 bash 的 ls，本工具只读文件"
        )));
    }
    if !metadata.is_file() {
        return Err(ToolError::ExecutionError(format!(
            "{path} 不是普通文件，无法读取"
        )));
    }

    // 4. 打开并做二进制探测。
    let mut file = tokio::fs::File::open(&resolved)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("打开文件 {path} 失败: {error}")))?;
    let mut sniff = vec![0u8; BINARY_SNIFF_BYTES];
    let sniffed = file
        .read(&mut sniff)
        .await
        .map_err(|error| ToolError::ExecutionError(format!("读取文件 {path} 失败: {error}")))?;
    if sniff[..sniffed].contains(&0) {
        return Err(ToolError::ExecutionError(format!(
            "{path} 看起来是二进制文件，已拒绝读取"
        )));
    }
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| ToolError::ExecutionError(format!("定位文件 {path} 失败: {error}")))?;

    // 5. 跳到起始行。
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut line_no = 1usize;
    while line_no < start_line {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|error| ToolError::ExecutionError(format!("读取文件 {path} 失败: {error}")))?;
        if read == 0 {
            return Ok(format!(
                "[{path} 在第 {start_line} 行之前已结束，没有可读取的内容]"
            ));
        }
        line_no += 1;
    }

    // 6. 逐行读取，同时受行数与字节两个上限约束。
    let mut out = String::new();
    let mut emitted = 0usize;
    let mut truncated = false;
    let mut more_lines = false;
    loop {
        if emitted >= count {
            // 行数已达标，探一下还有没有后续内容，决定是否提示续读。
            buffer.clear();
            let read = reader.read_until(b'\n', &mut buffer).await.map_err(|error| {
                ToolError::ExecutionError(format!("读取文件 {path} 失败: {error}"))
            })?;
            more_lines = read > 0;
            break;
        }

        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .await
            .map_err(|error| ToolError::ExecutionError(format!("读取文件 {path} 失败: {error}")))?;
        if read == 0 {
            break;
        }

        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim_end_matches(['\n', '\r']);
        let prefix = format!("{line_no:>6} | ");

        let remaining = MAX_OUTPUT_BYTES.saturating_sub(out.len());
        // 至少要为前缀与换行留出空间，否则这一行放不下，直接截断。
        if remaining <= prefix.len() + 1 {
            truncated = true;
            break;
        }
        out.push_str(&prefix);

        let budget = remaining - prefix.len() - 1;
        let clipped = clip_to_budget(text, budget);
        out.push_str(clipped);
        out.push('\n');

        emitted += 1;
        if clipped.len() < text.len() {
            // 单行超出剩余字节预算，本行已被截断。
            truncated = true;
            break;
        }
        line_no += 1;
    }

    if more_lines {
        truncated = true;
    }

    if emitted == 0 {
        return Ok(format!(
            "[{path} 在第 {start_line} 行之前已结束，没有可读取的内容]"
        ));
    }

    let last_line = start_line + emitted - 1;
    let mut result = format!("# {path}（第 {start_line}-{last_line} 行）\n{out}");
    if truncated {
        let next = last_line + 1;
        result.push_str(&format!(
            "[内容已截断；如需继续，请用 start_line={next} 续读]\n"
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试会改动进程级的工作区环境变量，串行执行避免相互干扰。
    // 用 tokio 的 Mutex：guard 允许跨 await 持有，std 的会被 clippy 拦下。
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 建一个唯一的临时工作区目录。
    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_read_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn reads_lines_with_line_numbers() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("basic");
        std::fs::write(dir.join("a.txt"), "first\nsecond\nthird\n").unwrap();
        // SAFETY: 测试内串行改环境变量，不与其他线程并发。
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read_file("a.txt".into(), None, None).await.unwrap();

        assert!(out.contains("     1 | first"), "应带行号: {out}");
        assert!(out.contains("     3 | third"), "应读到第三行: {out}");
        assert!(!out.contains("已截断"), "内容完整时不应提示截断: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn supports_start_line_and_continuation() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("range");
        let body: String = (1..=10).map(|n| format!("line{n}\n")).collect();
        std::fs::write(dir.join("b.txt"), body).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read_file("b.txt".into(), Some(3), Some(2)).await.unwrap();

        assert!(out.contains("     3 | line3"), "应从第 3 行开始: {out}");
        assert!(out.contains("     4 | line4"), "应读两行: {out}");
        assert!(!out.contains("line5"), "不应越过行数上限: {out}");
        assert!(out.contains("start_line=5"), "应提示续读位置: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_outside_workspace() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("escape");
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read_file("../etc/passwd".into(), None, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ArgumentsError(_)),
            "越界应是参数错误: {err:?}"
        );
        assert!(err.to_string().contains("outside workspace"), "应说明越界: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("absolute");
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read_file("/etc/hosts".into(), None, None).await.unwrap_err();
        assert!(
            matches!(err, ToolError::ArgumentsError(_)),
            "绝对路径应被拒绝: {err:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_secret_file() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("secret");
        std::fs::write(dir.join(".env"), "LOCAL_API_KEY=leak").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read_file(".env".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("密钥"), "应拒绝密钥文件: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_binary_file() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("binary");
        std::fs::write(dir.join("bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read_file("bin".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("二进制"), "应拒绝二进制: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_directory() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("dir");
        std::fs::create_dir(dir.join("sub")).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let err = read_file("sub".into(), None, None).await.unwrap_err();
        assert!(err.to_string().contains("目录"), "应引导用 ls: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn caps_output_bytes() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("bytes");
        // 一行就超过字节上限，必须被截断而不是整段塞进上下文。
        let big = "a".repeat(MAX_OUTPUT_BYTES * 2);
        std::fs::write(dir.join("big.txt"), big).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        let out = read_file("big.txt".into(), None, None).await.unwrap();
        assert!(out.contains("已截断"), "应提示截断: 长度 {}", out.len());
        assert!(
            out.len() <= MAX_OUTPUT_BYTES + 200,
            "输出应受字节上限约束，实际 {}",
            out.len()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn registers_and_invokes_through_tool_manager() {
        use shirley_agent_sdk::{ToolCall, ToolManager};

        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("manager");
        std::fs::write(dir.join("m.txt"), "alpha\nbeta\n").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        // 走宏生成的 `tool()`，验证注册 + 参数反序列化 + 调用的完整链路。
        let mut manager = ToolManager::new();
        manager.register(read_file::tool()).expect("注册应成功");

        let definition = manager
            .definitions()
            .into_iter()
            .find(|d| d.name == "read_file")
            .expect("应能拿到 read_file 定义");
        assert!(
            definition.description.contains("行"),
            "描述应说明读取语义: {}",
            definition.description
        );

        let call = ToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"m.txt","start_line":1,"line_count":1}"#.into(),
        };
        let output = manager
            .invoke(&call, shirley_agent_sdk::ToolContext::new())
            .await
            .expect("调用应成功");
        let text = output.as_str().unwrap_or_default();
        assert!(text.contains("alpha"), "应读到内容: {text}");
        assert!(!text.contains("beta"), "应受行数限制: {text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn caps_line_count() {
        let _guard = ENV_LOCK.lock().await;
        let dir = temp_workspace("lines");
        let body: String = (1..=MAX_LINE_COUNT + 50)
            .map(|n| format!("l{n}\n"))
            .collect();
        std::fs::write(dir.join("many.txt"), body).unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir) };

        // 显式要求远超上限的行数，也只能读到上限。
        let out = read_file("many.txt".into(), None, Some(MAX_LINE_COUNT * 2))
            .await
            .unwrap();
        let lines = out.lines().filter(|l| l.contains(" | ")).count();
        assert_eq!(lines, MAX_LINE_COUNT, "应恰好读到行数上限");
        assert!(out.contains("已截断"), "应提示还有更多: {out}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
