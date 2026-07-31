//! First-run setup wizard: authentication, model, channel, workspace.

use std::io::Write;
use std::path::{Path, PathBuf};

use apollo::config::{apply_permission_profile, Config};
use apollo::providers::defaults::default_model_for_provider;

#[derive(Debug, Default)]
pub struct InitOptions {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub channel: Option<String>,
    pub telegram_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub discord_token: Option<String>,
    pub discord_channel_id: Option<String>,
    pub model: Option<String>,
    pub start: bool,
    pub workspace: Option<PathBuf>,
    pub permission_profile: Option<String>,
    pub force: bool,
}

struct Auth {
    provider: String,
    api_key: String,
    base_url: Option<String>,
}

fn read_line(prompt: &str) -> anyhow::Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let mut buf = String::new();
    // A closed stdin must end the wizard, not spin the prompt loop forever.
    if std::io::stdin().read_line(&mut buf)? == 0 {
        anyhow::bail!("setup cancelled (end of input)");
    }
    Ok(buf.trim().to_string())
}

fn read_with_default(prompt: &str, default: &str) -> anyhow::Result<String> {
    let answer = read_line(&format!("{prompt} [{default}]: "))?;
    if answer.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(answer)
    }
}

/// Read a credential without echoing it to the terminal.
///
/// The value is never printed back, logged, or included in the summary — only
/// the fact that one was captured.
fn read_secret(prompt: &str) -> anyhow::Result<String> {
    eprint!("{prompt}");
    std::io::stderr().flush().ok();
    let restore = disable_echo();
    let mut buf = String::new();
    let read = std::io::stdin().read_line(&mut buf);
    restore();
    eprintln!();
    if read? == 0 {
        anyhow::bail!("setup cancelled (end of input)");
    }
    Ok(buf.trim().to_string())
}

#[cfg(unix)]
fn disable_echo() -> Box<dyn FnOnce()> {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();
    let mut term = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, term.as_mut_ptr()) } != 0 {
        return Box::new(|| {});
    }
    let original = unsafe { term.assume_init() };
    let mut quiet = original;
    quiet.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) } != 0 {
        return Box::new(|| {});
    }
    Box::new(move || {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    })
}

#[cfg(not(unix))]
fn disable_echo() -> Box<dyn FnOnce()> {
    Box::new(|| {})
}

#[cfg(unix)]
fn stdin_is_terminal() -> bool {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::isatty(std::io::stdin().as_raw_fd()) == 1 }
}

#[cfg(not(unix))]
fn stdin_is_terminal() -> bool {
    true
}

fn compiled_in_providers() -> Vec<&'static str> {
    let mut names = vec![
        "cerebras",
        "cloudflare",
        "deepseek",
        "fireworks",
        "groq",
        "huggingface",
        "minimax",
        "mistral",
        "moonshot",
        "openai",
        "openrouter",
        "perplexity",
        "siliconflow",
        "together",
        "venice",
        "vercel",
        "xai",
    ];
    #[cfg(feature = "provider-anthropic")]
    names.push("anthropic");
    #[cfg(feature = "provider-copilot")]
    names.push("copilot");
    #[cfg(feature = "provider-ollama")]
    names.push("ollama");
    names.sort();
    names.dedup();
    names
}

fn get_provider_matches<'a>(all: &[&'a str], filter: &str) -> Vec<&'a str> {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        all.to_vec()
    } else {
        all.iter()
            .copied()
            .filter(|n| n.to_lowercase().contains(&needle))
            .collect()
    }
}

fn print_provider_matches(matches: &[&str]) {
    if matches.is_empty() {
        println!("  (no matches — try another filter)");
    } else {
        println!("\n  Matching providers:");
        for (i, n) in matches.iter().enumerate() {
            println!("    [{}] {}", i + 1, n);
        }
    }
}

fn parse_provider_selection(input: &str, matches: &[&str], all: &[&str]) -> Option<String> {
    if input.is_empty() {
        if matches.len() == 1 {
            return Some(matches[0].to_string());
        }
        return None;
    }
    if let Ok(idx) = input.parse::<usize>() {
        if (1..=matches.len()).contains(&idx) {
            return Some(matches[idx - 1].to_string());
        }
    }
    if let Some(found) = matches.iter().find(|n| n.eq_ignore_ascii_case(input)) {
        return Some((*found).to_string());
    }
    if let Some(found) = all.iter().find(|n| n.eq_ignore_ascii_case(input)) {
        return Some((*found).to_string());
    }
    None
}

fn prompt_provider_interactive() -> anyhow::Result<String> {
    let all = compiled_in_providers();
    println!("  Choose a provider (type to filter the list, then pick a number or exact name):");
    let mut filter = String::new();
    loop {
        let matches = get_provider_matches(&all, &filter);
        print_provider_matches(&matches);

        let line = read_line("  Filter, #, or exact name (empty = pick if exactly one match): ")?;
        let t = line.trim();

        if let Some(selection) = parse_provider_selection(t, &matches, &all) {
            return Ok(selection);
        }

        if !t.is_empty() {
            filter = t.to_string();
        }
    }
}

fn prompt_permission_profile_interactive() -> anyhow::Result<String> {
    println!("\n  Permission profile:");
    println!("    full        — autonomous mode (no plan approval; shell and dynamic tools on)");
    println!("    auto        — default heuristics with shell enabled");
    println!("    prompt      — approve plans before executing tools (not per-tool prompts)");
    println!("    tools_only  — web + memory + session tools only (no shell or file writes)");
    let buf = read_line("  Choose [full / auto / prompt / tools_only] [auto]: ")?;
    let s = buf.trim();
    if s.is_empty() {
        return Ok("auto".to_string());
    }
    Ok(s.to_string())
}

/// The authentication methods this build can actually use, in the order the
/// wizard offers them.
fn auth_methods() -> Vec<(&'static str, &'static str)> {
    let mut methods: Vec<(&'static str, &'static str)> = Vec::new();
    #[cfg(feature = "provider-anthropic")]
    {
        methods.push((
            "anthropic-oauth",
            "Anthropic — Claude subscription sign-in (OAuth, no API key)",
        ));
        methods.push(("anthropic-key", "Anthropic — API key"));
    }
    #[cfg(feature = "provider-copilot")]
    methods.push((
        "copilot-oauth",
        "GitHub Copilot — existing GitHub OAuth login",
    ));
    #[cfg(feature = "provider-ollama")]
    methods.push(("ollama", "Ollama — local models, no key needed"));
    methods.push((
        "openai-compat",
        "OpenAI-compatible endpoint — API key (+ optional base URL)",
    ));
    methods
}

fn anthropic_oauth_present() -> bool {
    apollo::providers::oauth::load_oauth_token_from_file().is_ok()
}

fn prompt_auth_interactive(default_provider: &str) -> anyhow::Result<Auth> {
    let methods = auth_methods();
    println!("  How should apollo authenticate?\n");
    for (i, (id, label)) in methods.iter().enumerate() {
        let note = if *id == "anthropic-oauth" {
            if anthropic_oauth_present() {
                "  (existing Claude login detected)"
            } else {
                "  (requires an existing Claude Code login)"
            }
        } else {
            ""
        };
        println!("    [{}] {}{}", i + 1, label, note);
    }
    let default_index = methods
        .iter()
        .position(|(id, _)| id.starts_with(default_provider))
        .map(|i| i + 1)
        .unwrap_or(1);

    let choice = loop {
        let answer = read_with_default("\n  Choose", &default_index.to_string())?;
        match answer.parse::<usize>() {
            Ok(i) if (1..=methods.len()).contains(&i) => break methods[i - 1].0,
            _ => println!("  Pick a number between 1 and {}.", methods.len()),
        }
    };

    match choice {
        "anthropic-oauth" => {
            if !anthropic_oauth_present() {
                println!(
                    "\n  No Claude credentials found. Sign in with Claude Code (`claude`) first,\n  \
                     then re-run `apollo init`. Continuing with OAuth selected."
                );
            }
            Ok(Auth {
                provider: "anthropic".to_string(),
                api_key: String::new(),
                base_url: None,
            })
        }
        "anthropic-key" => {
            let key = read_secret("  Anthropic API key (input hidden): ")?;
            if key.is_empty() {
                anyhow::bail!("An API key is required for the API-key method");
            }
            Ok(Auth {
                provider: "anthropic".to_string(),
                api_key: key,
                base_url: None,
            })
        }
        "copilot-oauth" => {
            println!("\n  apollo reuses the GitHub token from your existing Copilot login.");
            let key = read_secret("  GitHub token (leave empty to use the stored login): ")?;
            Ok(Auth {
                provider: "copilot".to_string(),
                api_key: key,
                base_url: None,
            })
        }
        "ollama" => {
            let url = read_with_default("  Ollama base URL", "http://localhost:11434")?;
            Ok(Auth {
                provider: "ollama".to_string(),
                api_key: String::new(),
                base_url: Some(url),
            })
        }
        _ => {
            println!("\n  Pick the endpoint apollo should talk to.");
            let provider = prompt_provider_interactive()?;
            let key = read_secret(&format!("  {provider} API key (input hidden): "))?;
            let base_url = read_line("  Custom base URL (empty = provider default): ")?;
            Ok(Auth {
                provider,
                api_key: key,
                base_url: if base_url.is_empty() {
                    None
                } else {
                    Some(base_url)
                },
            })
        }
    }
}

fn prompt_channel_interactive(default_channel: &str) -> anyhow::Result<String> {
    println!("\n  Where should apollo listen?\n");
    println!("    [1] cli       — terminal only (TUI / line chat)");
    println!("    [2] telegram  — Telegram bot");
    println!("    [3] discord   — Discord bot");
    let default_index = match default_channel {
        "telegram" => "2",
        "discord" => "3",
        _ => "1",
    };
    loop {
        let answer = read_with_default("\n  Choose", default_index)?;
        match answer.as_str() {
            "1" | "cli" => return Ok("cli".to_string()),
            "2" | "telegram" => return Ok("telegram".to_string()),
            "3" | "discord" => return Ok("discord".to_string()),
            _ => println!("  Pick 1, 2, or 3."),
        }
    }
}

/// Run the setup wizard and write `apollo.json`, `.env`, `run.sh`, and a user
/// systemd unit. Returns the path of the config that was written.
pub async fn run_init(opts: InitOptions) -> anyhow::Result<PathBuf> {
    println!("🐾 apollo setup\n");

    let existing = Config::load("apollo.json").ok();
    let interactive = opts.provider.is_none();

    // === Resolve values (flags or interactive prompts) ===
    let auth = match opts.provider {
        Some(p) if !p.trim().is_empty() => {
            let provider = p.trim().to_string();
            let api_key = match opts.api_key {
                Some(k) => k,
                None if provider == "ollama" => String::new(),
                None => read_secret(&format!("  API key ({provider}, input hidden): "))?,
            };
            Auth {
                base_url: if provider == "ollama" {
                    Some("http://localhost:11434".to_string())
                } else {
                    None
                },
                provider,
                api_key,
            }
        }
        _ => {
            let default_provider = existing
                .as_ref()
                .map(|c| c.provider.name.clone())
                .unwrap_or_else(|| "anthropic".to_string());
            prompt_auth_interactive(&default_provider)?
        }
    };
    let provider = auth.provider.clone();
    let api_key = auth.api_key.clone();

    let default_model = default_model_for_provider(&provider)
        .map(|m| m.to_string())
        .or_else(|| existing.as_ref().map(|c| c.model.clone()))
        .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
    let model = match opts.model {
        Some(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ if interactive => read_with_default("\n  Model", &default_model)?,
        _ => default_model,
    };

    let channel = match opts.channel {
        Some(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ if interactive => {
            let default_channel = existing
                .as_ref()
                .map(|c| c.channel.kind.clone())
                .unwrap_or_else(|| "cli".to_string());
            prompt_channel_interactive(&default_channel)?
        }
        _ => "cli".to_string(),
    };

    // Channel-specific tokens
    let tg_token = if channel == "telegram" {
        match opts.telegram_token {
            Some(t) => Some(t),
            None => {
                let t = read_secret("  Telegram bot token (input hidden): ")?;
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            }
        }
    } else {
        opts.telegram_token
    };

    let tg_chat_id = if channel == "telegram" {
        match opts.telegram_chat_id {
            Some(c) => Some(c),
            None => {
                let c = read_line("  Telegram chat ID: ")?;
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            }
        }
    } else {
        opts.telegram_chat_id
    };

    let dc_token = if channel == "discord" {
        match opts.discord_token {
            Some(t) => Some(t),
            None => {
                let t = read_secret("  Discord bot token (input hidden): ")?;
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            }
        }
    } else {
        None
    };
    let dc_channel = if channel == "discord" {
        match opts.discord_channel_id {
            Some(c) => Some(c),
            None => {
                let c = read_line("  Discord channel ID: ")?;
                if c.is_empty() {
                    None
                } else {
                    Some(c)
                }
            }
        }
    } else {
        None
    };

    if channel == "telegram" && tg_token.is_none() {
        anyhow::bail!("Telegram channel requires a bot token (use --telegram-token or enter it when prompted)");
    }
    if channel == "discord" && dc_token.is_none() {
        anyhow::bail!(
            "Discord channel requires a bot token (use --discord-token or enter it when prompted)"
        );
    }

    let default_workspace = existing
        .as_ref()
        .map(|c| c.workspace.clone())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = match opts.workspace {
        Some(w) => w,
        None if interactive => PathBuf::from(read_with_default(
            "\n  Workspace directory",
            &default_workspace.display().to_string(),
        )?),
        None => default_workspace,
    };
    std::fs::create_dir_all(&workspace)?;

    let permission_profile = match opts.permission_profile {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        Some(_) | None => prompt_permission_profile_interactive()?,
    };

    let config_path = workspace.join("apollo.json");
    if config_path.exists() && !opts.force {
        let answer = read_with_default(
            &format!("\n  {} already exists. Overwrite?", config_path.display()),
            "n",
        )?;
        if !matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("  Left the existing config in place.");
            return Ok(config_path);
        }
    }

    // === Validate ===
    let client = reqwest::Client::new();
    match provider.as_str() {
        "ollama" => {
            print!("\n  Validating Ollama... ");
            let base_url = auth.base_url.as_deref().unwrap_or("http://localhost:11434");
            let resp = client.get(format!("{}/api/tags", base_url)).send().await;
            match resp {
                Ok(r) if r.status().is_success() => println!("✅"),
                Ok(r) => println!("⚠️  HTTP {} from local Ollama", r.status()),
                Err(e) => println!("❌ {}", e),
            }
        }
        "anthropic" | "claude" if !api_key.is_empty() => {
            print!("\n  Validating API key... ");
            let is_oauth = api_key.contains("sk-ant-oat");
            let auth_resp = if is_oauth {
                client
                    .get("https://api.anthropic.com/v1/models")
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("anthropic-version", "2023-06-01")
                    .send()
                    .await
            } else {
                client
                    .get("https://api.anthropic.com/v1/models")
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01")
                    .send()
                    .await
            };
            match auth_resp {
                Ok(r) if r.status().is_success() => println!("✅"),
                Ok(r) => println!("⚠️  HTTP {} (may still work)", r.status()),
                Err(e) => println!("❌ {}", e),
            }
        }
        "openai" => {
            print!("\n  Validating API key... ");
            let auth_resp = client
                .get("https://api.openai.com/v1/models")
                .bearer_auth(&api_key)
                .send()
                .await;
            match auth_resp {
                Ok(r) if r.status().is_success() => println!("✅"),
                Ok(r) => println!("⚠️  HTTP {} (may still work)", r.status()),
                Err(e) => println!("❌ {}", e),
            }
        }
        _ if !api_key.is_empty() => {
            println!(
                "\n  Skipping remote key validation for provider '{}'.",
                provider
            );
        }
        _ => {}
    }

    if let Some(ref token) = tg_token {
        print!("  Validating Telegram token... ");
        let tg_resp = client
            .get(format!("https://api.telegram.org/bot{}/getMe", token))
            .send()
            .await;
        match tg_resp {
            Ok(r) => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                if body["ok"].as_bool() == Some(true) {
                    let name = body["result"]["username"].as_str().unwrap_or("?");
                    println!("✅ @{}", name);
                } else {
                    println!("❌ Invalid token");
                }
            }
            Err(e) => println!("❌ {}", e),
        }
    }

    // === Write .env ===
    let env_path = workspace.join(".env");
    let mut env_content = String::new();
    if provider == "ollama" {
        env_content.push_str(&format!(
            "OLLAMA_BASE_URL=\"{}\"\n",
            auth.base_url.as_deref().unwrap_or("http://localhost:11434")
        ));
    } else if api_key.is_empty() {
        // OAuth methods keep their credentials in the provider's own store.
    } else if provider == "anthropic" || provider == "claude" {
        env_content.push_str(&format!("ANTHROPIC_API_KEY=\"{}\"\n", api_key));
    } else if provider == "copilot" || provider == "github-copilot" {
        env_content.push_str(&format!("GITHUB_TOKEN=\"{}\"\n", api_key));
    } else {
        env_content.push_str(&format!("OPENAI_API_KEY=\"{}\"\n", api_key));
    }
    if let Some(ref t) = tg_token {
        env_content.push_str(&format!("APOLLO_TELEGRAM_TOKEN=\"{}\"\n", t));
    }
    if let Some(ref c) = tg_chat_id {
        env_content.push_str(&format!("APOLLO_CHAT_ID=\"{}\"\n", c));
    }
    if let Some(ref t) = dc_token {
        env_content.push_str(&format!("APOLLO_DISCORD_TOKEN=\"{}\"\n", t));
    }
    if let Some(ref c) = dc_channel {
        env_content.push_str(&format!("APOLLO_DISCORD_CHANNEL=\"{}\"\n", c));
    }
    write_private(&env_path, &env_content)?;

    // === Write config ===
    let mut cfg = existing.unwrap_or_else(Config::default_config);
    cfg.provider.name = provider.clone();
    cfg.provider.api_key = None; // Secrets stay in .env
    cfg.provider.base_url = auth.base_url.clone();
    cfg.channel.kind = channel.clone();
    cfg.channel.token = None;
    if let Some(ref c) = tg_chat_id {
        if !cfg.channel.allowed_chat_ids.contains(c) {
            cfg.channel.allowed_chat_ids.push(c.clone());
        }
    }
    cfg.workspace = workspace.clone();
    if provider == "ollama" && cfg.embeddings.provider == "noop" {
        cfg.embeddings.enabled = true;
        cfg.embeddings.provider = "ollama".to_string();
        cfg.embeddings.model = Some("nomic-embed-text".to_string());
        cfg.embeddings.base_url = auth.base_url.clone();
    }
    cfg.model = model.clone();
    apply_permission_profile(&mut cfg, &permission_profile);
    let json = serde_json::to_string_pretty(&cfg)?;
    // The sandbox classifies apollo.json as a credential file, so it must not
    // be written world-readable.
    write_private(&config_path, &json)?;

    // === Write systemd service ===
    let bin_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("apollo"));
    let service_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".config/systemd/user");
    std::fs::create_dir_all(&service_dir)?;

    // Every value here reaches a shell, and a workspace path with a space in
    // it is legitimate — so quote rather than filter.
    use apollo::escape::shell_argument as sh;
    let mut exec_args = format!(
        "{} chat --channel {}",
        sh(&bin_path.display().to_string()),
        sh(&channel)
    );
    if tg_token.is_some() {
        // The token stays in APOLLO_TELEGRAM_TOKEN, sourced from .env above:
        // an argv value is visible in `ps` to every local user.
        exec_args.push_str(&format!(
            " --telegram-chat-id {}",
            sh(tg_chat_id.as_deref().unwrap_or("0"))
        ));
    }
    exec_args.push_str(&format!(" --model {}", sh(&model)));

    let run_script = format!(
        "#!/bin/bash\nset -a\nsource {}\nset +a\nexport RUST_LOG=info\ncd {}\nexec {}\n",
        sh(&env_path.display().to_string()),
        sh(&workspace.display().to_string()),
        exec_args,
    );
    let run_path = workspace.join("run.sh");
    std::fs::write(&run_path, &run_script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let service = format!(
        "[Unit]\nDescription=apollo AI agent\nAfter=network-online.target\n\n\
                [Service]\nType=simple\nExecStart={}\nRestart=always\nRestartSec=5\n\
                WorkingDirectory={}\nStandardOutput=append:/tmp/apollo.log\n\
                StandardError=append:/tmp/apollo.log\n\n\
                [Install]\nWantedBy=default.target\n",
        apollo::escape::systemd_argument(&run_path.display().to_string())?,
        apollo::escape::systemd_argument(&workspace.display().to_string())?
    );
    std::fs::write(service_dir.join("apollo.service"), &service)?;

    // === Summary ===
    println!("\n✅ Setup complete!\n");
    println!("  Provider:  {}", cfg.provider.name);
    println!("  Model:     {}", model);
    println!("  Channel:   {}", channel);
    println!(
        "  Safety:    {} (see agent.permission_profile in {})",
        cfg.agent.permission_profile,
        config_path.display()
    );
    println!("  Config:    {}", config_path.display());
    println!("  Secrets:   {}", env_path.display());
    println!("  Service:   ~/.config/systemd/user/apollo.service");
    println!("\n  Commands:");
    println!("    systemctl --user daemon-reload");
    println!("    systemctl --user enable --now apollo");
    println!("    journalctl --user -u apollo -f");

    // === Auto-start ===
    if opts.start {
        println!("\n  Starting...");
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "apollo"])
            .status();
        println!("  🐾 apollo is running!");
    }

    Ok(config_path)
}

/// Write a secrets file readable only by its owner.
fn write_private(path: &Path, content: &str) -> anyhow::Result<()> {
    apollo::fs_secure::write_secret_file(path, content)
}

/// Run the wizard when there is no config yet, so a bare `apollo` sets itself
/// up instead of failing on a missing file.
pub async fn ensure_config(path: &str) -> anyhow::Result<()> {
    if Path::new(path).exists() {
        return Ok(());
    }
    if !stdin_is_terminal() {
        anyhow::bail!("Config not found at {path}. Run `apollo init` to create one.");
    }
    println!("No configuration found — let's set apollo up.\n");
    run_init(InitOptions::default()).await?;
    if !Path::new(path).exists() {
        anyhow::bail!("Config not found at {path}. Run `apollo init` to create one.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_selection_accepts_index_and_name() {
        let all = ["anthropic", "ollama", "openai"];
        let matches = get_provider_matches(&all, "ol");
        assert_eq!(matches, vec!["ollama"]);
        assert_eq!(
            parse_provider_selection("1", &matches, &all).as_deref(),
            Some("ollama")
        );
        assert_eq!(
            parse_provider_selection("", &matches, &all).as_deref(),
            Some("ollama")
        );
        assert_eq!(
            parse_provider_selection("anthropic", &matches, &all).as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            parse_provider_selection("", &get_provider_matches(&all, "o"), &all),
            None
        );
    }

    #[test]
    fn every_offered_auth_method_is_recognized() {
        for (id, label) in auth_methods() {
            assert!(!label.is_empty(), "{id} needs a label");
            assert!(
                matches!(
                    id,
                    "anthropic-oauth"
                        | "anthropic-key"
                        | "copilot-oauth"
                        | "ollama"
                        | "openai-compat"
                ),
                "unexpected auth id {id}"
            );
        }
    }
}
