//! Shell/exec tool — execute commands (matches OpenClaw's exec tool).

use async_trait::async_trait;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::traits::*;
use crate::policy::ExecutionPolicy;
use crate::text::truncate_chars;

pub struct ShellTool {
    workspace: PathBuf,
    timeout_secs: u64,
    policy: Arc<ExecutionPolicy>,
}

impl ShellTool {
    pub fn new(workspace: PathBuf, policy: Arc<ExecutionPolicy>) -> Self {
        Self {
            workspace,
            timeout_secs: 120,
            policy,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
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
        if let Some(reason) = check_catastrophic_command(&args.command) {
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

        let timeout = args.timeout.unwrap_or(self.timeout_secs);

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

        let child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&args.command)
            .current_dir(&cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        let output = match tokio::time::timeout(
            Duration::from_secs(timeout),
            child.wait_with_output(),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
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
        let truncated = if result.len() > 20_000 {
            format!(
                "{}...\n[truncated {} chars]",
                truncate_chars(&result, 20_000),
                result.len() - 20_000
            )
        } else {
            result
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
fn check_catastrophic_command(cmd: &str) -> Option<&'static str> {
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
}
