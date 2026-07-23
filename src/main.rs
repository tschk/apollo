//! apollo — Lightweight agent runtime CLI
//! Successor to OpenClaw. Best-of-breed from ZeroClaw, NanoClaw, HiClaw.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use apollo::agent::hooks::PermissionHook;
use apollo::agent::{agent_mode_from_permission_profile, AgentRunner};
use apollo::bootstrap::{
    build_base_tools, build_embedding_provider, build_memory_backend, build_provider, load_config,
};
#[cfg(feature = "channel-cli")]
use apollo::channels::cli::CliChannel;
#[cfg(feature = "channel-discord")]
use apollo::channels::discord::DiscordChannel;
use apollo::config::{apply_permission_profile, Config};
use apollo::cron_scheduler::CronScheduler;
use apollo::diagnostics::{collect_doctor_report, render_doctor_report, render_findings};
use apollo::heartbeat::{self, HeartbeatConfig};
use apollo::policy::ExecutionPolicy;
use apollo::prompt;
use apollo::self_update::{SelfUpdater, UpdateOutcome};
use apollo::skills;
use apollo::telegram_runtime::{run_telegram_chat, TelegramChatRun};

#[derive(Parser)]
#[command(name = "apollo", about = "Lightweight agent runtime — Apollo", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start interactive agent chat
    Chat {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Override the model
        #[arg(short, long)]
        model: Option<String>,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Channel: cli, telegram, discord
        #[arg(long, default_value = "cli")]
        channel: String,

        /// Telegram bot token (required for --channel telegram)
        #[arg(long)]
        telegram_token: Option<String>,

        /// Telegram chat ID (required for --channel telegram)
        #[arg(long)]
        telegram_chat_id: Option<i64>,

        /// Discord bot token (required for --channel discord)
        #[arg(long)]
        discord_token: Option<String>,

        /// Discord channel ID (required for --channel discord)
        #[arg(long)]
        discord_channel_id: Option<String>,
    },

    /// Send a one-shot message
    Ask {
        /// The message to send
        message: String,

        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Override the model
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Run system diagnostics and config validation
    Doctor {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Show more dependency checks
        #[arg(short, long, default_value_t = false)]
        verbose: bool,

        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Run a focused security/config audit
    Audit {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Output JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Show runtime status
    Status,

    /// Run as an MCP server (stdio or HTTP for Cloudflare Container)
    Mcp {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Override the model
        #[arg(short, long)]
        model: Option<String>,

        /// Run in HTTP mode on this port (default: stdio mode)
        #[arg(long)]
        port: Option<u16>,
    },

    /// Run one self-update cycle against the current repo
    SelfUpdate {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Initialize configuration (interactive wizard or one-command setup)
    Init {
        /// Provider (omit to pick from compiled-in list with type-to-filter)
        #[arg(short, long)]
        provider: Option<String>,

        /// API key
        #[arg(short = 'k', long)]
        api_key: Option<String>,

        /// Channel (telegram, discord, cli)
        #[arg(long)]
        channel: Option<String>,

        /// Telegram bot token
        #[arg(long)]
        telegram_token: Option<String>,

        /// Telegram chat ID
        #[arg(long)]
        telegram_chat_id: Option<String>,

        /// Discord bot token
        #[arg(long)]
        discord_token: Option<String>,

        /// Discord channel ID
        #[arg(long)]
        discord_channel_id: Option<String>,

        /// Model to use
        #[arg(short, long)]
        model: Option<String>,

        /// Start the bot after init
        #[arg(long, default_value_t = false)]
        start: bool,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Permission profile: full | auto | prompt | tools_only
        #[arg(long)]
        permission_profile: Option<String>,
    },

    /// Send a message to the running apollo bot via Telegram
    #[command(alias = "msg")]
    Message {
        /// Message text
        message: String,

        /// Chat ID (defaults to APOLLO_CHAT_ID from .env)
        #[arg(long)]
        chat_id: Option<String>,

        /// Workspace directory (to find .env)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Manage cron jobs
    Cron {
        #[command(subcommand)]
        action: CronAction,

        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Swarm commands (multi-agent coordination)
    Swarm {
        #[command(subcommand)]
        action: SwarmAction,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum CronAction {
    /// Add a new cron job
    Add {
        /// Job name
        #[arg(short, long)]
        name: String,

        /// Cron expression (e.g. "0 0 9 * * * *")
        #[arg(short, long)]
        schedule: String,

        /// Task prompt text
        #[arg(short, long)]
        task: String,

        /// Channel (default: cli)
        #[arg(long, default_value = "cli")]
        channel: String,

        #[arg(long, default_value = "cli")]
        chat_id: String,

        /// Model override
        #[arg(long, default_value = "")]
        model: String,
    },

    /// List all cron jobs
    List,

    /// Remove a cron job by ID or name
    Remove {
        /// Job ID or name
        id_or_name: String,
    },

    /// Enable a cron job
    Enable {
        /// Job ID or name
        id_or_name: String,
    },

    /// Disable a cron job
    Disable {
        /// Job ID or name
        id_or_name: String,
    },
}

#[derive(Subcommand)]
enum SwarmAction {
    /// Start swarm coordinator
    Start {
        /// SurrealDB path
        #[arg(long, default_value = ".apollo/state.surreal")]
        surreal_path: String,

        /// RocksDB cache path
        #[arg(long, default_value = ".apollo/cache")]
        cache_path: String,
    },

    /// Register a named agent
    AgentCreate {
        /// Agent name (unique)
        name: String,

        /// LLM model
        #[arg(long, default_value = "claude-sonnet-4-5")]
        model: String,

        /// Capabilities (comma-separated: coding,research,review,testing,documentation,design,devops,security)
        #[arg(long, default_value = "coding")]
        capabilities: String,

        /// Tools (comma-separated)
        #[arg(long)]
        tools: Option<String>,

        /// Max concurrent incoming delegations
        #[arg(long, default_value = "5")]
        max_concurrent: i32,
    },

    /// Create a delegation link between agents
    AgentLink {
        /// Source agent name
        source: String,

        /// Target agent name
        target: String,

        /// Direction: outbound, inbound, bidirectional
        #[arg(long, default_value = "outbound")]
        direction: String,

        /// Max concurrent delegations on this link
        #[arg(long, default_value = "3")]
        max_concurrent: u32,
    },

    /// Create a team
    TeamCreate {
        /// Team name
        name: String,

        /// Lead agent name
        #[arg(long)]
        lead: String,
    },

    /// Add a task to a team's board
    TeamTaskAdd {
        /// Team name
        team: String,

        /// Task subject
        subject: String,

        /// Priority (0-10)
        #[arg(short, long, default_value = "0")]
        priority: i32,

        /// Blocked by task IDs (comma-separated)
        #[arg(long)]
        blocked_by: Option<String>,
    },

    /// List active agents
    Agents,

    /// List pending tasks
    Tasks,

    /// List teams
    Teams,

    /// List delegations for an agent
    Delegations {
        /// Agent name
        agent: String,
    },

    /// Submit a task to the swarm
    Task {
        /// Task description
        description: String,

        /// Priority (low, medium, high, critical)
        #[arg(short, long, default_value = "medium")]
        priority: String,

        /// Title (defaults to first line of description)
        #[arg(short, long)]
        title: Option<String>,
    },

    /// Queue a message (steering)
    Queue {
        /// Message to queue
        message: String,
    },

    /// Show scheduler status
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present — allows running without manually exporting env vars
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    let tracing_cfg = config_path_for_cli(&cli)
        .and_then(|path| Config::load(&path).ok())
        .map(|cfg| cfg.observability)
        .unwrap_or_default();
    init_tracing(&tracing_cfg)?;

    match cli.command {
        Commands::Chat {
            config,
            model,
            workspace,
            channel,
            telegram_token,
            telegram_chat_id,
            discord_token: _discord_token,
            discord_channel_id: _discord_channel_id,
        } => {
            let workspace = workspace.unwrap_or_else(|| load_config(&config).workspace.clone());
            let cfg = apollo::bootstrap::load_config_workspace(&config, Some(&workspace));
            let model = model.unwrap_or(cfg.model.clone());
            let _ = apollo::workspace_init::ensure_workspace_kit(&workspace);

            let provider = build_provider(&cfg);
            let policy = Arc::new(ExecutionPolicy::from_config(&cfg.policy));
            let memory = build_memory_backend(&workspace, &cfg).await?;
            let embedding_provider = build_embedding_provider(&cfg)?;
            let self_updater = SelfUpdater::new(workspace.clone(), cfg.runtime.self_update.clone());

            // Build system prompt from workspace context files
            let system_prompt = prompt::build_system_prompt(&workspace).await;

            // Discover skills
            let discovered_skills = skills::discover_skills_for_workspace(Some(&workspace));
            if !discovered_skills.is_empty() {
                tracing::info!("Discovered {} skills", discovered_skills.len());
            }

            // Build shared zkr memory store (used by both the zkr tool and the runner)
            #[cfg(feature = "zkr-memory")]
            let zkr_store = apollo::bootstrap::build_zkr_store(&workspace, &cfg)
                .ok()
                .flatten();

            // Register tools (including memory search/get)
            #[cfg(feature = "zkr-memory")]
            let mut tools = build_base_tools(
                &workspace,
                Arc::clone(&policy),
                memory.clone(),
                embedding_provider,
                Arc::clone(&provider),
                &cfg,
                zkr_store.clone(),
            );
            #[cfg(not(feature = "zkr-memory"))]
            let mut tools = build_base_tools(
                &workspace,
                Arc::clone(&policy),
                memory.clone(),
                embedding_provider,
                Arc::clone(&provider),
                &cfg,
            );

            // Load any previously created dynamic tools
            let dynamic_tools = apollo::tools::dynamic::DynamicTool::load_all(Arc::clone(&policy));
            let dynamic_count = dynamic_tools.len();
            for dt in dynamic_tools {
                tools.push(Arc::new(dt));
            }
            if dynamic_count > 0 {
                println!("   Loaded {} custom tool(s)", dynamic_count);
            }

            // Start swarm coordinator if requested
            #[cfg(feature = "swarm")]
            let coordinator = {
                let storage: Arc<dyn apollo::swarm::SwarmStorage> = Arc::new(
                    apollo::swarm::SurrealBackend::new(&workspace.join(".apollo/swarm.surreal"))
                        .await?,
                );
                let coord = Arc::new(apollo::swarm::SwarmCoordinator::new(storage));
                coord.init().await?;
                Some(coord)
            };

            #[cfg_attr(not(feature = "swarm"), allow(unused_mut))]
            let mut runner =
                AgentRunner::new(provider, tools, memory.clone(), &system_prompt, model)
                    .with_config(cfg.agent.clone())
                    .with_mode(agent_mode_from_permission_profile(
                        &cfg.agent.permission_profile,
                    ))
                    .with_workspace(workspace.clone())
                    .with_memory_ideas(cfg.memory.clone())
                    .with_group_chat(cfg.group_chat.clone())
                    .with_skills(discovered_skills.clone())
                    .await;
            #[cfg(feature = "zkr-memory")]
            {
                runner = runner.with_zkr(zkr_store, cfg.zkr.clone());
            }

            {
                let mut host_reg = apollo::plugin::PluginRegistry::new();
                host_reg.ingest_host_plugins(&workspace, &cfg.plugin_layer.host_plugin_roots);
                runner = runner.with_plugin_registry(host_reg).await;
            }

            #[cfg(feature = "swarm")]
            if let Some(coord) = coordinator {
                runner = runner.with_swarm(coord);
            }

            let runner_arc = Arc::new(runner);

            if std::env::var("APOLLO_HTTP")
                .map(|v| v != "0")
                .unwrap_or(true)
            {
                apollo::agent_http::spawn_http_server(Arc::clone(&runner_arc));
            }

            runner_arc.add_hook(Arc::new(PermissionHook::new(
                cfg.agent.permissions.deny.clone(),
                cfg.agent.permissions.allow.clone(),
            )));

            #[cfg(feature = "swarm")]
            runner_arc
                .add_tool(Arc::new(apollo::tools::CodingSwarmTool::new(
                    runner_arc.clone(),
                    3,
                )))
                .await;
            runner_arc
                .add_tool(Arc::new(apollo::tools::tool_search::ToolSearchTool::new(
                    runner_arc.tools.clone(),
                )))
                .await;
            runner_arc
                .add_tool(Arc::new(apollo::tools::mode_switch::ModeSwitchTool::new(
                    runner_arc.mode_handle(),
                )))
                .await;

            // Add claude_usage tool (needs cost tracker reference)
            runner_arc
                .add_tool(Arc::new(apollo::tools::claude_usage::ClaudeUsageTool::new(
                    runner_arc.cost_tracker(),
                )))
                .await;

            // Start cron scheduler background task and add tool
            let scheduled_chat_id = match channel.as_str() {
                "telegram" => telegram_chat_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                "discord" => _discord_channel_id.clone().unwrap_or_default(),
                _ => "cli".to_string(),
            };
            let mut cron_runtime = None;
            if let Some(surreal_mem) = memory
                .as_any()
                .downcast_ref::<apollo::memory::surreal::SurrealMemory>()
            {
                let cron_sched = Arc::new(CronScheduler::new(Arc::new(surreal_mem.clone())));
                let (cron_rx, cron_shutdown) =
                    apollo::cron_scheduler::start_cron_ticker(cron_sched.clone(), channel.clone());

                runner_arc
                    .add_tool(Arc::new(apollo::tools::cron_tool::CronTool::new(
                        cron_sched.clone(),
                        channel.clone(),
                        scheduled_chat_id.clone(),
                        cfg.model.clone(),
                    )))
                    .await;
                cron_runtime = Some((cron_rx, cron_shutdown, cron_sched));
            }

            let _self_update_handle = self_updater.start();

            match channel.as_str() {
                #[cfg(feature = "channel-cli")]
                "cli" => {
                    println!(
                        "apollo v{} — {} via {}",
                        env!("CARGO_PKG_VERSION"),
                        cfg.model,
                        cfg.provider.name
                    );
                    println!("   Workspace: {}", workspace.display());
                    println!("   Channel: CLI");
                    println!("   Type /quit to exit");
                    println!(
                        "   Agent HTTP: http://{}/v1/chat (APOLLO_HTTP_PORT)",
                        apollo::agent_http::http_listen_addr()
                    );
                    println!();

                    // Start heartbeat background task
                    let heartbeat_cfg = HeartbeatConfig {
                        workspace: workspace.clone(),
                        deliver_chat_id: cfg.memory.heartbeat_chat_id.clone(),
                        ..Default::default()
                    };
                    let (hb_tx, hb_rx) = tokio::sync::mpsc::channel(16);
                    let _heartbeat_handle = heartbeat::start_heartbeat(heartbeat_cfg, hb_tx);

                    let mut ch = CliChannel::new();
                    if let Some((cron_rx, cron_shutdown, cron_sched)) = cron_runtime.take() {
                        runner_arc
                            .run_with_runtime_rx(&mut ch, hb_rx, cron_rx, cron_sched)
                            .await?;
                        cron_shutdown.notify_waiters();
                    } else {
                        runner_arc.run_with_extra_rx(&mut ch, hb_rx).await?;
                    }
                }
                #[cfg(feature = "channel-telegram")]
                "telegram" => {
                    let token = telegram_token
                        .ok_or_else(|| anyhow::anyhow!("--telegram-token required"))?;
                    let chat_id = telegram_chat_id
                        .ok_or_else(|| anyhow::anyhow!("--telegram-chat-id required"))?;
                    run_telegram_chat(TelegramChatRun {
                        runner: runner_arc,
                        memory,
                        token,
                        chat_id,
                        model: cfg.model.clone(),
                        skills_count: discovered_skills.len(),
                        workspace: workspace.clone(),
                        channel_cfg: &cfg.channel,
                        cron_runtime: cron_runtime.take(),
                    })
                    .await?;
                }
                #[cfg(feature = "channel-discord")]
                "discord" => {
                    let token = _discord_token
                        .ok_or_else(|| anyhow::anyhow!("--discord-token required"))?;
                    let channel_id = _discord_channel_id
                        .ok_or_else(|| anyhow::anyhow!("--discord-channel-id required"))?;

                    println!("apollo — {} via Discord", cfg.model);
                    println!("   Channel ID: {}", channel_id);
                    println!("   Listening for messages...");

                    let mut ch = DiscordChannel::new(token, channel_id);
                    if let Some((cron_rx, cron_shutdown, cron_sched)) = cron_runtime.take() {
                        let (_extra_tx, extra_rx) = tokio::sync::mpsc::channel(1);
                        runner_arc
                            .run_with_runtime_rx(&mut ch, extra_rx, cron_rx, cron_sched)
                            .await?;
                        cron_shutdown.notify_waiters();
                    } else {
                        runner_arc.run(&mut ch).await?;
                    }
                }
                other => {
                    anyhow::bail!(
                        "Unknown channel: {} (supported: cli, telegram, discord)",
                        other
                    );
                }
            }
        }

        Commands::Ask {
            message,
            config,
            model,
        } => {
            let cfg = load_config(&config);
            let model = model.unwrap_or(cfg.model.clone());
            let provider = build_provider(&cfg);

            let response = provider.simple_chat(&message, &model).await?;
            println!("{}", response);
        }

        Commands::Doctor {
            config,
            verbose,
            json,
        } => {
            let cfg = load_config(&config);
            let report = collect_doctor_report(Some(&cfg), verbose).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", render_doctor_report(&report));
            }
        }

        Commands::Audit { config, json } => {
            let cfg = load_config(&config);
            let findings = apollo::diagnostics::audit_config(&cfg);
            if json {
                println!("{}", serde_json::to_string_pretty(&findings)?);
            } else {
                println!("{}", render_findings(&findings));
            }
        }

        Commands::Status => {
            println!("apollo v{}", env!("CARGO_PKG_VERSION"));
            println!("Status: OK");
            println!(
                "Commands: chat, ask, doctor, audit, status, mcp, self-update, init, cron, swarm"
            );
        }

        Commands::Mcp {
            config,
            workspace,
            model,
            port,
        } => {
            let cfg = load_config(&config);
            let model = model.unwrap_or(cfg.model.clone());
            let workspace = workspace.unwrap_or(cfg.workspace.clone());

            let provider = build_provider(&cfg);
            let policy = Arc::new(ExecutionPolicy::from_config(&cfg.policy));
            let memory = build_memory_backend(&workspace, &cfg).await?;
            let embedding_provider = build_embedding_provider(&cfg)?;

            #[cfg(feature = "zkr-memory")]
            let zkr_store = apollo::bootstrap::build_zkr_store(&workspace, &cfg)
                .ok()
                .flatten();
            #[cfg(feature = "zkr-memory")]
            let tools = build_base_tools(
                &workspace,
                Arc::clone(&policy),
                Arc::clone(&memory),
                embedding_provider,
                Arc::clone(&provider),
                &cfg,
                zkr_store,
            );
            #[cfg(not(feature = "zkr-memory"))]
            let tools = build_base_tools(
                &workspace,
                Arc::clone(&policy),
                Arc::clone(&memory),
                embedding_provider,
                Arc::clone(&provider),
                &cfg,
            );

            if let Some(port) = port {
                let system_prompt = cfg.system_prompt.clone();
                let runner = Arc::new(
                    apollo::agent::loop_runner::AgentRunner::new(
                        Arc::clone(&provider),
                        tools.clone(),
                        Arc::clone(&memory),
                        system_prompt,
                        model.clone(),
                    )
                    .with_config(cfg.agent.clone())
                    .with_mode(agent_mode_from_permission_profile(
                        &cfg.agent.permission_profile,
                    )),
                );
                runner.add_hook(Arc::new(PermissionHook::new(
                    cfg.agent.permissions.deny.clone(),
                    cfg.agent.permissions.allow.clone(),
                )));
                eprintln!(
                    "apollo v{} — MCP HTTP server on port {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    port,
                    model
                );
                apollo::mcp_server::run_mcp_server_http(
                    tools,
                    Some(provider),
                    Some(model),
                    Some(runner),
                    port,
                )
                .await?;
            } else {
                eprintln!(
                    "apollo v{} — MCP server mode ({})",
                    env!("CARGO_PKG_VERSION"),
                    model
                );
                apollo::mcp_server::run_mcp_server(tools, Some(provider), Some(model)).await?;
            }
        }

        Commands::SelfUpdate { config, workspace } => {
            let cfg = load_config(&config);
            let workspace = workspace.unwrap_or(cfg.workspace.clone());
            let updater = SelfUpdater::new(workspace, cfg.runtime.self_update.clone());
            match updater.run_once().await? {
                UpdateOutcome::NoRepo => println!("Not a git repo. Nothing to update."),
                UpdateOutcome::Disabled => println!("Self-update is disabled in config."),
                UpdateOutcome::DirtyWorktree => {
                    println!("Skipped self-update because the worktree is dirty.");
                }
                UpdateOutcome::AlreadyCurrent => println!("Already up to date."),
                UpdateOutcome::Updated { restarted } => {
                    if restarted {
                        println!("Updated, rebuilt, and restarted service.");
                    } else {
                        println!("Updated and rebuilt. Restart the process or service if needed.");
                    }
                }
            }
        }

        Commands::Init {
            provider,
            api_key,
            channel,
            telegram_token,
            telegram_chat_id,
            discord_token,
            discord_channel_id,
            model,
            start,
            workspace,
            permission_profile,
        } => {
            let workspace = workspace.unwrap_or_else(|| PathBuf::from("."));
            println!("🐾 apollo setup\n");

            // === Resolve values (flags or interactive prompts) ===
            let provider = match provider {
                Some(p) if !p.trim().is_empty() => p.trim().to_string(),
                Some(_) | None => prompt_provider_interactive()?,
            };

            let api_key = match api_key {
                Some(k) => k,
                None => {
                    if provider == "ollama" {
                        String::new()
                    } else {
                        eprint!("  API key ({}): ", provider);
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf)?;
                        let k = buf.trim().to_string();
                        if k.is_empty() {
                            anyhow::bail!("API key required (omit only for ollama)");
                        }
                        k
                    }
                }
            };

            let channel = match channel {
                Some(c) => c,
                None => {
                    eprint!("  Channel (telegram/discord/cli) [telegram]: ");
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf)?;
                    let c = buf.trim().to_string();
                    if c.is_empty() {
                        "telegram".to_string()
                    } else {
                        c
                    }
                }
            };

            let model = model.unwrap_or_else(|| "claude-sonnet-4-5".to_string());

            // Channel-specific tokens
            let tg_token = if channel == "telegram" {
                match telegram_token {
                    Some(t) => Some(t),
                    None => {
                        eprint!("  Telegram bot token: ");
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf)?;
                        let t = buf.trim().to_string();
                        if t.is_empty() {
                            None
                        } else {
                            Some(t)
                        }
                    }
                }
            } else {
                telegram_token
            };

            let tg_chat_id = if channel == "telegram" {
                match telegram_chat_id {
                    Some(c) => Some(c),
                    None => {
                        eprint!("  Telegram chat ID: ");
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf)?;
                        let c = buf.trim().to_string();
                        if c.is_empty() {
                            None
                        } else {
                            Some(c)
                        }
                    }
                }
            } else {
                telegram_chat_id
            };

            let dc_token = if channel == "discord" {
                discord_token
            } else {
                None
            };
            let dc_channel = if channel == "discord" {
                discord_channel_id
            } else {
                None
            };

            if channel == "telegram" && tg_token.is_none() {
                anyhow::bail!("Telegram channel requires a bot token (use --telegram-token or enter it when prompted)");
            }

            let permission_profile = match permission_profile {
                Some(p) if !p.trim().is_empty() => p.trim().to_string(),
                Some(_) | None => prompt_permission_profile_interactive()?,
            };

            // === Validate ===
            let client = reqwest::Client::new();
            match provider.as_str() {
                "ollama" => {
                    print!("\n  Validating Ollama... ");
                    let base_url = "http://localhost:11434";
                    let resp = client.get(format!("{}/api/tags", base_url)).send().await;
                    match resp {
                        Ok(r) if r.status().is_success() => println!("✅"),
                        Ok(r) => println!("⚠️  HTTP {} from local Ollama", r.status()),
                        Err(e) => println!("❌ {}", e),
                    }
                }
                "anthropic" | "claude" => {
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
                env_content.push_str("OLLAMA_BASE_URL=\"http://localhost:11434\"\n");
            } else if provider == "anthropic" || provider == "claude" {
                env_content.push_str(&format!("ANTHROPIC_API_KEY=\"{}\"\n", api_key));
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
            std::fs::write(&env_path, &env_content)?;

            // === Write config ===
            let mut cfg = Config::default_config();
            cfg.provider.name = provider.clone();
            cfg.provider.api_key = None; // Secrets stay in .env
            if provider == "ollama" {
                cfg.provider.base_url = Some("http://localhost:11434".to_string());
                if cfg.embeddings.provider == "noop" {
                    cfg.embeddings.enabled = true;
                    cfg.embeddings.provider = "ollama".to_string();
                    cfg.embeddings.model = Some("nomic-embed-text".to_string());
                    cfg.embeddings.base_url = Some("http://localhost:11434".to_string());
                }
            }
            cfg.model = model.clone();
            apply_permission_profile(&mut cfg, &permission_profile);
            let json = serde_json::to_string_pretty(&cfg)?;
            let config_path = workspace.join("apollo.json");
            std::fs::write(&config_path, &json)?;

            // === Write systemd service ===
            let bin_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("apollo"));
            let service_dir = dirs::home_dir()
                .unwrap_or_default()
                .join(".config/systemd/user");
            std::fs::create_dir_all(&service_dir)?;

            let mut exec_args = format!("{} chat --channel {}", bin_path.display(), channel);
            if tg_token.is_some() {
                exec_args.push_str(&format!(
                    " --telegram-token $APOLLO_TELEGRAM_TOKEN --telegram-chat-id {}",
                    tg_chat_id.as_deref().unwrap_or("0")
                ));
            }
            exec_args.push_str(&format!(" --model {}", model));

            let run_script = format!(
                "#!/bin/bash\nsource {}\nexport RUST_LOG=info\ncd {}\nexec {}\n",
                env_path.display(),
                workspace.display(),
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
                run_path.display(),
                workspace.display()
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
            if start {
                println!("\n  Starting...");
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .status();
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "enable", "--now", "apollo"])
                    .status();
                println!("  🐾 apollo is running!");
            }
        }

        Commands::Message {
            message,
            chat_id: _,
            workspace: _,
        } => {
            let client = reqwest::Client::new();
            let resp = client
                .post("http://127.0.0.1:31337/message")
                .json(&serde_json::json!({ "message": message }))
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    println!("✅ Sent to apollo");
                }
                Ok(r) => {
                    eprintln!(
                        "❌ HTTP {}: {}",
                        r.status(),
                        r.text().await.unwrap_or_default()
                    );
                }
                Err(_) => {
                    eprintln!(
                        "❌ Can't reach apollo. Is it running? (systemctl --user status apollo)"
                    );
                }
            }
        }

        Commands::Cron {
            action,
            workspace,
            config,
        } => {
            let workspace = workspace.unwrap_or_else(|| PathBuf::from("."));
            let cfg = load_config(&config);
            let memory = build_memory_backend(&workspace, &cfg).await?;

            if let Some(surreal_mem) = memory
                .as_any()
                .downcast_ref::<apollo::memory::surreal::SurrealMemory>()
            {
                let scheduler = CronScheduler::new(Arc::new(surreal_mem.clone()));

                match action {
                    CronAction::Add {
                        name,
                        schedule,
                        task,
                        channel,
                        chat_id,
                        model,
                    } => {
                        let id = scheduler
                            .add(&name, &schedule, &task, &channel, &chat_id, &model)
                            .await?;
                        println!("Added cron job: {} (id: {})", name, id);
                    }
                    CronAction::List => {
                        let jobs = scheduler.list().await?;
                        if jobs.is_empty() {
                            println!("No cron jobs configured.");
                        } else {
                            for job in &jobs {
                                println!(
                                    "{} [{}] {} — \"{}\" (next: {})",
                                    if job.enabled { "+" } else { "-" },
                                    job.name,
                                    job.schedule,
                                    job.task,
                                    job.next_run.as_deref().unwrap_or("none"),
                                );
                            }
                        }
                    }
                    CronAction::Remove { id_or_name } => {
                        if scheduler.remove(&id_or_name).await? {
                            println!("Removed: {}", id_or_name);
                        } else {
                            println!("Not found: {}", id_or_name);
                        }
                    }
                    CronAction::Enable { id_or_name } => {
                        if scheduler.enable(&id_or_name).await? {
                            println!("Enabled: {}", id_or_name);
                        } else {
                            println!("Not found: {}", id_or_name);
                        }
                    }
                    CronAction::Disable { id_or_name } => {
                        if scheduler.disable(&id_or_name).await? {
                            println!("Disabled: {}", id_or_name);
                        } else {
                            println!("Not found: {}", id_or_name);
                        }
                    }
                }
            } else {
                anyhow::bail!("Cron scheduler requires SurrealDB backend");
            }
        }

        Commands::Swarm { action, workspace } => {
            #[cfg(not(feature = "swarm"))]
            {
                let _ = (action, workspace);
                eprintln!("Swarm requires the 'swarm' feature. Build with: cargo build --release --features swarm");
                std::process::exit(1);
            }

            #[cfg(feature = "swarm")]
            {
                use apollo::swarm::models::LinkDirection;
                use apollo::swarm::{
                    AgentCapability, SurrealBackend, SwarmCoordinator, SwarmStorage, TaskPriority,
                };

                let workspace = workspace.unwrap_or_else(|| PathBuf::from("."));
                let surreal_path = workspace.join(".apollo/swarm.surreal");

                // Ensure directory exists
                if let Some(parent) = surreal_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                let storage: Arc<dyn SwarmStorage> =
                    Arc::new(SurrealBackend::new(&surreal_path).await?);
                let coordinator = SwarmCoordinator::new(storage.clone());
                coordinator.init().await?;

                match action {
                    SwarmAction::Start {
                        surreal_path: _,
                        cache_path: _,
                    } => {
                        println!(
                            "Swarm coordinator initialized at {}",
                            surreal_path.display()
                        );
                        println!("Ready for agent registration.");
                    }

                    SwarmAction::AgentCreate {
                        name,
                        model,
                        capabilities,
                        tools,
                        max_concurrent,
                    } => {
                        let caps: Vec<AgentCapability> = capabilities
                            .split(',')
                            .filter_map(|c| match c.trim() {
                                "coding" => Some(AgentCapability::Coding),
                                "research" => Some(AgentCapability::Research),
                                "review" => Some(AgentCapability::Review),
                                "testing" => Some(AgentCapability::Testing),
                                "documentation" => Some(AgentCapability::Documentation),
                                "design" => Some(AgentCapability::Design),
                                "devops" => Some(AgentCapability::DevOps),
                                "security" => Some(AgentCapability::Security),
                                _ => None,
                            })
                            .collect();

                        let tool_list =
                            tools.map(|t| t.split(',').map(|s| s.trim().to_string()).collect());

                        let agent_id = coordinator
                            .register_agent(name.clone(), caps, Some(model.clone()), tool_list)
                            .await?;

                        // Update max_concurrent
                        storage.update_agent_status(&agent_id, "active").await?;

                        println!("Agent '{}' created (id: {})", name, agent_id);
                        println!("  Model: {}", model);
                        println!("  Max concurrent: {}", max_concurrent);
                    }

                    SwarmAction::AgentLink {
                        source,
                        target,
                        direction,
                        max_concurrent,
                    } => {
                        let dir = match direction.as_str() {
                            "outbound" => LinkDirection::Outbound,
                            "inbound" => LinkDirection::Inbound,
                            "bidirectional" | "bidi" => LinkDirection::Bidirectional,
                            _ => {
                                eprintln!(
                                    "Unknown direction: {} (use: outbound, inbound, bidirectional)",
                                    direction
                                );
                                std::process::exit(1);
                            }
                        };

                        // Resolve names to IDs
                        let src = storage
                            .get_agent_by_name(&source)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", source))?;
                        let tgt = storage
                            .get_agent_by_name(&target)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", target))?;

                        let link = coordinator
                            .delegation
                            .create_link(&src.agent_id, &tgt.agent_id, dir, max_concurrent)
                            .await?;

                        println!(
                            "Link created: {} -> {} ({}, max {})",
                            source, target, direction, max_concurrent
                        );
                        println!("  Link ID: {}", link.link_id);
                    }

                    SwarmAction::TeamCreate { name, lead } => {
                        let lead_agent = storage
                            .get_agent_by_name(&lead)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Lead agent '{}' not found", lead))?;

                        let team = coordinator
                            .teams
                            .create_team(&name, &lead_agent.agent_id)
                            .await?;
                        println!("Team '{}' created (id: {})", name, team.team_id);
                        println!("  Lead: {}", lead);
                    }

                    SwarmAction::TeamTaskAdd {
                        team,
                        subject,
                        priority,
                        blocked_by,
                    } => {
                        let team_obj = coordinator
                            .teams
                            .get_team_by_name(&team)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team))?;

                        let blockers = blocked_by
                            .map(|b| b.split(',').map(|s| s.trim().to_string()).collect())
                            .unwrap_or_default();

                        let task = coordinator
                            .teams
                            .create_task(&team_obj.team_id, &subject, None, priority, blockers)
                            .await?;
                        println!(
                            "Task added to team '{}': {} (id: {})",
                            team, subject, task.task_id
                        );
                    }

                    SwarmAction::Agents => {
                        let agents = coordinator.list_all_agents().await?;
                        if agents.is_empty() {
                            println!("No agents registered.");
                        } else {
                            println!(
                                "{:<20} {:<12} {:<25} {:<10}",
                                "NAME", "STATUS", "MODEL", "MAX_CONC"
                            );
                            for a in &agents {
                                println!(
                                    "{:<20} {:<12} {:<25} {:<10}",
                                    a.name,
                                    a.status.to_string(),
                                    a.model.as_deref().unwrap_or("-"),
                                    a.max_concurrent.unwrap_or(5),
                                );
                            }
                        }
                    }

                    SwarmAction::Tasks => {
                        let tasks = coordinator.list_pending_tasks().await?;
                        if tasks.is_empty() {
                            println!("No pending tasks.");
                        } else {
                            for t in &tasks {
                                println!("[{:?}] {} — {}", t.priority, t.title, t.status);
                            }
                        }
                    }

                    SwarmAction::Teams => {
                        let teams = coordinator.teams.list_teams().await?;
                        if teams.is_empty() {
                            println!("No teams.");
                        } else {
                            let ids: Vec<String> =
                                teams.iter().map(|t| t.team_id.clone()).collect();
                            let counts = coordinator.teams.member_counts_for(&ids).await?;
                            for t in &teams {
                                let n = counts.get(&t.team_id).copied().unwrap_or(0);
                                println!(
                                    "{} (lead: {}, members: {}, status: {})",
                                    t.name, t.lead_agent_id, n, t.status
                                );
                            }
                        }
                    }

                    SwarmAction::Delegations { agent } => {
                        let agent_obj = storage
                            .get_agent_by_name(&agent)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent))?;
                        let delegations = coordinator
                            .delegation
                            .list_active(&agent_obj.agent_id)
                            .await?;
                        if delegations.is_empty() {
                            println!("No active delegations for '{}'.", agent);
                        } else {
                            for d in &delegations {
                                println!(
                                    "[{}] {} -> {} ({:?}): {}",
                                    d.status, d.source_agent_id, d.target_agent_id, d.mode, d.task
                                );
                            }
                        }
                    }

                    SwarmAction::Task {
                        description,
                        priority,
                        title,
                    } => {
                        let prio = match priority.as_str() {
                            "low" => TaskPriority::Low,
                            "medium" => TaskPriority::Medium,
                            "high" => TaskPriority::High,
                            "critical" => TaskPriority::Critical,
                            _ => TaskPriority::Medium,
                        };
                        let title = title.unwrap_or_else(|| {
                            description
                                .lines()
                                .next()
                                .unwrap_or(&description)
                                .to_string()
                        });
                        let task_id = coordinator
                            .submit_task(title.clone(), description, prio)
                            .await?;
                        println!("Task submitted: {} (id: {})", title, task_id);
                    }

                    SwarmAction::Queue { message } => {
                        coordinator.queue_message(message.clone()).await;
                        println!("Message queued: {}", message);
                    }

                    SwarmAction::Status => {
                        let status = coordinator.scheduler.get_status().await;
                        println!("Scheduler Status:");
                        for (lane, (active, max)) in &status.lane_usage {
                            println!("  {}: {}/{}", lane, active, max);
                        }
                        if !status.deadlocks.is_empty() {
                            println!("\nDEADLOCKS DETECTED:");
                            for cycle in &status.deadlocks {
                                println!("  Cycle: {}", cycle.join(" -> "));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
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

        eprint!("  Filter, #, or exact name (empty = pick if exactly one match): ");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
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
    eprint!("  Choose [full / auto / prompt / tools_only] [auto]: ");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let s = buf.trim();
    if s.is_empty() {
        return Ok("auto".to_string());
    }
    Ok(s.to_string())
}

fn config_path_for_cli(cli: &Cli) -> Option<String> {
    match &cli.command {
        Commands::Chat { config, .. }
        | Commands::Ask { config, .. }
        | Commands::Doctor { config, .. }
        | Commands::Audit { config, .. }
        | Commands::SelfUpdate { config, .. }
        | Commands::Mcp { config, .. } => Some(config.clone()),
        _ => None,
    }
}

fn init_tracing(cfg: &apollo::config::ObservabilityConfig) -> anyhow::Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::from_default_env();
    let fmt = tracing_subscriber::fmt().with_env_filter(env_filter);
    if cfg.json_logs {
        fmt.json()
            .try_init()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    } else {
        fmt.try_init()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    }
    tracing::info!(
        service_name = %cfg.service_name,
        environment = %cfg.environment,
        trace_header = %cfg.trace_header_name,
        "tracing initialized"
    );
    Ok(())
}
