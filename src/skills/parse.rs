//! Lenient parsing of `SKILL.md`: YAML frontmatter + Markdown body.
//!
//! Aligned with the Agent Skills open standard (`docs/skills.md` section 2): a skill is a directory,
//! and the minimal form contains just one `SKILL.md`. This module only extracts the fields the
//! Tier1 directory needs from the frontmatter; the other keys (`metadata` / `allowed-tools` / `license` ...) are
//! **leniently ignored** -- forward compatible, so one bad skill cannot bring down the whole directory.
//!
//! Frontmatter splitting shares its origin with `memory/format.rs` (hand-written `key: value`, no YAML
//! dependency); it is implemented independently here to avoid touching the memory module for reuse (`docs/skills.md` 10.2).

use std::path::PathBuf;

/// Character limit for a single `description` (aligned with the standard; keeps one description from blowing up the directory).
const MAX_DESCRIPTION_CHARS: usize = 1024;

/// A parsed skill (P0 keeps only the fields the Tier1 directory needs + the root directory).
///
/// The body / `scripts` / `references` are not preloaded in P0 -- they are read on demand at activation time (progressive
/// disclosure's Tier2/Tier3, see `docs/skills.md` 2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    /// skill name (= parent directory name, see [`parse_skill`]).
    pub name: String,
    /// One-line description: **what it does + when to use it** (the model uses this to decide whether to activate).
    pub description: String,
    /// skill directory (the directory containing `SKILL.md`).
    pub root: PathBuf,
}

/// Reason parsing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Missing `---`-wrapped frontmatter.
    MissingFrontmatter,
    /// frontmatter has no non-empty `description`.
    MissingDescription,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFrontmatter => write!(f, "missing `---` frontmatter"),
            Self::MissingDescription => write!(f, "missing `description` in frontmatter"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse one `SKILL.md`.
///
/// `dir_name` is the name of the directory the skill is in -- **the skill name follows the directory name** (the standard requires
/// the frontmatter `name` == the directory name; when they differ, use the directory name, so the model never sees a name that does not match the path).
/// `root` is the skill directory.
pub fn parse_skill(text: &str, dir_name: &str, root: PathBuf) -> Result<SkillMeta, ParseError> {
    let front = split_frontmatter(text).ok_or(ParseError::MissingFrontmatter)?;

    let mut description: Option<String> = None;
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue; // ignore blank / unrecognized lines (lenient parsing)
        };
        if key.trim() == "description" {
            let value = unquote(value.trim());
            if !value.is_empty() {
                description = Some(value);
            }
        }
    }

    let description = description.ok_or(ParseError::MissingDescription)?;
    let description = clip_chars(description, MAX_DESCRIPTION_CHARS);

    Ok(SkillMeta {
        name: dir_name.to_string(),
        description,
        root,
    })
}

/// Extract the `---`-wrapped frontmatter block (without the leading/trailing delimiters); returns `None` if missing or unterminated.
fn split_frontmatter(text: &str) -> Option<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate BOM
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "---" {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    None
}

/// Get the **body** of `SKILL.md` (strip the `---`-wrapped frontmatter block).
///
/// Activation (Tier2) hands only the body to the model -- the frontmatter is metadata, and the model has already
/// seen `name` / `description` in the Tier1 directory; the body is the actual instruction to execute. When there is no
/// frontmatter or it is unterminated, returns the original text (consistent with [`parse_skill`]'s lenient contract).
pub fn body(text: &str) -> &str {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate BOM
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return text;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == "---" {
            let after = offset + line.len();
            return rest[after..].trim_start_matches(['\n', '\r']);
        }
        offset += line.len();
    }
    text // unterminated: treat as having no frontmatter
}

/// Remove paired quotes from both ends of a value (`"..."` / `'...'`).
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// Truncate to `max` characters (by char, not byte).
fn clip_chars(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_from_dir_and_description_from_frontmatter() {
        let text = "---\nname: pdf-processing\ndescription: Extract text from PDFs. Use when handling PDFs.\nlicense: MIT\n---\n\n# PDF\nbody\n";
        let meta = parse_skill(text, "pdf-processing", PathBuf::from("/s/pdf-processing")).unwrap();
        assert_eq!(meta.name, "pdf-processing");
        assert_eq!(
            meta.description,
            "Extract text from PDFs. Use when handling PDFs."
        );
    }

    #[test]
    fn name_follows_directory_not_frontmatter() {
        // when the frontmatter name and the directory name differ, the directory name wins.
        let text = "---\nname: other\ndescription: does a thing\n---\nbody";
        let meta = parse_skill(text, "real-dir", PathBuf::from("/s/real-dir")).unwrap();
        assert_eq!(meta.name, "real-dir");
    }

    #[test]
    fn missing_description_is_error() {
        let text = "---\nname: x\nlicense: MIT\n---\nbody";
        assert_eq!(
            parse_skill(text, "x", PathBuf::from("/s/x")),
            Err(ParseError::MissingDescription)
        );
    }

    #[test]
    fn missing_frontmatter_is_error() {
        let text = "# no frontmatter\nbody";
        assert_eq!(
            parse_skill(text, "x", PathBuf::from("/s/x")),
            Err(ParseError::MissingFrontmatter)
        );
    }

    #[test]
    fn unquotes_quoted_description() {
        let text = "---\ndescription: \"quoted: with colon\"\n---\n";
        let meta = parse_skill(text, "q", PathBuf::from("/s/q")).unwrap();
        assert_eq!(meta.description, "quoted: with colon");
    }

    #[test]
    fn ignores_unknown_and_multiline_values() {
        let text = "---\nname: x\ndescription: works\nmetadata:\n  key: value\nallowed-tools: bash read_file\n---\nbody";
        let meta = parse_skill(text, "x", PathBuf::from("/s/x")).unwrap();
        assert_eq!(meta.description, "works");
    }

    #[test]
    fn handles_crlf_and_bom() {
        let text = "\u{feff}---\r\ndescription: crlf skill\r\n---\r\nbody";
        let meta = parse_skill(text, "c", PathBuf::from("/s/c")).unwrap();
        assert_eq!(meta.description, "crlf skill");
    }

    #[test]
    fn body_strips_frontmatter() {
        let text = "---\nname: x\ndescription: works\n---\n\n# Title\nbody line\n";
        assert_eq!(body(text), "# Title\nbody line\n");
    }

    #[test]
    fn body_without_frontmatter_is_verbatim() {
        let text = "# no frontmatter\nbody";
        assert_eq!(body(text), text);
    }

    #[test]
    fn body_tolerates_bom_and_crlf() {
        let text = "\u{feff}---\r\ndescription: crlf\r\n---\r\nbody";
        assert_eq!(body(text), "body");
    }

    #[test]
    fn clips_overlong_description() {
        let long = "x".repeat(MAX_DESCRIPTION_CHARS + 50);
        let text = format!("---\ndescription: {long}\n---\n");
        let meta = parse_skill(&text, "long", PathBuf::from("/s/long")).unwrap();
        assert_eq!(meta.description.chars().count(), MAX_DESCRIPTION_CHARS);
    }
}
