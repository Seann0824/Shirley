//! 技能运行时：记录本会话**已激活**的 skill，并渲染成每轮末尾追加注入的文本。
//!
//! 这是渐进式披露的 Tier2 状态（`docs/skills.md` 5.3）。与任务账本
//! [`TodoStore`](crate::todo::TodoStore) 完全同构：
//!
//! - 状态由**应用层**持有（`Arc<Mutex<..>>`），`provider` 与 `activate_skill`
//!   工具共享同一份；
//! - **不进 `self.messages`**，由 `SkillContextProvider` 每轮组装请求时追加在
//!   **末尾**的一条 system 消息里——压缩碰不到它，天然跨压缩存活。
//!
//! **KV 前缀缓存的关键约束（别改）**：`render()` 的输出必须对「旧输出」保持
//! **逐字节前缀**关系——已激活块**只追加、不重排、不重格式化、不在中间插入**。
//! 这样旧部分（目录 + 早先激活的块）在新一轮里逐字节不变，前缀缓存照常命中；
//! 新激活的 skill 只是把字符串往后接一段。代价与 todo 一样：新激活会让「末尾
//! 那条 system 之后」的前缀失效，用「只注入已激活的 + 每块设字符上限」压到最小。

use std::sync::{Arc, Mutex};

use super::catalog::SkillCatalog;

/// 激活一个 skill 时可能出的错，供工具层区分「模型写错名」与「读文件失败」。
#[derive(Debug)]
pub enum ActivateError {
    /// 目录里没有这个 skill（模型的请求问题：换个合法名字即可）。
    UnknownSkill(String),
    /// 读 `SKILL.md` / 列资源失败（执行问题）。
    ReadFailed(String),
}

/// 一个已激活 skill 的渲染块（按激活顺序保存）。
struct ActivatedBlock {
    name: String,
    /// 激活那一刻渲染好的完整块（`<skill_content>..</skill_content>`）。
    /// **一经写入不再改动**——这是追加式注入的前提。
    block: String,
}

#[derive(Default)]
struct RuntimeState {
    activated: Vec<ActivatedBlock>,
}

/// 技能运行时：目录（只读、跨会话共享）+ 本会话已激活集合。
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

    /// 目录里的全部 skill 名（供工具 schema 的 `enum` 用）。
    pub fn skill_names(&self) -> Vec<String> {
        self.catalog.skill_names()
    }

    /// 目录里的 `(name, description)` 列表（供 desktop `/` 候选浮层展示）。
    ///
    /// 与 [`SkillRuntime::skill_names`] 同源，只是多带一条 description——
    /// 前端候选需要它做副标题。
    pub fn entries(&self) -> Vec<(String, String)> {
        self.catalog
            .skills()
            .iter()
            .map(|skill| (skill.name.clone(), skill.description.clone()))
            .collect()
    }

    /// 是否已激活该 skill（诊断 / 测试用）。
    #[cfg(test)]
    pub fn is_active(&self, name: &str) -> bool {
        self.inner
            .lock()
            .expect("skill runtime lock poisoned")
            .activated
            .iter()
            .any(|block| block.name == name)
    }

    /// 渲染本轮要追加注入的文本：**Tier1 目录 + 已激活正文块**（只追加）。
    ///
    /// 目录为空且无激活 → `None`（不注入空内容）。
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

    /// 激活一个 skill：读正文块并**追加**到已激活列表（幂等）。
    ///
    /// 返回渲染好的正文块——工具把它作为结果返回，模型本轮即可看到指令。
    /// 重复激活同一 skill 不重复追加（保持输出稳定，避免无谓地破坏缓存）。
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

    /// 建一个临时 skill 目录，写入 `SKILL.md`，返回 `(SkillMeta, 根目录)`。
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
        // KV-cache 回归：激活第二个 skill 后，新输出必须以上一个输出为**逐字节前缀**
        // ——旧部分（目录 + 第一个块）一字不改，新内容只往后接。
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
            "激活 alpha 后应以上一输出为前缀"
        );
        assert!(
            after_beta.starts_with(&after_alpha),
            "激活 beta 后应以上一输出为前缀（追加式，KV 友好）"
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
        assert_eq!(once, twice, "重复激活不应改变输出（保持缓存稳定）");
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
        assert!(!block.contains("name: alpha"), "frontmatter 应被剥离");
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
