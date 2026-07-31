//! Spawning child processes from tools: the environment they inherit, and
//! waiting on them without leaking a live process.
//!
//! apollo's own process environment holds every secret the operator put in
//! `.env` — the generated `run.sh` does `set -a; source .env; set +a`, so
//! ANTHROPIC_API_KEY, GITHUB_TOKEN and the channel tokens are all exported.
//! The file sandbox refuses to read `.env`, which is worth nothing if `exec`
//! can simply run `env`.
//!
//! A deny-list is used rather than an allow-list on purpose. Ordinary tool use
//! depends on a long and open-ended tail of environment: CARGO_HOME,
//! RUSTUP_HOME, GOPATH, NVM_*, PKG_CONFIG_PATH, SSH_AUTH_SOCK, XDG_*, LC_*,
//! HTTP(S)_PROXY, VIRTUAL_ENV, CI markers. An allow-list would break those
//! silently and be re-widened until it stopped meaning anything, whereas
//! secret-bearing names are conventional and few.

/// Substrings that mark a variable as secret-bearing regardless of prefix.
const SECRET_MARKERS: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "AUTH",
    "SESSION",
    "COOKIE",
    "PRIVATE",
    "SIGNATURE",
];

/// Prefixes denied outright: apollo's own configuration surface, plus the
/// cloud providers whose SDKs read credentials straight from the environment.
const DENIED_PREFIXES: &[&str] = &["APOLLO_"];

/// Names that match a marker but carry no secret and are load-bearing for
/// ordinary work.
const MARKER_EXCEPTIONS: &[&str] = &[
    "SSH_AUTH_SOCK",
    "GPG_AGENT_INFO",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_CLASS",
    "XDG_SESSION_ID",
    "XDG_SESSION_DESKTOP",
    "KEYBOARD_LAYOUT",
];

/// True when a variable must not reach a child process.
pub fn is_secret_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if DENIED_PREFIXES.iter().any(|p| upper.starts_with(p)) {
        return true;
    }
    if MARKER_EXCEPTIONS.contains(&upper.as_str()) {
        return false;
    }
    SECRET_MARKERS.iter().any(|m| upper.contains(m))
}

/// apollo's environment with every secret-bearing variable removed.
pub fn child_env() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(name, _)| !is_secret_env_name(name))
        .collect()
}

/// Apply the filtered environment to a command, clearing the inherited one
/// first so nothing new leaks in by default.
pub fn scrub(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    command.env_clear().envs(child_env())
}

/// Wait for a child, reading both pipes concurrently so a chatty command
/// cannot deadlock by filling a pipe buffer, and killing it if the deadline
/// passes.
///
/// `wait_with_output` consumes the `Child`, which is why the timeout path
/// there leaks a live process: there is nothing left to kill. Borrowing
/// instead keeps the handle available.
pub async fn wait_with_timeout(
    child: &mut tokio::process::Child,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::process::Output>> {
    use tokio::io::AsyncReadExt;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let collected = {
        let drain = async {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let status = {
                let read_out = async {
                    if let Some(pipe) = stdout_pipe.as_mut() {
                        let _ = pipe.read_to_end(&mut stdout).await;
                    }
                };
                let read_err = async {
                    if let Some(pipe) = stderr_pipe.as_mut() {
                        let _ = pipe.read_to_end(&mut stderr).await;
                    }
                };
                let (_, _, status) = tokio::join!(read_out, read_err, child.wait());
                status?
            };
            Ok::<_, std::io::Error>(std::process::Output {
                status,
                stdout,
                stderr,
            })
        };
        tokio::time::timeout(timeout, drain).await
    };

    match collected {
        Ok(output) => output.map(Some),
        Err(_) => {
            let _ = child.kill().await;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_the_secrets_the_sandbox_refuses_to_read() {
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "APOLLO_TELEGRAM_TOKEN",
            "APOLLO_DISCORD_TOKEN",
            "apollo_telegram_token",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_ACCESS_KEY_ID",
            "DB_PASSWORD",
            "NPM_TOKEN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "SSH_PRIVATE_KEY",
            "SLACK_SIGNING_SECRET",
            "HF_TOKEN",
        ] {
            assert!(is_secret_env_name(name), "should be denied: {name}");
        }
    }

    #[test]
    fn keeps_what_ordinary_tool_use_needs() {
        for name in [
            "PATH",
            "HOME",
            "USER",
            "SHELL",
            "TERM",
            "LANG",
            "LC_ALL",
            "TMPDIR",
            "PWD",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "GOPATH",
            "PKG_CONFIG_PATH",
            "VIRTUAL_ENV",
            "HTTPS_PROXY",
            "SSH_AUTH_SOCK",
            "XDG_SESSION_TYPE",
        ] {
            assert!(!is_secret_env_name(name), "should be kept: {name}");
        }
    }

    #[test]
    fn child_env_carries_path_and_no_secret_names() {
        let env = child_env();
        assert!(env.iter().any(|(k, _)| k == "PATH"));
        assert!(
            !env.iter().any(|(k, _)| is_secret_env_name(k)),
            "the filtered environment must contain no secret-bearing names"
        );
    }
}
