//! Incremental parser for Claude Code's `--output-format stream-json` (NDJSON).
//! Feed it raw stdout chunks; it buffers partial lines and yields AgentEvents.
//!
//! Reference message shapes (Claude Code headless):
//!   {type:"system", subtype:"init", session_id, model, tools:[...]}
//!   {type:"assistant", message:{content:[{type:"text",text}|{type:"tool_use",name,input}]}, session_id}
//!   {type:"user", message:{content:[{type:"tool_result", is_error}]}, session_id}
//!   {type:"result", subtype, session_id, is_error, result}

use serde_json::Value;

use shepherd_core::agent::AgentEvent;

#[derive(Default)]
pub struct ClaudeStreamParser {
    buffer: String,
    session_id: Option<String>,
}

impl ClaudeStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The agent session id, once seen.
    pub fn agent_session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Feed a stdout chunk; returns any complete events parsed from it.
    pub fn feed(&mut self, chunk: &str) -> Vec<AgentEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(nl) = self.buffer.find('\n') {
            let line = self.buffer[..nl].trim().to_string();
            self.buffer.drain(..=nl);
            if !line.is_empty() {
                self.parse_line(&line, &mut events);
            }
        }
        events
    }

    /// Flush any trailing buffered line (call after the stream ends).
    pub fn flush(&mut self) -> Vec<AgentEvent> {
        let line = std::mem::take(&mut self.buffer).trim().to_string();
        let mut events = Vec::new();
        if !line.is_empty() {
            self.parse_line(&line, &mut events);
        }
        events
    }

    fn parse_line(&mut self, line: &str, out: &mut Vec<AgentEvent>) {
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return; // ignore non JSON noise
        };

        if let Some(sid) = msg.get("session_id").and_then(Value::as_str) {
            if self.session_id.as_deref() != Some(sid) {
                self.session_id = Some(sid.to_string());
                out.push(AgentEvent::Session {
                    agent_session_id: sid.to_string(),
                });
            }
        }

        match msg.get("type").and_then(Value::as_str) {
            Some("assistant") => parse_content(&msg, out, parse_assistant_block),
            Some("user") => parse_content(&msg, out, parse_user_block),
            Some("result") => {
                if msg.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let message = msg
                        .get("result")
                        .and_then(Value::as_str)
                        .unwrap_or("agent reported an error")
                        .to_string();
                    out.push(AgentEvent::Error { message });
                }
            }
            _ => {}
        }
    }
}

fn parse_content(msg: &Value, out: &mut Vec<AgentEvent>, f: fn(&Value) -> Option<AgentEvent>) {
    if let Some(blocks) = msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        for block in blocks {
            if let Some(ev) = f(block) {
                out.push(ev);
            }
        }
    }
}

fn parse_assistant_block(block: &Value) -> Option<AgentEvent> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => Some(AgentEvent::Text {
            text: block.get("text").and_then(Value::as_str)?.to_string(),
        }),
        Some("tool_use") => Some(AgentEvent::ToolUse {
            name: block
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            input: block.get("input").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    }
}

fn parse_user_block(block: &Value) -> Option<AgentEvent> {
    if block.get("type").and_then(Value::as_str) == Some("tool_result") {
        return Some(AgentEvent::ToolResult {
            name: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or("tool")
                .to_string(),
            ok: block.get("is_error").and_then(Value::as_bool) != Some(true),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        format!("{v}\n")
    }

    #[test]
    fn captures_session_id_once() {
        let mut p = ClaudeStreamParser::new();
        let ev = p.feed(&line(serde_json::json!({
            "type": "system", "subtype": "init", "session_id": "abc123", "model": "claude"
        })));
        assert!(matches!(ev.as_slice(), [AgentEvent::Session { agent_session_id }] if agent_session_id == "abc123"));
        assert_eq!(p.agent_session_id(), Some("abc123"));
        let ev2 = p.feed(&line(serde_json::json!({
            "type": "result", "subtype": "success", "session_id": "abc123"
        })));
        assert!(ev2.is_empty());
    }

    #[test]
    fn parses_assistant_text_and_tool_use() {
        let mut p = ClaudeStreamParser::new();
        let ev = p.feed(&line(serde_json::json!({
            "type": "assistant",
            "session_id": "s",
            "message": { "content": [
                { "type": "text", "text": "hello" },
                { "type": "tool_use", "name": "Bash", "input": { "command": "ls" } }
            ]}
        })));
        assert_eq!(ev.len(), 3);
        assert!(matches!(&ev[0], AgentEvent::Session { .. }));
        assert!(matches!(&ev[1], AgentEvent::Text { text } if text == "hello"));
        assert!(matches!(&ev[2], AgentEvent::ToolUse { name, .. } if name == "Bash"));
    }

    #[test]
    fn parses_tool_result_error() {
        let mut p = ClaudeStreamParser::new();
        p.feed(&line(serde_json::json!({ "type": "system", "session_id": "s" })));
        let ev = p.feed(&line(serde_json::json!({
            "type": "user",
            "session_id": "s",
            "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1", "is_error": true }] }
        })));
        assert!(matches!(ev.as_slice(), [AgentEvent::ToolResult { name, ok: false }] if name == "t1"));
    }

    #[test]
    fn handles_line_split_across_chunks() {
        let mut p = ClaudeStreamParser::new();
        let full = serde_json::json!({
            "type": "assistant", "session_id": "s",
            "message": { "content": [{ "type": "text", "text": "hi" }] }
        })
        .to_string();
        let (a, b) = full.split_at(20);
        assert!(p.feed(a).is_empty());
        let ev = p.feed(&format!("{b}\n"));
        assert_eq!(ev.len(), 2);
        assert!(matches!(&ev[1], AgentEvent::Text { text } if text == "hi"));
    }

    #[test]
    fn result_error_emits_error_event() {
        let mut p = ClaudeStreamParser::new();
        let ev = p.feed(&line(serde_json::json!({
            "type": "result", "subtype": "error_max_turns", "session_id": "s",
            "is_error": true, "result": "boom"
        })));
        assert!(matches!(&ev[1], AgentEvent::Error { message } if message == "boom"));
    }

    #[test]
    fn ignores_non_json_noise() {
        let mut p = ClaudeStreamParser::new();
        assert!(p.feed("not json\n").is_empty());
    }
}
