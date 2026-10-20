//! Provider that injects skill context at the end of each request (progressive disclosure
//! Tier1 + Tier2).
//!
//! It reuses the SDK's [`ContextProvider`] seam, same as the task ledger / memory: the
//! injection point is **a system message at the end of this turn's request** — it doesn't enter
//! `self.messages`, compaction can't touch it, and it doesn't disturb the prefix cache.
//!
//! **Render = Tier1 directory + activated body blocks**, and strictly **append-only**: the
//! directory is a stable prefix, activated blocks are concatenated after it in activation order,
//! and old content stays byte-for-byte unchanged. This keeps the prefix cache hitting, and a new
//! activation just appends another segment to the string (see
//! [`SkillRuntime::render`](super::runtime::SkillRuntime::render)).
//!
//! Empty directory and no activation -> returns `None` (injects nothing).

use std::sync::Arc;

use shirley_agent_sdk::ContextProvider;

use super::runtime::SkillRuntime;

/// Provider that injects the "available skills directory + activated bodies" each turn.
pub struct SkillContextProvider {
    runtime: Arc<SkillRuntime>,
}

impl SkillContextProvider {
    pub fn new(runtime: Arc<SkillRuntime>) -> Self {
        Self { runtime }
    }
}

impl ContextProvider for SkillContextProvider {
    fn context(&self) -> Option<String> {
        self.runtime.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::catalog::SkillCatalog;
    use crate::skills::parse::SkillMeta;
    use std::path::PathBuf;

    fn provider(skills: &[(&str, &str)]) -> SkillContextProvider {
        let catalog = Arc::new(SkillCatalog::new(
            skills
                .iter()
                .map(|(name, description)| SkillMeta {
                    name: (*name).into(),
                    description: (*description).into(),
                    root: PathBuf::from(format!("/nonexistent/{name}")),
                })
                .collect(),
        ));
        SkillContextProvider::new(Arc::new(SkillRuntime::new(catalog)))
    }

    #[test]
    fn empty_catalog_injects_nothing() {
        assert!(provider(&[]).context().is_none());
    }

    #[test]
    fn non_empty_catalog_injects_directory() {
        let text = provider(&[("alpha", "does alpha")]).context().unwrap();
        assert!(text.contains("<available_skills>"));
        assert!(text.contains("name=\"alpha\""));
    }

    #[test]
    fn activated_content_is_injected() {
        // Use a real temp dir to verify the activated body is injected through the provider.
        let dir = std::env::temp_dir().join(format!(
            "shirley_skill_provider_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let skill_dir = dir.join("alpha");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: alpha\ndescription: does alpha\n---\nBODY-INSTRUCTION\n",
        )
        .unwrap();

        let catalog = Arc::new(SkillCatalog::new(vec![SkillMeta {
            name: "alpha".into(),
            description: "does alpha".into(),
            root: skill_dir.clone(),
        }]));
        let runtime = Arc::new(SkillRuntime::new(catalog));
        let provider = SkillContextProvider::new(Arc::clone(&runtime));

        assert!(!provider.context().unwrap().contains("BODY-INSTRUCTION"));
        runtime.activate("alpha").unwrap();
        let text = provider.context().unwrap();
        assert!(text.contains("<skill_content name=\"alpha\">"));
        assert!(text.contains("BODY-INSTRUCTION"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
