//! Transport to a running apollo agent.
//!
//! Prefers the WebSocket stream at `/v1/chat/stream` so tool activity shows up
//! live, falls back to the blocking `/v1/chat` POST, and finally to shelling
//! out to `apollo ask` when no agent HTTP server is listening.

use std::sync::mpsc::Sender;

/// One update from the agent while a turn is in flight.
///
/// Mirrors `apollo::agent::stream::AgentStreamEvent`, kept as a separate type
/// so the UI does not depend on the agent crate.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Status(String),
    ToolStart { name: String, hint: String },
    ToolEnd { name: String, ok: bool, secs: u64 },
    Delta(String),
    Done(String),
    Error(String),
}

pub fn http_port() -> String {
    std::env::var("APOLLO_HTTP_PORT").unwrap_or_else(|_| "31338".into())
}

/// True when an apollo agent is listening locally.
pub fn agent_online() -> bool {
    let url = format!("http://127.0.0.1:{}/health", http_port());
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(600))
        .build()
        .ok()
        .and_then(|c| c.get(&url).send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Run one turn, forwarding every event to `tx` as it arrives.
///
/// Always terminates by sending exactly one `Done` or `Error`.
pub fn run_turn(prompt: &str, chat_id: &str, tx: &Sender<AgentEvent>) {
    match stream_over_ws(prompt, chat_id, tx) {
        Ok(()) => {}
        Err(ws_err) => {
            // No stream — fall back to a blocking turn, then to the CLI.
            match ask_via_http(prompt, chat_id) {
                Ok(text) => {
                    let _ = tx.send(AgentEvent::Done(text));
                }
                Err(http_err) => match ask_via_cli(prompt) {
                    Ok(text) => {
                        let _ = tx.send(AgentEvent::Done(text));
                    }
                    Err(cli_err) => {
                        let _ = tx.send(AgentEvent::Error(format!(
                            "no agent reachable.\n  stream: {ws_err}\n  http: {http_err}\n  cli: {cli_err}"
                        )));
                    }
                },
            }
        }
    }
}

/// Stream a turn over the agent's WebSocket endpoint.
///
/// Returns `Err` only when the socket could not be established or the turn
/// ended without a terminal event, so the caller can fall back. Once a
/// terminal event has been forwarded this returns `Ok`.
fn stream_over_ws(prompt: &str, chat_id: &str, tx: &Sender<AgentEvent>) -> Result<(), String> {
    let url = format!("ws://127.0.0.1:{}/v1/chat/stream", http_port());
    let (mut socket, _) = tungstenite::connect(&url).map_err(|e| e.to_string())?;

    let request = serde_json::json!({ "message": prompt, "chat_id": chat_id }).to_string();
    socket
        .send(tungstenite::Message::Text(request))
        .map_err(|e| e.to_string())?;

    loop {
        let message = match socket.read() {
            Ok(m) => m,
            Err(e) => return Err(format!("stream closed: {e}")),
        };
        let text = match message {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => return Err("stream closed early".into()),
            _ => continue,
        };
        let Some(event) = parse_event(&text) else {
            continue;
        };
        let terminal = matches!(event, AgentEvent::Done(_) | AgentEvent::Error(_));
        if tx.send(event).is_err() {
            // Receiver dropped — the window is gone.
            return Ok(());
        }
        if terminal {
            let _ = socket.close(None);
            return Ok(());
        }
    }
}

fn parse_event(text: &str) -> Option<AgentEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let field = |k: &str| v.get(k).and_then(|s| s.as_str()).unwrap_or("").to_string();
    Some(match v.get("type")?.as_str()? {
        "status" => AgentEvent::Status(field("message")),
        "tool_start" => AgentEvent::ToolStart {
            name: field("name"),
            hint: field("hint"),
        },
        "tool_end" => AgentEvent::ToolEnd {
            name: field("name"),
            ok: v.get("ok").and_then(|b| b.as_bool()).unwrap_or(true),
            secs: v.get("elapsed_secs").and_then(|n| n.as_u64()).unwrap_or(0),
        },
        "delta" => AgentEvent::Delta(field("text")),
        "done" => AgentEvent::Done(field("response")),
        "error" => AgentEvent::Error(field("message")),
        _ => return None,
    })
}

fn ask_via_http(prompt: &str, chat_id: &str) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/v1/chat", http_port());
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "message": prompt, "chat_id": chat_id }))
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let value: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    value
        .get("response")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "unexpected /v1/chat response".into())
}

fn ask_via_cli(prompt: &str) -> Result<String, String> {
    let apollo = find_apollo_bin().ok_or_else(|| {
        "apollo binary not found — `cargo install apollo-agent`, then `apollo chat`".to_string()
    })?;
    let output = std::process::Command::new(apollo)
        .args(["ask", prompt, "--config", "apollo.json"])
        .output()
        .map_err(|e| format!("failed to run apollo: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        let detail = if !err.trim().is_empty() { err } else { out };
        return Err(format!("apollo ask failed: {}", detail.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn find_apollo_bin() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("apollo");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("apollo"))
        .find(|candidate| candidate.is_file())
}

/// Model and engine reported by the local config, for the status bar.
pub fn config_summary() -> (String, String) {
    let Ok(text) = std::fs::read_to_string("apollo.json") else {
        return ("—".into(), "—".into());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return ("—".into(), "—".into());
    };
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("—")
        .to_string();
    let engine = v
        .get("agent")
        .and_then(|a| a.get("engine"))
        .and_then(|e| e.as_str())
        .unwrap_or("legacy")
        .to_string();
    (model, engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_stream_event() {
        assert!(matches!(
            parse_event(r#"{"type":"status","message":"Thinking…"}"#),
            Some(AgentEvent::Status(m)) if m == "Thinking…"
        ));
        assert!(matches!(
            parse_event(r#"{"type":"tool_start","name":"shell","hint":"ls"}"#),
            Some(AgentEvent::ToolStart { name, hint }) if name == "shell" && hint == "ls"
        ));
        assert!(matches!(
            parse_event(r#"{"type":"tool_end","name":"shell","ok":false,"elapsed_secs":3}"#),
            Some(AgentEvent::ToolEnd {
                ok: false,
                secs: 3,
                ..
            })
        ));
        assert!(matches!(
            parse_event(r#"{"type":"done","response":"hi"}"#),
            Some(AgentEvent::Done(r)) if r == "hi"
        ));
        assert!(matches!(
            parse_event(r#"{"type":"error","message":"boom"}"#),
            Some(AgentEvent::Error(m)) if m == "boom"
        ));
    }

    #[test]
    fn ignores_unknown_and_malformed_events() {
        assert!(parse_event(r#"{"type":"who_knows"}"#).is_none());
        assert!(parse_event("not json").is_none());
        assert!(parse_event(r#"{"no_type":1}"#).is_none());
    }

    #[test]
    fn tool_end_defaults_are_forgiving() {
        // A truncated tool_end should still render rather than drop the turn.
        assert!(matches!(
            parse_event(r#"{"type":"tool_end","name":"edit"}"#),
            Some(AgentEvent::ToolEnd { ok: true, secs: 0, name }) if name == "edit"
        ));
    }
}
