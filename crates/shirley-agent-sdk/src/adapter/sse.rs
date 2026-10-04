//! SSE 分帧工具。
//!
//! 只做**与协议无关**的部分：按空行切分事件段、从段里取出 `data:` 负载。
//! 事件语义（payload 形状、终态判定）各协议自写——两协议的 payload 完全不同，
//! 硬套一个泛型解码器只会更绕。

/// 从缓冲里切出所有完整的事件段，并从缓冲中移除。
///
/// SSE 以空行分隔事件；这里按 `\n\n` 切分。未收尾的不完整段留在缓冲里，
/// 等下一个 chunk 补齐。返回的段**不含**结尾的分隔符。
pub fn drain_sections(buffer: &mut String) -> Vec<String> {
    let mut sections = Vec::new();
    while let Some(pos) = buffer.find("\n\n") {
        sections.push(buffer[..pos].to_string());
        buffer.drain(..pos + 2);
    }
    sections
}

/// 从事件段里取出 `data:` 负载（多行 `data:` 直接拼接，末尾 trim）。
///
/// 返回 `None` 表示该段没有 `data:` 行（例如只有注释或 `event:` 行）。
/// `event:` / `id:` 行当前没有消费方（后者多用于断线续传），忽略而非 panic。
pub fn data_payload(section: &str) -> Option<String> {
    let mut data = String::new();
    let mut found = false;
    for line in section.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            found = true;
            data.push_str(rest.trim());
        }
    }
    found.then(|| data.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_complete_sections_only() {
        let mut buffer = String::from("data: a\n\ndata: b\n\ndata: partial");
        let sections = drain_sections(&mut buffer);
        assert_eq!(sections.len(), 2);
        assert_eq!(buffer, "data: partial");
    }

    #[test]
    fn extracts_data_payload() {
        let payload = data_payload("event: foo\ndata: {\"a\":1}").unwrap();
        assert_eq!(payload, "{\"a\":1}");
    }

    #[test]
    fn no_data_line_yields_none() {
        assert!(data_payload("event: ping").is_none());
    }
}
