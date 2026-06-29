//! Copy release binaries into a user-writable prefix (default `~/.local/bin`).

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "unthinkclaw-install", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Install {
        #[arg(long, default_value = "~/.local/bin")]
        dest: String,
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    Uninstall {
        #[arg(long, default_value = "~/.local/bin")]
        dest: String,
    },
}

fn expand_dest(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(p)
}

fn copy_exe(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::copy(src, dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dst)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(dst, perms)?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Install { dest, binary } => {
            let dest = expand_dest(&dest);
            fs::create_dir_all(&dest)?;
            let src = binary.unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("unthinkclaw")))
                    .unwrap_or_else(|| PathBuf::from("target/release/unthinkclaw"))
            });
            if !src.is_file() {
                anyhow::bail!("binary not found at {}", src.display());
            }
            let dst = dest.join("unthinkclaw");
            copy_exe(&src, &dst)?;
            println!("Installed {}", dst.display());
        }
        Cmd::Uninstall { dest } => {
            let dest = expand_dest(&dest);
            for name in ["unthinkclaw", "unthinkclaw-install"] {
                let p = dest.join(name);
                if p.is_file() {
                    fs::remove_file(&p)?;
                    println!("Removed {}", p.display());
                }
            }
        }
    }
    Ok(())
}
