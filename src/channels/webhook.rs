//! Shared webhook receiver for channels that are pushed to rather than polled.
//!
//! Google Chat, WhatsApp and Teams all need the same thing: bind a port, accept
//! a JSON POST, turn it into an `IncomingMessage`, hand it to the channel's
//! sender. Each of them used to hand-roll that, and each of them used to
//! `.unwrap()` the bind *inside* a spawned task — so a taken port panicked a
//! detached task and `start()` still returned an apparently healthy receiver.
//! Binding happens here in an async fn that returns a `Result`, so the caller
//! sees the failure.
//!
//! Every route is authenticated. These receivers bind publicly so the platform
//! can reach them, and whatever they accept goes straight to the agent, which
//! has shell and file tools. An unverified POST is therefore remote code
//! execution, so `json_route` takes a `WebhookVerifier` and refuses (401) any
//! request it does not vouch for before the body is even parsed.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::traits::IncomingMessage;

/// Turns a decoded webhook body into zero or more messages. A single payload
/// can carry a batch (WhatsApp nests them several levels deep), so this yields
/// a `Vec` rather than an `Option`.
pub type WebhookParser = fn(&Value) -> Vec<IncomingMessage>;

/// Proves a webhook request came from the platform, not from whoever found the
/// port. Gets the raw body because HMAC schemes sign the exact bytes.
#[async_trait::async_trait]
pub trait WebhookVerifier: Send + Sync {
    async fn verify(&self, headers: &HeaderMap, body: &[u8]) -> bool;
}

/// A `POST {path}` route that verifies, parses each payload and forwards what
/// it finds.
pub fn json_route(
    path: &str,
    parse: WebhookParser,
    verifier: Arc<dyn WebhookVerifier>,
    tx: mpsc::Sender<IncomingMessage>,
) -> Router {
    Router::new().route(
        path,
        post(move |headers: HeaderMap, body: Bytes| {
            let tx = tx.clone();
            let verifier = Arc::clone(&verifier);
            async move {
                if !verifier.verify(&headers, &body).await {
                    tracing::warn!("rejected webhook request that failed verification");
                    return StatusCode::UNAUTHORIZED;
                }
                let Ok(body) = serde_json::from_slice::<Value>(&body) else {
                    return StatusCode::BAD_REQUEST;
                };
                for incoming in parse(&body) {
                    if tx.send(incoming).await.is_err() {
                        break;
                    }
                }
                StatusCode::OK
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

/// Bind and serve a single verified JSON webhook route — the whole setup for a
/// channel that needs nothing else.
pub async fn serve_json(
    addr: SocketAddr,
    path: &str,
    parse: WebhookParser,
    verifier: Arc<dyn WebhookVerifier>,
    tx: mpsc::Sender<IncomingMessage>,
) -> anyhow::Result<()> {
    spawn(addr, json_route(path, parse, verifier, tx)).await
}

/// Verifies an RS256 bearer JWT against a platform's published JWKS.
///
/// Google Chat and the Bot Framework both authenticate their webhook calls
/// this way: `Authorization: Bearer <jwt>`, signed by a key from a well-known
/// JWKS URL, with a fixed issuer and the bot's own audience.
#[cfg(any(feature = "channel-googlechat", feature = "channel-msteams"))]
pub struct JwtVerifier {
    jwks_url: String,
    issuer: String,
    audience: String,
    keys: tokio::sync::Mutex<Option<(std::time::Instant, jsonwebtoken::jwk::JwkSet)>>,
}

#[cfg(any(feature = "channel-googlechat", feature = "channel-msteams"))]
impl JwtVerifier {
    /// Keys rotate; a cached set is trusted this long before a refetch.
    const TTL: std::time::Duration = std::time::Duration::from_secs(3600);
    /// An unknown `kid` triggers a refetch, but no more often than this, so
    /// garbage tokens cannot turn the receiver into a JWKS-fetch amplifier.
    const MIN_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

    pub fn new(
        jwks_url: impl Into<String>,
        issuer: impl Into<String>,
        audience: impl Into<String>,
    ) -> Self {
        Self {
            jwks_url: jwks_url.into(),
            issuer: issuer.into(),
            audience: audience.into(),
            keys: tokio::sync::Mutex::new(None),
        }
    }

    async fn key_for(&self, kid: &str) -> Option<jsonwebtoken::DecodingKey> {
        let mut cache = self.keys.lock().await;
        if let Some((fetched, set)) = cache.as_ref() {
            let age = fetched.elapsed();
            match set.find(kid) {
                Some(jwk) if age < Self::TTL => {
                    return jsonwebtoken::DecodingKey::from_jwk(jwk).ok()
                }
                None if age < Self::MIN_REFRESH => return None,
                _ => {}
            }
        }

        match self.fetch().await {
            Ok(set) => *cache = Some((std::time::Instant::now(), set)),
            // Keep a stale set rather than failing every request while the
            // JWKS endpoint is briefly unreachable.
            Err(e) => tracing::warn!("webhook JWKS fetch from {} failed: {e}", self.jwks_url),
        }
        let (_, set) = cache.as_ref()?;
        jsonwebtoken::DecodingKey::from_jwk(set.find(kid)?).ok()
    }

    async fn fetch(&self) -> anyhow::Result<jsonwebtoken::jwk::JwkSet> {
        Ok(crate::http::shared()
            .get(&self.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[cfg(any(feature = "channel-googlechat", feature = "channel-msteams"))]
#[async_trait::async_trait]
impl WebhookVerifier for JwtVerifier {
    async fn verify(&self, headers: &HeaderMap, _body: &[u8]) -> bool {
        use jsonwebtoken::{Algorithm, Validation};

        let Some(token) = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        let Ok(header) = jsonwebtoken::decode_header(token) else {
            return false;
        };
        // Pin the algorithm: never let the token choose how it is checked.
        if header.alg != Algorithm::RS256 {
            return false;
        }
        let Some(key) = header.kid.as_deref() else {
            return false;
        };
        let Some(key) = self.key_for(key).await else {
            return false;
        };

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        jsonwebtoken::decode::<Value>(token, &key, &validation).is_ok()
    }
}
