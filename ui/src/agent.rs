//! Transport to a running apollo agent.
//!
//! Prefers the WebSocket stream at `/v1/chat/stream` so tool activity shows up
//! live, and falls back to the blocking `/v1/chat` POST on the same server.
//! [`ensure_daemon`] starts `apollo serve` (the full tool loop) when nothing
//! is listening for this instance.

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
    let path = home_dir()?.join(".apollo").join("http-token");
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
        Err(failure) if failure.turn_started => {
            // The server already has the prompt and runs the turn to
            // completion whether or not the client is still listening.
            // Retrying it would re-run mutating tools, so report the break.
            let _ = tx.send(AgentEvent::Error(format!(
                "stream interrupted mid-turn: {}",
                failure.reason
            )));
        }
        Err(failure) => {
            let ws_err = failure.reason;
            // Same daemon, one blocking turn. Never a fresh `apollo ask`:
            // that path is a bare completion with no tools.
            match ask_via_http(prompt, chat_id) {
                Ok(text) => {
                    let _ = tx.send(AgentEvent::Done(text));
                }
                Err(http_err) => {
                    let _ = tx.send(AgentEvent::Error(format!(
                        "agent is not answering.\n  stream: {ws_err}\n  http: {http_err}"
                    )));
                }
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
/// `turn_started` records whether the prompt was handed to the server. What
/// must not repeat is the turn starting server-side, not the UI seeing output:
/// the server spawns the chat task and runs it to completion even if the
/// socket dies before the first frame. So falling back is only safe while the
/// request has not been sent.
struct StreamFailure {
    reason: String,
    turn_started: bool,
}

impl StreamFailure {
    fn before_the_turn_started(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            turn_started: false,
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
        .map_err(|e| StreamFailure::before_the_turn_started(e.to_string()))?;
    if let Some(token) = auth_token() {
        handshake.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| StreamFailure::before_the_turn_started("invalid token"))?,
        );
    }
    let (mut socket, _) = tungstenite::connect(handshake)
        .map_err(|e| StreamFailure::before_the_turn_started(e.to_string()))?;

    let request = serde_json::json!({ "message": prompt, "chat_id": chat_id }).to_string();
    socket
        .send(tungstenite::Message::Text(request))
        .map_err(|e| StreamFailure::before_the_turn_started(e.to_string()))?;

    // From here the server owns the turn: every later failure must be
    // reported, never retried.
    let turn_started = true;
    loop {
        let message = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                return Err(StreamFailure {
                    reason: format!("stream closed: {e}"),
                    turn_started,
                })
            }
        };
        let text = match message {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => {
                return Err(StreamFailure {
                    reason: "stream closed early".into(),
                    turn_started,
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

/// Run an `apollo` subcommand and return what it printed.
///
/// The CLI is the single implementation of `config`, `doctor` and friends —
/// masking included — so the UI drives it rather than reimplementing it. This
/// blocks on a child process, so callers must run it off the UI thread.
#[allow(dead_code)]
pub fn run_apollo(args: &[&str]) -> Result<String, String> {
    let apollo = find_apollo_bin().ok_or_else(|| {
        "apollo binary not found — install it, or put it next to apollo-tui".to_string()
    })?;
    let output = std::process::Command::new(apollo)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run apollo: {e}"))?;
    let out = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    let err = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();
    if output.status.success() {
        return Ok(if out.is_empty() { err } else { out });
    }
    let detail = if err.is_empty() { out } else { err };
    Err(if detail.is_empty() {
        format!("apollo {} failed", args.join(" "))
    } else {
        detail
    })
}

/// True when `name` resolves to a file on `PATH`.
#[allow(dead_code)]
pub fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// The agent binary beside this app, on `PATH`, or built from this checkout.
pub fn ensure_apollo_bin() -> Result<std::path::PathBuf, String> {
    if let Some(path) = find_apollo_bin() {
        return Ok(path);
    }
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = if root.join("src").join("main.rs").is_file() {
        root
    } else if root
        .parent()
        .is_some_and(|parent| parent.join("src").join("main.rs").is_file())
    {
        root.parent().unwrap().to_path_buf()
    } else {
        return Err("couldn't start the agent".into());
    };
    let output = std::process::Command::new("cargo")
        .args(["build", "-p", "apollo-agent", "--bin", "apollo"])
        .current_dir(&root)
        .output()
        .map_err(|_| "couldn't start the agent".to_string())?;
    if !output.status.success() {
        return Err("couldn't start the agent".into());
    }
    find_apollo_bin()
        .or_else(|| {
            let built = root.join("target").join("debug").join(APOLLO_EXE);
            built.is_file().then_some(built)
        })
        .ok_or_else(|| "couldn't start the agent".into())
}

pub fn find_apollo_bin() -> Option<std::path::PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(APOLLO_EXE);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(APOLLO_EXE))
        .find(|candidate| candidate.is_file())
}

/// `apollo` / `apollo.exe`.
const APOLLO_EXE: &str = if cfg!(windows) {
    "apollo.exe"
} else {
    "apollo"
};

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

/// Start `apollo serve` for this instance's config, or keep the one already
/// serving it. One process stays up across messages; a different instance
/// replaces it. The server is the tool loop, not a one-shot completion.
pub fn ensure_daemon(config_dir: &std::path::Path) -> Result<(), String> {
    let config = config_dir.join("apollo.json");
    if !config.is_file() {
        return Err(format!("no config at {}", config.display()));
    }
    let config = config.canonicalize().unwrap_or(config);
    if agent_online()
        && daemon_config().as_ref() == Some(&config)
        && daemon_stamp() == Some(config_stamp(&config))
    {
        return Ok(());
    }
    if agent_online() {
        shutdown_daemon();
    }
    let apollo = ensure_apollo_bin()?;
    let workspace = workspace_of(&config).unwrap_or_else(|| config_dir.to_path_buf());
    let log_path = daemon_log_path();
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log =
        std::fs::File::create(&log_path).map_err(|e| format!("could not open agent log: {e}"))?;
    let mut child = std::process::Command::new(apollo)
        .args([
            "serve",
            "--config",
            &config.display().to_string(),
            "--workspace",
            &workspace.display().to_string(),
        ])
        .current_dir(config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|e| format!("could not start the agent: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if agent_online() {
            remember_daemon(&config);
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let tail = std::fs::read_to_string(&log_path).unwrap_or_default();
                let tail: Vec<&str> = tail.lines().rev().take(6).collect();
                return Err(format!(
                    "the agent exited ({status}) before it was ready.\n{}",
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                ));
            }
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                return Err("the agent did not become ready within 30s".into());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(150)),
            Err(e) => return Err(format!("could not watch the agent: {e}")),
        }
    }
}

fn workspace_of(config: &std::path::Path) -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(config).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("workspace")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
}

fn home_dir() -> Option<std::path::PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .find_map(|var| std::env::var_os(var).filter(|h| !h.is_empty()))
        .map(std::path::PathBuf::from)
}

fn daemon_state_path() -> Option<std::path::PathBuf> {
    home_dir().map(|h| h.join(".apollo").join("ui-daemon.json"))
}

fn daemon_log_path() -> std::path::PathBuf {
    home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".apollo")
        .join("serve.log")
}

fn daemon_config() -> Option<std::path::PathBuf> {
    let text = std::fs::read_to_string(daemon_state_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let path = value.get("config")?.as_str()?;
    Some(std::path::PathBuf::from(path))
}

fn remember_daemon(config: &std::path::Path) {
    let Some(path) = daemon_state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::json!({ "config": config, "stamp": config_stamp(config) });
    let _ = std::fs::write(path, body.to_string());
}

fn config_stamp(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn daemon_stamp() -> Option<u64> {
    let text = std::fs::read_to_string(daemon_state_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("stamp")?.as_u64()
}

fn shutdown_daemon() {
    let url = format!("http://127.0.0.1:{}/shutdown", http_port());
    if let Ok(client) = blocking_client(2000) {
        let _ = authed(client.post(url)).send();
    }
    for _ in 0..40 {
        if !agent_online() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Model named in the instance config, for the status bar.
pub fn config_model() -> String {
    let Ok(text) = std::fs::read_to_string("apollo.json") else {
        return "—".into();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return "—".into();
    };
    v.get("model")
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("—")
        .to_string()
}

/// Model and engine reported by the local config. The TUI status bar still
/// asks for both; the desktop shows the model only.
#[allow(dead_code)]
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
        .filter(|s| !s.is_empty())
        .unwrap_or("—")
        .to_string();
    let engine = v
        .get("agent")
        .and_then(|a| a.get("engine"))
        .and_then(|e| e.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("—")
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

    /// `APOLLO_HTTP_PORT` is process-wide, so the turn tests take turns.
    static PORT_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serve_fetch_state(response_bytes: &[u8]) -> Option<AgentState> {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let response_vec = response_bytes.to_vec();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(&response_vec);
                let _ = stream.flush();
            }
        });

        let guard = PORT_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("APOLLO_HTTP_PORT", port.to_string());
        let result = fetch_state();
        std::env::remove_var("APOLLO_HTTP_PORT");
        drop(guard);
        server.join().unwrap();

        result
    }

    /// Run one turn against a throwaway WS server that drops the connection
    /// after `frames` have been sent, and collect what the UI was told.
    fn turn_against_dropping_server(frames: &'static [&'static str]) -> Vec<AgentEvent> {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            // The request has been handed over: the server owns the turn now.
            let _ = socket.read().unwrap();
            for frame in frames {
                socket
                    .send(tungstenite::Message::Text(frame.to_string()))
                    .unwrap();
            }
            socket.flush().unwrap();
            // Cut the connection mid-turn, without a terminal event.
            let _ = socket.get_mut().flush();
            drop(socket);
        });

        let guard = PORT_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("APOLLO_HTTP_PORT", port.to_string());
        let (tx, rx) = std::sync::mpsc::channel();
        run_turn("do something destructive", "chat", &tx);
        std::env::remove_var("APOLLO_HTTP_PORT");
        drop(guard);
        server.join().unwrap();

        rx.try_iter().collect()
    }

    fn assert_reported_not_retried(events: &[AgentEvent]) {
        let Some(AgentEvent::Error(message)) = events.last() else {
            panic!("expected a terminal error, got {events:?}");
        };
        assert!(
            message.contains("stream interrupted mid-turn"),
            "a turn the server already started must not be re-run: {message}"
        );
        assert!(
            !message.contains("no agent reachable"),
            "the HTTP/CLI fallback must not have run: {message}"
        );
    }

    #[test]
    fn mid_stream_drop_does_not_rerun_the_turn() {
        let events = turn_against_dropping_server(&[r#"{"type":"delta","text":"partial"}"#]);
        assert!(
            matches!(events.first(), Some(AgentEvent::Delta(t)) if t == "partial"),
            "expected the forwarded delta first, got {events:?}"
        );
        assert_reported_not_retried(&events);
    }

    #[test]
    fn drop_before_the_first_frame_does_not_rerun_the_turn() {
        // The server has the prompt and runs the turn to completion; nothing
        // reached the UI, but re-sending it would execute its tools twice.
        assert_reported_not_retried(&turn_against_dropping_server(&[]));
    }

    #[test]
    fn drop_after_only_unparseable_frames_does_not_rerun_the_turn() {
        assert_reported_not_retried(&turn_against_dropping_server(&[
            r#"{"type":"who_knows"}"#,
            "not json",
        ]));
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

    #[test]
    fn fetch_state_success() {
        let response = "HTTP/1.1 200 OK\r\n\
                        Content-Type: application/json\r\n\
                        Connection: close\r\n\
                        \r\n\
                        {\
                          \"model\": \"gpt-4o\",\
                          \"engine\": \"legacy\",\
                          \"mode\": \"chat\",\
                          \"cost_usd\": 0.15,\
                          \"total_tokens\": 1000,\
                          \"call_count\": 5,\
                          \"context_tokens\": 500,\
                          \"context_window\": 8000,\
                          \"context_pct\": 6,\
                          \"provider\": \"openai\",\
                          \"message_count\": 10\
                        }";

        let state = serve_fetch_state(response.as_bytes()).expect("Expected Some(AgentState)");
        assert_eq!(state.model, "gpt-4o");
        assert_eq!(state.engine, "legacy");
        assert_eq!(state.cost_usd, 0.15);
        assert_eq!(state.total_tokens, 1000);
        assert_eq!(state.call_count, 5);
        assert_eq!(state.context_pct, 6);
    }

    #[test]
    fn fetch_state_http_error() {
        let response = "HTTP/1.1 500 Internal Server Error\r\n\
                        Connection: close\r\n\
                        \r\n\
                        Server Error";
        let state = serve_fetch_state(response.as_bytes());
        assert!(state.is_none(), "Expected None on HTTP error");
    }

    #[test]
    fn fetch_state_invalid_json() {
        let response = "HTTP/1.1 200 OK\r\n\
                        Content-Type: application/json\r\n\
                        Connection: close\r\n\
                        \r\n\
                        { \"model\": \"gpt-4o\", oops }";
        let state = serve_fetch_state(response.as_bytes());
        assert!(state.is_none(), "Expected None on invalid JSON");
    }

    #[test]
    fn fetch_state_unreachable() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // Ensure the port is closed

        let guard = PORT_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("APOLLO_HTTP_PORT", port.to_string());
        let result = fetch_state();
        std::env::remove_var("APOLLO_HTTP_PORT");
        drop(guard);

        assert!(result.is_none(), "Expected None when unreachable");
    }
}
