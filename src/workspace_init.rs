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
    install_agent_plugin_stubs(workspace)?;
    Ok(())
}

const RS_GBRAIN_SKILL: &str = "---\nname: rs_gbrain\ndescription: Local hybrid RAG brain — search before people/company questions; brain_put to remember.\n---\n\nUse `brain_search` / `rs_gbrain search`. DB ~/.rs_gbrain/brain.db.\n";
const RS_GBRAIN_HERMES: &str =
    r#"{"id":"rs_gbrain","name":"rs_gbrain","description":"Local brain","tools":[]}"#;

pub fn install_agent_plugin_stubs(workspace: &Path) -> std::io::Result<()> {
    let plug = workspace.join("plugins/rs_gbrain");
    std::fs::create_dir_all(&plug)?;
    write_if_missing(&plug.join("SKILL.md"), RS_GBRAIN_SKILL)?;
    write_if_missing(&plug.join("plugin.json"), RS_GBRAIN_HERMES)?;
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
