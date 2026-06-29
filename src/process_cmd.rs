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

    let child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let output =
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(result) => result?,
            Err(_) => anyhow::bail!("command timed out after {}s", timeout_secs),
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
}
