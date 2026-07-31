use std::io::Write;
use std::path::Path;

/// Write a secret to disk so that it is never, even briefly, readable by
/// anyone but its owner.
///
/// Opening the destination directly is not enough: `mode` applies only when
/// the file is created, so rewriting a pre-existing 0644 file would hold the
/// new secret at 0644 for the whole write. Instead the content goes to a
/// fresh 0600 temporary file in the same directory and is renamed over the
/// destination — which also makes the replacement atomic, so a crash or a
/// full disk leaves the previous contents rather than a truncated file.
pub fn write_secret_file(path: &Path, content: &str) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("cannot write a secret to {}", path.display()))?;
    let temp = dir.join(format!(".{}.{}.tmp", file_name, std::process::id()));

    // Best effort: a temp file left by an earlier crash must not be reused
    // with whatever mode it carries.
    let _ = std::fs::remove_file(&temp);

    let result = (|| -> anyhow::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_file_is_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        write_secret_file(&path, "value").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn rewriting_a_loose_file_tightens_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        std::fs::write(&path, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_secret_file(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// The final mode is not the property that matters — a reader racing the
    /// write must never see the new secret at a loose mode. Assert that the
    /// inode carrying the new content was never the old, world-readable one.
    #[cfg(unix)]
    #[test]
    fn the_new_secret_never_lives_in_the_old_loose_inode() {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let old_inode = std::fs::metadata(&path).unwrap().ino();

        // An open handle to the old inode is exactly what a racing reader
        // holds. It must keep seeing the old content, never the new secret.
        let stale = std::fs::File::open(&path).unwrap();

        write_secret_file(&path, "new-secret").unwrap();

        let new_inode = std::fs::metadata(&path).unwrap().ino();
        assert_ne!(
            old_inode, new_inode,
            "the secret must land in a fresh 0600 inode, not the existing 0644 one"
        );
        let mut through_stale = String::new();
        {
            use std::io::Read;
            let mut stale = stale;
            stale.read_to_string(&mut through_stale).unwrap();
        }
        assert_eq!(
            through_stale, "old",
            "a handle opened before the write must never observe the new secret"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_leftover_temp_file_does_not_taint_the_write() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("secret");
        let stale_temp = tmp
            .path()
            .join(format!(".secret.{}.tmp", std::process::id()));
        std::fs::write(&stale_temp, "junk").unwrap();
        std::fs::set_permissions(&stale_temp, std::fs::Permissions::from_mode(0o666)).unwrap();

        write_secret_file(&path, "value").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
