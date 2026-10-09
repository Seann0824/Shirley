//! `apply_patch` —— 对工作区**文本文件**应用 Codex 风格上下文补丁。
//!
//! 设计见 `docs/apply-patch.md`。要点：
//!
//! - **格式**：`*** Begin Patch` / `*** Update File:` / `*** Add File:` /
//!   `*** Delete File:` / `*** End Patch`；更新用 `@@` 锚点 + 行首 ` `/`+`/`-`。
//! - **与 git 无关**：历史与回滚的真相来源是本工具持有的内存编辑栈，不碰 git。
//! - **原子**：先整体解析、整体匹配，任一 hunk 失败则一个字节都不写。
//! - **纯文本**：二进制 / 非 UTF-8 一律拒绝，绝不做有损转换。
//! - **失败响亮**：所有失败冒泡成 [`ToolError`]，绝不吞错、绝不返回"部分成功"。

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use shirley_agent_sdk::workspace::{WorkSpace, WorkspaceError};
use shirley_agent_sdk::{ToolContext, ToolError, tool};

use crate::tools::util::{BINARY_SNIFF_BYTES, is_secret_file, workspace_root};

/// 补丁文本大小上限：防一次调用灌爆解析器 / 上下文。
const MAX_PATCH_BYTES: usize = 1024 * 1024;
/// 单个目标文件大小上限：超过就不编辑（这类文件也不该用补丁改）。
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
/// 单次补丁最多涉及的文件操作数。
const MAX_OPS: usize = 128;

// ============================ 数据模型 ============================

/// 一个补丁块。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOp {
    /// 更新文件；`moved` 为 `*** Move to:` 的目标（重命名 / 移动）。
    Update {
        path: String,
        hunks: Vec<Hunk>,
        moved: Option<String>,
    },
    /// 新增文件。
    Add { path: String, lines: Vec<String> },
    /// 删除文件。
    Delete { path: String },
    /// 覆盖写入：由同一补丁里的 `Delete File` + `Add File`（同路径）合并而来。
    /// 作为确定性的单点原子操作，整体替换文件内容（不存在则等同新建）。
    Overwrite { path: String, lines: Vec<String> },
}

impl FileOp {
    fn path(&self) -> &str {
        match self {
            FileOp::Update { path, .. }
            | FileOp::Add { path, .. }
            | FileOp::Delete { path }
            | FileOp::Overwrite { path, .. } => path,
        }
    }
}

/// 一个 hunk：`@@` 锚点（可选）+ 若干行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    anchor: Option<String>,
    lines: Vec<HunkLine>,
}

/// hunk 内一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Context,
    Add,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HunkLine {
    kind: LineKind,
    text: String,
}

// ============================ 解析器 ============================

/// 解析补丁文本。任一语法错误都返回 `Err`，**一个字节都不写**。
fn parse_patch(input: &str) -> Result<Vec<FileOp>, String> {
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;

    // 跳过开头空行，要求以 `*** Begin Patch` 起始。
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    if i >= lines.len() || lines[i].trim() != "*** Begin Patch" {
        return Err("补丁必须以 `*** Begin Patch` 开头".into());
    }
    i += 1;

    let mut ops: Vec<FileOp> = Vec::new();
    let mut seen_end = false;

    while i < lines.len() {
        // 段间空行允许存在。
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        if i >= lines.len() {
            break;
        }
        let line = lines[i];

        if line.trim() == "*** End Patch" {
            seen_end = true;
            i += 1;
            break;
        }

        if let Some(rest) = line.strip_prefix("*** Update File:") {
            let (op, next) = parse_update(rest, &lines, i + 1)?;
            ops.push(op);
            i = next;
        } else if let Some(rest) = line.strip_prefix("*** Add File:") {
            let (op, next) = parse_add(rest, &lines, i + 1)?;
            ops.push(op);
            i = next;
        } else if let Some(rest) = line.strip_prefix("*** Delete File:") {
            let (op, next) = parse_delete(rest, &lines, i + 1)?;
            ops.push(op);
            i = next;
        } else {
            return Err(format!("无法识别的补丁行: `{line}`"));
        }
    }

    if !seen_end {
        return Err("补丁缺少 `*** End Patch` 结尾".into());
    }
    // `*** End Patch` 之后只允许空行。
    while i < lines.len() {
        if !lines[i].trim().is_empty() {
            return Err(format!("`*** End Patch` 之后不应再有内容: `{}`", lines[i]));
        }
        i += 1;
    }

    if ops.is_empty() {
        return Err("补丁没有任何文件操作".into());
    }
    if ops.len() > MAX_OPS {
        return Err(format!("一次补丁最多 {MAX_OPS} 个文件操作"));
    }

    // 同一路径重复出现通常有歧义，但 `Delete File` + `Add File`（同路径、Delete 在前）
    // 是"覆盖写入"的既定写法，合并成 Overwrite；其余重复一律拒绝。
    merge_overwrites(ops)
}

/// 把 `Delete File: x` + `Add File: x`（同路径、Delete 在前）合并为 `Overwrite`。
/// 任何其它形式的重复路径都返回 `Err`。
fn merge_overwrites(ops: Vec<FileOp>) -> Result<Vec<FileOp>, String> {
    use std::collections::{HashMap, HashSet};

    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        index.entry(op.path().to_string()).or_default().push(i);
    }

    // delete 下标 -> (path, 要写入的行)
    let mut overwrite: HashMap<usize, (String, Vec<String>)> = HashMap::new();
    let mut skip: HashSet<usize> = HashSet::new();
    for (path, idxs) in &index {
        match idxs.as_slice() {
            [_] => {}
            [a, b] => {
                if matches!(ops[*a], FileOp::Delete { .. }) && matches!(ops[*b], FileOp::Add { .. }) {
                    if let FileOp::Add { lines, .. } = &ops[*b] {
                        overwrite.insert(*a, (path.clone(), lines.clone()));
                        skip.insert(*b);
                    }
                } else {
                    return Err(format!("同一路径在一次补丁里出现了多次: `{path}`"));
                }
            }
            _ => return Err(format!("同一路径在一次补丁里出现了多次: `{path}`")),
        }
    }

    let mut result: Vec<FileOp> = Vec::with_capacity(ops.len());
    for (i, op) in ops.into_iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        if let Some((path, lines)) = overwrite.remove(&i) {
            result.push(FileOp::Overwrite { path, lines });
        } else {
            result.push(op);
        }
    }
    Ok(result)
}

fn clean_path(rest: &str) -> Result<String, String> {
    let path = rest.trim();
    if path.is_empty() {
        return Err("文件路径不能为空".into());
    }
    Ok(path.to_string())
}

/// 该行是否是段头（`*** ...`）。
fn is_header(line: &str) -> bool {
    line.starts_with("*** ")
}

fn parse_update(
    rest: &str,
    lines: &[&str],
    mut i: usize,
) -> Result<(FileOp, usize), String> {
    let path = clean_path(rest)?;
    let mut moved: Option<String> = None;
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<Hunk> = None;

    while i < lines.len() {
        let line = lines[i];
        if is_header(line) {
            if let Some(dest) = line.strip_prefix("*** Move to:") {
                if moved.is_some() || !hunks.is_empty() || current.is_some() {
                    return Err("`*** Move to:` 只能紧跟在 `*** Update File:` 之后".into());
                }
                moved = Some(clean_path(dest)?);
                i += 1;
                continue;
            }
            break;
        }

        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(Hunk {
                anchor: Some(line.to_string()),
                lines: Vec::new(),
            });
            i += 1;
            continue;
        }

        let hunk_line = parse_hunk_line(line)?;
        match current {
            Some(ref mut hunk) => hunk.lines.push(hunk_line),
            None => {
                current = Some(Hunk {
                    anchor: None,
                    lines: vec![hunk_line],
                });
            }
        }
        i += 1;
    }

    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }

    if hunks.is_empty() && moved.is_none() {
        return Err(format!("`*** Update File: {path}` 既没有改动也没有 `*** Move to:`"));
    }

    Ok((FileOp::Update { path, hunks, moved }, i))
}

fn parse_hunk_line(line: &str) -> Result<HunkLine, String> {
    if let Some(text) = line.strip_prefix('+') {
        Ok(HunkLine { kind: LineKind::Add, text: text.to_string() })
    } else if let Some(text) = line.strip_prefix('-') {
        Ok(HunkLine { kind: LineKind::Remove, text: text.to_string() })
    } else if let Some(text) = line.strip_prefix(' ') {
        Ok(HunkLine { kind: LineKind::Context, text: text.to_string() })
    } else if line.is_empty() {
        // 容忍模型把空上下文行写成真正的空行。
        Ok(HunkLine { kind: LineKind::Context, text: String::new() })
    } else {
        Err(format!("更新块内的行必须以 `+`、`-` 或空格开头: `{line}`"))
    }
}

fn parse_add(rest: &str, lines: &[&str], mut i: usize) -> Result<(FileOp, usize), String> {
    let path = clean_path(rest)?;
    let mut content: Vec<String> = Vec::new();
    while i < lines.len() && !is_header(lines[i]) {
        let line = lines[i];
        if let Some(text) = line.strip_prefix('+') {
            content.push(text.to_string());
        } else if line.is_empty() {
            content.push(String::new());
        } else {
            return Err(format!("`*** Add File:` 块内的行必须以 `+` 开头: `{line}`"));
        }
        i += 1;
    }
    Ok((FileOp::Add { path, lines: content }, i))
}

fn parse_delete(rest: &str, lines: &[&str], mut i: usize) -> Result<(FileOp, usize), String> {
    let path = clean_path(rest)?;
    while i < lines.len() && !is_header(lines[i]) {
        if !lines[i].trim().is_empty() {
            return Err(format!("`*** Delete File:` 块内不应有内容: `{}`", lines[i]));
        }
        i += 1;
    }
    Ok((FileOp::Delete { path }, i))
}

// ============================ 文本表示 ============================

/// 一个已读取的文本文件：保留 BOM / 换行风格 / 尾换行。
#[derive(Debug, Clone)]
struct TextFile {
    bom: bool,
    newline: String,
    trailing_newline: bool,
    lines: Vec<String>,
    raw: Vec<u8>,
}

impl TextFile {
    /// 从磁盘读取并做纯文本护栏（二进制 / 非 UTF-8 / 大小上限）。
    fn read(path: &Path, display: &str) -> Result<Self, ToolError> {
        let raw = fs::read(path)
            .map_err(|error| ToolError::ExecutionError(format!("读取 {display} 失败: {error}")))?;
        if raw.len() > MAX_FILE_BYTES {
            return Err(ToolError::ArgumentsError(format!(
                "{display} 超过单文件上限（{} 字节），拒绝用补丁编辑",
                MAX_FILE_BYTES
            )));
        }
        let sniff_len = raw.len().min(BINARY_SNIFF_BYTES);
        if raw[..sniff_len].contains(&0) {
            return Err(ToolError::ArgumentsError(format!(
                "{display} 看起来是二进制文件，补丁工具只处理文本"
            )));
        }
        let content = String::from_utf8(raw.clone()).map_err(|_| {
            ToolError::ArgumentsError(format!("{display} 不是 UTF-8 文本，拒绝编辑（不做有损转换）"))
        })?;

        let bom = content.starts_with('\u{feff}');
        let body = if bom { &content['\u{feff}'.len_utf8()..] } else { content.as_str() };
        let newline = if body.contains("\r\n") { "\r\n" } else { "\n" }.to_string();
        let trailing_newline = body.ends_with('\n');

        let mut lines: Vec<String> = if body.is_empty() {
            Vec::new()
        } else {
            body.split('\n')
                .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
                .collect()
        };
        if trailing_newline && !lines.is_empty() {
            lines.pop();
        }

        Ok(Self { bom, newline, trailing_newline, lines, raw })
    }

    /// 用（可能被改过的）行重建文件字节，保留原有风格。
    fn render(&self, lines: &[String]) -> Vec<u8> {
        render_lines(self.bom, &self.newline, self.trailing_newline, lines)
    }
}

fn render_lines(bom: bool, newline: &str, trailing_newline: bool, lines: &[String]) -> Vec<u8> {
    let mut text = String::new();
    if bom {
        text.push('\u{feff}');
    }
    text.push_str(&lines.join(newline));
    if trailing_newline && !lines.is_empty() {
        text.push_str(newline);
    }
    text.into_bytes()
}

// ============================ 匹配 ============================

/// 匹配结果。
#[derive(Debug, PartialEq, Eq)]
enum Match {
    /// 唯一命中；`tolerant` 表示是靠空白 / 缩进容错命中的（需显式标注）。
    Unique { pos: usize, tolerant: bool },
    /// 没找到。
    None,
    /// 多处命中，无法确定改哪里。
    Ambiguous(usize),
}

fn line_eq(a: &str, b: &str, tolerant: bool) -> bool {
    if tolerant { a.trim() == b.trim() } else { a == b }
}

fn matches_at(file: &[String], start: usize, pattern: &[String], tolerant: bool) -> bool {
    if start + pattern.len() > file.len() {
        return false;
    }
    pattern
        .iter()
        .enumerate()
        .all(|(offset, expected)| line_eq(&file[start + offset], expected, tolerant))
}

/// 在文件行里定位 `pattern`：先精确、后空白 / 缩进容错；要求唯一。
fn locate(file: &[String], pattern: &[String]) -> Match {
    if pattern.is_empty() || pattern.len() > file.len() {
        return Match::None;
    }
    let last_start = file.len() - pattern.len();
    let exact: Vec<usize> = (0..=last_start)
        .filter(|&start| matches_at(file, start, pattern, false))
        .collect();
    match exact.len() {
        1 => return Match::Unique { pos: exact[0], tolerant: false },
        n if n > 1 => return Match::Ambiguous(n),
        _ => {}
    }
    let tolerant: Vec<usize> = (0..=last_start)
        .filter(|&start| matches_at(file, start, pattern, true))
        .collect();
    match tolerant.len() {
        1 => Match::Unique { pos: tolerant[0], tolerant: true },
        0 => Match::None,
        n => Match::Ambiguous(n),
    }
}

/// 应用一个 hunk，返回（新增行数、删除行数、是否用了容错）。
fn apply_hunk(lines: &mut Vec<String>, hunk: &Hunk, display: &str) -> Result<(usize, usize, bool), ToolError> {
    let before: Vec<String> = hunk
        .lines
        .iter()
        .filter(|line| line.kind != LineKind::Add)
        .map(|line| line.text.clone())
        .collect();
    let after: Vec<String> = hunk
        .lines
        .iter()
        .filter(|line| line.kind != LineKind::Remove)
        .map(|line| line.text.clone())
        .collect();
    let added = hunk.lines.iter().filter(|line| line.kind == LineKind::Add).count();
    let removed = hunk.lines.iter().filter(|line| line.kind == LineKind::Remove).count();

    if before.is_empty() {
        return Err(ToolError::ArgumentsError(format!(
            "{display}: 该 hunk 没有上下文行，无法定位。请在 `+`/`-` 行前后各保留约 3 行上下文"
        )));
    }

    match locate(lines, &before) {
        Match::Unique { pos, tolerant } => {
            lines.splice(pos..pos + before.len(), after);
            Ok((added, removed, tolerant))
        }
        Match::None => Err(ToolError::ArgumentsError(format!(
            "{}: 找不到匹配的上下文（期望匹配 {} 行）:\n{}",
            display,
            before.len(),
            render_expectation(&before)
        ))),
        Match::Ambiguous(count) => Err(ToolError::ArgumentsError(format!(
            "{}: 上下文有 {count} 处匹配，无法确定改哪里。请补足更多上下文（前后各约 3 行）使其唯一",
            display
        ))),
    }
}

/// 回显期望匹配的上下文，帮助模型自我纠正。
fn render_expectation(before: &[String]) -> String {
    let mut text = String::new();
    for line in before.iter().take(12) {
        text.push_str("  | ");
        text.push_str(line);
        text.push('\n');
    }
    if before.len() > 12 {
        text.push_str(&format!("  | ...（还有 {} 行）\n", before.len() - 12));
    }
    text
}

// ============================ 变更与编辑栈 ============================

/// 单个文件的改动快照。
#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    /// 改前内容；`None` = 新建（撤销时删除该文件）。
    pub before: Option<Vec<u8>>,
    /// 改后内容；`None` = 删除（撤销时重建该文件）。
    pub after: Option<Vec<u8>>,
    pub added: usize,
    pub removed: usize,
}

/// 一次 `apply_patch` 调用 = 一帧（可能多文件），保证帧级原子回滚。
///
/// 字段供应用层 `/undo`、`/redo` 与 UI diff 渲染读取；这些接缝（M2）当前尚未接线，
/// 故先放行 dead_code。
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct EditFrame {
    pub summary: String,
    pub changes: Vec<FileChange>,
}

/// 自包含编辑栈：历史与回滚的真相来源，**与 git 无关**。内存态，退出即清。
#[derive(Debug, Default)]
struct EditStack {
    undo: Vec<EditFrame>,
    redo: Vec<EditFrame>,
}

/// 挂在 [`ToolContext`] 上的编辑栈状态（经 `on_register` 注入）。
pub struct PatchState {
    stack: Mutex<EditStack>,
}

impl PatchState {
    pub fn new() -> Self {
        Self { stack: Mutex::new(EditStack::default()) }
    }

    fn record(&self, frame: EditFrame) {
        let mut stack = self.stack.lock().expect("edit stack poisoned");
        stack.undo.push(frame);
        // 经典 undo 语义：新分支覆盖旧的重做路径。
        stack.redo.clear();
    }

    /// 撤销最后一帧。成功返回摘要；失败返回 `Err`（**绝不假装成功**）。
    #[allow(dead_code)] // 应用层 `/undo` 接缝（M2），尚未接线。
    pub fn undo(&self) -> Result<String, ToolError> {
        let mut stack = self.stack.lock().expect("edit stack poisoned");
        let Some(frame) = stack.undo.pop() else {
            return Err(ToolError::ExecutionError("没有可撤销的编辑".into()));
        };
        match write_snapshot(&frame.changes, false) {
            Ok(()) => {
                let summary = frame.summary.clone();
                stack.redo.push(frame);
                Ok(summary)
            }
            Err(error) => {
                // 撤销失败：把帧放回去，如实报错，绝不吞。
                stack.undo.push(frame);
                Err(error)
            }
        }
    }

    /// 重做最后一帧。
    #[allow(dead_code)] // 应用层 `/redo` 接缝（M2），尚未接线。
    pub fn redo(&self) -> Result<String, ToolError> {
        let mut stack = self.stack.lock().expect("edit stack poisoned");
        let Some(frame) = stack.redo.pop() else {
            return Err(ToolError::ExecutionError("没有可重做的编辑".into()));
        };
        match write_snapshot(&frame.changes, true) {
            Ok(()) => {
                let summary = frame.summary.clone();
                stack.undo.push(frame);
                Ok(summary)
            }
            Err(error) => {
                stack.redo.push(frame);
                Err(error)
            }
        }
    }
}

impl Default for PatchState {
    fn default() -> Self {
        Self::new()
    }
}

// ============================ 落盘 ============================

/// 原子写文件：同目录临时文件 + `rename`（与 `session.rs::truncate` 同一套）。
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".{file_name}.shirley-{}-{nanos}.tmp", std::process::id()));
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// 按快照写盘：`forward=true` 用 `after`（重做 / 正向），`false` 用 `before`（撤销，逆序）。
#[allow(dead_code)] // 由 undo / redo 调用（应用层接缝，M2）。
fn write_snapshot(changes: &[FileChange], forward: bool) -> Result<(), ToolError> {
    if forward {
        persist(changes)
    } else {
        let reversed: Vec<FileChange> = changes.iter().rev().cloned().collect();
        let mut snapshot: Vec<FileChange> = Vec::with_capacity(reversed.len());
        for change in reversed {
            // 撤销 = 反向应用：目标内容换成 before。
            snapshot.push(FileChange {
                path: change.path,
                before: change.after,
                after: change.before,
                added: change.removed,
                removed: change.added,
            });
        }
        persist(&snapshot)
    }
}

/// 依次落盘；任一失败则回滚已写的部分，并**如实报错**（回滚失败也报）。
fn persist(changes: &[FileChange]) -> Result<(), ToolError> {
    let mut done: Vec<&FileChange> = Vec::new();
    for change in changes {
        let result = match &change.after {
            None => fs::remove_file(&change.path),
            Some(bytes) => write_atomic(&change.path, bytes),
        };
        if let Err(error) = result {
            let rollback = rollback(&done);
            return Err(ToolError::ExecutionError(format!(
                "写入 {} 失败: {error}{rollback}",
                change.path.display()
            )));
        }
        done.push(change);
    }
    Ok(())
}

/// 回滚已落盘的变更（逆序恢复 `before`）。返回需要附加到错误里的说明。
fn rollback(done: &[&FileChange]) -> String {
    let mut failures: Vec<String> = Vec::new();
    for change in done.iter().rev() {
        let result = match &change.before {
            None => fs::remove_file(&change.path),
            Some(bytes) => write_atomic(&change.path, bytes),
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error}", change.path.display()));
        }
    }
    if failures.is_empty() {
        "（已回滚本次已写入的文件）".to_string()
    } else {
        format!("（回滚失败，请人工检查: {}）", failures.join("; "))
    }
}

// ============================ 工具 ============================

/// `apply_patch` 的注册钩子：注入自包含编辑栈。
fn apply_patch_on_register(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.insert(PatchState::new());
    Ok(())
}

/// `apply_patch` 的注销钩子：清掉编辑栈。
fn apply_patch_on_unregister(ctx: &mut ToolContext) -> Result<(), ToolError> {
    ctx.remove::<PatchState>();
    Ok(())
}

/// 对工作区文本文件应用补丁。
#[tool(
    description = "对工作区内的文本文件应用补丁（二进制 / 非 UTF-8 会被拒绝）。支持新增/更新/删除/重命名文件，用 *** Begin Patch / *** End Patch 包裹，块内以 *** Update File: / *** Add File: / *** Delete File: 分段，重命名用 *** Move to:。更新用 @@ 锚点定位，行首 + 为新增、- 为删除、空格为上下文；上下文请前后各保留约 3 行，以确保唯一匹配。改文件一律用本工具，不要用 shell 重定向 / sed -i。改动是原子的：任一 hunk 匹配失败则整体不落盘。动手前请先用 read_file 读取目标文件，补丁要基于文件的真实内容。",
    on_register = apply_patch_on_register,
    on_unregister = apply_patch_on_unregister,
)]
pub async fn apply_patch(
    ctx: &ToolContext,
    #[param(description = "补丁文本，使用 *** Begin Patch 格式")] patch: String,
) -> Result<String, ToolError> {
    if patch.len() > MAX_PATCH_BYTES {
        return Err(ToolError::ArgumentsError(format!(
            "补丁过大（{} 字节，上限 {MAX_PATCH_BYTES}）",
            patch.len()
        )));
    }

    let state = ctx
        .get::<PatchState>()
        .ok_or_else(|| ToolError::ExecutionError("编辑栈未初始化".into()))?;

    // 1. 解析（任何语法错误都在这里返回，不落盘）。
    let ops = parse_patch(&patch).map_err(ToolError::ArgumentsError)?;

    // 2. 解析路径 + 护栏 + 计算所有变更（仍不落盘）。
    let root = workspace_root()
        .ok_or_else(|| ToolError::ExecutionError("无法确定工作区根目录".into()))?;
    let workspace = WorkSpace::new(root)
        .map_err(|error| ToolError::ExecutionError(format!("工作区初始化失败: {error}")))?;

    let mut changes: Vec<FileChange> = Vec::new();
    let mut tolerant_used = false;
    for op in &ops {
        let produced = plan_op(op, &workspace, &mut tolerant_used)?;
        changes.extend(produced);
    }

    // 3. 原子落盘。
    persist(&changes)?;

    // 4. 记录到编辑栈（供 /undo、/redo；与 git 无关）。
    let summary = summarize(&changes, tolerant_used);
    state.record(EditFrame { summary: summary.clone(), changes });

    Ok(summary)
}

/// 计算一个 op 产生的文件变更（只读 + 纯计算，不落盘）。
fn plan_op(
    op: &FileOp,
    workspace: &WorkSpace,
    tolerant_used: &mut bool,
) -> Result<Vec<FileChange>, ToolError> {
    match op {
        FileOp::Add { path, lines } => {
            let resolved = resolve(workspace, path)?;
            guard_secret(&resolved, path)?;
            if resolved.exists() {
                return Err(ToolError::ArgumentsError(format!(
                    "{path} 已存在，无法 Add；如需覆盖请在同一次补丁里先 `*** Delete File:` 再 `*** Add File:`"
                )));
            }
            let after = render_lines(false, "\n", !lines.is_empty(), lines);
            Ok(vec![FileChange {
                path: resolved,
                before: None,
                after: Some(after),
                added: lines.len(),
                removed: 0,
            }])
        }
        FileOp::Delete { path } => {
            let resolved = resolve(workspace, path)?;
            guard_secret(&resolved, path)?;
            let before = read_existing(&resolved, path)?;
            let removed = count_lines(&before);
            Ok(vec![FileChange {
                path: resolved,
                before: Some(before),
                after: None,
                added: 0,
                removed,
            }])
        }
        FileOp::Overwrite { path, lines } => {
            let resolved = resolve(workspace, path)?;
            guard_secret(&resolved, path)?;
            // 存在则读旧内容（撤销要还原），不存在则等同新建。
            let before = if resolved.is_file() {
                Some(read_existing(&resolved, path)?)
            } else {
                None
            };
            let removed = before.as_ref().map(|b| count_lines(b)).unwrap_or(0);
            let after = render_lines(false, "\n", !lines.is_empty(), lines);
            Ok(vec![FileChange {
                path: resolved,
                before,
                after: Some(after),
                added: lines.len(),
                removed,
            }])
        }
        FileOp::Update { path, hunks, moved } => {
            let resolved = resolve(workspace, path)?;
            guard_secret(&resolved, path)?;
            if !resolved.is_file() {
                return Err(ToolError::ArgumentsError(format!(
                    "{path} 不存在或不是普通文件，无法 Update（新建请用 `*** Add File:`）"
                )));
            }
            let text = TextFile::read(&resolved, path)?;
            let mut lines = text.lines.clone();
            let mut added = 0usize;
            let mut removed = 0usize;
            for hunk in hunks {
                let (a, r, tolerant) = apply_hunk(&mut lines, hunk, path)?;
                added += a;
                removed += r;
                *tolerant_used |= tolerant;
            }
            let after = text.render(&lines);

            match moved {
                None => Ok(vec![FileChange {
                    path: resolved,
                    before: Some(text.raw.clone()),
                    after: Some(after),
                    added,
                    removed,
                }]),
                Some(dest) => {
                    let dest_resolved = resolve(workspace, dest)?;
                    guard_secret(&dest_resolved, dest)?;
                    if dest_resolved.exists() {
                        return Err(ToolError::ArgumentsError(format!(
                            "移动目标 {dest} 已存在，拒绝覆盖"
                        )));
                    }
                    // 移动 = 删源 + 建目标（两个 FileChange，撤销时自动互相还原）。
                    Ok(vec![
                        FileChange {
                            path: resolved,
                            before: Some(text.raw.clone()),
                            after: None,
                            added: 0,
                            removed: 0,
                        },
                        FileChange {
                            path: dest_resolved,
                            before: None,
                            after: Some(after),
                            added,
                            removed,
                        },
                    ])
                }
            }
        }
    }
}

fn resolve(workspace: &WorkSpace, path: &str) -> Result<PathBuf, ToolError> {
    match workspace.resolve(path) {
        Ok(resolved) => Ok(resolved),
        // SDK 的 `resolve` 要求父目录已存在（它会 canonicalize 父目录）。
        // 但 `*** Add File: sub/new.txt` 这种"顺带建目录"是合理用法，
        // 所以这里对"中间目录缺失"做宽松解析：向上找到已存在的祖先做边界校验，
        // 再把缺失的尾部组件拼回去。真正建目录交给落盘时的 `create_dir_all`。
        Err(WorkspaceError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            resolve_lenient(workspace, path)
        }
        Err(error) => Err(to_tool_error(error)),
    }
}

fn to_tool_error(error: WorkspaceError) -> ToolError {
    match error {
        WorkspaceError::OutsideRoot(_) | WorkspaceError::InvalidPath(_) => {
            ToolError::ArgumentsError(error.to_string())
        }
        WorkspaceError::Io(_) => ToolError::ExecutionError(error.to_string()),
    }
}

/// 宽松解析：允许中间目录不存在。
///
/// 先自校验相对路径不越界（与 SDK 同口径地展开 `.` / `..`），
/// 再逐级向上寻找"已存在"的祖先，对祖先做一次 `WorkSpace::resolve`
/// （它负责 symlink 越界校验），最后把缺失的尾部拼回去。
fn resolve_lenient(workspace: &WorkSpace, path: &str) -> Result<PathBuf, ToolError> {
    let input = Path::new(path);
    if input.is_absolute() {
        return Err(ToolError::ArgumentsError(format!(
            "[path outside workspace]: {path}"
        )));
    }

    // 展开 `.` / `..`，任何越界立即拒绝（不依赖文件系统）。
    let mut relative = PathBuf::new();
    for component in input.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => relative.push(part),
            Component::ParentDir => {
                if !relative.pop() {
                    return Err(ToolError::ArgumentsError(format!(
                        "[path outside workspace]: {path}"
                    )));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::ArgumentsError(format!(
                    "[path outside workspace]: {path}"
                )));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(ToolError::ArgumentsError(format!("[invalid path]: {path}")));
    }

    // 收集相对路径的各组件，从完整路径开始逐级向上找已存在的祖先。
    let parts: Vec<std::ffi::OsString> = relative
        .iter()
        .map(|part| part.to_os_string())
        .collect();
    for split in (1..=parts.len()).rev() {
        let ancestor: PathBuf = parts[..split].iter().collect();
        let ancestor_str = ancestor.to_string_lossy().to_string();
        // 祖先路径交给 SDK 校验（含 symlink 越界）；它内部会 canonicalize。
        if let Ok(resolved_ancestor) = workspace.resolve(&ancestor_str) {
            let mut result = resolved_ancestor;
            for tail in &parts[split..] {
                result.push(tail);
            }
            return Ok(result);
        }
    }

    Err(ToolError::ArgumentsError(format!(
        "[path outside workspace]: {path}"
    )))
}

fn guard_secret(resolved: &Path, display: &str) -> Result<(), ToolError> {
    if let Some(name) = resolved.file_name().and_then(|name| name.to_str())
        && is_secret_file(name)
    {
        return Err(ToolError::ArgumentsError(format!(
            "拒绝写入疑似密钥文件: {display}"
        )));
    }
    Ok(())
}

fn read_existing(path: &Path, display: &str) -> Result<Vec<u8>, ToolError> {
    let metadata = fs::metadata(path)
        .map_err(|error| ToolError::ArgumentsError(format!("{display} 不存在或无法访问: {error}")))?;
    if !metadata.is_file() {
        return Err(ToolError::ArgumentsError(format!("{display} 不是普通文件")));
    }
    let bytes = fs::read(path)
        .map_err(|error| ToolError::ExecutionError(format!("读取 {display} 失败: {error}")))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ToolError::ArgumentsError(format!(
            "{display} 超过单文件上限（{} 字节）",
            MAX_FILE_BYTES
        )));
    }
    let sniff_len = bytes.len().min(BINARY_SNIFF_BYTES);
    if bytes[..sniff_len].contains(&0) {
        return Err(ToolError::ArgumentsError(format!(
            "{display} 看起来是二进制文件，补丁工具只处理文本"
        )));
    }
    Ok(bytes)
}

fn count_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut count = text.matches('\n').count();
    if !text.ends_with('\n') {
        count += 1;
    }
    count
}

/// 生成给模型的文本摘要（改了哪些文件、各 `+N/-M`）。
fn summarize(changes: &[FileChange], tolerant_used: bool) -> String {
    let mut text = format!("已应用补丁，共 {} 个文件操作：\n", changes.len());
    for change in changes {
        let path = change.path.display();
        match (&change.before, &change.after) {
            (None, Some(_)) => {
                text.push_str(&format!("- {path}  新增（+{} 行）\n", change.added));
            }
            (Some(_), None) => {
                text.push_str(&format!("- {path}  删除（-{} 行）\n", change.removed));
            }
            _ => {
                text.push_str(&format!(
                    "- {path}  +{} -{}\n",
                    change.added, change.removed
                ));
            }
        }
    }
    if tolerant_used {
        text.push_str("注意：部分 hunk 使用了空白 / 缩进容错匹配（非精确匹配）。\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::util::WorkspaceGuard;
    use shirley_agent_sdk::Tool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_patch_{tag}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn state_ctx() -> ToolContext {
        let ctx = ToolContext::new();
        let mut ctx = ctx;
        ctx.insert(PatchState::new());
        ctx
    }

    // ---------- 解析 ----------

    #[test]
    fn parses_update_add_delete() {
        let patch = r"*** Begin Patch
*** Update File: a.txt
@@
 ctx
-old
+new
*** Add File: b.txt
+hello
*** Delete File: c.txt
*** End Patch
";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(&ops[0], FileOp::Update { path, .. } if path == "a.txt"));
        assert!(matches!(&ops[1], FileOp::Add { path, .. } if path == "b.txt"));
        assert!(matches!(&ops[2], FileOp::Delete { path } if path == "c.txt"));
    }

    #[test]
    fn rejects_missing_begin_and_end() {
        assert!(parse_patch("*** Update File: a\n*** End Patch").is_err());
        assert!(parse_patch("*** Begin Patch\n*** Update File: a\n").is_err());
    }

    #[test]
    fn rejects_unknown_line() {
        assert!(parse_patch("*** Begin Patch\ngarbage\n*** End Patch").is_err());
    }

    #[test]
    fn parses_move_to() {
        let patch = r"*** Begin Patch
*** Update File: a.txt
*** Move to: b.txt
*** End Patch
";
        let ops = parse_patch(patch).unwrap();
        assert!(matches!(&ops[0], FileOp::Update { moved: Some(d), .. } if d == "b.txt"));
    }

    #[test]
    fn delete_add_same_path_becomes_overwrite() {
        let patch = r"*** Begin Patch
*** Delete File: a.txt
*** Add File: a.txt
+x
*** End Patch
";
        let ops = parse_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert!(matches!(&ops[0], FileOp::Overwrite { path, .. } if path == "a.txt"));
    }

    #[test]
    fn rejects_other_duplicate_paths() {
        // 两次 Update 同一路径、或 Add 在前 Delete 在后，都是歧义，拒绝。
        let patch = r"*** Begin Patch
*** Update File: a.txt
@@
-x
+y
*** Update File: a.txt
@@
-y
+z
*** End Patch
";
        assert!(parse_patch(patch).is_err());

        let patch = r"*** Begin Patch
*** Add File: a.txt
+x
*** Delete File: a.txt
*** End Patch
";
        assert!(parse_patch(patch).is_err());
    }

    // ---------- 匹配 ----------

    #[test]
    fn locate_exact_unique() {
        let file: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let pat: Vec<String> = ["b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(locate(&file, &pat), Match::Unique { pos: 1, tolerant: false });
    }

    #[test]
    fn locate_ambiguous() {
        let file: Vec<String> = ["x", "y", "x", "y"].iter().map(|s| s.to_string()).collect();
        let pat: Vec<String> = ["x", "y"].iter().map(|s| s.to_string()).collect();
        assert_eq!(locate(&file, &pat), Match::Ambiguous(2));
    }

    #[test]
    fn locate_tolerant_on_indent() {
        let file: Vec<String> = ["fn f() {", "    return 1;", "}"].iter().map(|s| s.to_string()).collect();
        let pat: Vec<String> = ["fn f() {", "return 1;", "}"].iter().map(|s| s.to_string()).collect();
        assert_eq!(locate(&file, &pat), Match::Unique { pos: 0, tolerant: true });
    }

    // ---------- 端到端 ----------

    #[tokio::test]
    async fn applies_update_atomically() {
        let dir = temp_workspace("update");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let patch = r"*** Begin Patch
*** Update File: a.txt
@@
 one
-two
+TWO
 three
*** End Patch
";
        let out = apply_patch(&state_ctx(), patch.into()).await.unwrap();
        assert!(out.contains("a.txt"));
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\nTWO\nthree\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn add_and_delete() {
        let dir = temp_workspace("adddel");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();

        let patch = r"*** Begin Patch
*** Add File: sub/new.txt
+hi
+there
*** Delete File: gone.txt
*** End Patch
";
        apply_patch(&state_ctx(), patch.into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("sub/new.txt")).unwrap(), "hi\nthere\n");
        assert!(!dir.join("gone.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn overwrite_replaces_existing_file() {
        let dir = temp_workspace("overwrite");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("a.txt"), "old1\nold2\n").unwrap();

        let patch = r"*** Begin Patch
*** Delete File: a.txt
*** Add File: a.txt
+brand
+new
*** End Patch
";
        apply_patch(&state_ctx(), patch.into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "brand\nnew\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn failed_hunk_writes_nothing() {
        let dir = temp_workspace("atomic");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();

        // 第二个文件匹配失败 → 第一个文件也不能落盘。
        let patch = r"*** Begin Patch
*** Update File: a.txt
@@
-one
+ONE
*** Update File: missing.txt
@@
-nope
+NOPE
*** End Patch
";
        let err = apply_patch(&state_ctx(), patch.into()).await.unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)), "应是参数错误: {err:?}");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn preserves_crlf() {
        let dir = temp_workspace("crlf");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("w.txt"), "a\r\nb\r\n").unwrap();

        let patch = r"*** Begin Patch
*** Update File: w.txt
@@
 a
-b
+c
*** End Patch
";
        apply_patch(&state_ctx(), patch.into()).await.unwrap();
        assert_eq!(std::fs::read(dir.join("w.txt")).unwrap(), b"a\r\nc\r\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_binary_and_non_utf8() {
        let dir = temp_workspace("binary");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("bin"), b"abc\0def").unwrap();
        std::fs::write(dir.join("latin1"), b"caf\xe9").unwrap();

        for name in ["bin", "latin1"] {
            let patch = format!(
                "*** Begin Patch\n*** Update File: {name}\n@@\n-a\n+b\n*** End Patch\n"
            );
            let err = apply_patch(&state_ctx(), patch).await.unwrap_err();
            assert!(matches!(err, ToolError::ArgumentsError(_)), "{name}: {err:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn rejects_outside_workspace_and_secret() {
        let dir = temp_workspace("guard");
        let _guard = WorkspaceGuard::set(&dir).await;

        let patch = r"*** Begin Patch
*** Add File: ../evil.txt
+x
*** End Patch
";
        let err = apply_patch(&state_ctx(), patch.into()).await.unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)), "{err:?}");

        let patch = r"*** Begin Patch
*** Add File: .env
+SECRET=1
*** End Patch
";
        let err = apply_patch(&state_ctx(), patch.into()).await.unwrap_err();
        assert!(matches!(err, ToolError::ArgumentsError(_)), "{err:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn move_renames_file() {
        let dir = temp_workspace("move");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("a.txt"), "content\n").unwrap();

        let patch = r"*** Begin Patch
*** Update File: a.txt
*** Move to: b.txt
*** End Patch
";
        apply_patch(&state_ctx(), patch.into()).await.unwrap();
        assert!(!dir.join("a.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "content\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn undo_and_redo_round_trip() {
        let dir = temp_workspace("undo");
        let _guard = WorkspaceGuard::set(&dir).await;
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();

        let ctx = state_ctx();
        let patch = r"*** Begin Patch
*** Update File: a.txt
@@
-one
+ONE
*** End Patch
";
        apply_patch(&ctx, patch.into()).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "ONE\ntwo\n");

        let state = ctx.get::<PatchState>().unwrap();
        state.undo().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\ntwo\n");
        state.redo().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "ONE\ntwo\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn schema_has_only_patch() {
        let tool = crate::tools::apply_patch_tool::tool();
        let params = &tool.definition().parameters;
        let props = params.get("properties").unwrap().as_object().unwrap();
        assert!(props.contains_key("patch"));
        assert!(!props.contains_key("ctx"), "ToolContext 不应进入参数 schema");
    }
}
