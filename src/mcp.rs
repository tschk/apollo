//! MCP (Model Context Protocol) client for integrating with Codex and other MCP servers
//! Implements JSON-RPC 2.0 over stdio transport

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// MCP client for communicating with MCP servers (like Codex)
pub struct McpClient {
    process: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    request_id: Arc<Mutex<u64>>,
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(default)]
    #[allow(dead_code)]
    data: Option<Value>,
}

impl McpClient {
    /// Spawn a new MCP server process
    pub async fn spawn(command: &str, args: &[&str]) -> anyhow::Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

        Ok(Self {
            process: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
            request_id: Arc::new(Mutex::new(0)),
        })
    }

    async fn next_request_id(&self) -> u64 {
        let mut counter = self.request_id.lock().await;
        *counter += 1;
        *counter
    }

    async fn send_request(&self, request: &JsonRpcRequest) -> anyhow::Result<()> {
        let request_json = serde_json::to_string(request)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(request_json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_response(&self) -> anyhow::Result<JsonRpcResponse> {
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        stdout.read_line(&mut line).await?;
        let response: JsonRpcResponse = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Send a request and wait for response
    pub async fn call(&self, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        let id = self.next_request_id().await;

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        self.send_request(&request).await?;
        let response = self.read_response().await?;

        if let Some(error) = response.error {
            anyhow::bail!("MCP error {}: {}", error.code, error.message);
        }

        response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in MCP response"))
    }

    /// Initialize the MCP connection
    pub async fn initialize(&self, client_info: Value) -> anyhow::Result<Value> {
        self.call(
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": client_info
            })),
        )
        .await
    }

    /// List available tools from the MCP server
    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpTool>> {
        let result = self.call("tools/list", None).await?;

        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid tools/list response"))?;

        tools
            .iter()
            .map(|t| serde_json::from_value(t.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Failed to parse tool: {}", e))
    }

    /// Call a tool
    pub async fn call_tool(&self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        self.call(
            "tools/call",
            Some(serde_json::json!({
                "name": name,
                "arguments": arguments
            })),
        )
        .await
    }

    /// Shutdown the MCP server
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.call("shutdown", None).await?;

        let mut process = self.process.lock().await;
        process.kill().await?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Codex-specific MCP client wrapper
pub struct CodexClient {
    mcp: McpClient,
}

impl CodexClient {
    /// Spawn Codex in MCP server mode
    pub async fn spawn() -> anyhow::Result<Self> {
        let mcp = McpClient::spawn("codex", &["mcp-server"]).await?;

        // Initialize
        mcp.initialize(serde_json::json!({
            "name": "unthinkclaw",
            "version": "0.1.0"
        }))
        .await?;

        Ok(Self { mcp })
    }

    /// Run a Codex coding session
    pub async fn run_session(&self, goal: &str, repo_path: &str) -> anyhow::Result<Value> {
        self.mcp
            .call_tool(
                "run",
                serde_json::json!({
                    "goal": goal,
                    "path": repo_path
                }),
            )
            .await
    }

    /// Get available tools from Codex
    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpTool>> {
        self.mcp.list_tools().await
    }

    /// Shutdown Codex
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.mcp.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires codex binary
    async fn test_codex_spawn() {
        let client = CodexClient::spawn().await;
        assert!(client.is_ok());
    }
}
