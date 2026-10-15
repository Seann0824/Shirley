//! Slash commands.
//!
//! They act only on the TUI layer: when the user's input starts with `prefix` (default `/`), it enters command parsing.
//! Architecturally it **does not touch the SDK** -- a command either expands into a natural-language prompt sent to the AI
//! (it is essentially a prompt, see the "essentially a prompt" convention in the workspace root `Agent.md`),
//! or acts as a local system message shown directly to the user, never entering the model.
//!
//! Currently implements `/init`, `/model`, `/rewind`, `/session`, `/login`, and the **dynamic
//! `/<skill-name>`** -- the latter activates a skill directly via the harness (see `docs/skills.md` 5.6, P2).

use crate::prompt;

/// Command trigger, default `/`.
pub const DEFAULT_PREFIX: char = '/';

/// Parse result, consumed by `App`.
#[derive(Debug)]
pub enum CommandOutcome {
/// Expand into a prompt and go through the existing send path (the UI shows the expanded content, and the AI receives the same).
    Prompt(String),
/// TUI local action: show a system hint directly, without sending it to the AI.
    SystemMessage(String),
/// TUI local action: open the model picker. The list comes from the application-layer model catalog;
/// the command itself does not care where models come from (see the `models` module).
    ModelPicker,
/// TUI local action: rewind the last user message, filling its original text back into the input box for editing and resending.
/// Option A: only the most recent one, no selection panel, and candidates involve no async loading.
    Rewind,
/// TUI local action: open the session picker. The session list comes from the application-layer session catalog
/// (see `crate::session`); the command itself does not care where sessions come from or where they are stored.
    SessionPicker,
/// TUI local action: open the subagent picker (`docs/subagent.md` decision 10, P2).
/// The list comes from the current session's `SubagentManager`; the command itself does not care
/// where subagents come from. Confirming an entry opens a read-only child-transcript overlay.
    SubagentPicker,
/// TUI local action: enter the step-by-step login flow (asks for base_url / api_key / model in turn).
/// The command itself does not care how/where config is written -- that is `App`'s and `settings`' concern.
    Login,
/// Dynamic slash entry: `/<skill-name> [args]` -- the harness activates that skill directly
/// (a deterministic entry point that skips the model's judgment, see `docs/skills.md` 5.6).
    ///
/// When `args` is non-empty, after activation this text is sent as **this turn's prompt**; when empty, it only activates
/// without bothering the model (the `App` gives a local system hint).
    ActivateSkill { name: String, args: String },
/// Not a known command: send it as ordinary text (lenient policy, do not interrupt the user).
    Unknown,
}

/// Known built-in commands. An enum instead of a string table, to avoid runtime typos.
enum Builtin {
    Init,
    Model,
    Rewind,
    Session,
    Login,
    Subagents,
}

impl Builtin {
    fn name(&self) -> &'static str {
        match self {
            Builtin::Init => "init",
            Builtin::Model => "model",
            Builtin::Rewind => "rewind",
            Builtin::Session => "session",
            Builtin::Login => "login",
            Builtin::Subagents => "subagents",
        }
    }

    fn handle(&self) -> CommandOutcome {
        match self {
            Builtin::Init => handle_init(),
            // Open the model picker: the model list comes from the application-layer `models` catalog;
            // the command layer is only responsible for "open this panel" and does not meddle in where models come from.
            Builtin::Model => CommandOutcome::ModelPicker,
            // Rewind the latest user message: candidates come from the Agent's current message table; the command layer only "initiates the rewind".
            Builtin::Rewind => CommandOutcome::Rewind,
            // Open the session picker: the session list comes from the application-layer session catalog;
            // the command layer is only responsible for "open this panel" and does not meddle in where sessions come from or where they are stored.
            Builtin::Session => CommandOutcome::SessionPicker,
            // Enter step-by-step login: the actual Q&A and persistence are driven by `App`; the command layer only "initiates login".
            Builtin::Login => CommandOutcome::Login,
            // Open the subagent picker: the list comes from the session's `SubagentManager`; the command layer only "opens this panel".
            Builtin::Subagents => CommandOutcome::SubagentPicker,
        }
    }
}

/// `/init`: if the project already has a non-empty `Agent.md`, hint that there is no need to repeat;
/// otherwise expand into a "explore the project and generate Agent.md" prompt handed to the AI.
fn handle_init() -> CommandOutcome {
    let root = prompt::workspace_root();
    let path = prompt::guide_path(&root);
    let present = std::fs::read_to_string(&path)
        .map(|content| !content.trim().is_empty())
        .unwrap_or(false);

    if present {
        return CommandOutcome::SystemMessage(format!(
            "The project already has {} ({}); no need to regenerate.",
            prompt::GUIDE_FILE_NAME,
            path.display()
        ));
    }

    CommandOutcome::Prompt(
        "Please initialize the project guide (Agent.md) for the current project.\
         First explore the project structure under the workspace root, the build and test methods, and the code organization conventions,\
         then write a concise and accurate project guide into the `Agent.md` file at the workspace root.\
         The guide should cover: what the project is, how to build/run/test, key code organization and conventions, and common task entry points.\
         Only write confirmable facts, do not fabricate; mark anything uncertain as \"to be confirmed\".\
         When done, report what you did in one sentence."
            .to_owned(),
    )
}

/// Command registration and parsing. Stateless; holds only the trigger and the command list.
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
                Builtin::Subagents,
            ],
        }
    }

    /// Number of registered commands (for tests and UI display).
    #[allow(dead_code)]
    pub fn command_count(&self) -> usize {
        self.builtins.len()
    }

    /// Fuzzy-match candidate commands (fuse / fuzzy).
    ///
    /// Suggestions are given only when the input starts with the prefix and the "command-name fragment" (the part before the first
    /// whitespace) has not yet matched an exact command. Returns `(command name, score)` sorted by score descending;
    /// those below `MIN_SCORE` are filtered out to avoid meaningless noise.
    ///
    /// Example: `/initt` / `/inti` / `/nit` should all recall `init`.
    pub fn fuzzy_match(&self, input: &str, skills: &[String]) -> Vec<(String, f64)> {
        let trimmed = input.trim_start();
        if !trimmed.starts_with(self.prefix) {
            return Vec::new();
        }
        let rest = &trimmed[self.prefix.len_utf8()..];
        // The fragment before the first whitespace is the "command name being typed".
        let (frag, _) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let frag = frag.trim();
        // Candidate names = built-in commands + skills visible to the current session (dynamic slash entry).
        // Skill names are passed in as **data**; `CommandManager` stays stateless and does not cache (to avoid staleness).
        let names: Vec<String> = self
            .builtins
            .iter()
            .map(|b| b.name().to_owned())
            .chain(skills.iter().cloned())
            .collect();
        // When only the trigger is typed (e.g. `/`), list all commands for direct selection.
        if frag.is_empty() {
            let mut all: Vec<(String, f64)> = names.iter().map(|n| (n.clone(), 1.0)).collect();
            all.sort_by(|a, b| a.0.cmp(&b.0));
            return all;
        }
        // No need to suggest on an exact match (pressing Enter submits directly).
        let exact = names.iter().any(|n| n == frag);
        if exact {
            return Vec::new();
        }

        // Fuse matching: prefix match first (a strong signal, always kept),
        // otherwise a normalized edit distance score = 1 - dist/max_len tolerating transpositions/missing/extra characters.
        // Key point: when the fragment is a prefix of a command name, score full marks; otherwise a user who has just typed the first
        // character (e.g. `/i`) would be filtered out by a too-low edit-distance score, making candidates "vanish after one keystroke".
        const MIN_SCORE: f64 = 0.4;
        let mut scored: Vec<(String, f64)> = names
            .iter()
            .map(|name| {
                let score = if name.starts_with(frag) {
                    1.0
                } else {
                    let dist = levenshtein(name, frag);
                    let max_len = name.len().max(frag.len()) as f64;
                    if max_len == 0.0 { 1.0 } else { 1.0 - dist as f64 / max_len }
                };
                (name.clone(), score)
            })
            .filter(|(_, score)| *score >= MIN_SCORE)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Parse one line of input. If it does not start with the prefix, or is not a known command name, returns `Unknown`.
    ///
    /// `skills` are the skill names visible to the current session (dynamic slash entry, see `docs/skills.md` 5.6).
    /// Built-in commands take precedence; then skill names. When `/<skill-name> [args]` hits a skill it returns
    /// [`CommandOutcome::ActivateSkill`], activated directly by the harness -- a deterministic entry point that skips model judgment.
    pub fn resolve(&self, input: &str, skills: &[String]) -> CommandOutcome {
        let trimmed = input.trim();
        if !trimmed.starts_with(self.prefix) {
            return CommandOutcome::Unknown;
        }
        // Strip the prefix, take the first segment as the command name and the rest as args.
        let rest = &trimmed[self.prefix.len_utf8()..];
        let (name, args) = rest
            .split_once(char::is_whitespace)
            .unwrap_or((rest, ""));
        let name = name.trim();
        let args = args.trim();

        // Built-in commands take precedence (the slash semantics users know, and not stolen by a same-named skill).
        for builtin in &self.builtins {
            if builtin.name() == name {
                return builtin.handle();
            }
        }
        // Dynamic slash entry: `/<skill-name>` is activated directly by the harness.
        if skills.iter().any(|skill| skill == name) {
            return CommandOutcome::ActivateSkill {
                name: name.to_owned(),
                args: args.to_owned(),
            };
        }
        CommandOutcome::Unknown
    }
}

/// Classic Levenshtein edit distance (by bytes/char count; command names are all ASCII, so equivalent).
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
        assert!(matches!(mgr.resolve("hello /world", &[]), CommandOutcome::Unknown));
        assert!(matches!(mgr.resolve("/notacommand", &[]), CommandOutcome::Unknown));
        assert!(matches!(mgr.resolve("", &[]), CommandOutcome::Unknown));
    }

    #[test]
    fn init_expands_to_prompt_when_guide_missing() {
        // Use a temp directory as the workspace, ensuring there is no Agent.md there.
        let dir = std::env::temp_dir().join(format!("shirley_cmd_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir); }
        let mgr = CommandManager::new('/');
        match mgr.resolve("/init", &[]) {
            CommandOutcome::Prompt(p) => assert!(p.contains("Agent.md"), "should expand into a guide-generating prompt: {p}"),
            other => panic!("expected Prompt, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("SHIRLEY_WORKSPACE"); }
    }

    #[test]
    fn rewind_starts_last_turn() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/rewind", &[]), CommandOutcome::Rewind));
        // It should also hit with args (args are ignored -- only the latest one is rewound).
        assert!(matches!(mgr.resolve("/rewind 2", &[]), CommandOutcome::Rewind));
    }

    #[test]
    fn model_opens_picker() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/model", &[]), CommandOutcome::ModelPicker));
        // It should also hit with args (args are ignored by the picker UI).
        assert!(matches!(mgr.resolve("/model gpt", &[]), CommandOutcome::ModelPicker));
    }

    #[test]
    fn session_opens_picker() {
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/session", &[]), CommandOutcome::SessionPicker));
        // It should also hit with args (args are ignored by the picker UI).
        assert!(matches!(mgr.resolve("/session 2", &[]), CommandOutcome::SessionPicker));
    }

    #[test]
    fn init_reports_present_when_guide_exists() {
        let dir = std::env::temp_dir().join(format!("shirley_cmd_exist_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join(GUIDE_FILE_NAME), "# Project notes\n").unwrap();
        unsafe { std::env::set_var("SHIRLEY_WORKSPACE", &dir); }
        let mgr = CommandManager::new('/');
        assert!(matches!(mgr.resolve("/init", &[]), CommandOutcome::SystemMessage(_)));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::remove_var("SHIRLEY_WORKSPACE"); }
    }
}

    #[test]
    fn fuzzy_recalls_init_from_typos() {
        let mgr = CommandManager::new('/');
        // Tolerate common typos: transposition (inti), extra char (initt), missing char (nit), prefix (ini).
        for typo in ["/i", "/inti", "/initt", "/nit", "/ini"] {
            let hits = mgr.fuzzy_match(typo, &[]);
            assert!(!hits.is_empty(), "{typo} should recall init, got {hits:?}");
            assert_eq!(hits[0].0, "init", "{typo} should prefer init");
        }
        // A too-heavy typo (itn needs 3 edits) should not be recalled in the first place, to avoid noise.
        assert!(mgr.fuzzy_match("/itn", &[]).is_empty());
    }

    #[test]
    fn slash_alone_lists_all_commands() {
        // When only the trigger `/` is typed, all commands should be listed for direct selection.
        let mgr = CommandManager::new('/');
        let hits = mgr.fuzzy_match("/", &[]);
        assert_eq!(hits.len(), mgr.command_count(), "`/` should list all commands");
        assert!(hits.iter().any(|(n, _)| n == "init"));
    }

    #[test]
    fn fuzzy_no_suggestion_for_exact_or_plain_text() {
        let mgr = CommandManager::new('/');
        // No suggestion on an exact match
        assert!(mgr.fuzzy_match("/init", &[]).is_empty(), "an exact match should not suggest");
        // Not triggered by a non-command prefix
        assert!(mgr.fuzzy_match("hello", &[]).is_empty());
    }

    #[test]
    fn skill_name_resolves_to_activate() {
        let mgr = CommandManager::new('/');
        let skills = vec!["pdf".to_owned(), "docx".to_owned()];
        // No args: only activate, args empty.
        match mgr.resolve("/pdf", &skills) {
            CommandOutcome::ActivateSkill { name, args } => {
                assert_eq!(name, "pdf");
                assert!(args.is_empty());
            }
            other => panic!("expected ActivateSkill, got {other:?}"),
        }
        // With args: activate + use the args as this turn's prompt.
        match mgr.resolve("/docx convert this document to markdown", &skills) {
            CommandOutcome::ActivateSkill { name, args } => {
                assert_eq!(name, "docx");
                assert_eq!(args, "convert this document to markdown");
            }
            other => panic!("expected ActivateSkill, got {other:?}"),
        }
        // A name not in the catalog is still ordinary text (lenient policy, no interruption).
        assert!(matches!(mgr.resolve("/nope", &skills), CommandOutcome::Unknown));
    }

    #[test]
    fn builtin_wins_over_same_named_skill() {
        // Built-in precedence: even if the catalog has a skill named `model`, `/model` still opens the model picker.
        let mgr = CommandManager::new('/');
        let skills = vec!["model".to_owned()];
        assert!(matches!(mgr.resolve("/model", &skills), CommandOutcome::ModelPicker));
    }

    #[test]
    fn fuzzy_match_includes_skill_names() {
        let mgr = CommandManager::new('/');
        let skills = vec!["pdf".to_owned()];
        // `/` lists all: built-ins + skills.
        let all = mgr.fuzzy_match("/", &skills);
        assert_eq!(all.len(), mgr.command_count() + 1);
        assert!(all.iter().any(|(n, _)| n == "pdf"));
        // Prefix hits a skill name.
        let hits = mgr.fuzzy_match("/pd", &skills);
        assert_eq!(hits[0].0, "pdf");
    }

    #[test]
    fn fuzzy_ignores_args_segment() {
        // The args portion should not interfere with command-name matching
        let mgr = CommandManager::new('/');
        let hits = mgr.fuzzy_match("/inti some extra args", &[]);
        assert!(!hits.is_empty() && hits[0].0 == "init");
    }
