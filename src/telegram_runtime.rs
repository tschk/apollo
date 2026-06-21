//! Telegram chat runtime extracted from the CLI entrypoint.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{extract::State, routing::post, Json, Router};

use crate::agent::AgentRunner;
use crate::channels::telegram::TelegramChannel;
use crate::channels::telegram::TelegramIngressFilter;
use crate::channels::Channel;
use crate::channels::IncomingMessage;
use crate::config::ChannelConfig;
use crate::memory::MemoryBackend;
use crate::tools::message::MessageTool;

pub struct TelegramChatRun<'a> {
    pub runner: Arc<AgentRunner>,
    pub memory: Arc<dyn MemoryBackend>,
    pub token: String,
    pub chat_id: i64,
    pub model: String,
    pub skills_count: usize,
    pub workspace: PathBuf,
    pub channel_cfg: &'a ChannelConfig,
}

pub async fn run_telegram_chat(run: TelegramChatRun<'_>) -> anyhow::Result<()> {
    let TelegramChatRun {
        runner,
        memory,
        token,
        chat_id,
        model,
        skills_count,
        workspace,
        channel_cfg,
    } = run;
    let ingress = TelegramIngressFilter {
        allowed_chat_ids: channel_cfg.allowed_chat_ids.clone(),
        allowed_sender_ids: channel_cfg.allowed_sender_ids.clone(),
    };

    let tg = TelegramChannel::new(token.clone(), chat_id)
        .with_memory(memory.clone())
        .with_ingress_filter(ingress.clone());
    let tg_arc = Arc::new(tg.clone());

    runner.add_tool(Arc::new(MessageTool::new(tg_arc))).await;

    println!("unthinkclaw — {} via Telegram", model);
    println!("   Workspace: {}", workspace.display());
    println!("   Chat ID: {}", chat_id);
    println!("   Tools: {}", runner.list_tools().await.join(", "));
    println!("   API: http://127.0.0.1:31337/message");
    println!("   Listening for messages...");

    let mut ch = TelegramChannel::new(token, chat_id)
        .with_memory(memory.clone())
        .with_ingress_filter(ingress);
    let mut rx = ch.start().await?;

    let (cli_tx, mut cli_rx) = tokio::sync::mpsc::channel::<IncomingMessage>(32);
    spawn_local_message_bridge(cli_tx, chat_id);

    let processing = Arc::new(std::sync::atomic::AtomicBool::new(false));

    loop {
        let msg = tokio::select! {
            Some(msg) = rx.recv() => msg,
            Some(msg) = cli_rx.recv() => msg,
            else => break,
        };
        let text = msg.text.trim();

        if processing.load(std::sync::atomic::Ordering::SeqCst) && !text.starts_with('/') {
            runner.steer(text.to_string());
            let _ = tg.send_message("📌 Noted — steering current task.").await;
            continue;
        }

        if text.starts_with('/')
            && handle_command(&runner, &memory, &tg, &msg, text, skills_count).await?
        {
            continue;
        }

        processing.store(true, std::sync::atomic::Ordering::SeqCst);

        // Signal progress — AgentRunner will now use channel.send_typing() during rounds
        let _ = tg.send_typing(&msg.chat_id).await;

        match runner.handle_message(&msg, &tg).await {
            Ok(response) => {
                if response.trim().is_empty() {
                    continue;
                }
                let _ = tg.send_message(&response).await;
            }
            Err(error) => {
                let _ = tg.send_message(&format!("❌ {}", error)).await;
            }
        }

        processing.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    Ok(())
}

fn spawn_local_message_bridge(cli_tx: tokio::sync::mpsc::Sender<IncomingMessage>, chat_id: i64) {
    let chat_id_clone = chat_id.to_string();
    tokio::spawn(async move {
        let app = Router::new()
            .route(
                "/message",
                post(
                    |State(tx): State<tokio::sync::mpsc::Sender<IncomingMessage>>,
                     Json(body): Json<serde_json::Value>| async move {
                        let text = body["message"].as_str().unwrap_or("").to_string();
                        if text.is_empty() {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                "missing 'message' field",
                            );
                        }
                        let msg = IncomingMessage {
                            id: format!("cli-{}", chrono::Utc::now().timestamp()),
                            chat_id: chat_id_clone.clone(),
                            sender_id: "cli".to_string(),
                            sender_name: Some("CLI".to_string()),
                            text,
                            timestamp: chrono::Utc::now(),
                            is_group: false,
                            reply_to: None,
                        };
                        let _ = tx.send(msg).await;
                        (axum::http::StatusCode::OK, "queued")
                    },
                ),
            )
            .with_state(cli_tx);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:31337")
            .await
            .unwrap();
        axum::serve(listener, app).await.unwrap();
    });
}

async fn handle_command(
    runner: &Arc<AgentRunner>,
    memory: &Arc<dyn MemoryBackend>,
    tg: &TelegramChannel,
    msg: &IncomingMessage,
    text: &str,
    discovered_skills_len: usize,
) -> anyhow::Result<bool> {
    let parts: Vec<&str> = text.splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd.as_str() {
        "/stop" | "/cancel" => {
            let _ = tg.send_message("⛔ Stopped.").await;
            Ok(true)
        }
        "/help" => {
            let _ = tg
                .send_message(
                    "🐾 *unthinkclaw commands:*\n\n\
                    /stop — Stop current operation (saves tokens!)\n\
                    /help — Show this message\n\
                    /model — Show current model\n\
                    /model <name> — Switch model\n\
                    /models — List available models\n\
                    /tools — List available tools\n\
                    /status — Bot status\n\
                    /cost — API usage & spending\n\
                    /reset — Clear conversation history\n\n\
                    Everything else is sent to the AI.",
                )
                .await;
            Ok(true)
        }
        "/model" | "/model@unthinkclaw_bot" => {
            if arg.is_empty() {
                let _ = tg
                    .send_message(&format!(
                        "Current model: `{}`\n\nUse `/model <name>` to switch.\nUse `/models` for available options.",
                        runner.get_model()
                    ))
                    .await;
            } else {
                runner.set_model(arg);
                let _ = tg
                    .send_message(&format!("✅ Model switched to: `{}`", arg))
                    .await;
                tracing::info!("Model switched to: {}", arg);
            }
            Ok(true)
        }
        "/models" => {
            let _ = tg
                .send_message(
                    "📋 *Available models:*\n\n\
                    `claude-sonnet-4-5` — Fast, smart (default)\n\
                    `claude-opus-4` — Most capable\n\
                    `claude-haiku-3-5` — Fastest, cheapest\n\n\
                    Switch with: `/model claude-opus-4`",
                )
                .await;
            Ok(true)
        }
        "/tools" => {
            let tool_list = runner.list_tools().await;
            let formatted = tool_list
                .iter()
                .map(|t| format!("• `{}`", t))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = tg
                .send_message(&format!(
                    "🔧 *Available tools ({}):\n\n{}*",
                    tool_list.len(),
                    formatted
                ))
                .await;
            Ok(true)
        }
        "/status" => {
            let _ = tg
                .send_message(&format!(
                    "🐾 *unthinkclaw status:*\n\n\
                    Model: `{}`\n\
                    Tools: {}\n\
                    Skills: {}\n\
                    Channel: Telegram\n\
                    PID: {}",
                    runner.get_model(),
                    runner.list_tools().await.len(),
                    discovered_skills_len,
                    std::process::id(),
                ))
                .await;
            Ok(true)
        }
        "/reset" => {
            let _ = memory
                .forget("chat", &format!("conv_{}", msg.chat_id))
                .await;
            let _ = tg.send_message("🗑 Conversation history cleared.").await;
            Ok(true)
        }
        "/cost" => {
            let summary = runner.get_cost_summary().await;
            let mut by_model: Vec<_> = summary.by_model.iter().collect();
            by_model.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());

            let model_breakdown = if by_model.is_empty() {
                "No usage yet.".to_string()
            } else {
                by_model
                    .iter()
                    .map(|(model, cost)| format!("  • {}: ${:.4}", model, cost))
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            let _ = tg
                .send_message(&format!(
                    "💰 *Cost Summary:*\n\n\
                    Total: ${:.4}\n\
                    Tokens: {}\n\
                    Calls: {}\n\n\
                    By model:\n{}",
                    summary.total_cost, summary.total_tokens, summary.call_count, model_breakdown,
                ))
                .await;
            Ok(true)
        }
        "/start" => {
            let _ = tg
                .send_message(
                    "🐾 *unthinkclaw* — AI assistant\n\n\
                    Just type a message to chat.\n\
                    Use /help for commands.\n\
                    Use /tools to see what I can do.",
                )
                .await;
            Ok(true)
        }
        _ => Ok(false),
    }
}
