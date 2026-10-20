//! Prompt template rendering: substitute `{{placeholders}}` with runtime values.
//!
//! Prompt bodies live in `prompts/*.md` (embedded at compile time via `include_str!`); the code
//! keeps only the wiring of "which template, which placeholders". This way changing the wording
//! doesn't require touching Rust, and dynamic content (working dir, project guide, search
//! results, ...) is injected through placeholders.
//!
//! Design notes:
//! - **Single pass**: substituted values are not scanned again, so user content that happens to
//!   contain `{{...}}` is not mistaken for a placeholder (e.g. a project guide that literally
//!   writes `{{working_dir}}`).
//! - **Unknown placeholders are kept as-is**: when the template references a key the code does
//!   not provide, `{{key}}` is emitted verbatim, so a missing wire-up is visible instead of being
//!   silently swallowed.
//! - Placeholder names allow only `[A-Za-z0-9_]`; a `{{` followed by a non-name char is not a
//!   placeholder.

/// Replace `{{key}}` in `template` with the matching value from `vars`.
///
/// Unmatched placeholders are left as-is; keys in `vars` not referenced by the template are ignored.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < template.len() {
        if bytes[i] == b'{'
            && bytes.get(i + 1) == Some(&b'{')
            && let Some((key, after)) = placeholder_at(template, i)
            && let Some((_, value)) = vars.iter().find(|(name, _)| *name == key)
        {
            out.push_str(value);
            i = after;
            continue;
        }
        // Emit one character as-is (advance on UTF-8 boundaries to avoid splitting a multibyte char).
        let ch = template[i..]
            .chars()
            .next()
            .expect("i is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// If `start` is a valid placeholder `{{name}}`, return `(name, index just after it)`.
fn placeholder_at(template: &str, start: usize) -> Option<(&str, usize)> {
    let rest = &template[start + 2..];
    let end = rest.find("}}")?;
    let name = &rest[..end];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    Some((name, start + 2 + end + 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_known_placeholders() {
        let out = render("a {{x}} b {{y}}", &[("x", "1"), ("y", "2")]);
        assert_eq!(out, "a 1 b 2");
    }

    #[test]
    fn leaves_unknown_placeholders_intact() {
        let out = render("keep {{missing}} here", &[]);
        assert_eq!(out, "keep {{missing}} here");
    }

    #[test]
    fn inserted_values_are_not_rescanned() {
        // Injected values containing `{{...}}` must not be substituted again.
        let out = render("v={{value}}", &[("value", "{{other}}"), ("other", "boom")]);
        assert_eq!(out, "v={{other}}");
    }

    #[test]
    fn non_name_braces_are_left_alone() {
        let out = render("{{}} {{a-b}} {{ spaced }}", &[("", "x")]);
        assert_eq!(out, "{{}} {{a-b}} {{ spaced }}");
    }

    #[test]
    fn handles_multibyte_text() {
        let out = render("café {{dir}} 🚀", &[("dir", "/tmp/naïve")]);
        assert_eq!(out, "café /tmp/naïve 🚀");
    }
}
