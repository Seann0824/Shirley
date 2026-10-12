//! 提示词模板渲染：把 `{{占位符}}` 替换成运行时值。
//!
//! 提示词正文维护在 `prompts/*.md`（经 `include_str!` 编译期内嵌），代码里只保留
//! "用哪个模板、填哪些占位符"的接线。这样改文案不必改 Rust，动态内容（工作目录、
//! 项目指南、检索结果……）通过占位符注入。
//!
//! 设计要点：
//! - **单遍扫描**：替换进去的值不再被二次扫描，避免用户内容里恰好出现 `{{...}}`
//!   时被误当占位符（例如项目指南正文里写了 `{{working_dir}}`）。
//! - **未知占位符原样保留**：模板里写了代码没提供的键时，`{{key}}` 原样输出，
//!   便于肉眼发现漏接线，而不是静默吞掉。
//! - 占位符名只允许 `[A-Za-z0-9_]`，`{{` 后紧跟非名字字符时不视为占位符。

/// 把 `template` 里的 `{{key}}` 替换为 `vars` 中对应的值。
///
/// 未命中的占位符原样保留；`vars` 中未被模板引用的键忽略。
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < template.len() {
        if bytes[i] == b'{' && bytes.get(i + 1) == Some(&b'{') {
            if let Some((key, after)) = placeholder_at(template, i) {
                if let Some((_, value)) = vars.iter().find(|(name, _)| *name == key) {
                    out.push_str(value);
                    i = after;
                    continue;
                }
            }
        }
        // 原样输出一个字符（按 UTF-8 边界推进，避免切开多字节字符）。
        let ch = template[i..].chars().next().expect("i is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 若 `start` 处是合法占位符 `{{name}}`，返回 `(name, 结束后的下标)`。
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
        // 注入的值里含 `{{...}}` 时不应被再次替换。
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
        let out = render("工作区：{{dir}}，结束", &[("dir", "/tmp/项目")]);
        assert_eq!(out, "工作区：/tmp/项目，结束");
    }
}
