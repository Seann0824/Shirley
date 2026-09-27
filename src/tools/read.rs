use agent_sdk::{ToolError, tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncBufReadExt;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(inline)]
pub struct Start {
    pub line: Option<i32>,
    pub column: Option<i32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ReturnFormat {
    pub content: String,
    pub start: Start,
    pub end: Start,
    pub truncated: bool,
    pub error: Option<String>,
}

#[tool(description = "从文件的指定行列开始读取内容，并返回读取范围、截断状态及错误信息")]
pub async fn read(
    #[param(description = "要读取的文件路径，可以是绝对路径或相对当前工作目录的路径")] path: String,
    #[param(
        description = "起始位置。line 和 column 均从 0 开始，省略时默认为 0；column 仅作用于起始行"
    )]
    start: Option<Start>,
    #[param(description = "最多读取的行数，从起始行算起；省略时读取 200 行")] line_count: Option<
        i32,
    >,
) -> Result<ReturnFormat, agent_sdk::ToolError> {
    let start = start.unwrap_or(Start {
        line: None,
        column: None,
    });
    let start_line = usize::try_from(start.line.unwrap_or(0))
        .map_err(|_| ToolError::ExecutionError("起始行不能为负数".into()))?;
    let start_column = usize::try_from(start.column.unwrap_or(0))
        .map_err(|_| ToolError::ExecutionError("起始列不能为负数".into()))?;
    let count = usize::try_from(line_count.unwrap_or(200))
        .map_err(|_| ToolError::ExecutionError("读取行数不能为负数".into()))?;

    let position = || Start {
        line: Some(start_line as i32),
        column: Some(start_column as i32),
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(e) => {
            return Ok(ReturnFormat {
                content: String::new(),
                start: position(),
                end: position(),
                truncated: false,
                error: Some(format!("打开文件 {path} 失败: {e}")),
            });
        }
    };

    let mut lines = tokio::io::BufReader::new(file).lines();

    for _ in 0..start_line {
        match lines.next_line().await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Ok(ReturnFormat {
                    content: String::new(),
                    start: position(),
                    end: position(),
                    truncated: false,
                    error: None,
                });
            }
            Err(e) => {
                return Ok(ReturnFormat {
                    content: String::new(),
                    start: position(),
                    end: position(),
                    truncated: true,
                    error: Some(format!("跳过文件开头区域失败: {e}")),
                });
            }
        }
    }

    let mut output = String::new();
    let mut read_lines = 0usize;
    let mut error = None;
    for i in 0..count {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                error = Some(format!("读取文件第{}行失败: {e}", start_line + i));
                break;
            }
        };
        if i == 0 {
            output.extend(line.chars().skip(start_column));
        } else {
            output.push_str(&line);
        }
        output.push('\n');
        read_lines += 1;
    }

    let truncated = if error.is_some() {
        true
    } else if read_lines == count {
        match lines.next_line().await {
            Ok(next) => next.is_some(),
            Err(e) => {
                error = Some(format!("检查文件剩余内容失败: {e}"));
                true
            }
        }
    } else {
        false
    };

    let end = if read_lines == 0 {
        position()
    } else {
        Start {
            line: Some(
                i32::try_from(start_line + read_lines)
                    .map_err(|_| ToolError::ExecutionError("结束行超出可表示范围".into()))?,
            ),
            column: Some(0),
        }
    };

    Ok(ReturnFormat {
        content: output,
        start: position(),
        end,
        truncated,
        error,
    })
}
