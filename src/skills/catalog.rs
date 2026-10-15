//! skill directory: holds the discovery result and renders it into Tier1 disclosure text.
//!
//! The first layer of progressive disclosure (`docs/skills.md` 2.2): list only each skill's `name` +
//! `description` (+ location) in the request, at minimal cost -- N skills installed still cost only N lines. The body
//! and resources are read on demand at activation time (P1).
//!
//! `SkillCatalog` is scanned once at assembly time and shared across sessions (the directory is read-only and unchanging); it is
//! the data source of [`SkillContextProvider`](super::provider::SkillContextProvider).

use std::path::Path;

use super::discovery::discover;
use super::parse::{body, SkillMeta};

/// Within the directory injection text, a single skill's description is clipped once more to keep the whole directory from being too long.
const MAX_DIRECTORY_CHARS: usize = 8000;

/// Character limit for a single activated skill body block. A skill is external data; this keeps one body block from blowing up the
/// context (`docs/skills.md` section 6).
const MAX_ACTIVE_CHARS: usize = 20_000;

/// Maximum number of entries listed in the resource manifest (keeps a skill directory from having tens of thousands of files).
const MAX_RESOURCES: usize = 200;

/// The set of discovered skills (in discovery order, deduplicated).
#[derive(Debug, Default, Clone)]
pub struct SkillCatalog {
    skills: Vec<SkillMeta>,
}

impl SkillCatalog {
    /// Construct from a list of already-discovered skills.
    pub fn new(skills: Vec<SkillMeta>) -> Self {
        Self { skills }
    }

    /// Discover and construct from "project root + user home".
    ///
    /// When `home` is `None` (user directory unavailable), only scan project level.
    pub fn discover(project_root: &Path, home: Option<&Path>) -> Self {
        Self::new(discover(project_root, home).skills)
    }

    /// Discovered skills (in discovery order).
    #[allow(dead_code)] // P1: for activation / diagnostics
    pub fn skills(&self) -> &[SkillMeta] {
        &self.skills
    }

    /// Whether there are no skills at all.
    #[allow(dead_code)] // P1: for activation / diagnostics
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Get a skill by name (used by P1's activation / resource reading for enum validation).
    #[allow(dead_code)] // P1: used for enum validation in activation / resource reading
    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// All skill names in the directory (for the schema `enum` of `activate_skill` / `read_skill_file`).
    pub fn skill_names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    /// Render a skill's **activated body block** (Tier2): read `SKILL.md`, strip the frontmatter,
    /// attach a resource manifest, and wrap it in `<skill_content>` tags.
    ///
    /// A read failure (missing file / IO error) returns `Err(reason)` -- the caller reports an execution error based on this.
    /// This is the value of "dedicated tool activation" over "file-reading activation" (`docs/skills.md` decision 3).
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

    /// Render the Tier1 disclosure text: the `<available_skills>` block, one line per skill.
    ///
    /// An empty directory returns `None` (do not inject empty content). `<` `&` in the description / name are escaped,
    /// to prevent breaking the tag structure (a skill is external data, see `docs/skills.md` decision 5).
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

/// Header of the directory block: tells the model what this is and how to use it (Tier1 -> Tier2).
///
/// From P1 on, guide the model to load instructions with `activate_skill` (user-level skills live outside the workspace, and `read_file`
/// cannot reach them); `location` is still listed for human troubleshooting. See `docs/skills.md` 5.3 / section 9.
pub const SKILLS_HEADER: &str = include_str!("../../prompts/skills_header.md");

/// List the files under a skill directory's conventional resource subdirectories (`scripts` / `references` / `assets`),
/// with paths relative to the skill root. Stably sorted by subdirectory order -- **stable order** is the prerequisite for KV-cache
/// friendliness (the same skill renders the same manifest every time).
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

/// XML text escaping: keeps `<` / `&` in external data from breaking the tag structure.
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
