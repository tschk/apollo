//! Telekinesis tool — delegate a self-contained task to a worker agent.
//!
//! apollo is the manager, telekinesis (`tk`) is the worker. This shells out to
//! `tk exec`, telekinesis' headless one-shot mode, so apollo can hand off a
//! whole task rather than driving it tool call by tool call.
//!
//! `--json` is always passed: the contract is a single
//! `{"ok","text","error"}` object on stdout, so parsing never depends on the
//! worker's prose. Status and tool chatter go to stderr and are surfaced only
//! when something failed.

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;

use super::traits::*;
use crate::text::truncate_chars_counted;

/// Worker turns run a whole task, not a single command, so the default is far
/// longer than a shell tool's — but still bounded, because a hung worker must
/// not wedge the agent loop.
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Cap on the text handed back to the model.
const MAX_OUTPUT_CHARS: usize = 20_000;

pub struct TelekinesisTool {
    workspace: PathBuf,
    timeout_secs: u64,
    /// Binary to invoke. Overridable so tests can exercise the missing-binary
    /// path without depending on whether `tk` is installed.
    binary: String,
}

impl TelekinesisTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            binary: "tk".to_string(),
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    #[cfg(test)]
    fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Locate the worker binary the way apollo locates its other sibling
    /// binaries: next to the running executable first, then on PATH.
    async fn find_binary(&self) -> Option<PathBuf> {
        if let Ok(current) = std::env::current_exe() {
            if let Some(dir) = current.parent() {
                let sibling = dir.join(&self.binary);
                if sibling.is_file() {
                    return Some(sibling);
                }
            }
        }

        let on_path = tokio::process::Command::new("which")
            .arg(&self.binary)
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false);

        on_path.then(|| PathBuf::from(&self.binary))
    }

    /// Resolve the requested working directory, refusing anything outside the
    /// workspace exactly as the shell tool does.
    fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf, String> {
        let Some(dir) = requested else {
            return Ok(self.workspace.clone());
        };

        let requested_path = if dir.starts_with('/') {
            PathBuf::from(dir)
        } else {
            self.workspace.join(dir)
        };

        let canonical_workspace = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let canonical_requested = requested_path.canonicalize().unwrap_or(requested_path);

        if !canonical_requested.starts_with(&canonical_workspace) {
            return Err(format!(
                "Access denied: directory '{}' is outside the workspace.",
                dir
            ));
        }
        Ok(canonical_requested)
    }
}

#[derive(Deserialize)]
struct TelekinesisArgs {
    /// The task to delegate.
    #[serde(alias = "task")]
    prompt: String,
    /// Working directory (defaults to workspace).
    #[serde(alias = "workdir")]
    cwd: Option<String>,
    /// Timeout in seconds.
    timeout: Option<u64>,
}

/// `tk exec --json` contract: exactly this object on stdout.
#[derive(Deserialize)]
struct ExecOutput {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    error: Option<String>,
}

/// Cap text handed to the model, reporting the dropped count in the same unit
/// the gate used.
fn cap(text: &str) -> String {
    match truncate_chars_counted(text, MAX_OUTPUT_CHARS) {
        Some((head, dropped)) => format!("{}...\n[truncated {} chars]", head, dropped),
        None => text.to_string(),
    }
}

/// Turn one `tk exec --json` run into a tool result.
///
/// Kept free of I/O so every branch — clean success, structured failure,
/// non-JSON stdout — is directly testable.
fn interpret(stdout: &str, stderr: &str, exit_ok: bool) -> ToolResult {
    match serde_json::from_str::<ExecOutput>(stdout.trim()) {
        Ok(parsed) if parsed.ok => ToolResult::success(cap(&parsed.text)),
        Ok(parsed) => {
            let reason = parsed
                .error
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| "worker reported failure without a message".to_string());
            let body = if parsed.text.is_empty() {
                reason
            } else {
                format!("{}\n\n{}", reason, parsed.text)
            };
            ToolResult::error(cap(&body))
        }
        Err(_) => {
            // No parseable object. Exit status is then the only signal, and
            // stderr carries whatever the worker managed to say.
            let detail = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            if exit_ok && !stdout.trim().is_empty() {
                ToolResult::success(cap(stdout.trim()))
            } else {
                ToolResult::error(cap(&format!(
                    "telekinesis produced no parseable result. {}",
                    detail
                )))
            }
        }
    }
}

#[async_trait]
impl Tool for TelekinesisTool {
    fn name(&self) -> &str {
        "telekinesis"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "telekinesis".to_string(),
            description: "Delegate a self-contained task to a worker coding agent (telekinesis) \
                 and get back its final answer. Give it a complete, standalone brief — it starts \
                 with no knowledge of this conversation and reports back once, so it cannot ask \
                 follow-up questions. Best for work that is well specified and can run \
                 unattended, such as implementing a described change, investigating a codebase \
                 question, or writing a set of tests. Prefer the direct tools for a single file \
                 read or one shell command."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The complete, self-contained task for the worker agent"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory, must be inside the workspace (defaults to workspace)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 600)"
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: TelekinesisArgs = serde_json::from_str(arguments)?;

        if args.prompt.trim().is_empty() {
            return Ok(ToolResult::error(
                "No task given. Provide a complete, self-contained brief in 'prompt'.",
            ));
        }

        let cwd = match self.resolve_cwd(args.cwd.as_deref()) {
            Ok(dir) => dir,
            Err(message) => return Ok(ToolResult::error(message)),
        };

        let Some(binary) = self.find_binary().await else {
            return Ok(ToolResult::error(format!(
                "telekinesis is not installed: no '{}' binary next to apollo or on PATH. \
                 Install it from https://github.com/tschk/telekinesis, or put `tk` on PATH.",
                self.binary
            )));
        };

        let timeout = args.timeout.unwrap_or(self.timeout_secs);

        // The prompt is deliberately never logged — AGENTS.md 3.4 forbids
        // logging message content.
        let child = tokio::process::Command::new(&binary)
            .arg("exec")
            .arg(&args.prompt)
            .arg("--json")
            .arg("--cwd")
            .arg(&cwd)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let child = match child {
            Ok(child) => child,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "Failed to start telekinesis: {e}"
                )))
            }
        };

        let output = match tokio::time::timeout(
            Duration::from_secs(timeout),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Ok(ToolResult::error(format!("telekinesis failed to run: {e}"))),
            Err(_) => {
                return Ok(ToolResult::error(format!(
                    "Delegated task timed out after {timeout}s"
                )))
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(interpret(&stdout, &stderr, output.status.success()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(workspace: PathBuf) -> TelekinesisTool {
        TelekinesisTool::new(workspace)
    }

    #[tokio::test]
    async fn missing_binary_reports_a_clear_error_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let tool =
            tool(dir.path().to_path_buf()).with_binary("tk-definitely-not-installed-apollo-test");
        let args = serde_json::json!({ "prompt": "do a thing" }).to_string();

        let result = Tool::execute(&tool, &args).await.unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("telekinesis is not installed"),
            "got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn cwd_outside_the_workspace_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let tool = tool(workspace.path().to_path_buf());
        let args = serde_json::json!({
            "prompt": "do a thing",
            "cwd": outside.path().to_str().unwrap(),
        })
        .to_string();

        let result = Tool::execute(&tool, &args).await.unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("outside the workspace"),
            "got: {}",
            result.output
        );
    }

    /// The workspace check must run before the binary lookup, so an escape is
    /// refused whether or not `tk` happens to be installed on this machine.
    #[tokio::test]
    async fn cwd_escape_is_refused_regardless_of_binary_presence() {
        let workspace = tempfile::tempdir().unwrap();
        let tool =
            tool(workspace.path().to_path_buf()).with_binary("tk-definitely-not-installed-apollo");
        let args = serde_json::json!({ "prompt": "x", "cwd": "/etc" }).to_string();

        let result = Tool::execute(&tool, &args).await.unwrap();
        assert!(result.output.contains("outside the workspace"));
    }

    #[tokio::test]
    async fn relative_cwd_inside_the_workspace_is_allowed() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join("sub")).unwrap();
        let tool = tool(workspace.path().to_path_buf());
        assert!(tool.resolve_cwd(Some("sub")).is_ok());
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let args = serde_json::json!({ "prompt": "   " }).to_string();
        let result = Tool::execute(&tool(dir.path().to_path_buf()), &args)
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("No task given"));
    }

    #[test]
    fn parses_the_ok_shape() {
        let result = interpret(r#"{"ok":true,"text":"all done","error":null}"#, "", true);
        assert!(!result.is_error);
        assert_eq!(result.output, "all done");
    }

    #[test]
    fn parses_the_error_shape() {
        let result = interpret(
            r#"{"ok":false,"text":"","error":"provider auth failed"}"#,
            "some stderr noise",
            false,
        );
        assert!(result.is_error);
        assert!(result.output.contains("provider auth failed"));
    }

    #[test]
    fn error_shape_keeps_partial_text() {
        let result = interpret(
            r#"{"ok":false,"text":"got partway","error":"ran out of turns"}"#,
            "",
            false,
        );
        assert!(result.is_error);
        assert!(result.output.contains("ran out of turns"));
        assert!(result.output.contains("got partway"));
    }

    #[test]
    fn failure_without_a_message_still_reads_as_an_error() {
        let result = interpret(r#"{"ok":false}"#, "", false);
        assert!(result.is_error);
        assert!(result.output.contains("without a message"));
    }

    #[test]
    fn non_json_stdout_falls_back_to_stderr_detail() {
        let result = interpret("not json at all", "worker exploded", false);
        assert!(result.is_error);
        assert!(result.output.contains("worker exploded"));
    }

    #[test]
    fn non_json_stdout_on_a_clean_exit_is_still_returned() {
        let result = interpret("plain prose answer", "", true);
        assert!(!result.is_error);
        assert_eq!(result.output, "plain prose answer");
    }

    /// End-to-end against a real `tk`. Ignored by default: the test suite must
    /// pass on a machine without telekinesis installed. A provider auth or
    /// credit failure still exercises the contract, since it arrives as the
    /// documented `{"ok":false,...}` object.
    #[tokio::test]
    #[ignore = "requires telekinesis (tk) on PATH and a working provider login"]
    async fn delegates_to_a_real_worker() {
        let workspace = std::env::current_dir().unwrap();
        let tool = TelekinesisTool::new(workspace).with_timeout(180);
        let args = serde_json::json!({ "prompt": "Reply with exactly the word PONG." }).to_string();

        let result = Tool::execute(&tool, &args).await.unwrap();
        assert!(
            !result.output.is_empty(),
            "a real run must report something back"
        );
        assert!(
            !result.output.contains("no parseable result"),
            "tk exec --json must yield the documented object, got: {}",
            result.output
        );
    }

    #[test]
    fn long_output_is_truncated_by_chars_not_bytes() {
        let long = "日".repeat(30_000);
        let payload = serde_json::json!({ "ok": true, "text": long }).to_string();
        let result = interpret(&payload, "", true);
        assert!(!result.is_error);
        assert!(result.output.chars().count() < 21_000);
        assert!(result.output.contains("[truncated 10000 chars]"));
    }
}
