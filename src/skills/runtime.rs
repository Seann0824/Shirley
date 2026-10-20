//! Skill runtime: records the skills **activated** in this session and renders them into text appended at the end of each round.
//!
//! This is the Tier2 state of progressive disclosure (`docs/skills.md` 5.3). It is fully isomorphic to the task ledger
//! [`TodoStore`](crate::todo::TodoStore):
//!
//! - The state is held by the **application layer** (`Arc<Mutex<..>>`), and `provider` and the `activate_skill`
//!   tool share the same copy;
//! - it **does not enter `self.messages`**; `SkillContextProvider` appends it in a system message at the
//!   **end** when assembling the request each round -- compaction cannot touch it, so it survives compaction naturally.
//!
//! **Key constraint for KV prefix caching (do not change)**: the output of `render()` must maintain a
//! **byte-for-byte prefix** relationship with the "old output" -- activated blocks **are only appended, never reordered, reformatted, or inserted in the middle**.
//! This way the old part (directory + earlier activated blocks) stays byte-for-byte identical in the new round, and prefix caching hits as usual;
//! a newly activated skill just appends a segment to the string. The cost is the same as todo: a new activation invalidates the prefix
//! after "the last system message", minimized by "inject only activated ones + cap each block's characters".

use std::sync::{Arc, Mutex};

use super::catalog::SkillCatalog;

/// Errors that can occur when activating a skill, so the tool layer can distinguish "the model wrote a wrong name" from "failed to read the file".
#[derive(Debug)]
pub enum ActivateError {
    /// No such skill in the directory (a request problem by the model: use a valid name instead).
    UnknownSkill(String),
    /// Failed to read `SKILL.md` / list resources (an execution problem).
    ReadFailed(String),
}

/// A rendered block of an activated skill (stored in activation order).
struct ActivatedBlock {
    name: String,
    /// The complete block rendered at the moment of activation (`<skill_content>..</skill_content>`).
    /// **Once written, it never changes** -- this is the premise of append-only injection.
    block: String,
}

#[derive(Default)]
struct RuntimeState {
    activated: Vec<ActivatedBlock>,
}

/// Skill runtime: the directory (read-only, shared across sessions) + this session's activated set.
pub struct SkillRuntime {
    catalog: Arc<SkillCatalog>,
    inner: Mutex<RuntimeState>,
}

impl SkillRuntime {
    pub fn new(catalog: Arc<SkillCatalog>) -> Self {
        Self {
            catalog,
            inner: Mutex::new(RuntimeState::default()),
        }
    }

    /// All skill names in the directory (for the tool schema's `enum`).
    pub fn skill_names(&self) -> Vec<String> {
        self.catalog.skill_names()
    }

    /// The `(name, description)` list in the directory (for the desktop `/` suggestion popover).
    ///
    /// Same source as [`SkillRuntime::skill_names`], just carrying an extra description --
    /// the frontend suggestions need it for the subtitle.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.catalog
            .skills()
            .iter()
            .map(|skill| (skill.name.clone(), skill.description.clone()))
            .collect()
    }

    /// Whether the skill is already activated (for diagnostics / tests).
    #[cfg(test)]
    pub fn is_active(&self, name: &str) -> bool {
        self.inner
            .lock()
            .expect("skill runtime lock poisoned")
            .activated
            .iter()
            .any(|block| block.name == name)
    }

    /// Render the text to append-inject this round: **Tier1 directory + activated body blocks** (append-only).
    ///
    /// Empty directory and no activation -> `None` (do not inject empty content).
    pub fn render(&self) -> Option<String> {
        let mut out = String::new();
        if let Some(directory) = self.catalog.render_directory() {
            out.push_str(&directory);
        }
        let state = self.inner.lock().expect("skill runtime lock poisoned");
        for block in &state.activated {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&block.block);
        }
        (!out.is_empty()).then_some(out)
    }

    /// Activate a skill: read the body block and **append** it to the activated list (idempotent).
    ///
    /// Returns the rendered body block -- the tool returns it as the result, so the model sees the instruction this round.
    /// Re-activating the same skill does not append again (keeps the output stable and avoids needlessly breaking the cache).
    pub fn activate(&self, name: &str) -> Result<String, ActivateError> {
        if self.catalog.get(name).is_none() {
            return Err(ActivateError::UnknownSkill(name.to_string()));
        }
        let block = self
            .catalog
            .render_active(name)
            .map_err(ActivateError::ReadFailed)?;

        let mut state = self.inner.lock().expect("skill runtime lock poisoned");
        if !state.activated.iter().any(|existing| existing.name == name) {
            state.activated.push(ActivatedBlock {
                name: name.to_string(),
                block: block.clone(),
            });
        }
        Ok(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::parse::SkillMeta;
    use std::path::PathBuf;

    fn catalog_with(skills: &[(&str, &str)]) -> Arc<SkillCatalog> {
        Arc::new(SkillCatalog::new(
            skills
                .iter()
                .map(|(name, description)| SkillMeta {
                    name: (*name).into(),
                    description: (*description).into(),
                    root: PathBuf::from(format!("/nonexistent/{name}")),
                })
                .collect(),
        ))
    }

    #[test]
    fn empty_runtime_renders_directory_only() {
        let runtime = SkillRuntime::new(catalog_with(&[("alpha", "does alpha")]));
        let text = runtime.render().unwrap();
        assert!(text.contains("<available_skills>"));
        assert!(!text.contains("<skill_content"));
    }

    #[test]
    fn empty_catalog_and_no_activation_renders_none() {
        let runtime = SkillRuntime::new(catalog_with(&[]));
        assert!(runtime.render().is_none());
    }

    /// Create a temporary skill directory, write a `SKILL.md`, and return `(SkillMeta, root dir)`.
    fn real_skill(tag: &str, name: &str, body: &str) -> (SkillMeta, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "shirley_skill_runtime_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: does {name}\n---\n{body}\n"),
        )
        .unwrap();
        (
            SkillMeta {
                name: name.into(),
                description: format!("does {name}"),
                root: skill_dir,
            },
            dir,
        )
    }

    #[test]
    fn activation_is_append_only_kv_friendly() {
        // KV-cache regression: after activating a second skill, the new output must have the previous output as a **byte-for-byte prefix**
        // -- the old part (directory + first block) is unchanged, and new content is only appended.
        let (alpha, dir_a) = real_skill("alpha", "alpha", "ALPHA-INSTRUCTION");
        let (beta, dir_b) = real_skill("beta", "beta", "BETA-INSTRUCTION");
        let runtime = SkillRuntime::new(Arc::new(SkillCatalog::new(vec![alpha, beta])));

        let baseline = runtime.render().unwrap();
        runtime.activate("alpha").unwrap();
        let after_alpha = runtime.render().unwrap();
        runtime.activate("beta").unwrap();
        let after_beta = runtime.render().unwrap();

        assert!(
            after_alpha.starts_with(&baseline),
            "after activating alpha, the previous output should be a prefix"
        );
        assert!(
            after_beta.starts_with(&after_alpha),
            "after activating beta, the previous output should be a prefix (append-only, KV friendly)"
        );
        assert!(after_beta.contains("ALPHA-INSTRUCTION"));
        assert!(after_beta.contains("BETA-INSTRUCTION"));

        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }

    #[test]
    fn activating_twice_is_idempotent() {
        let (alpha, dir) = real_skill("idem", "alpha", "ALPHA-INSTRUCTION");
        let runtime = SkillRuntime::new(Arc::new(SkillCatalog::new(vec![alpha])));
        runtime.activate("alpha").unwrap();
        let once = runtime.render().unwrap();
        runtime.activate("alpha").unwrap();
        let twice = runtime.render().unwrap();
        assert_eq!(
            once, twice,
            "re-activation should not change the output (keep the cache stable)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn activate_reads_body_and_lists_resources() {
        let (alpha, dir) = real_skill("res", "alpha", "# Heading\nBODY-INSTRUCTION");
        std::fs::create_dir_all(alpha.root.join("scripts")).unwrap();
        std::fs::write(alpha.root.join("scripts/run.py"), "print(1)\n").unwrap();
        let runtime = SkillRuntime::new(Arc::new(SkillCatalog::new(vec![alpha])));
        let block = runtime.activate("alpha").unwrap();
        assert!(block.contains("<skill_content name=\"alpha\">"));
        assert!(block.contains("BODY-INSTRUCTION"));
        assert!(
            !block.contains("name: alpha"),
            "frontmatter should be stripped"
        );
        assert!(block.contains("<file>scripts/run.py</file>"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_skill_is_error_and_does_not_mutate() {
        let runtime = SkillRuntime::new(catalog_with(&[("alpha", "does alpha")]));
        assert!(matches!(
            runtime.activate("nope"),
            Err(ActivateError::UnknownSkill(_))
        ));
        assert!(!runtime.is_active("alpha"));
    }
}
