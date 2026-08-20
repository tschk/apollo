//! Dynamic tool system — AI can create, list, and execute custom tools at runtime.
//!
//! Tools are stored in ~/.apollo/tools/<name>/
//!   spec.json  — tool definition (name, description, parameters)
//!   run.v      — V language implementation (preferred, fast compile)
//!   run.py     — Python fallback
//!   run.sh     — Shell fallback
//!
//! The AI uses `create_tool` to write new tools, which are immediately available.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::traits::*;
use crate::config::PolicyConfig;
use crate::policy::ExecutionPolicy;
use crate::text::truncate_chars_counted;

/// Directory where dynamic tools live
fn tools_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".apollo/tools")
}

/// A dynamic tool loaded from disk
pub struct DynamicTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub tool_dir: PathBuf,
    pub language: String, // "v", "python", "shell"
    policy: Arc<ExecutionPolicy>,
}

#[derive(Serialize, Deserialize)]
struct DynamicToolSpec {
    name: String,
    description: String,
    parameters: serde_json::Value,
    language: Option<String>,
}

impl DynamicTool {
    /// Load a dynamic tool from its directory
    pub fn load(dir: &Path, policy: Arc<ExecutionPolicy>) -> Option<Self> {
        let spec_path = dir.join("spec.json");
        let spec_str = std::fs::read_to_string(&spec_path).ok()?;
        let spec: DynamicToolSpec = serde_json::from_str(&spec_str).ok()?;

        // Determine language from what exists
        let language = if dir.join("run.v").exists() {
            "v".to_string()
        } else if dir.join("run.py").exists() {
            "python".to_string()
        } else if dir.join("run.sh").exists() {
            "shell".to_string()
        } else {
            return None;
        };

        Some(Self {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters,
            tool_dir: dir.to_path_buf(),
            language,
            policy,
        })
    }

    /// Load all dynamic tools from the tools directory
    pub fn load_all(policy: Arc<ExecutionPolicy>) -> Vec<Self> {
        let dir = tools_dir();
        if !dir.exists() {
            return Vec::new();
        }

        let mut tools = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(tool) = Self::load(&entry.path(), Arc::clone(&policy)) {
                        tools.push(tool);
                    }
                }
            }
        }
        tools
    }
}

#[async_trait]
impl Tool for DynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_dynamic_tools {
            return ExecutionPolicy::deny("Dynamic tool execution is disabled by policy");
        }

        let run_file = match self.language.as_str() {
            "v" => "run.v",
            "python" => "run.py",
            "shell" => "run.sh",
            _ => return Ok(ToolResult::error("Unknown tool language")),
        };

        let run_path = self.tool_dir.join(run_file);

        // Build command based on language
        let output = match self.language.as_str() {
            "v" => {
                // V: compile and run (fast — ~0.3s compile)
                tokio::process::Command::new("v")
                    .arg("run")
                    .arg(&run_path)
                    .arg(arguments)
                    .current_dir(&self.tool_dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .await?
            }
            "python" => {
                tokio::process::Command::new("python3")
                    .arg(&run_path)
                    .arg(arguments)
                    .current_dir(&self.tool_dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .await?
            }
            "shell" => {
                tokio::process::Command::new("bash")
                    .arg("--")
                    .arg(&run_path)
                    .arg(arguments)
                    .current_dir(&self.tool_dir)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .output()
                    .await?
            }
            _ => return Ok(ToolResult::error("Unknown language")),
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let result = if stdout.is_empty() && !stderr.is_empty() {
            stderr
        } else if !stderr.is_empty() {
            format!("{}\n{}", stdout, stderr)
        } else {
            stdout
        };

        // Truncate
        let truncated = match truncate_chars_counted(&result, 20_000) {
            Some((head, dropped)) => format!("{}...\n[truncated {} chars]", head, dropped),
            None => result,
        };

        Ok(if output.status.success() {
            ToolResult::success(truncated)
        } else {
            ToolResult::error(format!(
                "Exit {}: {}",
                output.status.code().unwrap_or(-1),
                truncated
            ))
        })
    }
}

// ============================================================
// create_tool — meta-tool for the AI to create new tools
// ============================================================

pub struct CreateToolTool {
    policy: Arc<ExecutionPolicy>,
}

impl CreateToolTool {
    pub fn new(policy: Arc<ExecutionPolicy>) -> Self {
        Self { policy }
    }
}

#[derive(Deserialize)]
struct CreateToolArgs {
    /// Tool name (lowercase, no spaces)
    name: String,
    /// Tool description
    description: String,
    /// JSON Schema for parameters
    parameters: serde_json::Value,
    /// Source code for the tool
    code: String,
    /// Language: "v" (default), "python", "shell"
    language: Option<String>,
}

#[async_trait]
impl Tool for CreateToolTool {
    fn name(&self) -> &str {
        "create_tool"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_tool".to_string(),
            description: "Create a new custom tool. The tool becomes immediately available. Write the implementation in V (preferred), Python, or shell. The code receives arguments as a JSON string via argv[1] (V/Python) or $1 (shell).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tool name (lowercase, alphanumeric + underscores)"
                    },
                    "description": {
                        "type": "string",
                        "description": "What the tool does"
                    },
                    "parameters": {
                        "type": "object",
                        "description": "JSON Schema for the tool's input parameters"
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code for the tool implementation"
                    },
                    "language": {
                        "type": "string",
                        "enum": ["v", "python", "shell"],
                        "description": "Implementation language (default: v)"
                    }
                },
                "required": ["name", "description", "parameters", "code"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_dynamic_tools {
            return ExecutionPolicy::deny("Dynamic tool creation is disabled by policy");
        }

        let args: CreateToolArgs = serde_json::from_str(arguments)?;

        // Validate name
        if !args.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Ok(ToolResult::error(
                "Tool name must be alphanumeric + underscores only",
            ));
        }

        let language = args.language.unwrap_or_else(|| "v".to_string());
        let tool_dir = tools_dir().join(&args.name);

        // Create directory
        std::fs::create_dir_all(&tool_dir)?;

        // Write spec.json
        let spec = DynamicToolSpec {
            name: args.name.clone(),
            description: args.description.clone(),
            parameters: args.parameters,
            language: Some(language.clone()),
        };
        std::fs::write(
            tool_dir.join("spec.json"),
            serde_json::to_string_pretty(&spec)?,
        )?;

        // Write implementation
        let filename = match language.as_str() {
            "v" => "run.v",
            "python" => "run.py",
            "shell" => "run.sh",
            _ => {
                return Ok(ToolResult::error(
                    "Unsupported language. Use: v, python, shell",
                ))
            }
        };

        std::fs::write(tool_dir.join(filename), &args.code)?;

        // Make shell scripts executable
        if language == "shell" {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(tool_dir.join(filename), perms)?;
            }
        }

        // Verify the tool can be loaded
        match DynamicTool::load(&tool_dir, Arc::clone(&self.policy)) {
            Some(t) => Ok(ToolResult::success(format!(
                "✅ Tool '{}' created successfully!\n\
                Language: {}\n\
                Location: {}\n\
                Parameters: {}\n\n\
                Note: The tool is saved but requires a bot restart to be available in the current session. \
                It will be auto-loaded on next startup.",
                t.name, language, tool_dir.display(), serde_json::to_string_pretty(&t.parameters)?
            ))),
            None => Ok(ToolResult::error(format!(
                "Tool created but failed to load. Check {} in {}",
                filename,
                tool_dir.display()
            ))),
        }
    }
}

// ============================================================
// list_custom_tools — see what tools have been created
// ============================================================

pub struct ListCustomToolsTool;

impl ListCustomToolsTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ListCustomToolsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ListCustomToolsTool {
    fn name(&self) -> &str {
        "list_custom_tools"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_custom_tools".to_string(),
            description: "List all custom tools created by the AI.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        }
    }

    async fn execute(&self, _arguments: &str) -> anyhow::Result<ToolResult> {
        let tools = DynamicTool::load_all(Arc::new(ExecutionPolicy::from_config(
            &PolicyConfig::default(),
        )));
        if tools.is_empty() {
            return Ok(ToolResult::success(
                "No custom tools created yet.\n\
                Use create_tool to make one!\n\n\
                Example: create a V tool that fetches weather, a Python data processor, etc.",
            ));
        }

        let mut output = format!("Custom tools ({}):\n\n", tools.len());
        for t in &tools {
            output.push_str(&format!(
                "• {} ({}) — {}\n  Location: {}\n",
                t.name,
                t.language,
                t.description,
                t.tool_dir.display()
            ));
        }
        Ok(ToolResult::success(output))
    }
}
