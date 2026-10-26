//! skill directory: holds the discovery result and renders it into Tier1 disclosure text.
//!
//! The first layer of progressive disclosure (`docs/skills.md` 2.2): list only each skill's `name` +
//! `description` (+ location) in the request, at minimal cost -- N skills installed still cost only N lines. The body
//! and resources are read on demand at activation time (P1).
//!
//! `SkillCatalog` is shared across sessions and is the data source of
//! [`SkillContextProvider`](super::provider::SkillContextProvider).
//!
//! **The directory is re-scanned in place** ([`SkillCatalog::refresh`]) whenever the model activates a
//! skill that is not in the last scan. Without this, a skill the agent installs mid-session (write
//! `<root>/<name>/SKILL.md`) was invisible until the process restarted -- the remote/cloud case made
//! this fatal, because there is no easy restart there. Re-discovery only happens when the catalog
//! actually changes, so the Tier1 text stays byte-identical (KV prefix cache) in the steady state.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use super::discovery;
use super::parse::{SkillMeta, body};

/// Within the directory injection text, a single skill's description is clipped once more to keep the whole directory from being too long.
const MAX_DIRECTORY_CHARS: usize = 8000;

/// Character limit for a single activated skill body block. A skill is external data; this keeps one body block from blowing up the
/// context (`docs/skills.md` section 6).
const MAX_ACTIVE_CHARS: usize = 20_000;

/// Maximum number of entries listed in the resource manifest (keeps a skill directory from having tens of thousands of files).
const MAX_RESOURCES: usize = 200;

/// The set of discovered skills (in discovery order, deduplicated).
#[derive(Debug)]
pub struct SkillCatalog {
    /// Discovery roots, in priority order (project level before user level; first scanned wins).
    /// Empty for a catalog built directly from [`SkillCatalog::new`] (tests / diagnostics) -- such a
    /// catalog never re-scans and behaves exactly as the old read-only snapshot.
    roots: Vec<PathBuf>,
    skills: Mutex<Vec<SkillMeta>>,
}

impl Default for SkillCatalog {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl Clone for SkillCatalog {
    fn clone(&self) -> Self {
        Self {
            roots: self.roots.clone(),
            skills: Mutex::new(self.lock().clone()),
        }
    }
}

impl SkillCatalog {
    /// Construct from a list of already-discovered skills (no discovery roots: never re-scans).
    pub fn new(skills: Vec<SkillMeta>) -> Self {
        Self {
            roots: Vec::new(),
            skills: Mutex::new(skills),
        }
    }

    /// Discover and construct from "project root + user home".
    ///
    /// When `home` is `None` (user directory unavailable), only scan project level.
    pub fn discover(project_root: &Path, home: Option<&Path>) -> Self {
        let roots = discovery::skill_roots(project_root, home);
        Self {
            skills: Mutex::new(discovery::scan_roots(&roots).skills),
            roots,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<SkillMeta>> {
        self.skills.lock().expect("skill catalog lock poisoned")
    }

    /// The conventional directory a new skill should be installed into by default, and the
    /// project-level root that overrides user-level skills of the same name.
    ///
    /// **Default is the user-level root (`~/.agents/skills`), not the project one.** A skill is a
    /// reusable capability, and the project root is not always durable: on a remote/cloud host the
    /// workspace can be a per-session checkout that disappears with the session, so a skill written
    /// there is silently lost. The user home is stable across sessions in both the local and the
    /// cloud case, which makes it the only location that always works. The project root keeps its
    /// meaning as the **override** layer (same name wins by being scanned first).
    ///
    /// Returns `(default_install_root, project_override_root)`; both `None` for a catalog built from
    /// [`SkillCatalog::new`] (no discovery roots: nothing can be installed into).
    pub fn install_roots(&self) -> (Option<PathBuf>, Option<PathBuf>) {
        let project = self.roots.first().cloned();
        let user = user_root(&self.roots);
        match user {
            // Both levels present: user level is the default landing spot.
            Some(user) => (Some(user), project),
            // Project-only (no user home): the project root is all we have.
            None => (project.clone(), None),
        }
    }

    /// The single directory a new skill should be installed into (see [`Self::install_roots`]).
    pub fn install_root(&self) -> Option<PathBuf> {
        self.install_roots().0
    }

    /// Re-scan the discovery roots if the directory has changed since the last scan.
    ///
    /// Returns `true` when the catalog was replaced. If the scan produces the same set of skills
    /// (same names + roots + descriptions), the old vector is kept untouched so the rendered Tier1
    /// text stays byte-identical (KV prefix cache).
    pub fn refresh(&self) -> bool {
        if self.roots.is_empty() {
            return false;
        }
        let found = discovery::scan_roots(&self.roots).skills;
        let mut guard = self.lock();
        if fingerprint(&guard) == fingerprint(&found) {
            return false;
        }
        *guard = found;
        true
    }

    /// Copy of the discovered skills, in discovery order.
    pub fn skills(&self) -> Vec<SkillMeta> {
        self.lock().clone()
    }

    /// Get a skill by name (used by activation / resource reading).
    pub fn get(&self, name: &str) -> Option<SkillMeta> {
        self.lock().iter().find(|skill| skill.name == name).cloned()
    }

    /// All skill names in the directory (for the schema `enum` of `activate_skill` / `read_skill_file`).
    pub fn skill_names(&self) -> Vec<String> {
        self.lock().iter().map(|skill| skill.name.clone()).collect()
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
                let _ = writeln!(out, "  <file>{}</file>", escape(resource));
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
        let skills = self.lock().clone();
        let (install_root, override_root) = self.install_roots();
        let install_root = install_root.map(|root| root.to_string_lossy().to_string());
        let override_root = override_root.map(|root| root.to_string_lossy().to_string());

        let mut body = String::new();
        body.push_str(SKILLS_HEADER);
        body.push('\n');
        for skill in &skills {
            let location = skill.root.join("SKILL.md").to_string_lossy().to_string();
            let _ = writeln!(
                body,
                "  <skill name=\"{}\" location=\"{}\">{}</skill>",
                escape(&skill.name),
                escape(&location),
                escape(&skill.description),
            );
        }
        if skills.is_empty() {
            body.push_str("  (no skills installed)\n");
        }
        // No roots at all (built via `new`: tests / fixtures) -> nothing to install into either,
        // so keep the old "nothing to inject" behaviour instead of pointing at an imaginary path.
        if let Some(root) = install_root {
            let guide = INSTALL_GUIDE
                .replace("{{install_root}}", &escape(&root))
                .replace(
                    "{{override_root}}",
                    &override_root.as_deref().map(escape).unwrap_or_default(),
                );
            body.push_str(&guide);
        } else if skills.is_empty() {
            return None;
        }

        let mut rendered = format!("<available_skills>\n{body}</available_skills>");
        if rendered.chars().count() > MAX_DIRECTORY_CHARS {
            rendered = rendered.chars().take(MAX_DIRECTORY_CHARS).collect();
            rendered.push_str("\n... (skill directory truncated)");
        }
        Some(rendered)
    }
}

/// Install guidance appended to the directory (also when **no** skill is installed yet).
///
/// Without it the model "installs" a skill by writing `SKILL.md` into some arbitrary directory in
/// the project (or into a scratch folder on the remote host), then wonders why it is not loaded:
/// only the conventional roots are scanned. `{{install_root}}` is substituted with the
/// top-priority root ([`SkillCatalog::install_root`]).
pub const INSTALL_GUIDE: &str = include_str!("../../prompts/skills_install.md");

/// The user-level root among `roots` (`~/.agents/skills`, else `~/.claude/skills`).
///
/// `skill_roots` emits four roots in a fixed order: two project-level ones, then two user-level
/// ones -- and only the *last two* exist when `home` is known, so the user level is identified by
/// **position**, not by path shape (with `home == None` all roots are project-level).
fn user_root(roots: &[PathBuf]) -> Option<PathBuf> {
    if roots.len() <= discovery::PROJECT_SKILL_DIRS.len() {
        return None;
    }
    roots.get(discovery::PROJECT_SKILL_DIRS.len()).cloned()
}

/// Content fingerprint of a skill list, used to decide whether a re-scan actually changed anything
/// (an unchanged directory must keep the injected text byte-identical for the prefix cache).
fn fingerprint(skills: &[SkillMeta]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for skill in skills {
        skill.name.hash(&mut hasher);
        skill.description.hash(&mut hasher);
        skill.root.hash(&mut hasher);
    }
    hasher.finish()
}

/// Header of the directory block: tells the model what this is and how to use it (Tier1 -> Tier2).
///
/// From P1 on, guide the model to load instructions with `activate_skill` (user-level skills live outside the workspace, and `read`
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
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .map(|name| format!("{dir}/{name}"))
            })
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

    fn meta(name: &str, description: &str) -> SkillMeta {
        SkillMeta {
            name: name.into(),
            description: description.into(),
            root: PathBuf::from(format!("/s/{name}")),
        }
    }

    #[test]
    fn empty_catalog_still_renders_install_guide() {
        // With no install root (built from `new`) the guide can't name a path, so nothing renders.
        assert!(SkillCatalog::new(vec![]).render_directory().is_none());

        // A discovered catalog always knows where to install -> the guide is injected even when empty.
        let root = temp_dir("empty");
        let catalog = SkillCatalog::discover(&root, None);
        let text = catalog.render_directory().unwrap();
        assert!(text.contains("no skills installed"), "{text}");
        assert!(
            text.contains(&root.join(".agents/skills").to_string_lossy()[..]),
            "{text}"
        );
    }

    #[test]
    fn refresh_discovers_a_skill_installed_later() {
        let root = temp_dir("refresh");
        let catalog = SkillCatalog::discover(&root, None);
        assert!(catalog.get("late").is_none());

        let dir = root.join(".agents/skills/late");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: late\ndescription: appeared later\n---\nbody\n",
        )
        .unwrap();

        assert!(catalog.refresh());
        assert_eq!(catalog.get("late").unwrap().description, "appeared later");
        // A second refresh with an unchanged directory must not churn (prefix cache stability).
        assert!(!catalog.refresh());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn refresh_without_roots_is_a_noop() {
        assert!(!SkillCatalog::new(vec![meta("alpha", "does alpha")]).refresh());
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shirley_skill_catalog_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn renders_one_line_per_skill() {
        let catalog =
            SkillCatalog::new(vec![meta("alpha", "does alpha"), meta("beta", "does beta")]);
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
    fn install_defaults_to_the_user_level_root() {
        let project = temp_dir("install_project");
        let home = temp_dir("install_home");
        let catalog = SkillCatalog::discover(&project, Some(&home));
        let (default, override_root) = catalog.install_roots();
        // A skill is a reusable capability and the workspace may be a per-session cloud checkout,
        // so the durable user-level root is the default landing spot.
        assert_eq!(default, Some(home.join(".agents/skills")));
        assert_eq!(override_root, Some(project.join(".agents/skills")));
        assert_eq!(catalog.install_root(), default);
    }

    #[test]
    fn install_falls_back_to_the_project_root_without_a_home() {
        let project = temp_dir("install_no_home");
        let catalog = SkillCatalog::discover(&project, None);
        let (default, override_root) = catalog.install_roots();
        assert_eq!(default, Some(project.join(".agents/skills")));
        assert_eq!(override_root, None);
    }

    #[test]
    fn install_roots_are_empty_without_discovery_roots() {
        let catalog = SkillCatalog::new(vec![meta("alpha", "does alpha")]);
        assert_eq!(catalog.install_roots(), (None, None));
    }

    #[test]
    fn directory_points_at_the_user_level_install_root() {
        let project = temp_dir("guide_project");
        let home = temp_dir("guide_home");
        let catalog = SkillCatalog::discover(&project, Some(&home));
        let text = catalog.render_directory().unwrap();
        let user_root = home.join(".agents/skills");
        assert!(
            text.contains(&format!(
                "default (write here): {}",
                user_root.to_string_lossy()
            )),
            "{text}"
        );
    }

    #[test]
    fn get_finds_by_name() {
        let catalog = SkillCatalog::new(vec![meta("alpha", "does alpha")]);
        assert!(catalog.get("alpha").is_some());
        assert!(catalog.get("nope").is_none());
    }
}
