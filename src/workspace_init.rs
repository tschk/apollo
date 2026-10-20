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

    #[test]
    fn test_ensure_workspace_kit_creates_files() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        ensure_workspace_kit(workspace).unwrap();

        assert_eq!(
            std::fs::read_to_string(workspace.join("SOUL.md")).unwrap(),
            SOUL
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("USER.md")).unwrap(),
            USER
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("IDENTITY.md")).unwrap(),
            IDENTITY
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("MEMORY.md")).unwrap(),
            MEMORY
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("HEARTBEAT.md")).unwrap(),
            HEARTBEAT
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOW.md")).unwrap(),
            NOW
        );

        let mem = workspace.join("memory");
        assert!(mem.exists() && mem.is_dir());
        assert_eq!(
            std::fs::read_to_string(mem.join("open-loops.md")).unwrap(),
            BRIEF_LOOPS
        );
        assert_eq!(
            std::fs::read_to_string(mem.join("time-contexts.md")).unwrap(),
            BRIEF_TIME
        );
    }

    #[test]
    fn test_ensure_workspace_kit_does_not_overwrite() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Create some files with custom content
        let custom_soul = "Custom SOUL";
        let custom_time = "Custom TIME";

        std::fs::write(workspace.join("SOUL.md"), custom_soul).unwrap();

        let mem = workspace.join("memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("time-contexts.md"), custom_time).unwrap();

        // Run ensure_workspace_kit
        ensure_workspace_kit(workspace).unwrap();

        // Check that custom files are NOT overwritten
        assert_eq!(
            std::fs::read_to_string(workspace.join("SOUL.md")).unwrap(),
            custom_soul
        );
        assert_eq!(
            std::fs::read_to_string(mem.join("time-contexts.md")).unwrap(),
            custom_time
        );

        // Check that other files ARE created
        assert_eq!(
            std::fs::read_to_string(workspace.join("USER.md")).unwrap(),
            USER
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("IDENTITY.md")).unwrap(),
            IDENTITY
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("MEMORY.md")).unwrap(),
            MEMORY
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("HEARTBEAT.md")).unwrap(),
            HEARTBEAT
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("NOW.md")).unwrap(),
            NOW
        );
        assert_eq!(
            std::fs::read_to_string(mem.join("open-loops.md")).unwrap(),
            BRIEF_LOOPS
        );
    }
}
