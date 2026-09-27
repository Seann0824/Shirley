use agent_sdk::{ToolError, tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

const MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub struct Start {
    pub line: Option<i32>,
    pub column: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ReturnFormat {
    pub content: String,
    pub start: Position,
    pub end: Position,
    pub start_byte: u64,
    pub end_byte: u64,
    pub total_lines: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn empty(position: Position) -> ReturnFormat {
    ReturnFormat {
        content: String::new(),
        start: position.clone(),
        end: position,
        start_byte: 0,
        end_byte: 0,
        total_lines: 0,
        truncated: false,
        error: None,
    }
}

struct LineScan {
    total_lines: usize,
    file_len: u64,
    start_byte: Option<u64>,
    end_byte: Option<u64>,
}

// 扫完整个文件以计算总行数，但只保留请求范围的两个偏移。
async fn scan_lines(file: tokio::fs::File, line: usize, count: usize) -> std::io::Result<LineScan> {
    let mut reader = BufReader::new(file);
    let mut offset = 0u64;
    let mut newline_count = 0usize;
    let mut start_byte = (line == 1).then_some(0);
    let mut end_byte = None;
    let end_line = line.saturating_add(count);
    let mut last_was_newline = false;
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            break;
        }
        for index in memchr::memchr_iter(b'\n', buf) {
            newline_count += 1;
            let next_byte = offset + index as u64 + 1;
            if newline_count + 1 == line {
                start_byte = Some(next_byte);
            }
            if newline_count + 1 == end_line {
                end_byte = Some(next_byte);
            }
        }
        last_was_newline = buf.last() == Some(&b'\n');
        let len = buf.len();
        reader.consume(len);
        offset += len as u64;
    }
    let total_lines = newline_count + usize::from(offset > 0 && !last_was_newline);
    Ok(LineScan {
        total_lines,
        file_len: offset,
        start_byte,
        end_byte,
    })
}

#[tool(description = "按行列读取文件内容，返回带行号的文本、字节范围和总行数")]
pub async fn read(
    #[param(description = "文件路径，可以是绝对路径或相对当前工作目录的路径")] path: String,
    #[param(description = "起始行列，均从 1 开始；省略时为第 1 行第 1 列。列只作用于起始行")]
    start: Option<Start>,
    #[param(description = "最多读取多少行，省略时为 200 行")] line_count: Option<i32>,
) -> Result<ReturnFormat, ToolError> {
    let start = start.unwrap_or(Start {
        line: None,
        column: None,
    });
    let line = usize::try_from(start.line.unwrap_or(1))
        .map_err(|_| ToolError::ArgumentsError("起始行必须大于 0".into()))?;
    let column = usize::try_from(start.column.unwrap_or(1))
        .map_err(|_| ToolError::ArgumentsError("起始列必须大于 0".into()))?;
    if line == 0 || column == 0 {
        return Err(ToolError::ArgumentsError("起始行和列必须大于 0".into()));
    }
    let count = usize::try_from(line_count.unwrap_or(200))
        .map_err(|_| ToolError::ArgumentsError("读取行数不能为负数".into()))?;
    let mut out = empty(Position { line, column });

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            out.error = Some(format!("打开文件 {path} 失败: {e}"));
            return Ok(out);
        }
    };
    let scan_file = match file.try_clone().await {
        Ok(file) => file,
        Err(e) => {
            out.error = Some(format!("复制文件句柄失败: {e}"));
            return Ok(out);
        }
    };
    let scan = match scan_lines(scan_file, line, count).await {
        Ok(scan) => scan,
        Err(e) => {
            out.error = Some(format!("扫描文件 {path} 失败: {e}"));
            return Ok(out);
        }
    };

    out.total_lines = scan.total_lines;
    if line > out.total_lines || count == 0 {
        out.start_byte = scan.file_len;
        out.end_byte = out.start_byte;
        out.truncated = line <= out.total_lines;
        return Ok(out);
    }

    let last = (line - 1).saturating_add(count).min(out.total_lines);
    out.start_byte = scan.start_byte.unwrap_or(scan.file_len);
    let requested_end = scan.end_byte.unwrap_or(scan.file_len);
    out.end_byte = requested_end.min(out.start_byte.saturating_add(MAX_OUTPUT_BYTES as u64));
    out.truncated = last < out.total_lines || out.end_byte < requested_end;
    if let Err(e) = file.seek(SeekFrom::Start(out.start_byte)).await {
        out.error = Some(format!("定位文件 {path} 失败: {e}"));
        out.truncated = true;
        return Ok(out);
    }

    let length = out.end_byte - out.start_byte;
    let mut bytes = Vec::new();
    let read_error = file.take(length).read_to_end(&mut bytes).await.err();
    let mut emitted = 0usize;
    for raw in bytes.split_inclusive(|byte| *byte == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        let text = text.trim_end_matches(['\n', '\r']);
        let display = if emitted == 0 {
            text.chars().skip(column - 1).collect::<String>()
        } else {
            text.to_owned()
        };
        let prefix = format!("{:>5} | ", line + emitted);
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(out.content.len());
        if remaining <= prefix.len() {
            out.truncated = true;
            break;
        }
        out.content.push_str(&prefix);
        let available = remaining - prefix.len();
        let display_budget = available.saturating_sub(1);
        let clipped = display
            .char_indices()
            .take_while(|(index, ch)| index + ch.len_utf8() <= display_budget)
            .last()
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        out.content.push_str(&display[..clipped]);
        out.content.push('\n');
        if clipped < display.len() {
            out.truncated = true;
            break;
        }
        out.end = Position {
            line: line + emitted,
            column: text.chars().count(),
        };
        emitted += 1;
    }
    if read_error.is_some() || bytes.len() as u64 != length {
        out.error = Some(match read_error {
            Some(e) => format!("读取文件 {path} 失败: {e}"),
            None => format!("读取文件 {path} 失败: 文件在扫描后被截短"),
        });
        out.end_byte = out.start_byte + bytes.len() as u64;
        out.truncated = true;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_byte_range_with_column_and_line_numbers() {
        let path = std::env::temp_dir().join(format!("shirley-read-{}", std::process::id()));
        tokio::fs::write(&path, "甲乙\nabc\n末").await.unwrap();
        let out = read(
            path.to_string_lossy().into_owned(),
            Some(Start {
                line: Some(1),
                column: Some(2),
            }),
            Some(2),
        )
        .await
        .unwrap();
        tokio::fs::remove_file(path).await.unwrap();

        assert_eq!(out.content, "    1 | 乙\n    2 | abc\n");
        assert_eq!((out.start_byte, out.end_byte), (0, 11));
        assert_eq!(out.total_lines, 3);
        assert!(out.truncated);
        assert!(out.error.is_none());
    }

    #[tokio::test]
    async fn caps_a_single_long_line() {
        let path = std::env::temp_dir().join(format!("shirley-read-long-{}", std::process::id()));
        tokio::fs::write(&path, vec![b'a'; MAX_OUTPUT_BYTES * 2])
            .await
            .unwrap();
        let out = read(path.to_string_lossy().into_owned(), None, None)
            .await
            .unwrap();
        tokio::fs::remove_file(path).await.unwrap();

        assert_eq!(out.total_lines, 1);
        assert_eq!(out.end_byte, MAX_OUTPUT_BYTES as u64);
        assert!(out.content.len() <= MAX_OUTPUT_BYTES);
        assert!(out.truncated);
    }

    #[tokio::test]
    async fn caps_formatted_output_from_many_short_lines() {
        let path = std::env::temp_dir().join(format!("shirley-read-lines-{}", std::process::id()));
        tokio::fs::write(&path, "x\n".repeat(MAX_OUTPUT_BYTES))
            .await
            .unwrap();
        let out = read(
            path.to_string_lossy().into_owned(),
            None,
            Some(MAX_OUTPUT_BYTES as i32),
        )
        .await
        .unwrap();
        tokio::fs::remove_file(path).await.unwrap();

        assert_eq!(out.total_lines, MAX_OUTPUT_BYTES);
        assert!(out.content.len() <= MAX_OUTPUT_BYTES);
        assert!(out.truncated);
    }
}
