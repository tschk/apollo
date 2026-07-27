//! Streaming tool-call parser — state-machine extraction from partial LLM output.
//!
//! Detects `<tool_call>` blocks incrementally as tokens arrive, firing callbacks
//! as soon as `</tool_call>` is seen. Handles malformed tags and unclosed JSON tolerantly.
//!
//! Ported from hermes-rs parser.rs philosophy.

use serde_json::Value;
use tracing::debug;

// ── Types ─────────────────────────────────────────────────────────────────

/// A detected tool call from streaming output
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Events emitted by the streaming parser
#[derive(Debug, Clone)]
pub enum ParserEvent {
    /// Text content (between tags)
    Text(String),
    /// A complete tool call detected
    ToolCall { name: String, args: String },
    /// Parser encountered malformed input
    Error(String),
    /// Stream ended
    End,
}

/// Internal extracted tool call
struct ExtractedToolCall {
    id: String,
    name: String,
    args: String,
}

/// Callback for early tool call detection
type ToolCallback = Box<dyn Fn(ToolCall) + Send + Sync>;

// ── State machine ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Outside,
    InsideOpenTag,
    InsideContent,
    InsideNestedTag,
}

/// State-machine streaming parser for XML tool calls
pub struct StreamingToolCallParser {
    state: State,
    buffer: String,
    tag_buffer: String,
    nested_depth: usize,
    in_tool_call: bool,
    position: usize,
    on_tool_call: Option<ToolCallback>,
    call_counter: usize,
}

impl StreamingToolCallParser {
    pub fn new() -> Self {
        Self {
            state: State::Outside,
            buffer: String::new(),
            tag_buffer: String::new(),
            nested_depth: 0,
            in_tool_call: false,
            position: 0,
            on_tool_call: None,
            call_counter: 0,
        }
    }

    /// Register a callback fired when a complete tool call is parsed
    pub fn on_tool_call<F>(&mut self, callback: F)
    where
        F: Fn(ToolCall) + Send + Sync + 'static,
    {
        self.on_tool_call = Some(Box::new(callback));
    }

    /// Feed a chunk of streaming output. Returns emitted events.
    pub fn feed(&mut self, chunk: &str) -> Vec<ParserEvent> {
        let mut events = Vec::new();

        for ch in chunk.chars() {
            self.position += 1;
            match self.state {
                State::Outside => {
                    if ch == '<' {
                        if !self.buffer.is_empty() {
                            let text = std::mem::take(&mut self.buffer);
                            events.push(ParserEvent::Text(text));
                        }
                        self.state = State::InsideOpenTag;
                        self.tag_buffer.clear();
                    } else {
                        self.buffer.push(ch);
                    }
                }

                State::InsideOpenTag => {
                    if ch == '>' {
                        let tag = self.tag_buffer.trim().to_string();
                        if tag.starts_with("tool_call") {
                            self.in_tool_call = true;
                            self.state = State::InsideContent;
                            if tag.ends_with('/') || tag.starts_with("tool_call/") {
                                self.finish_tool_call(&mut events);
                            }
                        } else if tag.starts_with('/') && tag[1..].trim() == "tool_call" {
                            self.finish_tool_call(&mut events);
                        } else if self.in_tool_call {
                            self.nested_depth += 1;
                            self.buffer.push('<');
                            self.buffer.push_str(&tag);
                            self.buffer.push('>');
                            self.state = State::InsideNestedTag;
                        } else {
                            self.buffer.push('<');
                            self.buffer.push_str(&tag);
                            self.buffer.push('>');
                            self.state = State::Outside;
                        }
                    } else {
                        self.tag_buffer.push(ch);
                    }
                }

                State::InsideContent => {
                    if ch == '<' {
                        self.state = State::InsideOpenTag;
                        self.tag_buffer.clear();
                    } else {
                        self.buffer.push(ch);
                    }
                }

                State::InsideNestedTag => {
                    if ch == '>' {
                        self.buffer.push('>');
                        self.state = State::InsideContent;
                    } else {
                        self.buffer.push(ch);
                    }
                }
            }
        }

        events
    }

    /// Flush remaining content. Call when stream ends.
    pub fn end(&mut self) -> Vec<ParserEvent> {
        let mut events = Vec::new();

        if self.in_tool_call && !self.buffer.is_empty() {
            let content = std::mem::take(&mut self.buffer);
            if let Some(tc) = self.parse_json_content(&content) {
                if let Some(ref cb) = self.on_tool_call {
                    cb(ToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: tc.args.clone(),
                    });
                }
                events.push(ParserEvent::ToolCall {
                    name: tc.name,
                    args: tc.args,
                });
            } else {
                events.push(ParserEvent::Text(format!(
                    "<tool_call>{}</tool_call>",
                    content
                )));
            }
        } else if !self.buffer.is_empty() {
            events.push(ParserEvent::Text(std::mem::take(&mut self.buffer)));
        }

        self.in_tool_call = false;
        self.state = State::Outside;
        self.nested_depth = 0;
        events.push(ParserEvent::End);
        events
    }

    fn finish_tool_call(&mut self, events: &mut Vec<ParserEvent>) {
        self.in_tool_call = false;
        let content = std::mem::take(&mut self.buffer).trim().to_string();

        if let Some(tc) = self.parse_json_content(&content) {
            if let Some(ref cb) = self.on_tool_call {
                cb(ToolCall {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    arguments: tc.args.clone(),
                });
            }
            events.push(ParserEvent::ToolCall {
                name: tc.name,
                args: tc.args,
            });
        } else {
            debug!(
                "unparseable tool_call: {:?}",
                &content[..content.len().min(100)]
            );
            events.push(ParserEvent::Error(format!(
                "Malformed tool_call content: {}",
                &content[..content.len().min(100)]
            )));
        }

        self.state = State::Outside;
        self.nested_depth = 0;
    }

    fn parse_json_content(&mut self, content: &str) -> Option<ExtractedToolCall> {
        // ponytail: try full JSON parse. If fails, return None (caller emits Error).
        if let Ok(val) = serde_json::from_str::<Value>(content) {
            let name = val
                .get("name")
                .or_else(|| val.get("function"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let args = val
                .get("arguments")
                .or_else(|| val.get("input"))
                .and_then(|v| {
                    if v.is_string() {
                        v.as_str().map(|s| s.to_string())
                    } else {
                        Some(v.to_string())
                    }
                })
                .unwrap_or_else(|| "{}".to_string());
            self.call_counter += 1;
            return Some(ExtractedToolCall {
                id: format!("tool_{}", self.call_counter),
                name,
                args,
            });
        }
        None
    }

    pub fn reset(&mut self) {
        self.state = State::Outside;
        self.buffer.clear();
        self.tag_buffer.clear();
        self.nested_depth = 0;
        self.in_tool_call = false;
        self.position = 0;
    }
}

impl Default for StreamingToolCallParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: parse a full (non-streaming) response containing tool_call XML.
pub fn parse_tool_calls(content: &str) -> Vec<ParserEvent> {
    let mut parser = StreamingToolCallParser::new();
    let mut events = parser.feed(content);
    events.extend(parser.end());
    events.retain(|e| !matches!(e, ParserEvent::End));
    events
}

/// Split a completed response into prose and the tool calls it embedded as
/// `<tool_call>` XML.
///
/// Providers without native tool calling emit calls inline in the text. Running
/// the state machine over the finished body recovers them so callers can treat
/// them like native tool calls. Returns the text with the tool_call blocks
/// removed, plus the recovered calls in the order they appeared.
pub fn recover_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    let mut text = String::new();
    let mut calls = Vec::new();

    for event in parse_tool_calls(content) {
        match event {
            ParserEvent::Text(t) => text.push_str(&t),
            ParserEvent::ToolCall { name, args } => calls.push(ToolCall {
                id: format!("xml_{}", calls.len()),
                name,
                arguments: args,
            }),
            ParserEvent::Error(e) => debug!("streaming parser: {}", e),
            ParserEvent::End => {}
        }
    }

    (text, calls)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tool_call() {
        let input = r#"<tool_call>{"name":"read","arguments":{"path":"/tmp/x"}}</tool_call>"#;
        let events = parse_tool_calls(input);
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                if let ParserEvent::ToolCall { name, .. } = e {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(names, vec!["read"]);
    }

    #[test]
    fn test_mixed_text_and_tool_calls() {
        let input = concat!(
            "Let me check. ",
            r#"<tool_call>{"name":"read","arguments":{"path":"x"}}</tool_call>"#,
            " Found it."
        );
        let events = parse_tool_calls(input);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], ParserEvent::Text(t) if t == "Let me check. "));
        assert!(matches!(&events[1], ParserEvent::ToolCall { name, .. } if name == "read"));
        assert!(matches!(&events[2], ParserEvent::Text(t) if t == " Found it."));
    }

    #[test]
    fn test_streaming_chunks() {
        let mut parser = StreamingToolCallParser::new();
        let chunks = vec![
            "Hello. ",
            "<tool_call",
            ">",
            r#"{"name":"search","arguments":{"q":"hello"}}"#,
            "</tool_call>",
            " Done.",
        ];
        let mut all_events = Vec::new();
        for chunk in chunks {
            all_events.extend(parser.feed(chunk));
        }
        all_events.extend(parser.end());
        let texts: Vec<&str> = all_events
            .iter()
            .filter_map(|e| {
                if let ParserEvent::Text(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(texts.contains(&"Hello. "));
        assert!(texts.contains(&" Done."));
    }

    #[test]
    fn test_multiple_tool_calls() {
        let input = concat!(
            r#"<tool_call>{"name":"read","arguments":{"path":"a"}}</tool_call>"#,
            r#"<tool_call>{"name":"read","arguments":{"path":"b"}}</tool_call>"#
        );
        let events = parse_tool_calls(input);
        let tc_count = events
            .iter()
            .filter(|e| matches!(e, ParserEvent::ToolCall { .. }))
            .count();
        assert_eq!(tc_count, 2);
    }

    #[test]
    fn test_malformed_json_fallback() {
        // Unclosed JSON — parser should emit Error, not panic
        let input = r#"<tool_call>{"name": "shell", "arguments": {"cmd": "ls"}</tool_call>"#;
        let events = parse_tool_calls(input);
        let has_error = events.iter().any(|e| matches!(e, ParserEvent::Error(_)));
        // Either an error or a best-effort parse
        assert!(
            has_error
                || events
                    .iter()
                    .any(|e| matches!(e, ParserEvent::ToolCall { .. }))
        );
    }

    #[test]
    fn test_callback_on_complete() {
        let mut parser = StreamingToolCallParser::new();
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c = called.clone();
        parser.on_tool_call(move |_tc| {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        parser.feed(r#"<tool_call>{"name":"test","arguments":{}}</tool_call>"#);
        assert!(called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_plain_text() {
        let mut parser = StreamingToolCallParser::new();
        parser.feed("Just text");
        let events = parser.end();
        assert!(events
            .iter()
            .any(|e| matches!(e, ParserEvent::Text(t) if t == "Just text")));
    }

    #[test]
    fn test_reset() {
        let mut parser = StreamingToolCallParser::new();
        parser.feed("<tool_call>{\"name\":\"x\"");
        parser.reset();
        let events = parser.feed(r#"<tool_call>{"name":"y","arguments":{}}</tool_call>"#);
        assert!(!events.is_empty());
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                if let ParserEvent::ToolCall { name, .. } = e {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(names, vec!["y"]);
    }

    #[test]
    fn recover_splits_prose_from_tool_calls() {
        let input = concat!(
            "Let me check that.",
            r#"<tool_call>{"name":"read","arguments":{"path":"/tmp/x"}}</tool_call>"#,
            "Done."
        );
        let (text, calls) = recover_tool_calls(input);

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].id, "xml_0");
        assert!(text.contains("Let me check that."));
        assert!(text.contains("Done."));
        assert!(!text.contains("tool_call"));
    }

    #[test]
    fn recover_ids_are_unique_per_call() {
        let input = concat!(
            r#"<tool_call>{"name":"a","arguments":{}}</tool_call>"#,
            r#"<tool_call>{"name":"b","arguments":{}}</tool_call>"#
        );
        let (_, calls) = recover_tool_calls(input);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "xml_0");
        assert_eq!(calls[1].id, "xml_1");
    }

    #[test]
    fn recover_returns_no_calls_for_plain_text() {
        let (text, calls) = recover_tool_calls("just a normal answer");
        assert!(calls.is_empty());
        assert_eq!(text, "just a normal answer");
    }
}
