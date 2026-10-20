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
    fn test_ensure_workspace_kit_empty() -> std::io::Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path();

        ensure_workspace_kit(workspace)?;

        assert!(workspace.join("SOUL.md").exists());
        assert_eq!(std::fs::read_to_string(workspace.join("SOUL.md"))?, SOUL);

        assert!(workspace.join("USER.md").exists());
        assert!(workspace.join("IDENTITY.md").exists());
        assert!(workspace.join("MEMORY.md").exists());
        assert!(workspace.join("HEARTBEAT.md").exists());
        assert!(workspace.join("NOW.md").exists());

        let mem = workspace.join("memory");
        assert!(mem.is_dir());
        assert!(mem.join("open-loops.md").exists());
        assert!(mem.join("time-contexts.md").exists());

        Ok(())
    }

    #[test]
    fn test_ensure_workspace_kit_existing() -> std::io::Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path();

        // Pre-create some files with custom content
        let soul_path = workspace.join("SOUL.md");
        let custom_soul = "Custom SOUL";
        std::fs::write(&soul_path, custom_soul)?;

        let loops_path = workspace.join("memory/open-loops.md");
        std::fs::create_dir_all(loops_path.parent().unwrap())?;
        let custom_loops = "Custom Loops";
        std::fs::write(&loops_path, custom_loops)?;

        ensure_workspace_kit(workspace)?;

        // Existing files should not be overwritten
        assert_eq!(std::fs::read_to_string(&soul_path)?, custom_soul);
        assert_eq!(std::fs::read_to_string(&loops_path)?, custom_loops);

        // Missing files should be created
        assert!(workspace.join("USER.md").exists());
        assert!(workspace.join("NOW.md").exists());

        Ok(())
    }
}
