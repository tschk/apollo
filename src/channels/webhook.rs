//! Shared webhook receiver for channels that are pushed to rather than polled.
//!
//! Google Chat, WhatsApp and Teams all need the same thing: bind a port, accept
//! a JSON POST, turn it into an `IncomingMessage`, hand it to the channel's
//! sender. Each of them used to hand-roll that, and each of them used to
//! `.unwrap()` the bind *inside* a spawned task — so a taken port panicked a
//! detached task and `start()` still returned an apparently healthy receiver.
//! Binding happens here in an async fn that returns a `Result`, so the caller
//! sees the failure.

use std::net::SocketAddr;

use axum::{routing::post, Json, Router};
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::IncomingMessage;

/// Turns a decoded webhook body into zero or more messages. A single payload
/// can carry a batch (WhatsApp nests them several levels deep), so this yields
/// a `Vec` rather than an `Option`.
pub type WebhookParser = fn(&Value) -> Vec<IncomingMessage>;

/// A `POST {path}` route that parses each payload and forwards what it finds.
pub fn json_route(path: &str, parse: WebhookParser, tx: mpsc::Sender<IncomingMessage>) -> Router {
    Router::new().route(
        path,
        post(move |Json(body): Json<Value>| {
            let tx = tx.clone();
            async move {
                for incoming in parse(&body) {
                    if tx.send(incoming).await.is_err() {
                        break;
                    }
                }
                axum::http::StatusCode::OK
            }
        }),
    )
}

/// Bind `addr` and serve `app` in the background.
///
/// Binding is awaited here so a port conflict is returned to `start()` instead
/// of panicking a task nobody is watching.
pub async fn spawn(addr: SocketAddr, app: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("webhook receiver could not bind {addr}: {e}"))?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("webhook receiver on {addr} stopped: {e}");
        }
    });
    Ok(())
}

/// Bind and serve a single JSON webhook route — the whole setup for a channel
/// that needs nothing else.
pub async fn serve_json(
    addr: SocketAddr,
    path: &str,
    parse: WebhookParser,
    tx: mpsc::Sender<IncomingMessage>,
) -> anyhow::Result<()> {
    spawn(addr, json_route(path, parse, tx)).await
}
