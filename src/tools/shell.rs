//! Shell/exec tool — execute commands (matches OpenClaw's exec tool).

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::child_proc;
use super::confine::{confine, ConfineOutcome, ConfinePolicy};
use super::pty_worker::PtyWorker;
use super::traits::*;
use crate::policy::ExecutionPolicy;
use crate::text::truncate_chars_counted;

/// Upper bound on a model-supplied timeout — an hour is already far past any
/// legitimate single command, and beyond it the agent loop is simply stuck.
const MAX_TIMEOUT_SECS: u64 = 3600;

pub struct ShellTool {
    workspace: PathBuf,
    timeout_secs: u64,
    policy: Arc<ExecutionPolicy>,
    confine: ConfinePolicy,
    pty: Arc<PtyWorker>,
}

impl ShellTool {
    pub fn new(workspace: PathBuf, policy: Arc<ExecutionPolicy>) -> Self {
        Self {
            workspace,
            timeout_secs: 120,
            policy,
            confine: ConfinePolicy::runtime_default(),
            pty: Arc::new(PtyWorker::new()),
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_confine(mut self, confine: ConfinePolicy) -> Self {
        self.confine = confine;
        self
    }

    pub fn with_pty_worker(mut self, pty: Arc<PtyWorker>) -> Self {
        self.pty = pty;
        self
    }

    pub fn pty_worker(&self) -> Arc<PtyWorker> {
        Arc::clone(&self.pty)
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    /// Working directory (defaults to workspace)
    #[serde(alias = "workdir")]
    cwd: Option<String>,
    /// Timeout in seconds
    timeout: Option<u64>,
    stdin: Option<String>,
    process_id: Option<String>,
    pty: Option<bool>,
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exec".to_string(),
            description: "Execute shell commands. Returns stdout/stderr and exit code.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory (defaults to workspace)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default 120)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_shell {
            return ExecutionPolicy::deny("Shell execution is disabled by policy");
        }

        let args: ShellArgs = serde_json::from_str(arguments)?;

        // Guard: block catastrophic commands unconditionally
        if let Err(reason) = self.policy.check_shell_command(&args.command) {
            return Ok(ToolResult::error(format!(
                "⛔ Blocked catastrophic command: {}",
                reason
            )));
        }

        // Guard: prevent self-restart/self-kill mid-conversation
        let cmd_lower = args.command.to_lowercase();
        let self_name = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "apollo".to_string());

        if (cmd_lower.contains("systemctl") && cmd_lower.contains(&self_name))
            || (cmd_lower.contains("pkill") && cmd_lower.contains(&self_name))
            || (cmd_lower.contains("kill") && cmd_lower.contains(&self_name))
            || cmd_lower.contains("shutdown")
            || cmd_lower.contains("reboot")
        {
            return Ok(ToolResult::error(
                "⚠️ Restricted command: Cannot restart/kill the host or agent mid-conversation.",
            ));
        }

        // The model supplies this, so clamp it: an unbounded value parks the
        // agent loop on a hung command with no way back.
        let timeout = args
            .timeout
            .unwrap_or(self.timeout_secs)
            .clamp(1, MAX_TIMEOUT_SECS);

        let cwd = if let Some(dir) = &args.cwd {
            let requested = if dir.starts_with('/') {
                PathBuf::from(dir)
            } else {
                self.workspace.join(dir)
            };

            // Canonicalize to ensure it's inside workspace
            let canonical_workspace = self
                .workspace
                .canonicalize()
                .unwrap_or_else(|_| self.workspace.clone());
            let canonical_requested = requested.canonicalize().unwrap_or(requested);

            if !canonical_requested.starts_with(&canonical_workspace) {
                return Ok(ToolResult::error(format!(
                    "Access denied: directory '{}' is outside the workspace.",
                    dir
                )));
            }
            canonical_requested
        } else {
            self.workspace.clone()
        };

        if let Some(process_id) = args.process_id.as_deref() {
            if let Some(stdin) = args.stdin.as_deref() {
                self.pty.write_stdin(process_id, stdin.as_bytes()).await?;
            }
            return Ok(ToolResult::success(format!("wrote stdin to {process_id}")));
        }

        let raw_argv = vec!["bash".to_string(), "-c".to_string(), args.command.clone()];
        let confined = match confine(&raw_argv, &self.confine) {
            ConfineOutcome::Denial { reason } => {
                return Ok(ToolResult::error(format!("⛔ Confined: {reason}")));
            }
            ConfineOutcome::Runner { argv, .. } => argv,
        };

        let use_pty = args.pty.unwrap_or(false);
        if use_pty || args.stdin.is_some() {
            let process_id = self
                .pty
                .spawn(&confined, &cwd, &self.confine, use_pty)
                .await?;
            if let Some(stdin) = args.stdin.as_deref() {
                self.pty.write_stdin(&process_id, stdin.as_bytes()).await?;
                if !use_pty {
                    self.pty.close_stdin(&process_id).await?;
                }
            }
            let output = self
                .pty
                .wait(&process_id, Duration::from_secs(timeout))
                .await?;
            let result = if output.stdout.is_empty() && !output.stderr.is_empty() {
                output.stderr
            } else if !output.stderr.is_empty() {
                format!("{}\n{}", output.stdout, output.stderr)
            } else {
                output.stdout
            };
            let truncated = match truncate_chars_counted(&result, 20_000) {
                Some((head, dropped)) => format!("{}...\n[truncated {} chars]", head, dropped),
                None => result,
            };
            return Ok(if output.success {
                ToolResult::success(format!("process_id={process_id}\n{truncated}"))
            } else {
                ToolResult::error(format!("process_id={process_id}: {truncated}"))
            });
        }

        let (program, rest) = match confined.split_first() {
            Some(parts) => parts,
            None => {
                return Ok(ToolResult::error(
                    "⛔ Confined: empty runner argv".to_string(),
                ));
            }
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(rest)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // A timed-out command must die with the tool call, not keep
            // running against the workspace while the model retries.
            .kill_on_drop(true);
        child_proc::scrub(&mut command);
        let mut child = command.spawn()?;

        let output =
            match child_proc::wait_with_timeout(&mut child, Duration::from_secs(timeout)).await? {
                Some(output) => output,
                None => {
                    return Ok(ToolResult::error(format!(
                        "Command timed out after {}s",
                        timeout
                    )));
                }
            };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let result = if stdout.is_empty() && !stderr.is_empty() {
            stderr.to_string()
        } else if !stderr.is_empty() {
            format!("{}\n{}", stdout, stderr)
        } else {
            stdout.to_string()
        };

        // Truncate if too long
        let truncated = match truncate_chars_counted(&result, 20_000) {
            Some((head, dropped)) => format!("{}...\n[truncated {} chars]", head, dropped),
            None => result,
        };

        Ok(if output.status.success() {
            ToolResult::success(truncated)
        } else {
            ToolResult::error(format!(
                "Exit code {}: {}",
                output.status.code().unwrap_or(-1),
                truncated
            ))
        })
    }
}

/// Helper: block catastrophic commands like `rm -rf /` or `mkfs`.
///
/// ponytail: a typo-and-accident guard, not a security boundary. A determined
/// caller can always evade it (`eval`, base64, a shell script, an alias), so
/// the real controls are `policy.allow_shell` and the permission hooks. Keep
/// this cheap and obvious rather than growing it into a pattern zoo that
/// invites misplaced trust.
pub(crate) fn check_catastrophic_command(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();

    // Whitespace-insensitive, so reflowed variants still match.
    if lower
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .contains(":(){:|:&};:")
    {
        return Some("Fork bomb.");
    }

    // Check each command in a pipeline or list separately, so the target of an
    // `rm` is not confused with an argument to something else.
    lower
        .split(['&', '|', ';', '\n'])
        .find_map(check_catastrophic_segment)
}

/// Paths whose recursive deletion destroys the machine or the whole workspace.
///
/// Trailing slashes and globs are stripped first, so `/`, `/*` and `/ *` all
/// reduce to the same root target, while `./build` and `*.log` do not.
fn is_catastrophic_target(token: &str) -> bool {
    matches!(
        token.trim_end_matches(['*', '/']),
        "" | "." | "~" | "$home" | "${home}"
    )
}

fn check_catastrophic_segment(segment: &str) -> Option<&'static str> {
    let mut tokens = segment.split_whitespace();
    let program = tokens.next()?;
    // Strip any path prefix so `/bin/rm` is treated as `rm`.
    let program = program.rsplit('/').next().unwrap_or(program);
    let args: Vec<&str> = tokens.collect();

    if program.starts_with("mkfs") || program == "fdisk" {
        return Some("Disk formatting or low-level block write.");
    }

    if program == "dd" {
        if args
            .iter()
            .any(|a| a.starts_with("of=/dev/") || a.starts_with("if=/dev/"))
        {
            return Some("Disk formatting or low-level block write.");
        }
        return None;
    }

    if program != "rm" {
        return None;
    }

    let recursive = args.iter().any(|a| {
        *a == "--recursive" || (a.starts_with('-') && !a.starts_with("--") && a.contains('r'))
    });
    if !recursive {
        return None;
    }

    if args.contains(&"--no-preserve-root") {
        return Some("Destructive recursive delete on root or wildcard.");
    }

    let targets_root = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .any(|a| is_catastrophic_target(a));

    targets_root.then_some("Destructive recursive delete on root or wildcard.")
}

#[cfg(test)]
mod tests {
    use super::check_catastrophic_command;

    #[test]
    fn blocks_recursive_deletes_of_root_in_any_flag_form() {
        for cmd in [
            "rm -rf /",
            "rm -rf /*",
            "rm -fr /",
            "rm -r -f /",
            "rm --recursive --force /",
            "rm  -rf  /",
            "rm -rf ~",
            "rm -rf ~/",
            "rm -rf $HOME",
            "rm -rf .",
            "rm -rf *",
            "/bin/rm -rf /",
            "rm -r --no-preserve-root /tmp/x",
            "echo hi && rm -rf /",
        ] {
            assert!(
                check_catastrophic_command(cmd).is_some(),
                "should have blocked: {cmd}"
            );
        }
    }

    #[test]
    fn blocks_disk_writes_and_fork_bombs() {
        for cmd in [
            "mkfs.ext4 /dev/sda1",
            "fdisk /dev/sda",
            "dd if=/dev/zero of=/dev/sda",
            "dd of=/dev/sda if=/dev/zero",
            ":(){ :|:& };:",
            ":(){:|:&};:",
        ] {
            assert!(
                check_catastrophic_command(cmd).is_some(),
                "should have blocked: {cmd}"
            );
        }
    }

    #[test]
    fn allows_ordinary_work() {
        for cmd in [
            "rm -rf ./build",
            "rm -rf target",
            "rm -rf node_modules",
            "rm file.txt",
            "cargo build --release",
            "dd if=input.img of=output.img",
            "git status",
            "grep -r 'rm -rf /' src",
        ] {
            assert!(
                check_catastrophic_command(cmd).is_none(),
                "should have allowed: {cmd}"
            );
        }
    }

    /// The file sandbox refuses to read `.env`; that is worth nothing if
    /// `exec` can print the same values back out of its own environment.
    #[tokio::test]
    async fn secrets_do_not_reach_the_child_but_path_does() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({ "command": "env" }).to_string();
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        for line in result.output.lines() {
            let name = line.split('=').next().unwrap_or_default();
            assert!(
                !crate::tools::child_proc::is_secret_env_name(name),
                "secret-bearing variable {name} reached the child"
            );
        }
        assert!(result.output.contains("PATH="), "PATH must survive");
    }

    #[tokio::test]
    async fn an_over_long_timeout_is_clamped() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({ "command": "true", "timeout": u64::MAX }).to_string();
        // Would panic inside Duration/timeout arithmetic without the clamp.
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        assert!(!result.is_error, "got: {}", result.output);
    }

    #[tokio::test]
    async fn a_timed_out_command_is_killed_not_left_running() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("still-alive");
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({
            "command": format!("sleep 3; touch {}", marker.display()),
            "timeout": 1,
        })
        .to_string();
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        assert!(
            result.output.contains("timed out"),
            "got: {}",
            result.output
        );
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(
            !marker.exists(),
            "the killed command must not have kept running to completion"
        );
    }

    #[tokio::test]
    async fn truncates_multibyte_output_by_chars() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({
            "command": "printf '日%.0s' {1..30000}"
        })
        .to_string();
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        let body = result.output;
        assert!(
            body.chars().count() < 21_000,
            "multibyte output must be truncated, got {} chars",
            body.chars().count()
        );
        assert!(
            body.contains("[truncated 10000 chars]"),
            "footer must report the real dropped char count"
        );
    }

    #[tokio::test]
    async fn confine_denial_never_spawns() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({ "command": "rm -rf /" }).to_string();
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        assert!(result.is_error, "got: {}", result.output);
        assert!(
            result.output.contains("Blocked") || result.output.contains("Confined"),
            "got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn stdin_session_exposes_process_id() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = super::ShellTool::new(
            tmp.path().to_path_buf(),
            std::sync::Arc::new(crate::policy::ExecutionPolicy::default()),
        )
        .with_confine(crate::tools::confine::ConfinePolicy::host());
        let args = serde_json::json!({
            "command": "cat",
            "stdin": "from-stdin\n"
        })
        .to_string();
        let result = crate::tools::Tool::execute(&tool, &args).await.unwrap();
        assert!(!result.is_error, "got: {}", result.output);
        assert!(result.output.contains("process_id="), "{}", result.output);
        assert!(result.output.contains("from-stdin"), "{}", result.output);
    }
}
