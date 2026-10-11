//! skill 目录：持有发现结果，并把它渲染成 Tier1 披露文本。
//!
//! 渐进式披露的第一层（`docs/skills.md` 2.2）：只把每个 skill 的 `name` +
//! `description`（+ 位置）列进请求，代价极小——装 N 个 skill 也只占 N 行。正文
//! 与资源留到激活时按需读（P1）。
//!
//! `SkillCatalog` 在装配时扫一次、会话间共享（目录只读、不变）；它是
//! [`SkillContextProvider`](super::provider::SkillContextProvider) 的数据源。

use std::path::Path;

use super::discovery::discover;
use super::parse::{body, SkillMeta};

/// 目录注入文本里，单个 skill 的 description 再截一次，防止目录整体过长。
const MAX_DIRECTORY_CHARS: usize = 8000;

/// 单个已激活 skill 正文块的字符上限。skill 是外部数据，防一块正文撑爆上下文
/// （`docs/skills.md` 六）。
const MAX_ACTIVE_CHARS: usize = 20_000;

/// 资源清单里最多列出的条目数（防一个 skill 目录里有成千上万个文件）。
const MAX_RESOURCES: usize = 200;

/// 已发现的 skill 集合（按发现顺序，去重后）。
#[derive(Debug, Default, Clone)]
pub struct SkillCatalog {
    skills: Vec<SkillMeta>,
}

impl SkillCatalog {
    /// 用一份已发现的 skill 列表构造。
    pub fn new(skills: Vec<SkillMeta>) -> Self {
        Self { skills }
    }

    /// 从「项目根 + 用户 home」发现并构造。
    ///
    /// `home` 为 `None`（拿不到用户目录）时只扫项目级。
    pub fn discover(project_root: &Path, home: Option<&Path>) -> Self {
        Self::new(discover(project_root, home).skills)
    }

    /// 已发现的 skill（按发现顺序）。
    #[allow(dead_code)] // P1：激活 / 诊断用
    pub fn skills(&self) -> &[SkillMeta] {
        &self.skills
    }

    /// 是否没有任何 skill。
    #[allow(dead_code)] // P1：激活 / 诊断用
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// 按名字取 skill（供 P1 的激活 / 资源读取做枚举校验）。
    #[allow(dead_code)] // P1：激活 / 资源读取做枚举校验用
    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// 目录里的全部 skill 名（供 `activate_skill` / `read_skill_file` 的 schema `enum`）。
    pub fn skill_names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    /// 渲染一个 skill 的**已激活正文块**（Tier2）：读 `SKILL.md`、剥 frontmatter、
    /// 附带资源清单，包进 `<skill_content>` 标签。
    ///
    /// 读失败（文件缺失 / IO 错）返回 `Err(原因)`——调用方据此报执行错误。
    /// 这是「专用工具激活」相对「文件读取激活」的价值所在（`docs/skills.md` 决策 3）。
    pub fn render_active(&self, name: &str) -> Result<String, String> {
        let skill = self
            .get(name)
            .ok_or_else(|| format!("unknown skill `{name}`"))?;
        let path = skill.root.join("SKILL.md");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let instruction = body(&text).trim();

        let mut out = format!(
            "<skill_content name=\"{}\">\n{}\n",
            escape(&skill.name),
            escape(instruction),
        );

        let resources = list_resources(&skill.root);
        if !resources.is_empty() {
            out.push_str("\n<resources>\n");
            for resource in &resources {
                out.push_str(&format!("  <file>{}</file>\n", escape(resource)));
            }
            out.push_str("</resources>\n");
            out.push_str(
                "(Read a resource on demand with `read_skill_file`; do not load them all.)\n",
            );
        }

        out.push_str("</skill_content>");

        if out.chars().count() > MAX_ACTIVE_CHARS {
            out = out.chars().take(MAX_ACTIVE_CHARS).collect();
            out.push_str("\n... (skill content truncated)");
        }
        Ok(out)
    }

    /// 渲染 Tier1 披露文本：`<available_skills>` 块，每 skill 一行。
    ///
    /// 空目录返回 `None`（不注入空内容）。description / 名字里的 `<` `&` 会被转义，
    /// 防止破坏标签结构（skill 是外部数据，见 `docs/skills.md` 决策 5）。
    pub fn render_directory(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }

        let mut body = String::new();
        body.push_str(SKILLS_HEADER);
        body.push('\n');
        for skill in &self.skills {
            let location = skill
                .root
                .join("SKILL.md")
                .to_string_lossy()
                .to_string();
            body.push_str(&format!(
                "  <skill name=\"{}\" location=\"{}\">{}</skill>\n",
                escape(&skill.name),
                escape(&location),
                escape(&skill.description),
            ));
        }
        let mut rendered = format!("<available_skills>\n{body}</available_skills>");
        if rendered.chars().count() > MAX_DIRECTORY_CHARS {
            rendered = rendered.chars().take(MAX_DIRECTORY_CHARS).collect();
            rendered.push_str("\n... (skill directory truncated)");
        }
        Some(rendered)
    }
}

/// 目录块的开头说明：告诉模型这是什么、怎么用（Tier1 → Tier2）。
///
/// P1 起引导模型用 `activate_skill` 加载指令（用户级 skill 在工作区外，`read_file`
/// 够不到）；`location` 仍列出，供人排查。见 `docs/skills.md` 5.3 / 九。
pub const SKILLS_HEADER: &str = "Available skills are listed below. Each is a directory with a \
SKILL.md holding detailed instructions. When a task matches a skill's description, call \
`activate_skill` with that skill's name to load its instructions before proceeding. Load a \
skill's scripts / references / assets on demand with `read_skill_file`.";

/// 列出 skill 目录下约定资源子目录（`scripts` / `references` / `assets`）里的文件，
/// 路径相对 skill 根。按子目录顺序稳定排序——**顺序稳定**是 KV 缓存友好的前提
/// （同一 skill 每次渲染出同样的清单）。
fn list_resources(root: &Path) -> Vec<String> {
    const DIRS: [&str; 3] = ["scripts", "references", "assets"];
    let mut resources = Vec::new();
    for dir in DIRS {
        let dir_path = root.join(dir);
        let Ok(entries) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().map(|kind| kind.is_file()).unwrap_or(false))
            .filter_map(|entry| entry.file_name().to_str().map(|name| format!("{dir}/{name}")))
            .collect();
        names.sort();
        resources.extend(names);
        if resources.len() >= MAX_RESOURCES {
            resources.truncate(MAX_RESOURCES);
            break;
        }
    }
    resources
}

/// XML 文本转义：防止外部数据里的 `<` / `&` 破坏标签结构。
fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meta(name: &str, description: &str) -> SkillMeta {
        SkillMeta {
            name: name.into(),
            description: description.into(),
            root: PathBuf::from(format!("/s/{name}")),
        }
    }

    #[test]
    fn empty_catalog_renders_none() {
        assert!(SkillCatalog::new(vec![]).render_directory().is_none());
    }

    #[test]
    fn renders_one_line_per_skill() {
        let catalog = SkillCatalog::new(vec![
            meta("alpha", "does alpha"),
            meta("beta", "does beta"),
        ]);
        let text = catalog.render_directory().unwrap();
        assert!(text.starts_with("<available_skills>"));
        assert!(text.trim_end().ends_with("</available_skills>"));
        assert!(text.contains("name=\"alpha\""));
        assert!(text.contains("does alpha"));
        assert!(text.contains("name=\"beta\""));
        assert!(text.contains("location=\"/s/alpha/SKILL.md\""));
    }

    #[test]
    fn escapes_xml_special_chars() {
        let catalog = SkillCatalog::new(vec![meta("x", "a < b & c > d \"e\"")]);
        let text = catalog.render_directory().unwrap();
        assert!(text.contains("a &lt; b &amp; c &gt; d &quot;e&quot;"));
        assert!(!text.contains("a < b"));
    }

    #[test]
    fn get_finds_by_name() {
        let catalog = SkillCatalog::new(vec![meta("alpha", "does alpha")]);
        assert!(catalog.get("alpha").is_some());
        assert!(catalog.get("nope").is_none());
    }
}
