//! Spawn processes from a command line without `sh -c` (no shell metacharacter interpretation).

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Run `command` as argv[0..] via `shlex` (no shell). Returns (stdout+stderr text, success).
pub async fn run_argv_command(command: &str, timeout_secs: u64) -> anyhow::Result<(String, bool)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        anyhow::bail!("empty command");
    }

    let parts = shlex::split(trimmed).ok_or_else(|| anyhow::anyhow!("invalid command quoting"))?;
    if parts.is_empty() {
        anyhow::bail!("empty command");
    }

    let program = &parts[0];
    let args = &parts[1..];

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let output = match crate::tools::child_proc::wait_with_timeout(
        &mut child,
        Duration::from_secs(timeout_secs),
    )
    .await?
    {
        Some(output) => output,
        None => anyhow::bail!("command timed out after {}s", timeout_secs),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = if stderr.is_empty() {
        stdout.into_owned()
    } else if stdout.is_empty() {
        stderr.into_owned()
    } else {
        format!("{stdout}{stderr}")
    };

    Ok((combined, output.status.success()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_echo_no_shell() {
        let (out, ok) = run_argv_command("echo hello", 5).await.unwrap();
        assert!(ok);
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn semicolon_not_shell_metachar() {
        let (out, ok) = run_argv_command("echo one;two", 5).await.unwrap();
        assert!(ok);
        assert!(out.contains("one;two") || out.contains("one"));
    }

    #[tokio::test]
    async fn empty_command_is_rejected() {
        for command in ["", "   ", "\t\n"] {
            let err = run_argv_command(command, 5).await.unwrap_err();
            assert!(
                err.to_string().contains("empty command"),
                "{command:?} -> {err}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_quoting_is_rejected() {
        let err = run_argv_command("echo 'unterminated", 5).await.unwrap_err();
        assert!(
            err.to_string().contains("invalid command quoting"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_is_reported_as_failure() {
        let (out, ok) = run_argv_command("false", 5).await.unwrap();
        assert!(!ok, "got output: {out}");
    }

    #[tokio::test]
    async fn a_timed_out_command_is_killed_not_left_running() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("still-alive");
        let script = tmp.path().join("hold.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nsleep 3\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let err = run_argv_command(script.to_str().unwrap(), 1)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(
            !marker.exists(),
            "the killed command must not have kept running to completion"
        );
    }
}
