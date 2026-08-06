//! apollo — Lightweight agent runtime CLI
//! Successor to OpenClaw. Best-of-breed from ZeroClaw, NanoClaw, HiClaw.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};

use apollo::agent::hooks::PermissionHook;
use apollo::agent::{agent_mode_from_permission_profile, AgentRunner};
use apollo::autonomous::{AutonomousConfig, AutonomousLoop};
use apollo::autoresearch::{AutoresearchConfig, AutoresearchLoop};
use apollo::bootstrap::{
    build_base_tools, build_embedding_provider, build_memory_backend, build_provider, load_config,
    require_config_file,
};
#[cfg(feature = "channel-cli")]
use apollo::channels::cli::CliChannel;
#[cfg(feature = "channel-discord")]
use apollo::channels::discord::DiscordChannel;
use apollo::config::Config;
use apollo::cron_scheduler::CronScheduler;
use apollo::diagnostics::{collect_doctor_report, render_doctor_report, render_findings};
use apollo::heartbeat::{self, HeartbeatConfig};
use apollo::policy::ExecutionPolicy;
use apollo::prompt;
use apollo::self_update::{SelfUpdater, UpdateOutcome};
use apollo::skills;
use apollo::telegram_runtime::{run_telegram_chat, TelegramChatRun};

mod setup;

#[derive(Parser)]
#[command(
    name = "apollo",
    about = "Local-first AI agent runtime",
    version,
    help_template = "{name} {version}\n{about-with-newline}\n{usage-heading} {usage}\n\n{all-args}{after-help}"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
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
        #[arg(long, env = "APOLLO_TELEGRAM_TOKEN", hide_env_values = true)]
        telegram_token: Option<String>,

        /// Telegram chat ID (required for --channel telegram)
        #[arg(long, env = "APOLLO_CHAT_ID")]
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

    /// Verify a channel against its real service (send, receive, media)
    ChannelCheck {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Channel name, as `--channel` takes it
        #[arg(long)]
        channel: String,

        /// Seconds to wait for the echoed message before giving up
        #[arg(long, default_value_t = 45)]
        wait: u64,

        /// Also upload a small attachment if the channel supports media
        #[arg(long, default_value_t = false)]
        media: bool,
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
    #[command(alias = "setup")]
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

        /// Overwrite an existing apollo.json without asking
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Read or change configuration values
    Config {
        #[command(subcommand)]
        action: ConfigAction,

        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,
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

    /// Run autonomous TODO.md-driven coding loop
    Autonomous {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Check cycle interval in seconds
        #[arg(long)]
        interval: Option<u64>,

        /// Reset autonomous status and start fresh
        #[arg(long, default_value_t = false)]
        start: bool,

        /// Clear paused state and continue
        #[arg(long, default_value_t = false)]
        resume: bool,
    },

    /// Run bounded metric-driven experiments and keep only improvements
    Autoresearch {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,

        /// Autoresearch specification (TOML)
        #[arg(long, default_value = ".apollo/autoresearch.toml")]
        spec: PathBuf,

        /// Continue from the persisted ledger
        #[arg(long, default_value_t = false)]
        resume: bool,

        /// Override the spec's iteration limit
        #[arg(long)]
        iterations: Option<usize>,
    },

    /// Swarm commands (multi-agent coordination)
    Swarm {
        #[command(subcommand)]
        action: SwarmAction,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Launch the desktop UI (apollo-ui)
    Ui,

    /// Stop the background agent server
    Stop,

    /// Start the agent automatically when you log in
    Autostart {
        /// Remove the autostart entry instead of adding it
        #[arg(long, default_value_t = false)]
        disable: bool,

        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Workspace directory (defaults to the current directory)
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Run the agent headless, serving only the HTTP/WS API
    Serve {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Override the model
        #[arg(short, long)]
        model: Option<String>,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },

    /// Launch the terminal UI, starting a background server if none is running
    Tui {
        /// Configuration file path
        #[arg(short, long, default_value = "apollo.json")]
        config: String,

        /// Override the model
        #[arg(short, long)]
        model: Option<String>,

        /// Workspace directory
        #[arg(short, long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the effective configuration with secrets masked
    List,

    /// Print one value by dotted path (e.g. `agent.max_rounds`)
    Get {
        /// Dotted config key
        key: String,
    },

    /// Set one value by dotted path, validated against the schema
    Set {
        /// Dotted config key
        key: String,
        /// New value
        value: String,
    },

    /// Print the config file in use
    Path,
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
        #[arg(long, default_value = "gpt-5.5")]
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

/// Build the same unattended runner used by autonomous and autoresearch modes.
fn configure_restricted_automation(cfg: &mut Config) {
    // Autoresearch only needs local code execution and filesystem edits. Keep
    // all conversational memory disabled as well: tool filtering alone does
    // not prevent history, personal-context, or ZKR recall from being added to
    // the model prompt by AgentRunner.
    cfg.toolsets.enabled = vec!["runtime".into(), "fs".into()];
    cfg.toolsets.disabled = vec![
        "web".into(),
        "browser".into(),
        "memory".into(),
        "sessions".into(),
        "messaging".into(),
        "advanced".into(),
        "desktop".into(),
        "media".into(),
        "skills".into(),
    ];
    cfg.memory.inject_context = false;
    cfg.memory.principal_id = None;
    cfg.zkr.enabled = false;
    cfg.zkr.auto_capture = false;
    cfg.zkr.inject_recall = false;
    cfg.zkr.self_improve = false;
}

async fn build_automation_agent(
    config_path: &str,
    workspace: &Path,
    restricted: bool,
) -> anyhow::Result<(Arc<AgentRunner>, Config)> {
    let mut cfg = apollo::bootstrap::load_config_workspace(config_path, Some(workspace));
    if restricted {
        configure_restricted_automation(&mut cfg);
    }
    let provider = build_provider(&cfg);
    let policy = Arc::new(ExecutionPolicy::from_config(&cfg.policy));
    let memory = build_memory_backend(workspace, &cfg).await?;
    let embedding_provider = build_embedding_provider(&cfg)?;
    let system_prompt = prompt::build_system_prompt(workspace).await;
    let discovered_skills = if restricted {
        Vec::new()
    } else {
        skills::discover_skills_for_workspace(Some(workspace))
    };

    #[cfg(feature = "zkr-memory")]
    let zkr_store = apollo::bootstrap::build_zkr_store(workspace, &cfg)
        .ok()
        .flatten();
    #[cfg(feature = "zkr-memory")]
    let mut tools = build_base_tools(
        workspace,
        Arc::clone(&policy),
        memory.clone(),
        embedding_provider,
        Arc::clone(&provider),
        &cfg,
        zkr_store.clone(),
    );
    #[cfg(not(feature = "zkr-memory"))]
    let mut tools = build_base_tools(
        workspace,
        Arc::clone(&policy),
        memory.clone(),
        embedding_provider,
        Arc::clone(&provider),
        &cfg,
    );

    if !restricted {
        for tool in apollo::tools::dynamic::DynamicTool::load_all(Arc::clone(&policy)) {
            tools.push(Arc::new(tool));
        }
    }

    let mut runner = AgentRunner::new(provider, tools, memory, &system_prompt, cfg.model.clone())
        .with_config(cfg.agent.clone())
        .with_mode(agent_mode_from_permission_profile(
            &cfg.agent.permission_profile,
        ))
        .with_workspace(workspace.to_path_buf())
        .with_memory_enabled(!restricted)
        .with_memory_ideas(cfg.memory.clone())
        .with_group_chat(cfg.group_chat.clone())
        .with_skills(discovered_skills)
        .await;
    #[cfg(feature = "zkr-memory")]
    {
        runner = runner.with_zkr(zkr_store, cfg.zkr.clone());
    }

    if !restricted {
        let mut host_reg = apollo::plugin::PluginRegistry::new();
        host_reg.ingest_host_plugins_trusting(
            workspace,
            &cfg.plugin_layer.host_plugin_roots,
            &cfg.plugin_layer.trusted_host_plugins,
        );
        runner = runner.with_plugin_registry(host_reg).await;
    }

    let runner = Arc::new(runner);
    runner.add_hook(Arc::new(PermissionHook::new(
        cfg.agent.permissions.deny.clone(),
        cfg.agent.permissions.allow.clone(),
    )));
    Ok((runner, cfg))
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

    // ponytail: `serve` is `chat` with a channel that never reads stdin, so the
    // whole runner/tool/plugin setup below is shared verbatim.
    let command = match cli.command {
        Some(Commands::Serve {
            config,
            model,
            workspace,
        }) => Some(Commands::Chat {
            config,
            model,
            workspace,
            channel: "none".into(),
            telegram_token: None,
            telegram_chat_id: None,
            discord_token: None,
            discord_channel_id: None,
        }),
        other => other,
    };

    // A bare `apollo` on a machine with no config should set itself up rather
    // than fail on the missing file.
    if command.is_none() {
        setup::ensure_config("apollo.json").await?;
    }

    // Hermes-style: bare `apollo` opens the TUI against a background server,
    // falling back to the line-based CLI chat where apollo-tui isn't installed.
    let command = match command {
        Some(command) => command,
        None if find_sibling_binary("apollo-tui").await.is_some() => Commands::Tui {
            config: "apollo.json".into(),
            model: None,
            workspace: None,
        },
        None => {
            // Say so rather than degrading quietly — a bare `cargo build
            // --release` builds apollo-agent alone, so the usual reason the
            // binary is missing is that nobody knew to ask for it.
            eprintln!(
                "apollo-tui not found — starting the line-based chat instead.\n\
                 Build the terminal UI with: cargo build --release -p apollo-tui"
            );
            Commands::Chat {
                config: "apollo.json".into(),
                model: None,
                workspace: None,
                channel: "cli".into(),
                telegram_token: None,
                telegram_chat_id: None,
                discord_token: None,
                discord_channel_id: None,
            }
        }
    };

    match command {
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
            require_config_file(&config)?;
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
                runner = runner.with_zkr(zkr_store.clone(), cfg.zkr.clone());
            }

            // Built-ins plus any channel a plugin registered. Taken before the
            // registry moves into the runner, so `--channel` can reach it.
            let channel_registry;
            {
                let mut host_reg = apollo::plugin::PluginRegistry::new();
                host_reg.ingest_host_plugins_trusting(
                    &workspace,
                    &cfg.plugin_layer.host_plugin_roots,
                    &cfg.plugin_layer.trusted_host_plugins,
                );
                channel_registry = host_reg.channels().clone();
                runner = runner.with_plugin_registry(host_reg).await;
            }

            #[cfg(feature = "swarm")]
            if let Some(coord) = coordinator {
                runner = runner.with_swarm(coord);
            }

            let runner_arc = Arc::new(runner);

            let http_server = if std::env::var("APOLLO_HTTP")
                .map(|v| v != "0")
                .unwrap_or(true)
            {
                Some(apollo::agent_http::spawn_http_server(Arc::clone(
                    &runner_arc,
                )))
            } else {
                None
            };

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

            // Every channel arm parks forever; `/shutdown` unwinds this scope
            // so the state layer's destructors run.
            let serve_channels = async {
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
                    "none" => {
                        println!(
                            "apollo v{} — {} via {}",
                            env!("CARGO_PKG_VERSION"),
                            cfg.model,
                            cfg.provider.name
                        );
                        println!("   Workspace: {}", workspace.display());
                        println!(
                            "   Agent HTTP: http://{}/v1/chat (APOLLO_HTTP_PORT)",
                            apollo::agent_http::http_listen_addr()
                        );
                        // ponytail: headless park. No cron/heartbeat here — wire them
                        // in if `apollo serve` ever needs to outlive an attached client.
                        std::future::pending::<()>().await;
                    }
                    // Everything else comes from the registry, so a channel is
                    // reachable as soon as it is registered — including one a
                    // plugin added. The arms above stay because they carry
                    // extra wiring (heartbeat, telegram's draft runtime) that
                    // the generic path does not.
                    other => {
                        let settings = apollo::channels::ChannelSettings::new(
                            cfg.channel.token.clone(),
                            cfg.channel.settings.clone(),
                        );
                        let mut ch = channel_registry.build(other, &settings)?;

                        println!("apollo — {} via {}", cfg.model, ch.name());
                        println!("   Listening for messages...");

                        if let Some((cron_rx, cron_shutdown, cron_sched)) = cron_runtime.take() {
                            let (_extra_tx, extra_rx) = tokio::sync::mpsc::channel(1);
                            runner_arc
                                .run_with_runtime_rx(ch.as_mut(), extra_rx, cron_rx, cron_sched)
                                .await?;
                            cron_shutdown.notify_waiters();
                        } else {
                            runner_arc.run(ch.as_mut()).await?;
                        }
                    }
                }
                Ok::<(), anyhow::Error>(())
            };

            tokio::select! {
                result = serve_channels => result?,
                _ = apollo::agent_http::wait_for_shutdown() => {
                    println!("shutting down");
                    // The server task owns the graceful-shutdown future;
                    // nothing drains unless it is awaited here, and dropping
                    // the runtime instead cuts in-flight responses.
                    if let Some(handle) = http_server {
                        apollo::agent_http::drain_http_server(handle).await;
                    }
                }
            }
        }

        Commands::Ask {
            message,
            config,
            model,
        } => {
            require_config_file(&config)?;
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
            let report = collect_doctor_report(Some(&cfg), Some(&config), verbose).await;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", render_doctor_report(&report));
            }
        }

        Commands::ChannelCheck {
            config,
            channel,
            wait,
            media,
        } => {
            let cfg = load_config(&config);
            let registry = apollo::channels::ChannelRegistry::with_builtins();
            let settings = apollo::channels::ChannelSettings::new(
                cfg.channel.token.clone(),
                cfg.channel.settings.clone(),
            );
            let mut ch = registry.build(&channel, &settings)?;

            println!("channel-check: {channel}");
            let steps = apollo::channel_check::run(
                ch.as_mut(),
                &settings,
                &apollo::channel_check::CheckOptions {
                    chat_id: None,
                    wait: std::time::Duration::from_secs(wait),
                    media,
                },
            )
            .await;

            let failed = steps.iter().filter(|s| !s.ok).count();
            if failed == 0 {
                println!("\n{channel}: verified against the real service");
            } else {
                println!("\n{channel}: {failed} step(s) failed");
                std::process::exit(1);
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
                "Commands: chat, ask, doctor, audit, status, mcp, self-update, init, cron, autonomous, swarm"
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
                zkr_store.clone(),
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
                let mut runner = apollo::agent::AgentRunner::new(
                    Arc::clone(&provider),
                    tools.clone(),
                    Arc::clone(&memory),
                    system_prompt,
                    model.clone(),
                )
                .with_config(cfg.agent.clone())
                .with_mode(agent_mode_from_permission_profile(
                    &cfg.agent.permission_profile,
                ));
                #[cfg(feature = "zkr-memory")]
                {
                    runner = runner.with_zkr(zkr_store.clone(), cfg.zkr.clone());
                }
                let runner = Arc::new(runner);
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
            force,
        } => {
            setup::run_init(setup::InitOptions {
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
                force,
            })
            .await?;
        }

        Commands::Config { action, config } => {
            run_config_command(action, &config)?;
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

        Commands::Autoresearch {
            config,
            workspace,
            spec,
            resume,
            iterations,
        } => {
            let workspace = workspace.unwrap_or_else(|| load_config(&config).workspace.clone());
            let spec_path = if spec.is_absolute() {
                spec
            } else {
                workspace.join(spec)
            };
            let mut autoresearch_config = AutoresearchConfig::load(&spec_path)?;
            if let Some(iterations) = iterations {
                autoresearch_config.max_iterations = iterations;
            }
            let (runner, _cfg) = build_automation_agent(&config, &workspace, true).await?;
            println!(
                "apollo v{} — autoresearch (objective: {})",
                env!("CARGO_PKG_VERSION"),
                autoresearch_config.objective
            );
            println!("   Workspace: {}", workspace.display());
            let ledger = AutoresearchLoop::new(autoresearch_config, workspace)
                .run(runner, resume)
                .await?;
            println!(
                "   Best metric: {} ({} records)",
                ledger.best_metric,
                ledger.records.len()
            );
        }

        Commands::Autonomous {
            config,
            workspace,
            interval,
            start,
            resume,
        } => {
            let workspace = workspace.unwrap_or_else(|| load_config(&config).workspace.clone());
            let cfg = apollo::bootstrap::load_config_workspace(&config, Some(&workspace));
            let model = cfg.model.clone();
            let _ = apollo::workspace_init::ensure_workspace_kit(&workspace);

            let provider = build_provider(&cfg);
            let policy = Arc::new(ExecutionPolicy::from_config(&cfg.policy));
            let memory = build_memory_backend(&workspace, &cfg).await?;
            let embedding_provider = build_embedding_provider(&cfg)?;

            let system_prompt = prompt::build_system_prompt(&workspace).await;
            let discovered_skills = skills::discover_skills_for_workspace(Some(&workspace));

            #[cfg(feature = "zkr-memory")]
            let zkr_store = apollo::bootstrap::build_zkr_store(&workspace, &cfg)
                .ok()
                .flatten();
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

            for dt in apollo::tools::dynamic::DynamicTool::load_all(Arc::clone(&policy)) {
                tools.push(Arc::new(dt));
            }

            let mut runner =
                AgentRunner::new(provider, tools, memory.clone(), &system_prompt, model)
                    .with_config(cfg.agent.clone())
                    .with_mode(agent_mode_from_permission_profile(
                        &cfg.agent.permission_profile,
                    ))
                    .with_workspace(workspace.clone())
                    .with_memory_ideas(cfg.memory.clone())
                    .with_group_chat(cfg.group_chat.clone())
                    .with_skills(discovered_skills)
                    .await;
            #[cfg(feature = "zkr-memory")]
            {
                runner = runner.with_zkr(zkr_store.clone(), cfg.zkr.clone());
            }

            {
                let mut host_reg = apollo::plugin::PluginRegistry::new();
                host_reg.ingest_host_plugins_trusting(
                    &workspace,
                    &cfg.plugin_layer.host_plugin_roots,
                    &cfg.plugin_layer.trusted_host_plugins,
                );
                runner = runner.with_plugin_registry(host_reg).await;
            }

            let runner_arc = Arc::new(runner);
            runner_arc.add_hook(Arc::new(PermissionHook::new(
                cfg.agent.permissions.deny.clone(),
                cfg.agent.permissions.allow.clone(),
            )));

            let mut autonomous_config = AutonomousConfig::default();
            if let Some(secs) = interval {
                autonomous_config.interval_secs = secs;
            }
            let interval_secs = autonomous_config.interval_secs;

            let mut autonomous_loop = AutonomousLoop::new(autonomous_config, workspace.clone());
            if start {
                autonomous_loop.start_fresh();
            } else if resume {
                autonomous_loop.resume();
            }

            println!(
                "apollo v{} — autonomous mode (interval={}s)",
                env!("CARGO_PKG_VERSION"),
                interval_secs
            );
            println!("   Workspace: {}", workspace.display());
            println!("   Press Ctrl+C to stop");

            autonomous_loop.run(runner_arc).await;
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

        Commands::Ui => {
            launch_apollo_ui().await?;
        }

        Commands::Tui {
            config,
            model,
            workspace,
        } => {
            launch_apollo_tui(config, model, workspace).await?;
        }

        Commands::Stop => {
            stop_background_server().await?;
        }

        Commands::Autostart {
            disable,
            config,
            workspace,
        } => {
            let workspace = match workspace {
                Some(w) => w,
                None => std::env::current_dir()?,
            };
            configure_autostart(disable, &config, &workspace)?;
        }

        Commands::Serve { .. } => unreachable!("rewritten to Chat above"),
    }

    Ok(())
}

async fn launch_apollo_ui() -> anyhow::Result<()> {
    let binary = find_apollo_ui_binary().await.ok_or_else(|| {
        eprintln!("apollo-ui binary not found.");
        eprintln!("  cargo run -p apollo-ui");
        eprintln!("  cargo build --release -p apollo-ui");
        anyhow::anyhow!("apollo-ui binary not found")
    })?;

    let cwd = std::env::current_dir()?;
    let status = tokio::process::Command::new(&binary)
        .current_dir(&cwd)
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("apollo-ui exited with status: {status}");
    }

    Ok(())
}

/// Spawn `apollo serve` detached and wait until it answers `/health`.
///
/// `kill_on_drop` is deliberately NOT set: the point is a server that keeps
/// running after the client that started it exits.
async fn start_background_server(
    config: &str,
    model: Option<&str>,
    workspace: Option<&Path>,
) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("serve").arg("--config").arg(config);
    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    if let Some(workspace) = workspace {
        cmd.arg("--workspace").arg(workspace);
    }
    // Keep the server's own diagnostics: a bad config is the usual failure and
    // discarding stderr leaves nothing to report.
    let log_path = std::env::temp_dir().join("apollo-serve.log");
    let log = std::fs::File::create(&log_path)?;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log));
    let mut child = cmd.spawn()?;

    let health = async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if server_healthy().await {
                return Ok(());
            }
            if std::time::Instant::now() > deadline {
                return Err(anyhow::anyhow!(
                    "background apollo serve did not become healthy in 30s (see {})",
                    log_path.display()
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    let result = tokio::select! {
        result = health => result,
        status = child.wait() => {
            let detail = std::fs::read_to_string(&log_path).unwrap_or_default();
            let tail = detail.lines().rev().take(10).collect::<Vec<_>>();
            Err(anyhow::anyhow!(
                "background apollo serve exited ({}) before becoming healthy:\n{}",
                status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|e| format!("wait failed: {e}")),
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            ))
        }
    };

    // `kill_on_drop` is off, so dropping the handle leaves the server running;
    // tokio's Drop also queues the child for reaping, which `mem::forget`
    // would skip and leave a zombie behind.
    drop(child);
    result
}

/// Register (or remove) a login item that runs `apollo serve` at startup.
///
/// The config path, the executable path and the workspace path are all
/// interpolated into a launchd plist and a systemd unit. Each is escaped for
/// the format it lands in (see `apollo::escape`) rather than filtered, because
/// a workspace path containing a space is perfectly legitimate on macOS.
#[cfg(any(target_os = "macos", test))]
/// Render the launchd agent. Every interpolated value is XML-escaped, so a
/// directory literally named `</string><key>…` cannot add a plist key.
fn launchd_plist(exe: &str, config: &str, workspace: &str) -> String {
    use apollo::escape::xml_text;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.apollo.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>serve</string>
    <string>--config</string>
    <string>{config}</string>
    <string>--workspace</string>
    <string>{workspace}</string>
  </array>
  <key>WorkingDirectory</key><string>{workspace}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
"#,
        exe = xml_text(exe),
        config = xml_text(config),
        workspace = xml_text(workspace),
    )
}

#[cfg(any(target_os = "linux", test))]
/// Render the systemd user unit. `ExecStart` words are quoted so a path with a
/// space stays one argument and nothing can start a new directive.
fn systemd_unit(exe: &str, config: &str, workspace: &str) -> anyhow::Result<String> {
    use apollo::escape::systemd_argument;
    Ok(format!(
        "[Unit]\n\
         Description=apollo agent\n\
         After=network-online.target\n\n\
         [Service]\n\
         ExecStart={exe} serve --config {config} --workspace {workspace}\n\
         WorkingDirectory={workdir}\n\
         Restart=on-failure\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = systemd_argument(exe)?,
        config = systemd_argument(config)?,
        workspace = systemd_argument(workspace)?,
        workdir = systemd_argument(workspace)?,
    ))
}

fn validate_autostart_config_path(config: &str) -> anyhow::Result<String> {
    let path = config.trim();
    if path.is_empty() {
        anyhow::bail!("--config is required for autostart");
    }
    Ok(path.to_string())
}

/// Writes a launchd agent on macOS and a systemd user unit on Linux, both in
/// the user's own directory — nothing here needs root, and nothing is
/// installed system-wide.
#[allow(clippy::needless_return)]
fn configure_autostart(disable: bool, config: &str, workspace: &Path) -> anyhow::Result<()> {
    let config = validate_autostart_config_path(config)?;
    let config = config.as_str();
    let exe = std::env::current_exe()?;
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.into());
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot locate the home directory"))?;

    #[cfg(target_os = "macos")]
    {
        let path = home.join("Library/LaunchAgents/dev.apollo.agent.plist");
        if disable {
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &path.to_string_lossy()])
                .status();
            if path.is_file() {
                std::fs::remove_file(&path)?;
                println!("Removed {}", path.display());
            } else {
                println!("Autostart was not enabled.");
            }
            return Ok(());
        }

        let plist = launchd_plist(
            &exe.display().to_string(),
            config,
            &workspace.display().to_string(),
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, plist)?;
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &path.to_string_lossy()])
            .status();
        let status = std::process::Command::new("launchctl")
            .args(["load", &path.to_string_lossy()])
            .status()?;
        if !status.success() {
            anyhow::bail!("launchctl load failed for {}", path.display());
        }
        println!("Autostart enabled — {}", path.display());
        println!("Disable with: apollo autostart --disable");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let path = home.join(".config/systemd/user/apollo.service");
        if disable {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "disable", "--now", "apollo.service"])
                .status();
            if path.is_file() {
                std::fs::remove_file(&path)?;
                println!("Removed {}", path.display());
            } else {
                println!("Autostart was not enabled.");
            }
            return Ok(());
        }

        let unit = systemd_unit(
            &exe.display().to_string(),
            config,
            &workspace.display().to_string(),
        )?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, unit)?;
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "apollo.service"])
            .status()?;
        if !status.success() {
            anyhow::bail!("systemctl enable failed for {}", path.display());
        }
        println!("Autostart enabled — {}", path.display());
        println!("Disable with: apollo autostart --disable");
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (disable, config, workspace, exe, home);
        anyhow::bail!("autostart is only implemented for macOS and Linux")
    }
}

/// Ask a running server to exit, using the same token its clients use.
async fn stop_background_server() -> anyhow::Result<()> {
    if !server_healthy().await {
        println!("No apollo server is running.");
        return Ok(());
    }
    let token = apollo::agent_http::load_or_create_token()?;
    let url = format!("http://{}/shutdown", apollo::agent_http::http_listen_addr());
    let resp = apollo::http::standard()
        .post(url)
        .bearer_auth(token)
        .send()
        .await?;
    if resp.status().is_success() {
        println!("Stopped the apollo server.");
        Ok(())
    } else {
        anyhow::bail!("server refused the shutdown: HTTP {}", resp.status())
    }
}

async fn server_healthy() -> bool {
    let url = format!("http://{}/health", apollo::agent_http::http_listen_addr());
    reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn launch_apollo_tui(
    config: String,
    model: Option<String>,
    workspace: Option<PathBuf>,
) -> anyhow::Result<()> {
    let binary = find_sibling_binary("apollo-tui").await.ok_or_else(|| {
        eprintln!("apollo-tui binary not found.");
        eprintln!("  cargo build --release -p apollo-tui");
        anyhow::anyhow!("apollo-tui binary not found")
    })?;

    // ponytail: reuse an already-running server rather than owning a pidfile.
    // The server outlives this TUI on purpose — the next `apollo` starts
    // instantly against it, and cron and heartbeat keep running in between.
    // `apollo stop` shuts it down.
    if !server_healthy().await {
        start_background_server(&config, model.as_deref(), workspace.as_deref()).await?;
    }

    let status = tokio::process::Command::new(&binary)
        .current_dir(std::env::current_dir()?)
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("apollo-tui exited with status: {status}");
    }
    Ok(())
}

async fn find_sibling_binary(name: &str) -> Option<PathBuf> {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join(name);
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }

    let on_path = tokio::process::Command::new("which")
        .arg(name)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false);

    on_path.then(|| PathBuf::from(name))
}

async fn find_apollo_ui_binary() -> Option<PathBuf> {
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join("apollo-ui");
            if sibling.is_file() {
                return Some(sibling);
            }
        }
    }

    let on_path = tokio::process::Command::new("which")
        .arg("apollo-ui")
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false);

    if on_path {
        Some(PathBuf::from("apollo-ui"))
    } else {
        None
    }
}

fn run_config_command(action: ConfigAction, path: &str) -> anyhow::Result<()> {
    match action {
        ConfigAction::Path => {
            let full = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
            println!("{}", full.display());
            if !Path::new(path).exists() {
                eprintln!("(does not exist yet — run `apollo init`)");
            }
        }
        ConfigAction::List => {
            require_config_file(path)?;
            let cfg = load_config(path);
            let mut value = serde_json::to_value(&cfg)?;
            apollo::config::mask_secrets(&mut value);
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        ConfigAction::Get { key } => {
            require_config_file(path)?;
            let cfg = load_config(path);
            let mut value = cfg.get_path(&key)?;
            let leaf = key.rsplit('.').next().unwrap_or(&key).to_string();
            let mut wrapper = serde_json::json!({ leaf.clone(): value });
            apollo::config::mask_secrets(&mut wrapper);
            value = wrapper[&leaf].take();
            match value {
                serde_json::Value::String(s) => println!("{s}"),
                other => println!("{}", serde_json::to_string_pretty(&other)?),
            }
        }
        ConfigAction::Set { key, value } => {
            require_config_file(path)?;
            // Validate against the file itself, not the env-merged view, so a
            // credential picked up from the environment is never written out.
            let cfg = Config::load(path)?;
            let (_validated, written) = cfg.set_path(&key, &value)?;
            let mut raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            Config::splice_into_raw(&mut raw, &key, written.clone());
            let rendered = serde_json::to_string_pretty(&raw)?;
            // apollo.json is a credential file, and a plain write truncates
            // first — a crash there leaves an empty config apollo cannot
            // start from. write_secret_file is 0600 and atomic.
            apollo::fs_secure::write_secret_file(
                std::path::Path::new(path),
                &format!("{rendered}\n"),
            )?;
            let shown = if apollo::config::is_secret_key(key.rsplit('.').next().unwrap_or(&key)) {
                serde_json::Value::String("********".to_string())
            } else {
                written
            };
            println!("{key} = {shown}");
        }
    }
    Ok(())
}

fn config_path_for_cli(cli: &Cli) -> Option<String> {
    match &cli.command {
        None => Some("apollo.json".into()),
        Some(Commands::Chat { config, .. })
        | Some(Commands::Ask { config, .. })
        | Some(Commands::Doctor { config, .. })
        | Some(Commands::Audit { config, .. })
        | Some(Commands::SelfUpdate { config, .. })
        | Some(Commands::Mcp { config, .. })
        | Some(Commands::Autonomous { config, .. })
        | Some(Commands::Autoresearch { config, .. })
        | Some(Commands::Serve { config, .. })
        | Some(Commands::Tui { config, .. }) => Some(config.clone()),
        Some(_) => None,
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

#[cfg(test)]
mod autostart_tests {
    use super::{
        configure_restricted_automation, launchd_plist, systemd_unit,
        validate_autostart_config_path,
    };
    use apollo::config::Config;

    #[test]
    fn restricted_automation_disables_ambient_memory() {
        let mut config = Config::default();
        configure_restricted_automation(&mut config);
        assert!(!config.memory.inject_context);
        assert!(config.memory.principal_id.is_none());
        assert!(!config.zkr.enabled);
        assert!(!config.zkr.inject_recall);
        assert!(!config.zkr.self_improve);
        assert!(!config.zkr.auto_capture);
    }

    /// A path with a space is ordinary on macOS, so it must be accepted and
    /// escaped rather than refused.
    #[test]
    fn accepts_plain_and_spaced_paths() {
        assert_eq!(
            validate_autostart_config_path(" apollo.json ").unwrap(),
            "apollo.json"
        );
        assert_eq!(
            validate_autostart_config_path("/Users/u/My Files/apollo.json").unwrap(),
            "/Users/u/My Files/apollo.json"
        );
        assert!(validate_autostart_config_path("   ").is_err());
    }

    /// exe, config and workspace are all attacker-influenceable; a directory
    /// named to close a <string> element is legal on Unix.
    #[test]
    fn plist_neutralises_hostile_values_in_every_slot() {
        let hostile = "</string><key>RunAtLoad</key><true/><string>/bin/sh";
        for (exe, config, workspace) in [
            (hostile, "apollo.json", "/tmp/ws"),
            ("/usr/bin/apollo", hostile, "/tmp/ws"),
            ("/usr/bin/apollo", "apollo.json", hostile),
        ] {
            let plist = launchd_plist(exe, config, workspace);
            assert!(
                !plist.contains("<key>RunAtLoad</key><true/><string>/bin/sh"),
                "injection survived: {plist}"
            );
            assert!(plist.contains("&lt;/string&gt;"), "not escaped: {plist}");
        }
    }

    #[test]
    fn plist_keeps_an_ordinary_spaced_path_usable() {
        let plist = launchd_plist("/usr/bin/apollo", "apollo.json", "/Users/u/My Files");
        assert!(
            plist.contains("<string>/Users/u/My Files</string>"),
            "{plist}"
        );
    }

    #[test]
    fn systemd_unit_neutralises_hostile_values_in_every_slot() {
        // Extra words must stay inside one quoted argument rather than
        // becoming further options to `apollo serve`.
        let hostile = r#"/tmp/ws" --evil-flag "x"#;
        for (exe, config, workspace) in [
            (hostile, "apollo.json", "/tmp/ws"),
            ("/usr/bin/apollo", hostile, "/tmp/ws"),
            ("/usr/bin/apollo", "apollo.json", hostile),
        ] {
            let unit = systemd_unit(exe, config, workspace).unwrap();
            assert!(!unit.contains(r#"" --evil-flag ""#), "unquoted: {unit}");
            assert!(unit.contains(r#"\" --evil-flag \""#), "not escaped: {unit}");
        }

        // A real line break is the only way to open a new directive, and no
        // unit value can represent one.
        assert!(systemd_unit("/usr/bin/apollo", "a.json\nExecStartPre=/bin/sh", "/tmp").is_err());
        assert!(systemd_unit("/usr/bin/apollo", "a.json", "/tmp\nExecStartPre=/bin/sh").is_err());
        assert!(systemd_unit("/tmp/apollo\nExecStartPre=/bin/sh", "a.json", "/tmp").is_err());
    }

    #[test]
    fn systemd_unit_keeps_specifiers_and_spaces_literal() {
        let unit = systemd_unit("/usr/bin/apollo", "%h/evil.json", "/home/u/my ws").unwrap();
        assert!(unit.contains(r#""%%h/evil.json""#), "{unit}");
        assert!(
            unit.contains(r#"WorkingDirectory="/home/u/my ws""#),
            "{unit}"
        );
    }
}
