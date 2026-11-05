//! Standalone committed checkouts for autonomous runs, not a security sandbox.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

/// Locations and committed base of a single isolated run.
///
/// Its identity cannot be rewritten by callers:
/// ```compile_fail
/// fn redirect(checkout: &mut apollo::autonomous_isolation::AutonomousCheckout) {
///     checkout.workspace = std::path::PathBuf::from(".");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AutonomousCheckout {
    clone_root: PathBuf,
    workspace: PathBuf,
    base_commit: String,
    source_root: PathBuf,
    /// The unique per-run directory containing `clone_root`.
    clone_parent: PathBuf,
    /// Retained until the operator inspects the run and explicitly recovers it.
    recovery_marker: PathBuf,
}

async fn git(dir: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .await
        .context("could not execute git")?;
    if !output.status.success() {
        // Git's output can contain credentials or other sensitive data.
        bail!("git command failed with {}", output.status);
    }
    Ok(output.stdout)
}

/// Clone the committed HEAD into a unique directory under `private_parent`.
///
/// `ledger` is a workspace-relative path to a committed regular file. The
/// caller must provide a private parent directory. Failed runs are retained.
pub async fn prepare_autonomous_checkout(
    source_workspace: &Path,
    private_parent: &Path,
    ledger: &Path,
) -> Result<AutonomousCheckout> {
    if ledger.as_os_str().is_empty()
        || ledger
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("ledger must be a nonempty workspace-relative path");
    }
    let source = tokio::fs::canonicalize(source_workspace)
        .await
        .context("cannot resolve source workspace")?;
    let root_output = git(&source, &["rev-parse".as_ref(), "--show-toplevel".as_ref()]).await?;
    let root_text = std::str::from_utf8(&root_output).context("git root is not UTF-8")?;
    let root_text = root_text.strip_suffix('\n').unwrap_or(root_text);
    let root = tokio::fs::canonicalize(root_text)
        .await
        .context("cannot resolve git root")?;
    let relative = source
        .strip_prefix(&root)
        .context("workspace is outside git root")?;
    let head = git(
        &root,
        &[
            "rev-parse".as_ref(),
            "--verify".as_ref(),
            "HEAD^{commit}".as_ref(),
        ],
    )
    .await?;
    let base_commit = std::str::from_utf8(&head)
        .context("invalid git HEAD")?
        .trim()
        .to_owned();

    if !private_parent.is_absolute()
        || private_parent
            .components()
            .any(|part| part == Component::ParentDir)
    {
        bail!("private clone parent must be an absolute path without traversal");
    }
    let mut existing = private_parent;
    while !tokio::fs::try_exists(existing).await? {
        existing = existing
            .parent()
            .context("no existing clone parent ancestor")?;
    }
    if tokio::fs::canonicalize(existing).await?.starts_with(&root) {
        bail!("clone parent must be outside the source repository");
    }
    tokio::fs::create_dir_all(private_parent).await?;
    let metadata = tokio::fs::symlink_metadata(private_parent).await?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!("clone parent must be a real directory");
    }
    let parent = tokio::fs::canonicalize(private_parent)
        .await
        .context("cannot resolve private clone parent")?;
    if parent.starts_with(&root) {
        bail!("clone parent must be outside the source repository");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).await?;
    }
    #[cfg(not(unix))]
    bail!("isolated autonomous mode requires a private Unix state directory");
    let clone_parent = parent.join(Uuid::new_v4().to_string());
    let workspace_hash = Sha256::digest(source.as_os_str().as_encoded_bytes());
    let hash = workspace_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let recovery_marker = parent.join(format!("{hash}.recovery"));
    let mut marker = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&recovery_marker)
        .await
        .with_context(|| {
            format!(
                "cannot acquire autonomous recovery marker {}; inspect any previous run before removing it",
                recovery_marker.display()
            )
        })?;
    let clone_root = clone_parent.join("checkout");
    let result = async {
        let marker_content = serde_json::json!({
            "source_workspace": source,
            "clone_parent": clone_parent,
            "base_commit": base_commit,
        });
        marker
            .write_all(serde_json::to_string_pretty(&marker_content)?.as_bytes())
            .await?;
        marker.sync_all().await?;
        tokio::fs::File::open(&parent).await?.sync_all().await?;
        tokio::fs::create_dir(&clone_parent)
            .await
            .context("cannot create unique clone parent")?;
        git(
            &parent,
            &[
                "clone".as_ref(),
                "--no-local".as_ref(),
                "--no-hardlinks".as_ref(),
                "--no-checkout".as_ref(),
                "--".as_ref(),
                root.as_os_str(),
                clone_root.as_os_str(),
            ],
        )
        .await?;
        git(
            &clone_root,
            &[
                "checkout".as_ref(),
                "--detach".as_ref(),
                base_commit.as_ref(),
                "--".as_ref(),
            ],
        )
        .await?;
        git(
            &clone_root,
            &["remote".as_ref(), "remove".as_ref(), "origin".as_ref()],
        )
        .await?;
        let workspace = tokio::fs::canonicalize(clone_root.join(relative))
            .await
            .context("committed workspace is missing")?;
        if !workspace.starts_with(&clone_root) || !tokio::fs::metadata(&workspace).await?.is_dir() {
            bail!("committed workspace is not a directory inside the clone");
        }
        git(
            &workspace,
            &[
                "--literal-pathspecs".as_ref(),
                "ls-files".as_ref(),
                "--error-unmatch".as_ref(),
                "--".as_ref(),
                ledger.as_os_str(),
            ],
        )
        .await
        .context("ledger is not committed")?;
        let ledger_path = tokio::fs::canonicalize(workspace.join(ledger))
            .await
            .context("committed ledger is missing")?;
        if !ledger_path.starts_with(&workspace)
            || !tokio::fs::metadata(&ledger_path).await?.is_file()
        {
            bail!("committed ledger is not a regular file inside the workspace");
        }
        Ok(AutonomousCheckout {
            clone_root,
            workspace,
            base_commit,
            source_root: root,
            clone_parent: clone_parent.clone(),
            recovery_marker: recovery_marker.clone(),
        })
    }
    .await;
    result.with_context(|| {
        format!(
            "checkout preparation failed; artifacts retained at {}; recovery marker at {}",
            clone_parent.display(),
            recovery_marker.display()
        )
    })
}

impl AutonomousCheckout {
    pub fn clone_root(&self) -> &Path {
        &self.clone_root
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }
    pub fn clone_parent(&self) -> &Path {
        &self.clone_parent
    }
    pub fn recovery_marker(&self) -> &Path {
        &self.recovery_marker
    }

    /// Bind the clone to the source's explicit push destination, never to the source itself.
    pub async fn configure_remote(&self, remote: &str) -> Result<()> {
        if remote.is_empty() {
            return Ok(());
        }
        let output = git(
            &self.source_root,
            &[
                "remote".as_ref(),
                "get-url".as_ref(),
                "--push".as_ref(),
                "--all".as_ref(),
                "--".as_ref(),
                remote.as_ref(),
            ],
        )
        .await
        .context("cannot resolve configured push remote")?;
        let text = std::str::from_utf8(&output).context("remote URL is not UTF-8")?;
        let mut lines = text.lines();
        let url = lines.next().context("push remote has no URL")?;
        if url.is_empty() || lines.next().is_some() {
            bail!("push remote must have exactly one URL");
        }
        if url.starts_with("file:") {
            bail!("file URL push remotes are not supported for isolated runs; use a local path");
        }
        let local_path;
        let url = if !url.contains(':') {
            local_path = tokio::fs::canonicalize(self.source_root.join(url))
                .await
                .context("cannot resolve local push remote")?;
            if local_path.starts_with(&self.source_root) {
                bail!("push remote must not be inside the source repository");
            }
            local_path.as_os_str()
        } else {
            url.as_ref()
        };
        git(
            &self.clone_root,
            &[
                "remote".as_ref(),
                "add".as_ref(),
                "--".as_ref(),
                remote.as_ref(),
                url,
            ],
        )
        .await?;
        for key in ["user.name", "user.email"] {
            if let Ok(value) = git(
                &self.source_root,
                &["config".as_ref(), "--get".as_ref(), key.as_ref()],
            )
            .await
            {
                let value = std::str::from_utf8(&value)?.trim_end_matches('\n');
                git(
                    &self.clone_root,
                    &[
                        "config".as_ref(),
                        "--local".as_ref(),
                        key.as_ref(),
                        value.as_ref(),
                    ],
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    async fn run(dir: &Path, args: &[&str]) -> Vec<u8> {
        git(dir, &args.iter().map(OsStr::new).collect::<Vec<_>>())
            .await
            .unwrap()
    }

    async fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        tokio::fs::create_dir_all(root.join("nested/workspace"))
            .await
            .unwrap();
        run(&root, &["init"]).await;
        run(&root, &["config", "user.email", "test@example.invalid"]).await;
        run(&root, &["config", "user.name", "Test"]).await;
        tokio::fs::write(root.join("nested/workspace/TODO.md"), "committed ledger")
            .await
            .unwrap();
        tokio::fs::write(root.join("tracked"), "committed")
            .await
            .unwrap();
        tokio::fs::write(root.join(".gitignore"), "ignored\n")
            .await
            .unwrap();
        run(&root, &["add", "."]).await;
        run(&root, &["commit", "-m", "base"]).await;
        (dir, root)
    }

    #[tokio::test]
    async fn clone_contains_only_committed_content_and_preserves_source() {
        let (dir, root) = repo().await;
        tokio::fs::write(root.join("tracked"), "staged")
            .await
            .unwrap();
        run(&root, &["add", "tracked"]).await;
        tokio::fs::write(root.join("tracked"), "unstaged")
            .await
            .unwrap();
        tokio::fs::write(root.join("untracked"), "private")
            .await
            .unwrap();
        tokio::fs::write(root.join("ignored"), "secret")
            .await
            .unwrap();
        let before = run(&root, &["status", "--porcelain=v1", "--ignored"]).await;
        let index = tokio::fs::read(root.join(".git/index")).await.unwrap();
        let checkout = prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            dir.path(),
            Path::new("TODO.md"),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(checkout.clone_root.join("tracked"))
                .await
                .unwrap(),
            b"committed"
        );
        assert_eq!(
            tokio::fs::read(checkout.workspace.join("TODO.md"))
                .await
                .unwrap(),
            b"committed ledger"
        );
        assert!(!checkout.clone_root.join("untracked").exists());
        assert!(!checkout.clone_root.join("ignored").exists());
        assert_eq!(
            before,
            run(&root, &["status", "--porcelain=v1", "--ignored"]).await
        );
        assert_eq!(
            index,
            tokio::fs::read(root.join(".git/index")).await.unwrap()
        );
        assert_eq!(
            tokio::fs::read(root.join("tracked")).await.unwrap(),
            b"unstaged"
        );
        assert_eq!(
            run(&checkout.clone_root, &["rev-parse", "HEAD"]).await,
            format!("{}\n", checkout.base_commit).as_bytes()
        );
        assert!(git(
            &checkout.clone_root,
            &["symbolic-ref".as_ref(), "-q".as_ref(), "HEAD".as_ref()]
        )
        .await
        .is_err());
        assert!(checkout.recovery_marker.exists());
        assert!(prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            dir.path(),
            Path::new("TODO.md")
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn rejects_uncommitted_ledger() {
        let (dir, root) = repo().await;
        tokio::fs::write(root.join("nested/workspace/new.md"), "untracked")
            .await
            .unwrap();
        let error = prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            dir.path(),
            Path::new("new.md"),
        )
        .await
        .unwrap_err();
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut count = 0;
        let mut marker = None;
        while let Some(entry) = entries.next_entry().await.unwrap() {
            count += 1;
            if entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "recovery")
            {
                marker = Some(entry.path());
            }
        }
        assert_eq!(count, 3, "failed clone and recovery marker remain");
        let marker = marker.expect("recovery marker retained");
        assert!(error.to_string().contains(&marker.display().to_string()));
    }

    #[tokio::test]
    async fn rejects_clone_parent_inside_source_repository() {
        let (_dir, root) = repo().await;
        assert!(prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            &root,
            Path::new("TODO.md")
        )
        .await
        .is_err());
        assert!(prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            &root.join("new/state"),
            Path::new("TODO.md")
        )
        .await
        .is_err());
        assert!(!root.join("new").exists());
        assert!(run(&root, &["status", "--porcelain"]).await.is_empty());
    }

    #[tokio::test]
    async fn binds_explicit_remote_not_source_clone_origin() {
        let (dir, root) = repo().await;
        let remote = dir.path().join("remote.git");
        run(dir.path(), &["init", "--bare", remote.to_str().unwrap()]).await;
        run(&root, &["remote", "add", "origin", "../remote.git"]).await;
        let checkout = prepare_autonomous_checkout(
            &root.join("nested/workspace"),
            dir.path(),
            Path::new("TODO.md"),
        )
        .await
        .unwrap();
        assert!(run(&checkout.clone_root, &["remote"]).await.is_empty());
        checkout.configure_remote("origin").await.unwrap();
        assert_eq!(
            run(&checkout.clone_root, &["remote", "get-url", "origin"]).await,
            format!(
                "{}\n",
                tokio::fs::canonicalize(&remote).await.unwrap().display()
            )
            .as_bytes()
        );
    }
}
