//! Session-end notes → workspace memory/YYYY-MM-DD.md

use std::path::Path;

pub fn daily_note_path(workspace: &Path, day: chrono::NaiveDate) -> std::path::PathBuf {
    workspace
        .join("memory")
        .join(format!("{}.md", day.format("%Y-%m-%d")))
}

pub fn append_session_note(workspace: &Path, chat_id: &str, line: &str) -> std::io::Result<()> {
    let dir = workspace.join("memory");
    std::fs::create_dir_all(&dir)?;
    let path = daily_note_path(workspace, chrono::Utc::now().date_naive());
    let stamp = chrono::Utc::now().format("%H:%M");
    let entry = format!("\n- [{stamp} UTC] chat `{chat_id}`: {line}\n");
    use std::io::Write;
    if path.exists() {
        let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
        f.write_all(entry.as_bytes())?;
    } else {
        let header = format!(
            "# Session notes {}\n",
            chrono::Utc::now().format("%Y-%m-%d")
        );
        std::fs::write(&path, format!("{header}{entry}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_path_format() {
        let p = daily_note_path(
            Path::new("/w"),
            chrono::NaiveDate::from_ymd_opt(2026, 6, 20).unwrap(),
        );
        assert!(p.to_string_lossy().ends_with("2026-06-20.md"));
    }
}
