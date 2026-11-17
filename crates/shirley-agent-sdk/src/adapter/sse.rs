//! SSE framing utilities.
//!
//! Only does the **protocol-agnostic** part: split event sections on blank lines and extract the
//! `data:` payload from each section. Event semantics (payload shape, terminal-state detection)
//! are written per protocol — the two protocols' payloads are completely different, and forcing a
//! single generic decoder on them would only be more convoluted.

/// Turns a byte stream into text without breaking characters at chunk boundaries.
///
/// A network chunk can end in the middle of a multi-byte character (a CJK character is three
/// bytes). Decoding each chunk on its own turns both halves into U+FFFD, which is how replies
/// showed `分���` in the middle of Chinese text. The incomplete tail is held back until the next
/// chunk completes it; genuinely invalid bytes are still replaced, as before.
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    /// Decode `chunk`, returning every complete character it finishes.
    pub fn push(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(text) => {
                    out.push_str(text);
                    self.pending.clear();
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // SAFETY-free: `valid_up_to` marks a prefix that is valid UTF-8.
                    out.push_str(&String::from_utf8_lossy(&self.pending[..valid]));
                    match error.error_len() {
                        // An incomplete sequence at the end: wait for the next chunk.
                        None => {
                            self.pending.drain(..valid);
                            return out;
                        }
                        // Bytes that can never become a character: replace and continue.
                        Some(bad) => {
                            out.push(char::REPLACEMENT_CHARACTER);
                            self.pending.drain(..valid + bad);
                        }
                    }
                }
            }
        }
    }
}

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
mod utf8_tests {
    use super::Utf8Decoder;

    #[test]
    fn a_character_split_across_chunks_survives() {
        let bytes = "分项里程".as_bytes();
        let mut decoder = Utf8Decoder::default();
        // Cut inside the first character, and again inside the third.
        let mut text = decoder.push(&bytes[..1]);
        text.push_str(&decoder.push(&bytes[1..7]));
        text.push_str(&decoder.push(&bytes[7..]));
        assert_eq!(text, "分项里程");
    }

    #[test]
    fn invalid_bytes_are_still_replaced() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(b"a\xffb"), "a\u{fffd}b");
    }
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
