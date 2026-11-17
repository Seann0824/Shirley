//! `save_skill`: a bot writes down how to do something, for next time.
//!
//! The skill goes into the bot's own skill directory (`bots/<bot>/skills/<name>/`), the same
//! place market skills install to; only this bot sees it, and it is never offered to the market.
//! Saving under an existing name of the bot's own replaces that skill (that is how a bot improves
//! one); a skill installed from the market is left alone -- it would be overwritten by the next
//! install, so the bot saves its variant under a new name.
//!
//! The write is staged in a hidden directory and swapped in whole, so a half-written skill is
//! never scanned. The catalog re-scans on `activate_skill` with an unknown name, so the skill can
//! be activated in the same turn; the runtime rebuild (`BotRuntimeManager`) lists it from the next
//! message on.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use shirley_agent_sdk::{Tool, ToolContext, ToolDefinition, ToolError, ToolFuture};

use crate::skills::market::valid_id;

const SKILL_FILE: &str = "SKILL.md";
/// Present in a skill installed from the market.
const MARKER_FILE: &str = ".market.json";
const MAX_FILES: usize = 20;
const MAX_BYTES: usize = 512 * 1024;

pub struct SaveSkillTool {
    root: PathBuf,
    definition: ToolDefinition,
}

impl SaveSkillTool {
    /// `root`: the bot's own skill directory.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            definition: ToolDefinition {
                name: "save_skill".into(),
                description: "Save a skill of your own: instructions (and optional scripts or \
                    templates) for a kind of task you will be asked to do again. Saving under a \
                    name you already saved replaces that skill. It can be activated right away \
                    with `activate_skill`."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Short id: lowercase letters, digits and '-' (e.g. \"monthly-invoice-report\")."
                        },
                        "description": {
                            "type": "string",
                            "description": "One line: what the skill does and when to use it. This is what decides whether it gets activated later."
                        },
                        "instructions": {
                            "type": "string",
                            "description": "The Markdown body: the steps, conventions and pitfalls you would want to know next time. Refer to bundled files by relative path (e.g. scripts/report.py)."
                        },
                        "files": {
                            "type": "array",
                            "description": "Optional files to bundle, under scripts/, references/ or assets/.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "path": { "type": "string", "description": "Relative path, e.g. scripts/report.py" },
                                    "content": { "type": "string" }
                                },
                                "required": ["path", "content"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["name", "description", "instructions"],
                    "additionalProperties": false
                }),
            },
        }
    }
}

#[derive(Deserialize)]
struct Args {
    name: String,
    description: String,
    instructions: String,
    #[serde(default)]
    files: Vec<FileArg>,
}

#[derive(Deserialize)]
struct FileArg {
    path: String,
    content: String,
}

impl Tool for SaveSkillTool {
    fn definition(&self) -> &ToolDefinition {
        &self.definition
    }

    fn invoke(&self, input: serde_json::Value, _ctx: ToolContext) -> ToolFuture<'_> {
        let root = self.root.clone();
        Box::pin(async move {
            let args: Args = serde_json::from_value(input)
                .map_err(|error| ToolError::ArgumentsError(format!("invalid tool arguments: {error}")))?;
            let replaced = tokio::task::spawn_blocking(move || save(&root, &args))
                .await
                .map_err(|error| ToolError::ExecutionError(error.to_string()))??;
            Ok(serde_json::json!({
                "saved": replaced.0,
                "replaced_existing": replaced.1,
                "note": "Activate it with activate_skill when a task calls for it.",
            }))
        })
    }
}

/// Write the skill; returns its name and whether it replaced one.
fn save(root: &Path, args: &Args) -> Result<(String, bool), ToolError> {
    let bad = |message: String| ToolError::ArgumentsError(message);
    let name = args.name.trim();
    if !valid_id(name) {
        return Err(bad(format!(
            "`{name}` is not a valid skill name: use lowercase letters, digits and '-'"
        )));
    }
    let description = args.description.split_whitespace().collect::<Vec<_>>().join(" ");
    if description.is_empty() || args.instructions.trim().is_empty() {
        return Err(bad("`description` and `instructions` must not be empty".into()));
    }
    if args.files.len() > MAX_FILES {
        return Err(bad(format!("at most {MAX_FILES} files")));
    }
    let total = args.instructions.len() + args.files.iter().map(|file| file.content.len()).sum::<usize>();
    if total > MAX_BYTES {
        return Err(bad("the skill is too large (512 KB at most)".into()));
    }
    for file in &args.files {
        if !bundled_path(&file.path) {
            return Err(bad(format!(
                "`{}`: files go under scripts/, references/ or assets/, as relative paths",
                file.path
            )));
        }
    }

    let target = root.join(name);
    if target.join(MARKER_FILE).exists() {
        return Err(bad(format!(
            "`{name}` was installed from the skill market; save your version under another name"
        )));
    }
    let replacing = target.join(SKILL_FILE).is_file();

    let staging = root.join(format!(".saving-{name}-{}", uuid::Uuid::new_v4().simple()));
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&staging)?;
        let skill = format!(
            "---\nname: {name}\ndescription: {}\n---\n\n{}\n",
            yaml_scalar(&description),
            args.instructions.trim()
        );
        std::fs::write(staging.join(SKILL_FILE), skill)?;
        for file in &args.files {
            let path = staging.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, &file.content)?;
        }
        if replacing {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::rename(&staging, &target)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ToolError::ExecutionError(format!("could not save the skill: {error}")));
    }
    Ok((name.to_owned(), replacing))
}

/// A bundled file: relative, inside the skill, under one of the resource folders.
fn bundled_path(path: &str) -> bool {
    let path = Path::new(path);
    let mut components = path.components();
    matches!(
        components.next(),
        Some(Component::Normal(first)) if ["scripts", "references", "assets"].iter().any(|dir| first == *dir)
    ) && components.clone().next().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)))
}

/// The description on one frontmatter line: quoted when YAML would misread it.
fn yaml_scalar(text: &str) -> String {
    let risky = text.contains(": ")
        || text.contains(" #")
        || text.starts_with(|c: char| "\"'[]{}>|*&!%@`#,?-".contains(c));
    if risky {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::parse::parse_skill;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shirley_save_skill_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn args(name: &str, description: &str, files: Vec<(&str, &str)>) -> Args {
        Args {
            name: name.into(),
            description: description.into(),
            instructions: "1. Collect the invoices.\n2. Run scripts/report.py.".into(),
            files: files
                .into_iter()
                .map(|(path, content)| FileArg {
                    path: path.into(),
                    content: content.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_saved_skill_parses_and_is_replaced_by_saving_again() {
        let root = tmp("save");
        let first = args(
            "monthly-invoices",
            "Monthly invoice report: use when asked for it",
            vec![("scripts/report.py", "print('hi')")],
        );
        assert_eq!(save(&root, &first).unwrap(), ("monthly-invoices".into(), false));
        let dir = root.join("monthly-invoices");
        let text = std::fs::read_to_string(dir.join(SKILL_FILE)).unwrap();
        let meta = parse_skill(&text, "monthly-invoices", dir.clone()).unwrap();
        assert_eq!(meta.description, "Monthly invoice report: use when asked for it");
        assert!(dir.join("scripts/report.py").is_file());

        let second = args("monthly-invoices", "Better version", vec![]);
        assert!(save(&root, &second).unwrap().1, "replaced an existing skill");
        assert!(!dir.join("scripts/report.py").exists(), "replaced whole");
        // Nothing staged is left behind.
        let leftovers = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(leftovers, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bad_names_paths_and_market_skills_are_refused() {
        let root = tmp("refuse");
        assert!(save(&root, &args("../x", "d", vec![])).is_err());
        assert!(save(&root, &args("Upper", "d", vec![])).is_err());
        assert!(save(&root, &args("ok", "", vec![])).is_err());
        for path in ["../evil", "/etc/passwd", "SKILL.md", "scripts", "notes/a.md", "scripts/../../x"] {
            assert!(save(&root, &args("ok", "d", vec![(path, "x")])).is_err(), "{path}");
        }
        std::fs::create_dir_all(root.join("pdf")).unwrap();
        std::fs::write(root.join("pdf").join(MARKER_FILE), "{}").unwrap();
        assert!(save(&root, &args("pdf", "d", vec![])).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn descriptions_that_would_break_yaml_are_quoted() {
        assert_eq!(yaml_scalar("plain text"), "plain text");
        assert_eq!(yaml_scalar("Use: when asked"), "\"Use: when asked\"");
        assert_eq!(yaml_scalar("- list-like"), "\"- list-like\"");
    }
}
