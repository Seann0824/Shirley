//! `SKILL.md` 的容错解析：YAML frontmatter + Markdown 正文。
//!
//! 对齐 Agent Skills 开放标准（`docs/skills.md` 第二节）：一个 skill 是一个目录，
//! 最小形态只含一个 `SKILL.md`。本模块只把 frontmatter 里 Tier1 目录需要的字段
//! 抠出来，其余键（`metadata` / `allowed-tools` / `license` …）**宽容忽略**——
//! 向前兼容，一个坏 skill 不该拖垮整个目录。
//!
//! frontmatter 的切分与 `memory/format.rs` 同源（手写 `key: value`，不引 YAML
//! 依赖）；这里独立实现，避免为复用去动 memory 模块（`docs/skills.md` 十.2）。

use std::path::PathBuf;

/// 单条 `description` 的字符上限（对齐标准，防一条描述自己撑爆目录）。
const MAX_DESCRIPTION_CHARS: usize = 1024;

/// 一个已解析的 skill（P0 只保留 Tier1 目录需要的字段 + 根目录）。
///
/// 正文 / `scripts` / `references` 不在 P0 预加载——激活时按需读（渐进式披露的
/// Tier2/Tier3，见 `docs/skills.md` 2.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMeta {
    /// skill 名（= 父目录名，见 [`parse_skill`]）。
    pub name: String,
    /// 一句话描述：**做什么 + 何时用**（模型据此决定是否激活）。
    pub description: String,
    /// skill 目录（`SKILL.md` 所在目录）。
    pub root: PathBuf,
}

/// 解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 缺 `---` 包裹的 frontmatter。
    MissingFrontmatter,
    /// frontmatter 里没有非空的 `description`。
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

/// 解析一份 `SKILL.md`。
///
/// `dir_name` 是 skill 所在目录名——**skill 名以目录名为准**（标准要求 frontmatter
/// `name` == 目录名；不一致时用目录名，避免模型看到一个和路径对不上的名字）。
/// `root` 是 skill 目录。
pub fn parse_skill(text: &str, dir_name: &str, root: PathBuf) -> Result<SkillMeta, ParseError> {
    let front = split_frontmatter(text).ok_or(ParseError::MissingFrontmatter)?;

    let mut description: Option<String> = None;
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue; // 忽略空行 / 无法识别的行（宽松解析）
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

/// 取出 `---` 包裹的 frontmatter 段（不含首尾分隔线）；缺失或未闭合返回 `None`。
fn split_frontmatter(text: &str) -> Option<&str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // 容忍 BOM
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

/// 取 `SKILL.md` 的**正文**（剥掉 `---` 包裹的 frontmatter 段）。
///
/// 激活（Tier2）只把正文交给模型——frontmatter 是元数据，模型已在 Tier1 目录里
/// 见过 `name` / `description`，正文才是要执行的指令。无 frontmatter 或未闭合时
/// 返回原文（与 [`parse_skill`] 的容错口径一致）。
pub fn body(text: &str) -> &str {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // 容忍 BOM
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
    text // 未闭合：当没有 frontmatter
}

/// 去掉值两端成对的引号（`"..."` / `'...'`）。
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

/// 按字符（非字节）截断到 `max` 个字符。
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
        // frontmatter 名与目录名不一致时以目录名为准。
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
