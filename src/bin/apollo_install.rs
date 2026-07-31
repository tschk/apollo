//! Copy release binaries into a user-writable prefix (default `~/.local/bin`).

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "apollo-install", version)]
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

fn path_hint(dest: &Path) {
    let in_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p == dest))
        .unwrap_or(false);
    if in_path {
        return;
    }
    eprintln!();
    eprintln!("Add to PATH (e.g. in ~/.bashrc or ~/.zshrc):");
    eprintln!("  export PATH=\"{}:$PATH\"", dest.display());
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
                    .and_then(|p| p.parent().map(|d| d.join("apollo")))
                    .unwrap_or_else(|| PathBuf::from("target/release/apollo"))
            });
            if !src.is_file() {
                anyhow::bail!("binary not found at {}", src.display());
            }
            let dst = dest.join("apollo");
            copy_exe(&src, &dst)?;
            println!("Installed {}", dst.display());
            if let Some(parent) = src.parent() {
                // apollo-tui matters here: `apollo` with no arguments opens the
                // TUI only when this binary is alongside it, and otherwise
                // falls back to the line-based chat.
                for name in ["apollo-install", "apollo-tui"] {
                    let sibling = parent.join(name);
                    if sibling.is_file() {
                        let sdst = dest.join(name);
                        copy_exe(&sibling, &sdst)?;
                        println!("Installed {}", sdst.display());
                    } else if name == "apollo-tui" {
                        println!(
                            "Skipped apollo-tui (not built) — `cargo build --release -p apollo-tui`"
                        );
                    }
                }
            }
            path_hint(&dest);
        }
        Cmd::Uninstall { dest } => {
            let dest = expand_dest(&dest);
            for name in ["apollo", "apollo-install", "apollo-tui"] {
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
