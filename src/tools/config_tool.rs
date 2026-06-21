//! ConfigTool — read and patch the agent's config file from within a conversation.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::traits::*;

pub struct ConfigTool {
    config_path: PathBuf,
}

impl ConfigTool {
    pub fn new(config_path: PathBuf) -> Self {
        Self { config_path }
    }
}

#[derive(Deserialize)]
struct ConfigArgs {
    /// Action: "read", "get", or "set"
    action: String,
    /// JSON pointer path for get/set (e.g. "/agent/max_rounds")
    #[serde(default)]
    path: String,
    /// New value for "set" (any JSON value)
    value: Option<Value>,
}

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "config"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "config".to_string(),
            description: "Read or update the agent's configuration file. \
                Use 'read' to show the full config, 'get' to retrieve a specific field \
                (JSON pointer, e.g. /agent/max_rounds), and 'set' to update a field."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["read", "get", "set"],
                        "description": "read = full config, get = read field, set = write field"
                    },
                    "path": {
                        "type": "string",
                        "description": "JSON pointer path (e.g. /agent/max_rounds). Required for get/set."
                    },
                    "value": {
                        "description": "New value for set action (any JSON type)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: ConfigArgs = serde_json::from_str(arguments)?;

        let raw = tokio::fs::read_to_string(&self.config_path)
            .await
            .unwrap_or_else(|_| "{}".to_string());

        match args.action.as_str() {
            "read" => Ok(ToolResult::success(raw)),

            "get" => {
                if args.path.is_empty() {
                    return Ok(ToolResult::error("path is required for get"));
                }
                let mut config: Value = serde_json::from_str(&raw)?;
                match config.pointer_mut(&args.path) {
                    Some(val) => Ok(ToolResult::success(serde_json::to_string_pretty(val)?)),
                    None => Ok(ToolResult::error(format!("Path '{}' not found", args.path))),
                }
            }

            "set" => {
                if args.path.is_empty() {
                    return Ok(ToolResult::error("path is required for set"));
                }
                let new_value = match args.value {
                    Some(v) => v,
                    None => return Ok(ToolResult::error("value is required for set")),
                };

                let mut config: Value = serde_json::from_str(&raw)?;

                // Walk the pointer and set the value
                let parts: Vec<&str> = args.path.trim_start_matches('/').split('/').collect();

                if parts.is_empty() || parts[0].is_empty() {
                    return Ok(ToolResult::error("invalid path"));
                }

                let mut current = &mut config;
                for (i, part) in parts.iter().enumerate() {
                    if i == parts.len() - 1 {
                        if let Some(obj) = current.as_object_mut() {
                            obj.insert(part.to_string(), new_value.clone());
                        } else {
                            return Ok(ToolResult::error("parent is not an object"));
                        }
                    } else {
                        current = current
                            .as_object_mut()
                            .and_then(|o| o.get_mut(*part))
                            .ok_or_else(|| anyhow::anyhow!("path segment '{}' not found", part))?;
                    }
                }

                let updated = serde_json::to_string_pretty(&config)?;
                tokio::fs::write(&self.config_path, &updated).await?;
                Ok(ToolResult::success(format!(
                    "Set {} = {}",
                    args.path,
                    serde_json::to_string(&new_value)?
                )))
            }

            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn read_missing_returns_empty_object() {
        let dir = TempDir::new().unwrap();
        let tool = ConfigTool::new(dir.path().join("config.json"));
        let r = tool.execute(r#"{"action":"read"}"#).await.unwrap();
        assert!(!r.is_error);
        assert_eq!(r.output.trim(), "{}");
    }

    #[tokio::test]
    async fn set_and_get() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        tokio::fs::write(&path, r#"{"agent":{"max_rounds":50}}"#)
            .await
            .unwrap();
        let tool = ConfigTool::new(path);

        let s = tool
            .execute(r#"{"action":"set","path":"/agent/max_rounds","value":99}"#)
            .await
            .unwrap();
        assert!(!s.is_error);

        let g = tool
            .execute(r#"{"action":"get","path":"/agent/max_rounds"}"#)
            .await
            .unwrap();
        assert!(g.output.contains("99"));
    }
}
