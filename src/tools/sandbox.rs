use std::path::{Component, Path, PathBuf};

fn workspace_canonical(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

fn validate_within_workspace(
    workspace: &Path,
    resolved: &Path,
    original: &str,
) -> anyhow::Result<PathBuf> {
    let canonical_workspace = workspace_canonical(workspace);
    if !resolved.starts_with(&canonical_workspace) {
        anyhow::bail!(
            "Access denied: path '{}' is outside the workspace.",
            original
        );
    }
    Ok(resolved.to_path_buf())
}

/// Files inside the workspace that hold apollo's own credentials or runtime
/// configuration. Prompt injection reaches the file tools, so these are denied
/// for both reads and writes even though they sit within the boundary.
const DENIED_FILE_NAMES: &[&str] = &[".env", "apollo.json"];

/// Name prefixes denied the same way — `.env.local`, `.env.production`, …
const DENIED_FILE_PREFIXES: &[&str] = &[".env."];

/// Directories denied anywhere in a resolved path, relative to the home
/// directory. `~/.apollo` holds the HTTP token that drives the agent.
const DENIED_HOME_DIRS: &[&str] = &[".apollo"];

fn ensure_not_denied(resolved: &Path, original: &str) -> anyhow::Result<()> {
    if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
        let denied = DENIED_FILE_NAMES.contains(&name)
            || DENIED_FILE_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix));
        if denied {
            anyhow::bail!(
                "Access denied: '{}' holds apollo's own credentials or configuration.",
                original
            );
        }
    }

    if let Some(home) = dirs::home_dir() {
        for dir in DENIED_HOME_DIRS {
            let denied_root = home.join(dir);
            if resolved.starts_with(&denied_root) {
                anyhow::bail!(
                    "Access denied: '{}' is inside {}, which holds apollo's own credentials.",
                    original,
                    denied_root.display()
                );
            }
        }
    }

    Ok(())
}

fn resolve_write_candidate(requested: &Path) -> PathBuf {
    let mut prefix = if requested.is_absolute() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        PathBuf::new()
    };
    let mut canonical_prefix = prefix.clone();
    let mut remainder = PathBuf::new();
    let mut saw_missing_component = false;

    for component in requested.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => {
                if saw_missing_component {
                    remainder.pop();
                } else {
                    prefix.pop();
                    if prefix.exists() {
                        canonical_prefix = prefix.canonicalize().unwrap_or_else(|_| prefix.clone());
                    }
                }
            }
            Component::Normal(part) => {
                if saw_missing_component {
                    remainder.push(part);
                    continue;
                }

                prefix.push(part);
                if prefix.exists() {
                    canonical_prefix = prefix.canonicalize().unwrap_or_else(|_| prefix.clone());
                } else {
                    saw_missing_component = true;
                    remainder.push(part);
                }
            }
        }
    }

    if saw_missing_component {
        canonical_prefix.join(remainder)
    } else {
        canonical_prefix
    }
}

pub fn resolve_workspace_existing_path(
    workspace: &Path,
    raw_path: &str,
) -> anyhow::Result<PathBuf> {
    let path = raw_path.trim();
    if path.is_empty() {
        anyhow::bail!("Path is required");
    }
    if path.starts_with('~') {
        anyhow::bail!("Home directory expansion (~) is disabled for security.");
    }

    let requested = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    let resolved = requested
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("Cannot resolve '{}': {}", raw_path, e))?;

    ensure_not_denied(&resolved, raw_path)?;
    validate_within_workspace(workspace, &resolved, raw_path)
}

pub fn resolve_workspace_write_path(workspace: &Path, raw_path: &str) -> anyhow::Result<PathBuf> {
    let path = raw_path.trim();
    if path.is_empty() {
        anyhow::bail!("Path is required");
    }
    if path.starts_with('~') {
        anyhow::bail!("Home directory expansion (~) is disabled for security.");
    }

    let requested = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    };
    let resolved = resolve_write_candidate(&requested);
    ensure_not_denied(&resolved, raw_path)?;
    validate_within_workspace(workspace, &resolved, raw_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let err = resolve_workspace_write_path(&ws, "/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("outside the workspace"));
    }

    #[test]
    fn refuses_to_read_workspace_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        for name in [".env", ".env.local", "apollo.json"] {
            std::fs::write(ws.join(name), "secret").unwrap();
            let err = resolve_workspace_existing_path(&ws, name).unwrap_err();
            assert!(err.to_string().contains("Access denied"), "{name}: {err}");
        }
    }

    #[test]
    fn refuses_to_write_workspace_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        for name in [".env", ".env.production", "apollo.json", "sub/.env"] {
            let err = resolve_workspace_write_path(&ws, name).unwrap_err();
            assert!(err.to_string().contains("Access denied"), "{name}: {err}");
        }
    }

    #[test]
    fn allows_ordinary_workspace_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("notes.md"), "hello").unwrap();
        resolve_workspace_existing_path(&ws, "notes.md").unwrap();
        resolve_workspace_write_path(&ws, "sub/new.txt").unwrap();
        resolve_workspace_write_path(&ws, "environment.json").unwrap();
    }

    #[test]
    fn rejects_relative_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        let outside = tmp.path().join("secrets.txt");
        std::fs::write(&outside, "secret").unwrap();
        let err = resolve_workspace_existing_path(&ws, "../secrets.txt").unwrap_err();
        assert!(err.to_string().contains("outside the workspace"));
    }
}
