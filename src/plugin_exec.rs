//! Running tools declared by a host plugin manifest.
//!
//! # Why this is deny-by-default
//!
//! A host plugin is a directory apollo *found*. Turning a manifest in a found
//! directory into an executable the agent can call means anything that can
//! write a file into `plugins/` gets code execution — which is the prompt
//! injection to persistent execution path in AGENTS.md section 10, arrived at
//! from the other side.
//!
//! So discovery grants nothing. A manifest's tools are only built if its
//! plugin id appears in `plugin_layer.trusted_host_plugins`, which is empty by
//! default. Enabling the feature at all is not enough; each plugin is named.
//!
//! Within that gate the execution is still narrow:
//! - the command is executed directly, **never** through a shell, so there is
//!   no quoting or metacharacter surface;
//! - JSON arguments arrive on **stdin**, not argv, so a crafted argument
//!   cannot turn into another flag or another command;
//! - the working directory is the plugin's own directory;
//! - the run is bounded by a timeout and the captured output is truncated.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use crate::plugin_hosts::{HostPluginEntry, HostPluginTool};
use crate::tools::{Tool, ToolResult, ToolSpec};

/// How long a host plugin tool may run before it is killed.
const EXEC_TIMEOUT: Duration = Duration::from_secs(60);
/// How much of its output is kept.
const MAX_OUTPUT: usize = 64 * 1024;

/// A manifest-declared command, exposed to the agent as a tool.
pub struct HostPluginToolAdapter {
    name: String,
    description: String,
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
}

impl HostPluginToolAdapter {
    /// Build the adapters for a plugin, but only if it is trusted.
    ///
    /// `trusted` holds plugin ids exactly as `HostPluginEntry::id` reports them
    /// (`hermes:foo`, `openclaw:bar`). An untrusted plugin yields nothing —
    /// no error, because a discovered-but-untrusted plugin is the normal case,
    /// not a fault.
    pub fn build(entry: &HostPluginEntry, trusted: &[String]) -> Vec<Self> {
        if entry.tools.is_empty() {
            return Vec::new();
        }
        if !trusted.iter().any(|t| t == &entry.id) {
            tracing::info!(
                "[plugin-host] {} declares {} tool(s) but is not in \
                 plugin_layer.trusted_host_plugins — not loading them",
                entry.id,
                entry.tools.len()
            );
            return Vec::new();
        }
        let cwd = entry
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        entry
            .tools
            .iter()
            .map(|t| Self::from_declaration(t, &cwd))
            .collect()
    }

    fn from_declaration(tool: &HostPluginTool, cwd: &std::path::Path) -> Self {
        Self {
            name: tool.name.clone(),
            description: tool.description.clone(),
            command: tool.command.clone(),
            args: tool.args.clone(),
            cwd: cwd.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for HostPluginToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            // The manifest does not describe a schema, so the contract is
            // "whatever JSON you send arrives on stdin".
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.args)
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!("host plugin tool '{}' failed to start: {e}", self.name)
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(arguments.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        let output = match tokio::time::timeout(EXEC_TIMEOUT, child.wait_with_output()).await {
            Ok(out) => out?,
            Err(_) => {
                return Ok(ToolResult::error(format!(
                    "host plugin tool '{}' exceeded {}s and was abandoned",
                    self.name,
                    EXEC_TIMEOUT.as_secs()
                )))
            }
        };

        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        if text.is_empty() {
            text = String::from_utf8_lossy(&output.stderr).to_string();
        }
        // Truncate by character so a multi-byte boundary cannot panic.
        if text.chars().count() > MAX_OUTPUT {
            text = text.chars().take(MAX_OUTPUT).collect::<String>() + "\n… output truncated";
        }

        if output.status.success() {
            Ok(ToolResult::success(text))
        } else {
            Ok(ToolResult::error(format!(
                "host plugin tool '{}' exited with {}: {text}",
                self.name, output.status
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_hosts::{HostPluginKind, HostPluginTool};

    fn entry_with_tool(id: &str, command: &str, args: &[&str]) -> HostPluginEntry {
        HostPluginEntry {
            id: id.to_string(),
            kind: HostPluginKind::Hermes,
            path: std::env::temp_dir().join("plugin.json"),
            name: Some("t".into()),
            description: None,
            tools: vec![HostPluginTool {
                name: "echoer".into(),
                description: "echoes".into(),
                command: command.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
            }],
        }
    }

    #[test]
    fn discovery_alone_grants_nothing() {
        let entry = entry_with_tool("hermes:evil", "/bin/sh", &["-c", "echo pwned"]);
        // The trust list is empty — the normal case for anything merely found.
        assert!(HostPluginToolAdapter::build(&entry, &[]).is_empty());
        // Trusting a *different* plugin must not help.
        assert!(HostPluginToolAdapter::build(&entry, &["hermes:other".into()]).is_empty());
    }

    #[test]
    fn a_named_plugin_is_built() {
        let entry = entry_with_tool("hermes:good", "/bin/echo", &[]);
        let tools = HostPluginToolAdapter::build(&entry, &["hermes:good".into()]);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "echoer");
    }

    #[tokio::test]
    async fn arguments_go_to_stdin_not_argv() {
        // `cat` proves the payload arrived on stdin: if arguments were passed
        // as argv this would print nothing.
        let entry = entry_with_tool("hermes:cat", "cat", &[]);
        let tools = HostPluginToolAdapter::build(&entry, &["hermes:cat".into()]);
        let result = tools[0].execute(r#"{"k":"v"}"#).await.unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("\"k\":\"v\""), "{}", result.output);
    }

    #[tokio::test]
    async fn a_hostile_argument_is_not_interpreted() {
        // Executed directly, so shell metacharacters are inert data. If this
        // ever went through a shell, the `;` would run a second command.
        let entry = entry_with_tool("hermes:cat", "cat", &[]);
        let tools = HostPluginToolAdapter::build(&entry, &["hermes:cat".into()]);
        let result = tools[0]
            .execute("; touch /tmp/apollo-should-not-exist; echo")
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            !std::path::Path::new("/tmp/apollo-should-not-exist").exists(),
            "argument was interpreted by a shell"
        );
    }

    #[tokio::test]
    async fn a_failing_command_reports_rather_than_panics() {
        let entry = entry_with_tool("hermes:missing", "/nonexistent/apollo-binary", &[]);
        let tools = HostPluginToolAdapter::build(&entry, &["hermes:missing".into()]);
        let err = tools[0].execute("{}").await.unwrap_err().to_string();
        assert!(err.contains("failed to start"), "{err}");
    }
}
