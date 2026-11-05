//! Headless one-shot entry point (`shirley exec`): one prompt in, one answer out, then exit.
//!
//! This is the shape external supervisors expect (`claude -p`, `codex exec`, `kimi -p`): a
//! **stateless** call that answers without any UI and without a session. today-board's brain
//! layer speaks exactly this contract, so a personal daemon can use Shirley as its brain instead
//! of shelling out to one vendor's CLI.
//!
//! Deliberately **minimal**. None of the interactive session capabilities are attached: no
//! session store, no task ledger, no memory / skill injection, no subagents, no computer use.
//! They all assume "a conversation that continues"; a one-shot call has nothing to attach them
//! to. What survives is the model, the working directory, and whichever local tools the caller
//! explicitly whitelists — a daemon calling this must not silently get `bash`.
//!
//! Two failure shapes, on purpose:
//!
//! - **The model failed** → a `claude`-style envelope with `is_error: true` on stdout, exit 0.
//!   The caller reads the reason from `result` in one piece, the same way it does for `claude -p`.
//! - **The call was set up wrong** (no config, unknown / unconfigured tool) → message on stderr,
//!   non-zero exit. Nothing was attempted, so there is no envelope to fake.

use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

use shirley_agent_sdk::{Agent, AgentError, Message, ModelConfig, ToolManager, Usage};

use crate::settings::Settings;
use crate::tools;

/// The one-shot system prompt. Copy lives in `prompts/exec_system.md` (project convention: prompt
/// text is markdown, code only fills placeholders).
const EXEC_SYSTEM: &str = include_str!("../../prompts/exec_system.md");

/// Tools a whitelist may name, and the only ones this entry point will ever register.
///
/// `todo` / `recall` / skills / computer use are absent on purpose: they read or write session
/// state that a one-shot call does not have.
const AVAILABLE_TOOLS: &[&str] = &["bash", "grep", "read", "web_search"];

const USAGE: &str = "\
shirley exec — 一次性跑一条任务，打印结果后退出（给外部程序当大脑用）

用法：
  shirley exec [选项] [任务文本]
  echo '<任务>' | shirley exec

选项：
  --prompt-file <路径|->      从文件读任务；`-` 表示标准输入（默认就是标准输入）
  --output-format <json|text> json（默认，claude 同款信封）/ text（只打正文）
  --model <名字>              覆盖配置里的模型
  --tools <列表>              工具白名单，逗号分隔；`all` 全开，空串 = 不给工具（默认）
  --cwd <目录>                工作目录（默认当前目录）
  --max-steps <N>             这次任务的模型调用上限
  -h, --help                  显示这段话

模型自己报错时，stdout 仍是一份信封、`is_error` 为 true、退出码 0（调用方只读一个地方）；
配置缺失、工具名不认识这类「根本没开始跑」的错误走 stderr + 非零退出码。
";

/// How much of the answer to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// The `claude -p --output-format json` envelope.
    Json,
    /// Just the answer text.
    Text,
}

/// Where the task text comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Stdin,
    File(PathBuf),
    Inline(String),
}

/// Parsed command line. Pure data: parsing touches no files and no environment, so it is testable.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    prompt: Source,
    format: Format,
    model: Option<String>,
    tools: Vec<String>,
    cwd: Option<PathBuf>,
    max_steps: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            prompt: Source::Stdin,
            format: Format::Json,
            model: None,
            tools: Vec::new(),
            cwd: None,
            max_steps: None,
        }
    }
}

/// Entry point for `shirley exec …` (`args` excludes the program name and the `exec` word).
///
/// # Errors
///
/// Returns [`AgentError`] when the call cannot be set up or the run cannot be started (missing
/// config, unreadable prompt file, unknown tool name). A failure reported *by the model* is not an
/// error here — it is printed as an `is_error` envelope.
pub(crate) fn run(args: &[String]) -> Result<(), AgentError> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    let options = parse(args).map_err(other)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| other(format!("起运行时失败：{error}")))?;
    runtime.block_on(execute(options))
}

async fn execute(options: Options) -> Result<(), AgentError> {
    let started = Instant::now();
    let prompt = read_prompt(&options.prompt)?;

    let cwd = options
        .cwd
        .clone()
        .unwrap_or_else(crate::prompt::workspace_root);
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);

    let settings = Settings::load(Some(&crate::storage::config_path()), &|key| {
        std::env::var(key).ok()
    })
    .map_err(|error| other(format!("读配置失败：{error}")))?;
    if !settings.is_configured() {
        return Err(other(
            "还没有配置模型服务（base_url 是空的）；先跑一次 `shirley` 用 /login 填好".to_string(),
        ));
    }

    let model = options
        .model
        .clone()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| settings.model.clone());

    let mut config = ModelConfig::builder()
        .protocol(settings.protocol)
        .base_url(settings.base_url.clone())
        .maybe_api_key(settings.api_key.clone())
        .model(model.clone())
        // Not streaming: a one-shot call has nobody to render deltas to, and a non-streaming body
        // is one round trip instead of a connection held open for the whole answer.
        .stream(false)
        .thinking(settings.thinking)
        .reasoning_effort(settings.reasoning_effort.clone())
        .maybe_extra_body(settings.extra_body.clone())
        .build();
    config.context_window_tokens = Some(settings.context_window_tokens);
    config.supports_images = settings.supports_images;

    let mut agent = Agent::builder()
        .model_config(config)
        .system_prompt(EXEC_SYSTEM.replace("{working_dir}", &cwd.display().to_string()))
        .working_dir(cwd.clone())
        .tools(build_tools(std::slice::from_ref(&cwd), &options.tools)?)
        .max_steps(options.max_steps.unwrap_or(settings.max_steps))
        .build()?;

    let outcome = agent.run(&prompt).await;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    match outcome {
        Ok(result) => {
            let text = answer_text(&result.messages);
            let turns = result
                .messages
                .iter()
                .filter(|message| matches!(message, Message::Assistant { .. }))
                .count();
            emit(
                options.format,
                &text,
                false,
                &model,
                elapsed_ms,
                turns,
                &result.usage,
            );
            Ok(())
        }
        Err(error) => {
            // The model (or the transport) failed: report it the way `claude -p` does, so the
            // caller has one place to read the reason from.
            emit(
                options.format,
                &format!("{error}"),
                true,
                &model,
                elapsed_ms,
                0,
                &Usage::default(),
            );
            Ok(())
        }
    }
}

/// Read the task text.
fn read_prompt(source: &Source) -> Result<String, AgentError> {
    let text = match source {
        Source::Inline(text) => text.clone(),
        Source::File(path) => std::fs::read_to_string(path)
            .map_err(|error| other(format!("读不到任务文件 {}：{error}", path.display())))?,
        Source::Stdin => {
            let mut buffer = String::new();
            std::io::stdin()
                .read_to_string(&mut buffer)
                .map_err(|error| other(format!("读标准输入失败：{error}")))?;
            buffer
        }
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(other("任务文本是空的".to_string()));
    }
    Ok(trimmed.to_string())
}

/// The answer is the **last** assistant message: intermediate turns of a tool loop are progress,
/// not the answer.
fn answer_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant { content, .. } => content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
            _ => None,
        })
        .unwrap_or_default()
}

fn emit(
    format: Format,
    text: &str,
    is_error: bool,
    model: &str,
    elapsed_ms: u64,
    turns: usize,
    usage: &Usage,
) {
    match format {
        Format::Text => println!("{text}"),
        Format::Json => {
            // Field names follow `claude -p --output-format json` on purpose: a caller written
            // against that envelope keeps working, and `total_cost_usd` is `null` rather than a
            // made-up number (nothing here can price a token).
            let envelope = serde_json::json!({
                "type": "result",
                "subtype": if is_error { "error" } else { "success" },
                "is_error": is_error,
                "result": text,
                "duration_ms": elapsed_ms,
                "num_turns": turns,
                "model": model,
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_input_tokens": usage.cached_input_tokens,
                },
                "total_cost_usd": serde_json::Value::Null,
            });
            match serde_json::to_string(&envelope) {
                Ok(line) => println!("{line}"),
                // Unreachable in practice (every value is a plain scalar / string); printing the
                // text alone beats losing the answer to a serialization error.
                Err(error) => {
                    eprintln!("[exec] 信封序列化失败：{error}");
                    println!("{text}");
                }
            }
        }
    }
}

/// Register exactly the whitelisted tools. An unknown or unconfigured name is a hard error: a
/// silent downgrade would hand the caller a confident answer produced without the tool it asked
/// for.
fn build_tools(roots: &[PathBuf], names: &[String]) -> Result<ToolManager, AgentError> {
    let mut manager = ToolManager::new();
    manager
        .context_mut()
        .insert(tools::read::WorkspaceRoots(roots.to_vec()));
    for name in names {
        let registered = match name.as_str() {
            "bash" => manager.register(tools::bash_tool::tool()),
            "grep" => manager.register(tools::grep_tool::tool()),
            "read" => manager.register(tools::read_tool::tool()),
            "web_search" => manager.register(tools::web_search_tool::tool()),
            unknown => return Err(other(format!("内部错误：{unknown} 没有对应的构造"))),
        };
        registered.map_err(|error| {
            other(format!(
                "工具 {name} 注册失败：{error}（web_search 需要先配好搜索凭据）"
            ))
        })?;
    }
    Ok(manager)
}

/// Parse the command line. Pure: no filesystem, no environment.
fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut prompt_given = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        // Reads the value that follows a flag and advances past it.
        let take = |name: &str, index: &mut usize| -> Result<String, String> {
            *index += 1;
            args.get(*index)
                .cloned()
                .ok_or_else(|| format!("{name} 后面缺少值"))
        };
        match arg.as_str() {
            "--prompt-file" | "-p" => {
                let value = take("--prompt-file", &mut index)?;
                options.prompt = if value == "-" {
                    Source::Stdin
                } else {
                    Source::File(PathBuf::from(value))
                };
                prompt_given = true;
            }
            "--output-format" => {
                let value = take("--output-format", &mut index)?;
                options.format = match value.as_str() {
                    "json" => Format::Json,
                    "text" => Format::Text,
                    other => {
                        return Err(format!("--output-format 只支持 json / text，收到 {other}"));
                    }
                };
            }
            "--model" | "-m" => options.model = Some(take("--model", &mut index)?),
            "--tools" => {
                let value = take("--tools", &mut index)?;
                options.tools = split_tools(&value)?;
            }
            "--cwd" => options.cwd = Some(PathBuf::from(take("--cwd", &mut index)?)),
            "--max-steps" => {
                let value = take("--max-steps", &mut index)?;
                let steps = value.parse::<usize>().map_err(|error| {
                    format!("--max-steps 需要一个数字，收到 {value}（{error}）")
                })?;
                options.max_steps = Some(steps);
            }
            other if other.starts_with('-') => return Err(format!("不认识这个选项：{other}")),
            other => {
                if prompt_given {
                    return Err("任务文本只能给一份（位置参数 / --prompt-file 二选一）".to_string());
                }
                options.prompt = Source::Inline(other.to_string());
                prompt_given = true;
            }
        }
        index += 1;
    }
    Ok(options)
}

/// Normalize a tool whitelist. Empty = no tools (the safe default for a daemon); `all` = every
/// tool this entry point can offer; names are case-insensitive and aliased to what today-board
/// calls them (`WebSearch` → `web_search`).
fn split_tools(raw: &str) -> Result<Vec<String>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.eq_ignore_ascii_case("all") {
        return Ok(AVAILABLE_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect());
    }
    let mut tools: Vec<String> = Vec::new();
    for piece in raw.split(',') {
        // Keep the caller's spelling for the error message: a lowercased name in a "不认识这个
        // 工具" report reads like the tool is called `webfetch`, which is not what was asked for.
        let original = piece.trim();
        if original.is_empty() {
            continue;
        }
        let name = original.to_ascii_lowercase();
        let resolved = match name.as_str() {
            "websearch" => "web_search",
            other => other,
        };
        if !AVAILABLE_TOOLS.contains(&resolved) {
            return Err(format!(
                "不认识的工具 {original}；可用：{}（`all` 表示全开，空串表示不给工具）",
                AVAILABLE_TOOLS.join(", ")
            ));
        }
        if !tools.iter().any(|existing| existing == resolved) {
            tools.push(resolved.to_string());
        }
    }
    Ok(tools)
}

/// Wrap a message as an [`AgentError`]. The project's error contract keeps a `[prefix]: detail`
/// display, and `Other` is the only variant that carries free-form setup failures.
fn other(message: String) -> AgentError {
    AgentError::Other(Box::new(std::io::Error::other(message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn defaults_are_stdin_json_no_tools() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.prompt, Source::Stdin);
        assert_eq!(options.format, Format::Json);
        assert!(
            options.tools.is_empty(),
            "a daemon must not get bash by default"
        );
        assert_eq!(options.max_steps, None);
    }

    #[test]
    fn parses_every_flag() {
        let options = parse(&args(&[
            "--prompt-file",
            "-",
            "--output-format",
            "text",
            "--model",
            "sonnet",
            "--tools",
            "read,bash",
            "--cwd",
            "/tmp",
            "--max-steps",
            "7",
        ]))
        .unwrap();
        assert_eq!(options.prompt, Source::Stdin);
        assert_eq!(options.format, Format::Text);
        assert_eq!(options.model.as_deref(), Some("sonnet"));
        assert_eq!(options.tools, vec!["read".to_string(), "bash".to_string()]);
        assert_eq!(options.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(options.max_steps, Some(7));
    }

    #[test]
    fn positional_prompt_and_file_prompt_conflict() {
        assert_eq!(
            parse(&args(&["把这件事总结一下"])).unwrap().prompt,
            Source::Inline("把这件事总结一下".to_string())
        );
        let file = parse(&args(&["--prompt-file", "/tmp/task.txt"])).unwrap();
        assert_eq!(file.prompt, Source::File(PathBuf::from("/tmp/task.txt")));
        assert!(parse(&args(&["--prompt-file", "/tmp/task.txt", "又给一份"])).is_err());
    }

    #[test]
    fn unknown_option_and_missing_value_are_errors() {
        assert!(parse(&args(&["--nope"])).is_err());
        assert!(parse(&args(&["--model"])).is_err());
        assert!(parse(&args(&["--output-format", "yaml"])).is_err());
        assert!(parse(&args(&["--max-steps", "many"])).is_err());
    }

    #[test]
    fn tool_whitelist_is_normalized_and_aliased() {
        assert!(split_tools("").unwrap().is_empty());
        assert_eq!(split_tools("all").unwrap().len(), AVAILABLE_TOOLS.len());
        assert_eq!(
            split_tools("WebSearch, read ,READ").unwrap(),
            vec!["web_search".to_string(), "read".to_string()]
        );
        // Unknown names fail loudly: the caller must not get an answer produced without the tool.
        let error = split_tools("WebFetch").unwrap_err();
        assert!(error.contains("WebFetch"), "报错要点名：{error}");
        assert!(error.contains("web_search"), "报错要给出可用集合：{error}");
    }

    #[test]
    fn answer_is_the_last_non_empty_assistant_message() {
        let messages = vec![
            Message::User {
                content: "任务".into(),
            },
            Message::Assistant {
                content: Some("先看看".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::Assistant {
                content: Some("最终答案".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
            Message::Assistant {
                content: Some("   ".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
            },
        ];
        assert_eq!(answer_text(&messages), "最终答案");
        assert_eq!(answer_text(&[]), "");
    }
}
