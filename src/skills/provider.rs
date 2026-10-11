//! 每轮请求末尾注入 skill 上下文的提供者（渐进式披露的 Tier1 + Tier2）。
//!
//! 复用 SDK 的 [`ContextProvider`] 接缝，与任务账本 / 记忆同款：注入位置是**本轮
//! 请求末尾的一条 system 消息**——不进 `self.messages`、压缩碰不到、不动前缀缓存。
//!
//! **渲染 = Tier1 目录 + 已激活正文块**，且严格**追加式**：目录是稳定前缀，已激活
//! 块按激活顺序拼接在它之后，旧内容逐字节不变。这样前缀缓存命中，新激活只是把
//! 字符串往后接一段（见 [`SkillRuntime::render`](super::runtime::SkillRuntime::render)）。
//!
//! 目录为空且无激活 → 返回 `None`（不注入空内容）。

use std::sync::Arc;

use shirley_agent_sdk::ContextProvider;

use super::runtime::SkillRuntime;

/// 每轮末尾注入「可用 skill 目录 + 已激活正文」的提供者。
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
        // 用真实临时目录，验证激活后正文经 provider 注入。
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
        let provider = SkillContextProvider::new(runtime.clone());

        assert!(!provider.context().unwrap().contains("BODY-INSTRUCTION"));
        runtime.activate("alpha").unwrap();
        let text = provider.context().unwrap();
        assert!(text.contains("<skill_content name=\"alpha\">"));
        assert!(text.contains("BODY-INSTRUCTION"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
