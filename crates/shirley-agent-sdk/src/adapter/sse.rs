//! SSE framing utilities.
//!
//! Only does the **protocol-agnostic** part: split event sections on blank lines and extract the
//! `data:` payload from each section. Event semantics (payload shape, terminal-state detection)
//! are written per protocol — the two protocols' payloads are completely different, and forcing a
//! single generic decoder on them would only be more convoluted.

/// Cut all complete event sections out of the buffer and remove them from it.
///
/// SSE separates events by blank lines; here we split on `\n\n`. An unfinished, incomplete
/// section stays in the buffer until the next chunk completes it. The returned sections **do not
/// include** the trailing separator.
pub fn drain_sections(buffer: &mut String) -> Vec<String> {
    let mut sections = Vec::new();
    while let Some(pos) = buffer.find("\n\n") {
        sections.push(buffer[..pos].to_string());
        buffer.drain(..pos + 2);
    }
    sections
}

/// Extract the `data:` payload from an event section (multiple `data:` lines are concatenated
/// directly, then trimmed at the end).
///
/// Returns `None` when the section has no `data:` line (e.g. only a comment or `event:` line).
/// `event:` / `id:` lines currently have no consumer (the latter is mostly used for resume after
/// disconnect), so they are ignored rather than panicking.
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
