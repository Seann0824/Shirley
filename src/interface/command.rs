//! 快捷指令（slash commands）。
//!
//! 仅作用于 TUI 层：用户输入以 `prefix`（默认 `/`）开头时进入指令解析。
//! 架构上它**不接触 SDK**——指令要么展开成一段自然语言 prompt 发给 AI
//! （本质就是 prompt，见工作区根 `Agent.md` 的"本质是 prompt"约定），
//! 要么作为本地系统消息直接提示用户，完全不进入模型。
//!
//! 当前实现 `/init`、`/model`、`/rewind`、`/session` 与 `/login`。

use crate::prompt;

/// 指令触发符，默认 `/`。
pub const DEFAULT_PREFIX: char = '/';

/// 解析结果，由 `App` 消费。
#[derive(Debug)]
pub enum CommandOutcome {
    /// 展开为一段 prompt，走现有发送链路（UI 显示展开后内容，AI 收到的也是它）。
    Prompt(String),
    /// TUI 本地动作：直接给一条系统提示，不发给 AI。
    SystemMessage(String),
    /// TUI 本地动作：打开模型选择器。列表由应用层的模型目录提供，
    /// 指令本身不关心模型从哪来（见 `models` 模块）。
    ModelPicker,
    /// TUI 本地动作：回退最后一条用户消息，把原文填回输入框供编辑重发。
    /// 方案 A：只做最新一条，无需选择面板，候选也不涉及异步加载。
    Rewind,
    /// TUI 本地动作：打开会话选择器。会话清单由应用层的会话目录提供
    /// （见 `crate::session`），指令本身不关心会话从哪来、存哪里。
    SessionPicker,
    /// TUI 本地动作：进入分步登录流程（依次询问 base_url / api_key / model）。
    /// 指令本身不关心配置怎么写、写哪里——那是 `App` 与 `settings` 的事。
    Login,
    /// 不是已知指令：按普通文本照发（宽松策略，不打断用户）。
    Unknown,
}

/// 已知的内置指令。用枚举而非字符串表，避免运行期拼错。
enum Builtin {
    Init,
    Model,
    Rewind,
    Session,
    Login,
}

impl Builtin {
    fn name(&self) -> &'static str {
        match self {
            Builtin::Init => "init",
            Builtin::Model => "model",
            Builtin::Rewind => "rewind",
            Builtin::Session => "session",
            Builtin::Login => "login",
        }
    }

    fn handle(&self) -> CommandOutcome {
        match self {
            Builtin::Init => handle_init(),
            // 打开模型选择器：模型清单由应用层的 `models` 目录提供，
            // 指令层只负责"要开这个面板"，不掺和模型从哪来。
            Builtin::Model => CommandOutcome::ModelPicker,
            // 回退最新一条用户消息：候选来自 Agent 当前的消息表，指令层只管"发起回退"。
            Builtin::Rewind => CommandOutcome::Rewind,
            // 打开会话选择器：会话清单由应用层的会话目录提供，
            // 指令层只负责"要开这个面板"，不掺和会话从哪来、存哪里。
            Builtin::Session => CommandOutcome::SessionPicker,
            // 进入分步登录：具体问答与落盘由 `App` 驱动，指令层只管"发起登录"。
            Builtin::Login => CommandOutcome::Login,
        }
    }
}

/// `/init`：若项目已有非空 `Agent.md` 则提示无需重复；
/// 否则展开成"探索项目并生成 Agent.md"的 prompt 交给 AI。
fn handle_init() -> CommandOutcome {
    let root = prompt::workspace_root();
    let path = prompt::guide_path(&root);
    let present = std::fs::read_to_string(&path)
        .map(|content| !content.trim().is_empty())
        .unwrap_or(false);

    if present {
        return CommandOutcome::SystemMessage(format!(
            "项目已存在 {}（{}），无需重新生成。",
            prompt::GUIDE_FILE_NAME,
            path.display()
        ));
    }

    CommandOutcome::Prompt(
        "请为当前项目初始化项目指南（Agent.md）。\
         先探索工作区根目录下的项目结构、构建与测试方式、代码组织约定，\
         然后把一份简明准确的中文项目指南写入工作区根目录的 `Agent.md` 文件。\
         指南应包含：项目是什么、如何构建/运行/测试、关键的代码组织与约定、常见任务入口。\
         只写可确认的事实，不要编造；无法确定处标注「待确认」。\
         写完后用一句话汇报你做了什么。"
            .to_owned(),
    )
}

/// 指令注册与解析。无状态，仅持有触发符与指令清单。
pub struct CommandManager {
    prefix: char,
    builtins: Vec<Builtin>,
}

impl CommandManager {
    pub fn new(prefix: char) -> Self {
        Self {
            prefix,
            builtins: vec![
                Builtin::Init,
                Builtin::Model,
                Builtin::Rewind,
                Builtin::Session,
                Builtin::Login,
            ],
        }
    }

    /// 已注册指令数量（供测试与 UI 展示）。
    #[allow(dead_code)]
    pub fn command_count(&self) -> usize {
        self.builtins.len()
    }

    /// 模糊匹配候选指令（fuse / fuzzy）。
    ///
    /// 仅当输入以 prefix 开头、且"指令名片段"（首个空白符之前的部分）尚未命中
    /// 精确指令时才给出建议。返回按得分降序排列的 `(指令名, 得分)`，
    /// 得分低于 `MIN_SCORE` 的会被过滤掉，避免无意义的噪声。
    ///
    /// 例：`/initt` / `/inti` / `/nit` 都应召回 `init`。
    pub fn fuzzy_match(&self, input: &str) -> Vec<(String, f64)> {
        let trimmed = input.trim_start();
        if !trimmed.starts_with(self.prefix) {
            return Vec::new();
        }
        let rest = &trimmed[self.prefix.len_utf8()..];
        // 首个空白前的片段是"正在输入的指令名"。
        let (frag, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let frag = frag.trim();
        // 仅输入触发符（如 `/`）时，列出全部指令供直接选择。
        if frag.is_empty() {
            let mut all: Vec<(String, f64)> = self
                .builtins
                .iter()
                .map(|b| (b.name().to_owned(), 1.0))
                .collect();
            all.sort_by(|a, b| a.0.cmp(&b.0));
            return all;
        }
        // 精确命中就不必提示（直接回车即可提交）。
        let exact = self.builtins.iter().any(|b| b.name() == frag);
        if exact {
            return Vec::new();
        }

        // fuse 匹配：前缀匹配优先（强信号，始终保留），
        // 否则用归一化编辑距离 score = 1 - dist/max_len 容忍错位/缺字/多字。
        // 关键：片段是某个指令名的前缀时直接满分，否则用户刚敲出第一个字符
        // （如 `/i`）就会因编辑距离得分过低被过滤，导致候选"打一个字就消失"。
        const MIN_SCORE: f64 = 0.4;
        let mut scored: Vec<(String, f64)> = self
            .builtins
            .iter()
            .map(|b| {
                let name = b.name();
                let score = if name.starts_with(frag) {
                    1.0
                } else {
                    let dist = levenshtein(name, frag);
                    let max_len = name.len().max(frag.len()) as f64;
                    if max_len == 0.0 { 1.0 } else { 1.0 - dist as f64 / max_len }
                };
                (name.to_owned(), score)
            })
            .filter(|(_, score)| *score >= MIN_SCORE)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// 解析一行输入。不以 prefix 开头、或不是已知指令名，都返回 `Unknown`。
    pub fn resolve(&self, input: &str) -> CommandOutcome {
        let trimmed = input.trim();
        if !trimmed.starts_with(self.prefix) {
            return CommandOutcome::Unknown;
        }
        // 去掉 prefix，取首段作为指令名，其余为参数。
        let rest = &trimmed[self.prefix.len_utf8()..];
        let (name, args) = rest
            .split_once(char::is_whitespace)
            .unwrap_or((rest, ""));
        let name = name.trim();
        let _args = args.trim();

        for builtin in &self.builtins {
            if builtin.name() == name {
                return builtin.handle();
            }
        }
        CommandOutcome::Unknown
    }
}

/// 经典 Levenshtein 编辑距离（按字节/字符数，指令名均为 ASCII，等价）。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::GUIDE_FILE_NAME;

    #[test]
    fn unknown_prefix_is_passthrough() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("hello /world"), CommandOutcome::Unknown));
        assert!(matches!(mgr.resolve("/notacommand"), CommandOutcome::Unknown));
        assert!(matches!(mgr.resolve(""), CommandOutcome::Unknown));
    }

    #[test]
    fn init_expands_to_prompt_when_guide_missing() {
        // 用一个临时目录作为工作区，确保那里没有 Agent.md。
        let dir = std::env::temp_dir().join(format!("shirley_cmd_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir); }
        let mgr = CommandManager::new('/');
        match mgr.resolve("/init") {
            CommandOutcome::Prompt(p) => assert!(p.contains("Agent.md"), "应展开为生成指南的 prompt: {p}"),
            other => panic!("应为 Prompt，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("SHIRLEY_WORKSPACE"); }
    }

    #[test]
    fn rewind_starts_last_turn() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/rewind"), CommandOutcome::Rewind));
        // 带参数也应命中（参数被忽略——只回退最新一条）。
        assert!(matches!(mgr.resolve("/rewind 2"), CommandOutcome::Rewind));
    }

    #[test]
    fn model_opens_picker() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/model"), CommandOutcome::ModelPicker));
        // 带参数也应命中（参数由选择器 UI 忽略）。
        assert!(matches!(mgr.resolve("/model gpt"), CommandOutcome::ModelPicker));
    }

    #[test]
    fn session_opens_picker() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/session"), CommandOutcome::SessionPicker));
        // 带参数也应命中（参数由选择器 UI 忽略）。
        assert!(matches!(mgr.resolve("/session 2"), CommandOutcome::SessionPicker));
    }

    #[test]
    fn init_reports_present_when_guide_exists() {
        let dir = std::env::temp_dir().join(format!("shirley_cmd_exist_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# 项目说明\n").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir); }
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/init"), CommandOutcome::SystemMessage(_)));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("SHIRLEY_WORKSPACE"); }
    }
}

    #[test]
    fn fuzzy_recalls_init_from_typos() {
        let mgr = CommandManager::new('/');
        // 容忍常见手误：错位(inti)、多字(initt)、缺字(nit)、前缀(ini)。
        for typo in ["/i", "/inti", "/initt", "/nit", "/ini"] {
            let hits = mgr.fuzzy_match(typo);
            assert!(!hits.is_empty(), "{typo} 应召回 init，实际 {hits:?}");
            assert_eq!(hits[0].0, "init", "{typo} 首选应为 init");
        }
        // 过重的错字（itn 需 3 次编辑）本就不应召回，避免噪声。
        assert!(mgr.fuzzy_match("/itn").is_empty());
    }

    #[test]
    fn slash_alone_lists_all_commands() {
        // 只输入触发符 `/` 时应列出全部指令，供直接选择。
        let mgr = CommandManager::new('/');
        let hits = mgr.fuzzy_match("/");
        assert_eq!(hits.len(), mgr.command_count(), "`/` 应列出全部指令");
        assert!(hits.iter().any(|(n, _)| n == "init"));
    }

    #[test]
    fn fuzzy_no_suggestion_for_exact_or_plain_text() {
        let mgr = CommandManager::new('/');
        // 精确命中不提示
        assert!(mgr.fuzzy_match("/init").is_empty(), "精确命中不应提示");
        // 非指令前缀不触发
        assert!(mgr.fuzzy_match("hello").is_empty());
    }

    #[test]
    fn fuzzy_ignores_args_segment() {
        // 参数部分不应干扰指令名匹配
        let mgr = CommandManager::new('/');
        let hits = mgr.fuzzy_match("/inti some extra args");
        assert!(!hits.is_empty() && hits[0].0 == "init");
    }
