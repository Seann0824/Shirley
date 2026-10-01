//! 召回用的分词（`docs/recall.md` 3.2）。
//!
//! 与 `token` 模块的字符分档思路一致：**CJK 按字符（unigram）、ASCII 按词**。
//! 不引入分词器——够用，有噪声，记为技术债（`docs/recall.md` 第八节）。

/// 把一段文本切成检索词元。
///
/// 规则：
/// - ASCII 连续字母数字（含 `_` / `-`）成一个词元，保留大小写信息但比较时统一小写；
/// - 每个 CJK 字符（含汉字 / 假名 / 谚文 / CJK 标点）独立成词元；
/// - 其余字符是分隔符。
///
/// 保留 `_` / `-` 是刻意的：coding agent 的信号词大量出现在标识符
/// （`plan_cut`、`cargo-build`）里，切断它们会丢掉最强的 IDF 信号。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut ascii_run = String::new();

    let flush = |run: &mut String, out: &mut Vec<String>| {
        if !run.is_empty() {
            out.push(run.to_ascii_lowercase());
            run.clear();
        }
    };

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ((ch == '_' || ch == '-') && !ascii_run.is_empty()) {
            ascii_run.push(ch);
        } else if is_cjk(ch) {
            flush(&mut ascii_run, &mut tokens);
            tokens.push(ch.to_string());
        } else {
            flush(&mut ascii_run, &mut tokens);
        }
    }
    flush(&mut ascii_run, &mut tokens);
    tokens
}

/// CJK 判定（表意文字，不含 CJK 标点）。
///
/// Unicode 范围与 `token` 模块共享同一份定义（`token::is_cjk_word_char`），
/// 这里不做记账、只做切词：标点是分隔符不是检索信号，不入词元——
/// 否则"、"",""这类高频标点会以高 TF 噪声污染打分。
fn is_cjk(ch: char) -> bool {
    crate::token::is_cjk_word_char(ch as u32)
}

#[cfg(test)]
mod tests {
    use super::tokenize;

    #[test]
    fn ascii_words() {
        assert_eq!(
            tokenize("cargo build --release"),
            vec!["cargo", "build", "release"]
        );
    }

    #[test]
    fn leading_hyphens_dropped() {
        // 开头的 `-` 是命令行开关前缀（分隔符），信号是 `release` 本身
        assert_eq!(tokenize("--release"), vec!["release"]);
        assert_eq!(tokenize("-f"), vec!["f"]);
    }

    #[test]
    fn cjk_per_char() {
        assert_eq!(tokenize("我叫夏莉"), vec!["我", "叫", "夏", "莉"]);
    }

    #[test]
    fn mixed() {
        assert_eq!(
            tokenize("用 plan_cut 算切点"),
            vec!["用", "plan_cut", "算", "切", "点"]
        );
    }

    #[test]
    fn lowercased() {
        assert_eq!(tokenize("Cargo BUILD"), vec!["cargo", "build"]);
    }

    #[test]
    fn hyphen_kept() {
        assert_eq!(tokenize("cargo-build"), vec!["cargo-build"]);
    }

    #[test]
    fn lone_hyphen_is_separator() {
        assert!(tokenize("- --").is_empty());
    }

    #[test]
    fn empty_and_punct() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("!?,. 。").is_empty());
    }
}
