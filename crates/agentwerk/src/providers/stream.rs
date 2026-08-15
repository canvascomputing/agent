//! Reads Server-Sent Events from an LLM provider, reassembling whole `data:`
//! events however the bytes arrive.

use serde_json::Value;

use super::error::{ProviderError, ProviderResult};

/// Read an SSE response to its end, handing every `data:` JSON payload to
/// `ingest`.
pub(crate) async fn for_each_data(
    mut response: reqwest::Response,
    mut ingest: impl FnMut(&Value),
) -> ProviderResult<()> {
    let mut parser = StreamParser::new();
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| ProviderError::StreamInterrupted {
                message: error.to_string(),
            })?
    {
        for payload in parser.push(&chunk) {
            ingest(&payload);
        }
    }
    Ok(())
}

/// Line-buffered SSE parser. Feed raw bytes via `push()`, get `data:` payloads
/// back. Holds bytes rather than text because a chunk may end partway through
/// a multi-byte character.
struct StreamParser {
    buffer: Vec<u8>,
}

impl StreamParser {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feed a chunk of bytes and return the payload of every complete `data:`
    /// line found.
    fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buffer.extend_from_slice(chunk);
        let Some(last_newline) = self.buffer.iter().rposition(|&byte| byte == b'\n') else {
            return Vec::new();
        };
        let payloads = self.buffer[..last_newline]
            .split(|&byte| byte == b'\n')
            .filter_map(data_payload)
            .collect();
        self.buffer.drain(..=last_newline);
        payloads
    }
}

/// The JSON payload of one `data:` line; `None` for other lines and malformed
/// JSON. The space after the colon is optional, and an endpoint that omits it
/// would otherwise stream a reply that parses to nothing at all. The `[DONE]`
/// sentinel some endpoints close with says only that the stream is over, which
/// the response ending says too.
fn data_payload(line: &[u8]) -> Option<Value> {
    let line = String::from_utf8_lossy(line);
    let data = line.trim().strip_prefix("data:")?.trim_start();
    if data == "[DONE]" {
        return None;
    }
    serde_json::from_str(data).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_data_line_yields_its_json_payload() {
        let mut parser = StreamParser::new();
        let payloads = parser.push(b"data: {\"type\":\"ping\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn a_data_line_without_a_space_yields_its_json_payload() {
        let mut parser = StreamParser::new();
        let payloads = parser.push(b"data:{\"type\":\"ping\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn the_done_sentinel_yields_no_payload() {
        let mut parser = StreamParser::new();
        assert!(parser.push(b"data: [DONE]\n\n").is_empty());
    }

    #[test]
    fn non_data_lines_are_ignored() {
        let mut parser = StreamParser::new();
        assert!(parser
            .push(b"event: message_start\n: comment\n\n")
            .is_empty());
    }

    #[test]
    fn a_line_split_across_chunks_is_reassembled() {
        let mut parser = StreamParser::new();

        assert!(parser.push(b"data: {\"type\":\"pi").is_empty());

        let payloads = parser.push(b"ng\"}\n\n");
        assert_eq!(payloads, [json!({"type": "ping"})]);
    }

    #[test]
    fn a_character_split_across_chunks_is_reassembled() {
        let mut parser = StreamParser::new();
        let line = "data: {\"text\":\"é\"}\n".as_bytes();
        let mid_character = line.len() - 4;

        assert!(parser.push(&line[..mid_character]).is_empty());

        let payloads = parser.push(&line[mid_character..]);
        assert_eq!(payloads, [json!({"text": "é"})]);
    }

    #[test]
    fn one_chunk_can_hold_many_payloads() {
        let mut parser = StreamParser::new();
        let chunk = b"data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: [DONE]\n\n";
        assert_eq!(parser.push(chunk), [json!({"a": 1}), json!({"a": 2})]);
    }

    #[test]
    fn malformed_json_is_skipped() {
        let mut parser = StreamParser::new();
        let payloads = parser.push(b"data: not-json\ndata: {\"ok\":true}\n\n");
        assert_eq!(payloads, [json!({"ok": true})]);
    }
}
