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
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_ensure_workspace_kit_creates_files() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        ensure_workspace_kit(workspace).expect("failed to ensure workspace kit");

        let files_to_check = vec![
            ("SOUL.md", SOUL),
            ("USER.md", USER),
            ("IDENTITY.md", IDENTITY),
            ("MEMORY.md", MEMORY),
            ("HEARTBEAT.md", HEARTBEAT),
            ("NOW.md", NOW),
            ("memory/open-loops.md", BRIEF_LOOPS),
            ("memory/time-contexts.md", BRIEF_TIME),
        ];

        for (file, expected_content) in files_to_check {
            let path = workspace.join(file);
            assert!(path.exists(), "File {} should have been created", file);
            let content = fs::read_to_string(&path).expect("failed to read file");
            assert_eq!(content, expected_content);
        }
    }

    #[test]
    fn test_ensure_workspace_kit_does_not_overwrite() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();

        // Create a custom SOUL.md before calling ensure_workspace_kit
        let custom_soul_content = "This is a custom soul.";
        fs::write(workspace.join("SOUL.md"), custom_soul_content).unwrap();

        ensure_workspace_kit(workspace).expect("failed to ensure workspace kit");

        // Verify that SOUL.md still has the custom content
        let soul_path = workspace.join("SOUL.md");
        let current_soul_content = fs::read_to_string(&soul_path).expect("failed to read SOUL.md");
        assert_eq!(current_soul_content, custom_soul_content);

        // Verify that other files were created
        assert!(workspace.join("USER.md").exists());
        assert!(workspace.join("memory/open-loops.md").exists());
    }
}
