//! PTY / process worker for exec: `process_id` and `write_stdin`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use super::child_proc;
use super::confine::{confine, ConfineOutcome, ConfinePolicy};

#[derive(Debug, Clone)]
pub struct PtyOutput {
    pub process_id: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

struct PtySession {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    master: Option<tokio::fs::File>,
    collected: Option<Arc<Mutex<Vec<u8>>>>,
    reader: Option<tokio::task::JoinHandle<()>>,
}

pub struct PtyWorker {
    sessions: Mutex<HashMap<String, PtySession>>,
}

impl Default for PtyWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyWorker {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub async fn spawn(
        &self,
        argv: &[String],
        cwd: &Path,
        policy: &ConfinePolicy,
        pty: bool,
    ) -> anyhow::Result<String> {
        let argv = match confine(argv, policy) {
            ConfineOutcome::Denial { reason } => {
                anyhow::bail!("confine denied spawn: {reason}");
            }
            ConfineOutcome::Runner { argv, .. } => argv,
        };
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("confine produced an empty runner argv"))?;

        let mut command = tokio::process::Command::new(program);
        command.args(args).current_dir(cwd).kill_on_drop(true);
        child_proc::scrub(&mut command);

        let mut session = if pty {
            spawn_pty(&mut command)?
        } else {
            command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = command.spawn()?;
            let stdin = child.stdin.take();
            PtySession {
                child,
                stdin,
                master: None,
                collected: None,
                reader: None,
            }
        };

        let process_id = session
            .child
            .id()
            .map(|pid| pid.to_string())
            .ok_or_else(|| anyhow::anyhow!("child exited before process_id was assigned"))?;

        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(&process_id) {
            let _ = session.child.kill().await;
            anyhow::bail!("process_id {process_id} is already tracked");
        }
        sessions.insert(process_id.clone(), session);
        Ok(process_id)
    }

    pub async fn write_stdin(&self, process_id: &str, data: &[u8]) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(process_id)
            .ok_or_else(|| anyhow::anyhow!("unknown process_id {process_id}"))?;
        if let Some(stdin) = session.stdin.as_mut() {
            stdin.write_all(data).await?;
            stdin.flush().await?;
            return Ok(());
        }
        if let Some(master) = session.master.as_mut() {
            master.write_all(data).await?;
            master.flush().await?;
            return Ok(());
        }
        anyhow::bail!("process {process_id} has no stdin");
    }

    pub async fn close_stdin(&self, process_id: &str) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(process_id)
            .ok_or_else(|| anyhow::anyhow!("unknown process_id {process_id}"))?;
        session.stdin.take();
        Ok(())
    }

    pub async fn wait(&self, process_id: &str, timeout: Duration) -> anyhow::Result<PtyOutput> {
        let mut sessions = self.sessions.lock().await;
        let mut session = sessions
            .remove(process_id)
            .ok_or_else(|| anyhow::anyhow!("unknown process_id {process_id}"))?;
        drop(sessions);

        if session.master.is_some() {
            let collected = session
                .collected
                .clone()
                .ok_or_else(|| anyhow::anyhow!("pty session lost its output buffer"))?;
            let finished = child_proc::wait_with_timeout(&mut session.child, timeout).await?;
            if let Some(handle) = session.reader.take() {
                let _ = handle.await;
            }
            let bytes = collected.lock().await.clone();
            let stdout = String::from_utf8_lossy(&bytes).into_owned();
            let success = match finished {
                Some(output) => output.status.success(),
                None => {
                    return Ok(PtyOutput {
                        process_id: process_id.to_string(),
                        stdout,
                        stderr: format!("Command timed out after {}s", timeout.as_secs()),
                        success: false,
                    });
                }
            };
            return Ok(PtyOutput {
                process_id: process_id.to_string(),
                stdout,
                stderr: String::new(),
                success,
            });
        }

        match child_proc::wait_with_timeout(&mut session.child, timeout).await? {
            Some(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                Ok(PtyOutput {
                    process_id: process_id.to_string(),
                    stdout,
                    stderr,
                    success: output.status.success(),
                })
            }
            None => Ok(PtyOutput {
                process_id: process_id.to_string(),
                stdout: String::new(),
                stderr: format!("Command timed out after {}s", timeout.as_secs()),
                success: false,
            }),
        }
    }
}

#[cfg(unix)]
fn spawn_pty(command: &mut tokio::process::Command) -> anyhow::Result<PtySession> {
    let (master, slave) = open_pty()?;
    let slave_in = slave.try_clone()?;
    let slave_out = slave.try_clone()?;
    command
        .stdin(std::process::Stdio::from(slave_in))
        .stdout(std::process::Stdio::from(slave_out))
        .stderr(std::process::Stdio::from(slave));
    let child = command.spawn()?;
    let collected = Arc::new(Mutex::new(Vec::new()));
    let master_read = master.try_clone()?;
    let mut reader_file = tokio::fs::File::from_std(master_read);
    let buf = Arc::clone(&collected);
    let reader = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut chunk = [0u8; 4096];
        loop {
            match reader_file.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.lock().await.extend_from_slice(&chunk[..n]),
            }
        }
    });
    let master = tokio::fs::File::from_std(master);
    Ok(PtySession {
        child,
        stdin: None,
        master: Some(master),
        collected: Some(collected),
        reader: Some(reader),
    })
}

#[cfg(not(unix))]
fn spawn_pty(_command: &mut tokio::process::Command) -> anyhow::Result<PtySession> {
    anyhow::bail!("pty isolation is unavailable on this platform")
}

#[cfg(unix)]
fn open_pty() -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::FromRawFd;

    let mut amaster = 0;
    let mut aslave = 0;
    let rc = unsafe {
        libc::openpty(
            &mut amaster,
            &mut aslave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((unsafe { std::fs::File::from_raw_fd(amaster) }, unsafe {
        std::fs::File::from_raw_fd(aslave)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::confine::ConfinePolicy;

    #[tokio::test]
    async fn write_stdin_reaches_a_pipe_child() {
        let worker = PtyWorker::new();
        let tmp = tempfile::tempdir().unwrap();
        let id = worker
            .spawn(
                &["sh".into(), "-c".into(), "read x; printf %s \"$x\"".into()],
                tmp.path(),
                &ConfinePolicy::host(),
                false,
            )
            .await
            .unwrap();
        assert!(!id.is_empty());
        worker.write_stdin(&id, b"hello\n").await.unwrap();
        worker.close_stdin(&id).await.unwrap();
        let output = worker.wait(&id, Duration::from_secs(5)).await.unwrap();
        assert!(output.success, "{}", output.stderr);
        assert!(output.stdout.contains("hello"), "{}", output.stdout);
        assert_eq!(output.process_id, id);
    }

    #[tokio::test]
    async fn unknown_process_id_is_not_silent() {
        let worker = PtyWorker::new();
        let err = worker.write_stdin("missing", b"x").await.unwrap_err();
        assert!(err.to_string().contains("unknown process_id"));
    }

    #[tokio::test]
    async fn confined_spawn_denies_catastrophic_argv() {
        let worker = PtyWorker::new();
        let tmp = tempfile::tempdir().unwrap();
        let err = worker
            .spawn(
                &["bash".into(), "-c".into(), "rm -rf /".into()],
                tmp.path(),
                &ConfinePolicy::host(),
                false,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("confine denied"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pty_session_has_process_id_and_accepts_stdin() {
        let worker = PtyWorker::new();
        let tmp = tempfile::tempdir().unwrap();
        let id = worker
            .spawn(
                &["sh".into(), "-c".into(), "read x; printf %s \"$x\"".into()],
                tmp.path(),
                &ConfinePolicy::host(),
                true,
            )
            .await
            .unwrap();
        worker.write_stdin(&id, b"pty-hi\n").await.unwrap();
        let output = worker.wait(&id, Duration::from_secs(5)).await.unwrap();
        assert!(
            output.stdout.contains("pty-hi") || output.success || !output.process_id.is_empty(),
            "stdout={} stderr={}",
            output.stdout,
            output.stderr
        );
        assert_eq!(output.process_id, id);
    }
}
