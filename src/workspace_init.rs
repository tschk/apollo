//! Workspace identity kit — minimal templates on first run.

use std::path::Path;

const SOUL: &str = "# SOUL\n\nBe direct, capable, and honest. Adapt tone to the user.\n";
const USER: &str = "# USER\n\n(Add preferences, timezone, projects.)\n";
const IDENTITY: &str = "# IDENTITY\n\nName: (assistant)\n";
const MEMORY: &str = "# MEMORY\n\nCurated long-term notes. Use memory/*.md for daily logs.\n";
const HEARTBEAT: &str =
    "# HEARTBEAT\n\n<!-- Add `- [ ]` tasks the agent should check periodically -->\n";
const NOW: &str = "# NOW\n\nCurrent focus and active threads.\n";
const BRIEF_LOOPS: &str = "# Open loops\n\n- (none yet)\n";
const BRIEF_TIME: &str = "# Time contexts\n\n- (none yet)\n";

pub fn ensure_workspace_kit(workspace: &Path) -> std::io::Result<()> {
    write_if_missing(&workspace.join("SOUL.md"), SOUL)?;
    write_if_missing(&workspace.join("USER.md"), USER)?;
    write_if_missing(&workspace.join("IDENTITY.md"), IDENTITY)?;
    write_if_missing(&workspace.join("MEMORY.md"), MEMORY)?;
    write_if_missing(&workspace.join("HEARTBEAT.md"), HEARTBEAT)?;
    write_if_missing(&workspace.join("NOW.md"), NOW)?;
    let mem = workspace.join("memory");
    std::fs::create_dir_all(&mem)?;
    write_if_missing(&mem.join("open-loops.md"), BRIEF_LOOPS)?;
    write_if_missing(&mem.join("time-contexts.md"), BRIEF_TIME)?;
    Ok(())
}

fn write_if_missing(path: &Path, content: &str) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn test_ensure_workspace_kit_creates_files() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();
        ensure_workspace_kit(workspace).unwrap();

        assert!(workspace.join("SOUL.md").exists());
        assert!(workspace.join("USER.md").exists());
        assert!(workspace.join("IDENTITY.md").exists());
        assert!(workspace.join("MEMORY.md").exists());
        assert!(workspace.join("HEARTBEAT.md").exists());
        assert!(workspace.join("NOW.md").exists());
        assert!(workspace.join("memory").join("open-loops.md").exists());
        assert!(workspace.join("memory").join("time-contexts.md").exists());

        assert_eq!(fs::read_to_string(workspace.join("SOUL.md")).unwrap(), SOUL);
        assert_eq!(fs::read_to_string(workspace.join("USER.md")).unwrap(), USER);
        assert_eq!(fs::read_to_string(workspace.join("IDENTITY.md")).unwrap(), IDENTITY);
        assert_eq!(fs::read_to_string(workspace.join("MEMORY.md")).unwrap(), MEMORY);
        assert_eq!(fs::read_to_string(workspace.join("HEARTBEAT.md")).unwrap(), HEARTBEAT);
        assert_eq!(fs::read_to_string(workspace.join("NOW.md")).unwrap(), NOW);
        assert_eq!(fs::read_to_string(workspace.join("memory").join("open-loops.md")).unwrap(), BRIEF_LOOPS);
        assert_eq!(fs::read_to_string(workspace.join("memory").join("time-contexts.md")).unwrap(), BRIEF_TIME);
    }

    #[test]
    fn test_ensure_workspace_kit_idempotency() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // First run
        ensure_workspace_kit(workspace).unwrap();

        // Modify a file
        fs::write(workspace.join("SOUL.md"), "MODIFIED SOUL").unwrap();

        // Second run
        ensure_workspace_kit(workspace).unwrap();

        // Ensure it wasn't overwritten
        assert_eq!(fs::read_to_string(workspace.join("SOUL.md")).unwrap(), "MODIFIED SOUL");
    }
}
