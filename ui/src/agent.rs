//! Transport to a running apollo agent.
//!
//! Prefers the WebSocket stream at `/v1/chat/stream` so tool activity shows up
//! live, falls back to the blocking `/v1/chat` POST, and finally to shelling
//! out to `apollo ask` when no agent HTTP server is listening.

use std::sync::mpsc::Sender;

use tungstenite::client::IntoClientRequest;

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

/// Bearer token shared with the agent server.
///
/// The server writes this to `~/.apollo/http-token` on first run. Without it
/// the agent API answers 401, so a client that cannot read the file cannot
/// drive the agent — which is the point.
fn auth_token() -> Option<String> {
    if let Ok(token) = std::env::var("APOLLO_HTTP_TOKEN") {
        if !token.trim().is_empty() {
            return Some(token.trim().to_string());
        }
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let path = std::path::PathBuf::from(home).join(".apollo/http-token");
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
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
        Err(failure) if failure.forwarded => {
            // The turn already ran and its output reached the UI. Retrying it
            // would re-run mutating tools, so report the break instead.
            let _ = tx.send(AgentEvent::Error(format!(
                "stream interrupted mid-turn: {}",
                failure.reason
            )));
        }
        Err(failure) => {
            let ws_err = failure.reason;
            // Nothing was forwarded — fall back to a blocking turn, then the CLI.
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
///
/// `forwarded` records whether any event already reached the UI. Re-running
/// the prompt after that would re-execute mutating tools, so it is only safe
/// to fall back when nothing was forwarded.
struct StreamFailure {
    reason: String,
    forwarded: bool,
}

impl StreamFailure {
    fn before_any_output(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            forwarded: false,
        }
    }
}

fn stream_over_ws(
    prompt: &str,
    chat_id: &str,
    tx: &Sender<AgentEvent>,
) -> Result<(), StreamFailure> {
    let url = format!("ws://127.0.0.1:{}/v1/chat/stream", http_port());
    let mut handshake = url
        .into_client_request()
        .map_err(|e| StreamFailure::before_any_output(e.to_string()))?;
    if let Some(token) = auth_token() {
        handshake.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| StreamFailure::before_any_output("invalid token"))?,
        );
    }
    let (mut socket, _) = tungstenite::connect(handshake)
        .map_err(|e| StreamFailure::before_any_output(e.to_string()))?;

    let request = serde_json::json!({ "message": prompt, "chat_id": chat_id }).to_string();
    socket
        .send(tungstenite::Message::Text(request))
        .map_err(|e| StreamFailure::before_any_output(e.to_string()))?;

    let mut forwarded = false;
    loop {
        let message = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                return Err(StreamFailure {
                    reason: format!("stream closed: {e}"),
                    forwarded,
                })
            }
        };
        let text = match message {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => {
                return Err(StreamFailure {
                    reason: "stream closed early".into(),
                    forwarded,
                })
            }
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
        forwarded = true;
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
    let mut request = client
        .post(&url)
        .json(&serde_json::json!({ "message": prompt, "chat_id": chat_id }));
    if let Some(token) = auth_token() {
        request = request.bearer_auth(token);
    }
    let resp = request.send().map_err(|e| e.to_string())?;
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

/// Live agent state from `GET /v1/state`.
///
/// The server is the only place these are true: the local config file says
/// what the agent started with, not what it is running now.
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(dead_code)]
pub struct AgentState {
    pub model: String,
    pub engine: String,
    pub mode: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub call_count: u64,
    pub context_tokens: u64,
    pub context_window: u64,
    pub context_pct: u8,
    pub provider: String,
    pub message_count: u64,
}

#[allow(dead_code)]
fn parse_state(value: &serde_json::Value) -> AgentState {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("—")
            .to_string()
    };
    let number = |key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    AgentState {
        model: text("model"),
        engine: text("engine"),
        mode: text("mode"),
        cost_usd: value
            .get("cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        total_tokens: number("total_tokens"),
        call_count: number("call_count"),
        context_tokens: number("context_tokens"),
        context_window: number("context_window"),
        provider: text("provider"),
        message_count: number("message_count"),
        context_pct: number("context_pct").min(100) as u8,
    }
}

#[allow(dead_code)]
fn blocking_client(timeout_ms: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| e.to_string())
}

#[allow(dead_code)]
fn authed(request: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    match auth_token() {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

/// Read the agent's live state. `None` when no agent is reachable.
#[allow(dead_code)]
pub fn fetch_state() -> Option<AgentState> {
    let url = format!("http://127.0.0.1:{}/v1/state", http_port());
    let response = authed(blocking_client(1500).ok()?.get(&url)).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: serde_json::Value = response.json().ok()?;
    Some(parse_state(&value))
}

/// Switch the running agent's model, returning its new state.
#[allow(dead_code)]
pub fn set_model(model: &str) -> Result<AgentState, String> {
    let url = format!("http://127.0.0.1:{}/v1/model", http_port());
    let request = blocking_client(5000)?
        .post(&url)
        .json(&serde_json::json!({ "model": model }));
    let response = authed(request).send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let value: serde_json::Value = response.json().map_err(|e| e.to_string())?;
    Ok(parse_state(&value))
}

/// Ask the agent to clear a chat's stored history.
///
/// `path` is `/v1/clear`. `/v1/compact` was removed rather than left as a
/// permanent 501: compaction in apollo is turn-local, so there is no stored
/// state for a server-side compaction to act on.
///
/// On failure the server's own explanation is returned and shown as-is — the
/// UI must not claim work that did not happen.
#[allow(dead_code)]
pub fn post_chat_action(path: &str, chat_id: &str) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}{}", http_port(), path);
    let request = blocking_client(30_000)?
        .post(&url)
        .json(&serde_json::json!({ "chat_id": chat_id }));
    let response = authed(request).send().map_err(|e| e.to_string())?;
    let status = response.status();
    let value: serde_json::Value = response.json().unwrap_or(serde_json::Value::Null);
    let detail = value
        .get("error")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if status.is_success() {
        return Ok(detail.unwrap_or_else(|| "done".to_string()));
    }
    Err(detail.unwrap_or_else(|| format!("HTTP {status}")))
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
    fn mid_stream_drop_does_not_rerun_the_turn() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let _ = socket.read().unwrap();
            socket
                .send(tungstenite::Message::Text(
                    r#"{"type":"delta","text":"partial"}"#.to_string(),
                ))
                .unwrap();
            socket.flush().unwrap();
            // Cut the connection mid-turn, without a terminal event.
            let _ = socket.get_mut().flush();
            drop(socket);
        });

        std::env::set_var("APOLLO_HTTP_PORT", port.to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        run_turn("do something destructive", "chat", &tx);
        std::env::remove_var("APOLLO_HTTP_PORT");
        server.join().unwrap();

        let events: Vec<AgentEvent> = rx.try_iter().collect();
        assert!(
            matches!(events.first(), Some(AgentEvent::Delta(t)) if t == "partial"),
            "expected the forwarded delta first, got {events:?}"
        );
        let Some(AgentEvent::Error(message)) = events.last() else {
            panic!("expected a terminal error, got {events:?}");
        };
        assert!(
            message.contains("stream interrupted mid-turn"),
            "a partially streamed turn must not be re-run: {message}"
        );
        assert!(
            !message.contains("no agent reachable"),
            "the HTTP/CLI fallback must not have run: {message}"
        );
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
