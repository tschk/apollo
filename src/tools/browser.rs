//! Browser tool — agent-browser backend with full automation support

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

use super::traits::*;

pub struct BrowserTool {
    session_name: Option<String>,
    allowed_domains: Vec<String>,
}

impl BrowserTool {
    pub fn new() -> Self {
        Self {
            session_name: None,
            allowed_domains: Vec::new(),
        }
    }

    pub fn with_session(mut self, name: String) -> Self {
        self.session_name = Some(name);
        self
    }

    pub fn with_allowed_domains(mut self, domains: Vec<String>) -> Self {
        self.allowed_domains = domains;
        self
    }

    /// Check if agent-browser CLI is available
    pub async fn is_available() -> bool {
        Command::new("agent-browser")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Validate URL against allowlist
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        // Block file:// URLs (security risk)
        if url.starts_with("file://") {
            anyhow::bail!("file:// URLs are not allowed");
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        // If allowlist is set, enforce it
        if !self.allowed_domains.is_empty() {
            let host = extract_host(url)?;
            if !host_matches_allowlist(&host, &self.allowed_domains) {
                anyhow::bail!("Host '{host}' not in allowed_domains");
            }
        }

        Ok(())
    }

    /// Execute agent-browser command
    async fn run_command(&self, args: &[&str]) -> anyhow::Result<AgentBrowserResponse> {
        let mut cmd = Command::new("agent-browser");

        if let Some(ref session) = self.session_name {
            cmd.arg("--session").arg(session);
        }

        cmd.args(args).arg("--json");

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            tracing::debug!("agent-browser stderr: {}", stderr);
        }

        // Parse JSON response
        if let Ok(resp) = serde_json::from_str::<AgentBrowserResponse>(&stdout) {
            return Ok(resp);
        }

        // Fallback for non-JSON output
        if output.status.success() {
            Ok(AgentBrowserResponse {
                success: true,
                data: Some(serde_json::json!({ "output": stdout.trim() })),
                error: None,
            })
        } else {
            Ok(AgentBrowserResponse {
                success: false,
                data: None,
                error: Some(stderr.trim().to_string()),
            })
        }
    }

    fn to_result(&self, resp: AgentBrowserResponse) -> anyhow::Result<ToolResult> {
        if resp.success {
            let output = resp
                .data
                .and_then(|d| serde_json::to_string_pretty(&d).ok())
                .unwrap_or_else(|| "Success".to_string());
            Ok(ToolResult::success(output))
        } else {
            Ok(ToolResult::error(
                resp.error.unwrap_or_else(|| "Unknown error".to_string()),
            ))
        }
    }
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct AgentBrowserResponse {
    success: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum BrowserAction {
    Open {
        url: String,
    },
    Snapshot {
        #[serde(default)]
        interactive_only: bool,
        #[serde(default)]
        compact: bool,
        #[serde(default)]
        depth: Option<u32>,
    },
    Click {
        selector: String,
    },
    Fill {
        selector: String,
        value: String,
    },
    Type {
        selector: String,
        text: String,
    },
    GetText {
        selector: String,
    },
    GetTitle,
    GetUrl,
    Screenshot {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        full_page: bool,
    },
    Wait {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        ms: Option<u64>,
    },
    Press {
        key: String,
    },
    Hover {
        selector: String,
    },
    Scroll {
        direction: String,
        #[serde(default)]
        pixels: Option<u32>,
    },
    IsVisible {
        selector: String,
    },
    Close,
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "browser".to_string(),
            description: "Control a browser via agent-browser CLI. Actions: open (navigate), snapshot (get accessibility tree), click, fill, type, get_text, get_title, get_url, screenshot, wait, press, hover, scroll, is_visible, close.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "snapshot", "click", "fill", "type", "get_text", "get_title", "get_url", "screenshot", "wait", "press", "hover", "scroll", "is_visible", "close"],
                        "description": "Browser action to perform"
                    },
                    "url": {
                        "type": "string",
                        "description": "URL (for open action)"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector or element ref (for click, fill, type, get_text, hover, is_visible)"
                    },
                    "value": {
                        "type": "string",
                        "description": "Value to fill (for fill action)"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (for type action)"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key to press (for press action)"
                    },
                    "direction": {
                        "type": "string",
                        "description": "Scroll direction: up, down, left, right (for scroll action)"
                    },
                    "pixels": {
                        "type": "integer",
                        "description": "Pixels to scroll (for scroll action)"
                    },
                    "interactive_only": {
                        "type": "boolean",
                        "description": "Only show interactive elements in snapshot"
                    },
                    "compact": {
                        "type": "boolean",
                        "description": "Compact snapshot output"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Max depth for snapshot tree"
                    },
                    "path": {
                        "type": "string",
                        "description": "File path for screenshot"
                    },
                    "full_page": {
                        "type": "boolean",
                        "description": "Capture full page screenshot"
                    },
                    "ms": {
                        "type": "integer",
                        "description": "Milliseconds to wait (for wait action)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let action: BrowserAction = serde_json::from_str(arguments)?;

        match action {
            BrowserAction::Open { url } => {
                self.validate_url(&url)?;
                let resp = self.run_command(&["open", &url]).await?;
                self.to_result(resp)
            }

            BrowserAction::Snapshot {
                interactive_only,
                compact,
                depth,
            } => {
                let mut args = vec!["snapshot"];
                if interactive_only {
                    args.push("-i");
                }
                if compact {
                    args.push("-c");
                }
                let depth_str;
                if let Some(d) = depth {
                    args.push("-d");
                    depth_str = d.to_string();
                    args.push(&depth_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Click { selector } => {
                let resp = self.run_command(&["click", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Fill { selector, value } => {
                let resp = self.run_command(&["fill", &selector, &value]).await?;
                self.to_result(resp)
            }

            BrowserAction::Type { selector, text } => {
                let resp = self.run_command(&["type", &selector, &text]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetText { selector } => {
                let resp = self.run_command(&["get", "text", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetTitle => {
                let resp = self.run_command(&["get", "title"]).await?;
                self.to_result(resp)
            }

            BrowserAction::GetUrl => {
                let resp = self.run_command(&["get", "url"]).await?;
                self.to_result(resp)
            }

            BrowserAction::Screenshot { path, full_page } => {
                let mut args = vec!["screenshot"];
                if let Some(ref p) = path {
                    args.push(p);
                }
                if full_page {
                    args.push("--full");
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Wait { selector, ms } => {
                let mut args = vec!["wait"];
                let ms_str;
                if let Some(sel) = selector.as_ref() {
                    args.push(sel);
                } else if let Some(millis) = ms {
                    ms_str = millis.to_string();
                    args.push(&ms_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::Press { key } => {
                let resp = self.run_command(&["press", &key]).await?;
                self.to_result(resp)
            }

            BrowserAction::Hover { selector } => {
                let resp = self.run_command(&["hover", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Scroll { direction, pixels } => {
                let mut args = vec!["scroll", &direction];
                let px_str;
                if let Some(px) = pixels {
                    px_str = px.to_string();
                    args.push(&px_str);
                }
                let resp = self.run_command(&args).await?;
                self.to_result(resp)
            }

            BrowserAction::IsVisible { selector } => {
                let resp = self.run_command(&["is", "visible", &selector]).await?;
                self.to_result(resp)
            }

            BrowserAction::Close => {
                let resp = self.run_command(&["close"]).await?;
                self.to_result(resp)
            }
        }
    }
}

/// Extract host from URL
fn extract_host(url: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(url)?;
    parsed
        .host_str()
        .map(|h| h.to_string())
        .ok_or_else(|| anyhow::anyhow!("No host in URL"))
}

/// Check if host matches allowlist (supports wildcards like *.example.com)
fn host_matches_allowlist(host: &str, allowlist: &[String]) -> bool {
    for pattern in allowlist {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if host.ends_with(suffix) || host == suffix {
                return true;
            }
        } else if host == pattern {
            return true;
        }
    }
    false
}
