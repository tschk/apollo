//! HTTP/WebSocket gateway for apollo.
//! Exposes an authenticated control plane for hosted sessions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{
        ws::{WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Json, Path, State,
    },
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::GatewayConfig;
use crate::diagnostics::{classify_tool, DEFAULT_GATEWAY_HTTP_TOOL_DENY};
use crate::hosted::HostedRuntime;

#[derive(Clone)]
pub struct Gateway {
    agents: Arc<RwLock<HashMap<String, String>>>,
    auth_token: String,
    started_at: Instant,
    config: GatewayConfig,
    hosted_runtime: Option<Arc<HostedRuntime>>,
    rate_limit_state: Arc<RwLock<HashMap<String, Vec<Instant>>>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatRequest {
    pub text: String,
    pub context: Option<serde_json::Value>,
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    pub channel: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub text: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContainerStatus {
    pub id: String,
    pub status: String,
    pub memory_mb: u64,
    pub cpu_percent: f32,
}

impl Gateway {
    pub fn new(config: GatewayConfig, auth_token: impl Into<String>) -> Self {
        Self {
            agents: Arc::new(RwLock::new(HashMap::new())),
            auth_token: auth_token.into(),
            started_at: Instant::now(),
            config,
            hosted_runtime: None,
            rate_limit_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_runtime(mut self, hosted_runtime: Arc<HostedRuntime>) -> Self {
        self.hosted_runtime = Some(hosted_runtime);
        self
    }

    pub async fn register_agent(&self, id: String) {
        let mut agents = self.agents.write().await;
        agents.insert(id, String::new());
    }

    pub fn router(&self) -> Router {
        let mut router = Router::new()
            .route("/api/chat", post(Self::handle_chat))
            .route("/api/chat/:agent_id", post(Self::handle_chat_agent))
            .route("/ws", get(Self::handle_websocket))
            .route("/ws/:agent_id", get(Self::handle_websocket_agent))
            .route("/api/status", get(Self::handle_status))
            .route("/metrics", get(Self::handle_metrics_prometheus))
            .route("/api/status/:agent_id", get(Self::handle_agent_status))
            .route("/api/containers", get(Self::handle_containers))
            .route("/api/sessions", get(Self::handle_sessions));

        if self.config.enable_admin_api {
            router = router
                .route("/api/metrics", get(Self::handle_metrics))
                .route("/api/health", get(Self::handle_health))
                .route("/api/memory/:namespace", get(Self::handle_memory_list))
                .route("/api/memory/:namespace/:key", get(Self::handle_memory_get))
                .route("/api/tools", get(Self::handle_tools))
                .route(
                    "/api/tools/:tool_name/execute",
                    post(Self::handle_tool_execute),
                )
                .route("/api/swarm/tasks", get(Self::handle_swarm_tasks))
                .route("/api/swarm/tasks", post(Self::handle_swarm_enqueue))
                .route(
                    "/api/swarm/tasks/:task_id",
                    get(Self::handle_swarm_task_status),
                )
                .route("/api/swarm/workers", get(Self::handle_swarm_workers))
                .route("/api/swarm/status", get(Self::handle_swarm_status))
                .route("/api/plugins", get(Self::handle_plugins_list))
                .route("/api/plugins/:plugin_name", get(Self::handle_plugin_info))
                .route(
                    "/api/plugins/:plugin_name/call/:method",
                    post(Self::handle_plugin_call),
                );
        }

        router
            .with_state(self.clone())
            .layer(DefaultBodyLimit::max(
                self.config.request_body_limit_kb * 1024,
            ))
            .layer(middleware::from_fn_with_state(
                self.clone(),
                Self::enforce_ingress_policy,
            ))
    }

    async fn enforce_ingress_policy(
        State(gateway): State<Gateway>,
        headers: HeaderMap,
        request: Request<axum::body::Body>,
        next: Next,
    ) -> impl IntoResponse {
        if !origin_allowed(&gateway.config, &headers) {
            return StatusCode::FORBIDDEN.into_response();
        }

        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let token = bearer.or_else(|| {
            headers
                .get("x-apollo-token")
                .and_then(|value| value.to_str().ok())
        });

        let Some(token) = token else {
            gateway.record_auth_failure().await;
            return StatusCode::UNAUTHORIZED.into_response();
        };
        if token != gateway.auth_token {
            gateway.record_auth_failure().await;
            return StatusCode::UNAUTHORIZED.into_response();
        }

        if !gateway
            .consume_rate_limit(client_identity(&gateway.config, &headers, token))
            .await
        {
            gateway.record_rate_limited().await;
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }

        let trace_id = headers
            .get("traceparent")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        tracing::info!(trace_id = %trace_id, "gateway ingress accepted");

        let mut response = next.run(request).await;
        if let Ok(value) = header::HeaderValue::from_str(&trace_id) {
            response.headers_mut().insert("x-trace-id", value);
        }
        response
    }

    async fn consume_rate_limit(&self, key: String) -> bool {
        if self.config.rate_limit_per_minute == 0 {
            return true;
        }
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut buckets = self.rate_limit_state.write().await;
        let bucket = buckets.entry(key).or_default();
        bucket.retain(|instant| now.duration_since(*instant) < window);
        if bucket.len() >= self.config.rate_limit_per_minute {
            return false;
        }
        bucket.push(now);
        true
    }

    async fn record_auth_failure(&self) {
        if let Some(runtime) = &self.hosted_runtime {
            runtime.record_auth_failure().await;
        }
    }

    async fn record_rate_limited(&self) {
        if let Some(runtime) = &self.hosted_runtime {
            runtime.record_rate_limited().await;
        }
    }

    async fn handle_chat(
        State(gateway): State<Gateway>,
        Json(json): Json<ChatRequest>,
    ) -> (StatusCode, Json<ChatResponse>) {
        gateway.handle_chat_request(None, json).await
    }

    async fn handle_chat_agent(
        State(gateway): State<Gateway>,
        Path(agent_id): Path<String>,
        Json(json): Json<ChatRequest>,
    ) -> (StatusCode, Json<ChatResponse>) {
        gateway.handle_chat_request(Some(agent_id), json).await
    }

    async fn handle_websocket(
        State(gateway): State<Gateway>,
        ws: WebSocketUpgrade,
    ) -> impl IntoResponse {
        ws.on_upgrade(|socket| Self::websocket_handler(gateway, socket))
    }

    async fn handle_websocket_agent(
        State(gateway): State<Gateway>,
        Path(agent_id): Path<String>,
        ws: WebSocketUpgrade,
    ) -> impl IntoResponse {
        ws.on_upgrade(|socket| Self::websocket_handler_agent(gateway, agent_id, socket))
    }

    async fn websocket_handler(gateway: Gateway, mut socket: WebSocket) {
        let _ = socket
            .send(axum::extract::ws::Message::Text(
                serde_json::json!({
                    "type": "hello",
                    "message": "apollo gateway websocket connected"
                })
                .to_string(),
            ))
            .await;
        while let Some(message) = socket.next().await {
            match message {
                Ok(axum::extract::ws::Message::Text(text)) => {
                    let parsed = serde_json::from_str::<ChatRequest>(&text);
                    match parsed {
                        Ok(request) => {
                            let response = gateway.handle_chat_request(None, request).await;
                            let (_, Json(body)) = response;
                            let response_id = body.id.clone();
                            let _ = socket
                                .send(axum::extract::ws::Message::Text(
                                    serde_json::json!({
                                        "type": "chat_started",
                                        "id": response_id,
                                    })
                                    .to_string(),
                                ))
                                .await;
                            for (index, chunk) in
                                chunk_response(&body.text, 512).into_iter().enumerate()
                            {
                                let _ = socket
                                    .send(axum::extract::ws::Message::Text(
                                        serde_json::json!({
                                            "type": "response_chunk",
                                            "id": response_id,
                                            "index": index,
                                            "chunk": chunk,
                                        })
                                        .to_string(),
                                    ))
                                    .await;
                            }
                            let _ = socket
                                .send(axum::extract::ws::Message::Text(
                                    serde_json::json!({
                                        "type": "chat_completed",
                                        "id": response_id,
                                        "metadata": body.metadata,
                                    })
                                    .to_string(),
                                ))
                                .await;
                        }
                        Err(_) => {
                            let _ = socket
                                .send(axum::extract::ws::Message::Text(
                                    serde_json::json!({
                                        "type": "echo",
                                        "text": text,
                                    })
                                    .to_string(),
                                ))
                                .await;
                        }
                    }
                }
                Ok(axum::extract::ws::Message::Close(_)) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }

    async fn websocket_handler_agent(gateway: Gateway, agent_id: String, mut socket: WebSocket) {
        let session = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .runtime_instances()
                .await
                .into_iter()
                .find(|instance| instance.session_id == agent_id),
            None => None,
        };
        let _ = socket
            .send(axum::extract::ws::Message::Text(
                serde_json::json!({
                    "type": "session_status",
                    "agent_id": agent_id,
                    "session": session,
                })
                .to_string(),
            ))
            .await;
        let mut event_rx = gateway
            .hosted_runtime
            .as_ref()
            .map(|runtime| runtime.subscribe_events());
        loop {
            tokio::select! {
                incoming = socket.next() => {
                    match incoming {
                        Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                        Some(Err(_)) => break,
                        _ => {}
                    }
                }
                event = async {
                    match &mut event_rx {
                        Some(rx) => rx.recv().await.ok(),
                        None => None,
                    }
                } => {
                    if let Some(event) = event {
                        if event.session_id == agent_id {
                            let _ = socket.send(
                                axum::extract::ws::Message::Text(
                                    serde_json::json!({
                                        "type": "session_event",
                                        "payload": event,
                                    }).to_string()
                                )
                            ).await;
                        }
                    }
                }
            }
        }
    }

    async fn handle_status(State(gateway): State<Gateway>) -> Json<serde_json::Value> {
        let agents_connected = gateway.agents.read().await.len();
        let hosted_status = match &gateway.hosted_runtime {
            Some(runtime) => runtime.status().await.ok(),
            None => None,
        };
        let metrics = match &gateway.hosted_runtime {
            Some(runtime) => Some(runtime.metrics().await),
            None => None,
        };
        Json(serde_json::json!({
            "agents_connected": agents_connected,
            "uptime_secs": gateway.started_at.elapsed().as_secs(),
            "admin_api_enabled": gateway.config.enable_admin_api,
            "hosted": hosted_status,
            "metrics": metrics,
        }))
    }

    async fn handle_agent_status(
        State(gateway): State<Gateway>,
        Path(agent_id): Path<String>,
    ) -> (StatusCode, Json<ContainerStatus>) {
        if let Some(runtime) = &gateway.hosted_runtime {
            let runtimes = runtime.runtime_instances().await;
            if let Some(instance) = runtimes
                .into_iter()
                .find(|entry| entry.session_id == agent_id)
            {
                return (
                    StatusCode::OK,
                    Json(ContainerStatus {
                        id: instance.session_id,
                        status: "active".to_string(),
                        memory_mb: 0,
                        cpu_percent: 0.0,
                    }),
                );
            }
        }

        (
            StatusCode::NOT_FOUND,
            Json(ContainerStatus {
                id: agent_id,
                status: "unknown".to_string(),
                memory_mb: 0,
                cpu_percent: 0.0,
            }),
        )
    }

    async fn handle_containers(State(gateway): State<Gateway>) -> Json<Vec<ContainerStatus>> {
        let instances = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .runtime_instances()
                .await
                .into_iter()
                .map(|instance| ContainerStatus {
                    id: instance.session_id,
                    status: "active".to_string(),
                    memory_mb: 0,
                    cpu_percent: 0.0,
                })
                .collect(),
            None => Vec::new(),
        };
        Json(instances)
    }

    async fn handle_sessions(State(gateway): State<Gateway>) -> Json<Vec<serde_json::Value>> {
        let sessions = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .list_sessions()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|session| {
                    serde_json::json!({
                        "session_id": session.session_id,
                        "tenant_id": session.tenant_id,
                        "agent_key": session.agent_key,
                        "channel": session.channel,
                        "model": session.model,
                        "workspace": session.workspace,
                        "runtime_kind": session.runtime_kind,
                        "status": session.status,
                        "last_active": session.last_active,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Json(sessions)
    }

    async fn handle_metrics(State(gateway): State<Gateway>) -> Json<serde_json::Value> {
        let Some(runtime) = &gateway.hosted_runtime else {
            return Json(serde_json::json!({}));
        };
        Json(serde_json::json!({
            "runtime": runtime.metrics().await,
            "tenants": runtime.tenant_health().await,
            "sessions": runtime.runtime_instances().await,
        }))
    }

    async fn handle_metrics_prometheus(State(gateway): State<Gateway>) -> Response {
        let Some(runtime) = &gateway.hosted_runtime else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "apollo_gateway_runtime_attached 0\n".to_string(),
            )
                .into_response();
        };

        let runtime_metrics = runtime.metrics().await;
        let tenant_health = runtime.tenant_health().await;
        let sessions = runtime.runtime_instances().await;

        let mut body = String::new();
        body.push_str(
            "# HELP apollo_gateway_runtime_attached Whether a hosted runtime is attached.\n",
        );
        body.push_str("# TYPE apollo_gateway_runtime_attached gauge\n");
        body.push_str("apollo_gateway_runtime_attached 1\n");
        body.push_str(
            "# HELP apollo_gateway_requests_total Total authenticated gateway requests.\n",
        );
        body.push_str("# TYPE apollo_gateway_requests_total counter\n");
        body.push_str(&format!(
            "apollo_gateway_requests_total {}\n",
            runtime_metrics.total_requests
        ));
        body.push_str("# HELP apollo_gateway_errors_total Total gateway request errors.\n");
        body.push_str("# TYPE apollo_gateway_errors_total counter\n");
        body.push_str(&format!(
            "apollo_gateway_errors_total {}\n",
            runtime_metrics.total_errors
        ));
        body.push_str(
            "# HELP apollo_gateway_auth_failures_total Total gateway auth failures.\n",
        );
        body.push_str("# TYPE apollo_gateway_auth_failures_total counter\n");
        body.push_str(&format!(
            "apollo_gateway_auth_failures_total {}\n",
            runtime_metrics.auth_failures
        ));
        body.push_str(
            "# HELP apollo_gateway_rate_limited_total Total gateway rate-limited requests.\n",
        );
        body.push_str("# TYPE apollo_gateway_rate_limited_total counter\n");
        body.push_str(&format!(
            "apollo_gateway_rate_limited_total {}\n",
            runtime_metrics.rate_limited
        ));
        body.push_str(
            "# HELP apollo_gateway_latency_ms Average gateway latency in milliseconds.\n",
        );
        body.push_str("# TYPE apollo_gateway_latency_ms gauge\n");
        body.push_str(&format!(
            "apollo_gateway_latency_ms {}\n",
            runtime_metrics.average_latency_ms
        ));
        body.push_str("# HELP apollo_gateway_sessions Active runtime sessions.\n");
        body.push_str("# TYPE apollo_gateway_sessions gauge\n");
        body.push_str(&format!(
            "apollo_gateway_sessions {}\n",
            sessions.len()
        ));
        body.push_str(
            "# HELP apollo_gateway_tenant_requests_total Total requests per tenant.\n",
        );
        body.push_str("# TYPE apollo_gateway_tenant_requests_total counter\n");
        for tenant in &tenant_health {
            body.push_str(&format!(
                "apollo_gateway_tenant_requests_total{{tenant_id=\"{}\"}} {}\n",
                tenant.tenant_id, tenant.total_requests
            ));
            body.push_str(&format!(
                "apollo_gateway_tenant_errors_total{{tenant_id=\"{}\"}} {}\n",
                tenant.tenant_id, tenant.total_errors
            ));
            body.push_str(&format!(
                "apollo_gateway_tenant_latency_ms{{tenant_id=\"{}\"}} {}\n",
                tenant.tenant_id, tenant.average_latency_ms
            ));
        }

        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response()
    }

    async fn handle_health(State(gateway): State<Gateway>) -> Json<serde_json::Value> {
        let Some(runtime) = &gateway.hosted_runtime else {
            return Json(serde_json::json!({
                "ok": false,
                "reason": "gateway runtime is not attached"
            }));
        };
        let status = runtime.status().await.ok();
        Json(serde_json::json!({
            "ok": true,
            "uptime_secs": gateway.started_at.elapsed().as_secs(),
            "status": status,
        }))
    }

    async fn handle_memory_list(
        State(gateway): State<Gateway>,
        Path(namespace): Path<String>,
    ) -> Json<Vec<String>> {
        let entries = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .list_memory(&namespace)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|entry| entry.key)
                .collect(),
            None => Vec::new(),
        };
        Json(entries)
    }

    async fn handle_memory_get(
        State(gateway): State<Gateway>,
        Path((namespace, key)): Path<(String, String)>,
    ) -> Json<serde_json::Value> {
        let value = match &gateway.hosted_runtime {
            Some(runtime) => runtime.get_memory(&namespace, &key).await.ok().flatten(),
            None => None,
        };
        Json(match value {
            Some(entry) => serde_json::json!({
                "namespace": namespace,
                "key": entry.key,
                "value": entry.value,
                "metadata": entry.metadata,
                "created_at": entry.created_at,
            }),
            None => serde_json::json!({
                "namespace": namespace,
                "key": key,
                "value": null,
            }),
        })
    }

    async fn handle_tools(State(gateway): State<Gateway>) -> Json<Vec<serde_json::Value>> {
        let safe_tools = gateway
            .hosted_runtime
            .as_ref()
            .map(|runtime| runtime.safe_gateway_tools())
            .unwrap_or_default();
        let mut tools = safe_tools
            .into_iter()
            .map(|tool| {
                let spec = tool.spec();
                let classification = classify_tool(tool.name());
                serde_json::json!({
                    "name": spec.name,
                    "description": spec.description,
                    "risk": classification.risk,
                    "approval_required": classification.approval_required,
                    "denied_over_gateway_http_by_default": classification.denied_over_gateway_http_by_default,
                })
            })
            .collect::<Vec<_>>();
        tools.push(serde_json::json!({ "http_default_deny": DEFAULT_GATEWAY_HTTP_TOOL_DENY }));
        Json(tools)
    }

    async fn handle_tool_execute(
        State(gateway): State<Gateway>,
        Path(tool_name): Path<String>,
        Json(payload): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let Some(runtime) = &gateway.hosted_runtime else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "tool": tool_name,
                    "error": "gateway runtime is not attached",
                })),
            );
        };

        match runtime
            .execute_gateway_tool(&tool_name, &payload.to_string())
            .await
        {
            Ok(result) => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "tool": tool_name,
                    "output": result.output,
                    "is_error": result.is_error,
                })),
            ),
            Err(error) => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "tool": tool_name,
                    "error": error.to_string(),
                })),
            ),
        }
    }

    async fn handle_swarm_tasks(State(gateway): State<Gateway>) -> Json<Vec<serde_json::Value>> {
        let tasks = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .list_swarm_tasks()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|task| {
                    serde_json::json!({
                        "task_id": task.task_id,
                        "title": task.title,
                        "description": task.description,
                        "status": task.status,
                        "priority": task.priority,
                        "assigned_to": task.assigned_to,
                        "created_at": task.created_at,
                        "updated_at": task.updated_at,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Json(tasks)
    }

    async fn handle_swarm_enqueue(
        State(gateway): State<Gateway>,
        Json(payload): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let Some(runtime) = &gateway.hosted_runtime else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "gateway runtime is not attached"
                })),
            );
        };

        let goal = payload
            .get("goal")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled");
        match runtime.enqueue_swarm_task(goal).await {
            Ok(task_id) => (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "task_id": task_id,
                    "goal": goal,
                    "status": "pending"
                })),
            ),
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": error.to_string(),
                })),
            ),
        }
    }

    async fn handle_swarm_task_status(
        State(gateway): State<Gateway>,
        Path(task_id): Path<String>,
    ) -> Json<serde_json::Value> {
        let task = match &gateway.hosted_runtime {
            Some(runtime) => runtime.get_swarm_task(&task_id).await.ok().flatten(),
            None => None,
        };
        Json(match task {
            Some(task) => serde_json::json!({
                "task_id": task.task_id,
                "status": task.status,
                "title": task.title,
                "priority": task.priority,
                "assigned_to": task.assigned_to,
            }),
            None => serde_json::json!({
                "task_id": task_id,
                "status": "unknown",
            }),
        })
    }

    async fn handle_swarm_workers(State(gateway): State<Gateway>) -> Json<Vec<serde_json::Value>> {
        let workers = match &gateway.hosted_runtime {
            Some(runtime) => runtime
                .list_swarm_agents()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|agent| {
                    serde_json::json!({
                        "agent_id": agent.agent_id,
                        "name": agent.name,
                        "status": agent.status,
                        "model": agent.model,
                        "tools": agent.tools,
                        "max_concurrent": agent.max_concurrent,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Json(workers)
    }

    async fn handle_swarm_status(State(gateway): State<Gateway>) -> Json<serde_json::Value> {
        let (tasks, workers) = match &gateway.hosted_runtime {
            Some(runtime) => (
                runtime.list_swarm_tasks().await.unwrap_or_default(),
                runtime.list_swarm_agents().await.unwrap_or_default(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        Json(serde_json::json!({
            "total_workers": workers.len(),
            "idle_workers": workers.iter().filter(|agent| agent.status.to_string() == "idle").count(),
            "total_tasks": tasks.len(),
            "pending_tasks": tasks.iter().filter(|task| task.status.to_string() == "pending").count(),
            "completed_tasks": tasks.iter().filter(|task| task.status.to_string() == "done").count()
        }))
    }

    async fn handle_plugins_list(State(gateway): State<Gateway>) -> Json<Vec<String>> {
        let plugins = match &gateway.hosted_runtime {
            Some(runtime) => runtime.list_plugins(),
            None => Vec::new(),
        };
        Json(plugins)
    }

    async fn handle_plugin_info(
        State(gateway): State<Gateway>,
        Path(plugin_name): Path<String>,
    ) -> Json<serde_json::Value> {
        let info = match &gateway.hosted_runtime {
            Some(runtime) => runtime.plugin_info(&plugin_name),
            None => None,
        };
        Json(match info {
            Some(info) => serde_json::json!(info),
            None => serde_json::json!({
                "name": plugin_name,
                "error": "plugin not found",
            }),
        })
    }

    async fn handle_plugin_call(
        State(gateway): State<Gateway>,
        Path((plugin_name, method)): Path<(String, String)>,
        Json(params): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let Some(runtime) = &gateway.hosted_runtime else {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "plugin": plugin_name,
                    "error": "gateway runtime is not attached",
                })),
            );
        };

        tracing::info!(plugin = %plugin_name, method = %method, "gateway plugin call");
        match runtime.call_plugin(&plugin_name, &method, params).await {
            Ok(result) => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "plugin": plugin_name,
                    "method": method,
                    "result": result,
                })),
            ),
            Err(error) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "plugin": plugin_name,
                    "method": method,
                    "error": error.to_string(),
                })),
            ),
        }
    }

    async fn handle_chat_request(
        &self,
        agent_id: Option<String>,
        json: ChatRequest,
    ) -> (StatusCode, Json<ChatResponse>) {
        let Some(runtime) = &self.hosted_runtime else {
            let response = ChatResponse {
                id: uuid::Uuid::new_v4().to_string(),
                text: "Gateway runtime is not attached".to_string(),
                metadata: serde_json::json!({}),
            };
            return (StatusCode::SERVICE_UNAVAILABLE, Json(response));
        };

        let result = tokio::time::timeout(
            Duration::from_secs(self.config.request_timeout_secs),
            runtime.chat(
                &json.text,
                json.user_id.as_deref(),
                json.session_id.as_deref(),
                json.channel.as_deref(),
                agent_id.as_deref().or(json.agent_id.as_deref()),
            ),
        )
        .await;

        tracing::info!(
            gateway_agent = agent_id
                .as_deref()
                .or(json.agent_id.as_deref())
                .unwrap_or("main"),
            gateway_user = json.user_id.as_deref().unwrap_or("gateway-user"),
            gateway_session = json.session_id.as_deref().unwrap_or("auto"),
            "gateway chat request"
        );

        match result {
            Ok(Ok((text, lease))) => {
                let response = ChatResponse {
                    id: uuid::Uuid::new_v4().to_string(),
                    text,
                    metadata: serde_json::json!({
                        "session_id": lease.session.session_id,
                        "tenant_id": lease.tenant.tenant_id,
                        "runtime_kind": lease.session.runtime_kind,
                        "agent_key": lease.session.agent_key,
                        "workspace": lease.session.workspace,
                    }),
                };
                (StatusCode::OK, Json(response))
            }
            Ok(Err(error)) => {
                let response = ChatResponse {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: format!("Gateway chat failed: {error}"),
                    metadata: serde_json::json!({}),
                };
                (StatusCode::INTERNAL_SERVER_ERROR, Json(response))
            }
            Err(_) => {
                let response = ChatResponse {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: "Gateway chat timed out".to_string(),
                    metadata: serde_json::json!({}),
                };
                (StatusCode::REQUEST_TIMEOUT, Json(response))
            }
        }
    }
}

fn origin_allowed(config: &GatewayConfig, headers: &HeaderMap) -> bool {
    if config.allowed_origins.is_empty() {
        return true;
    }
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    config
        .allowed_origins
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(origin))
}

fn client_identity(_config: &GatewayConfig, _headers: &HeaderMap, token: &str) -> String {
    token.to_string()
}

fn chunk_response(text: &str, chunk_size: usize) -> Vec<String> {
    if text.is_empty() || chunk_size == 0 {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.len() >= chunk_size {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub async fn start_gateway(
    addr: &str,
    config: GatewayConfig,
    auth_token: &str,
) -> anyhow::Result<()> {
    if auth_token.trim().is_empty() {
        anyhow::bail!("gateway auth token must be configured");
    }
    let gateway = Gateway::new(config, auth_token);
    let app = gateway.router();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Gateway listening on {}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn start_gateway_with_runtime(
    addr: &str,
    config: GatewayConfig,
    auth_token: &str,
    hosted_runtime: Arc<HostedRuntime>,
) -> anyhow::Result<()> {
    if auth_token.trim().is_empty() {
        anyhow::bail!("gateway auth token must be configured");
    }
    let gateway = Gateway::new(config, auth_token).with_runtime(hosted_runtime);
    let app = gateway.router();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("Gateway listening on {}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}
