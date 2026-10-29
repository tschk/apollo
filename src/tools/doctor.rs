//! Doctor tool — system diagnostics, dependency checks, health report.

use async_trait::async_trait;
use serde::Deserialize;

use crate::diagnostics::{collect_doctor_report, render_doctor_report};

use super::traits::*;

pub struct DoctorTool;

impl DoctorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for DoctorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct DoctorArgs {
    #[serde(default)]
    verbose: bool,
}

#[async_trait]
impl Tool for DoctorTool {
    fn name(&self) -> &str {
        "doctor"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "doctor".to_string(),
            description: "Run system diagnostics — check deps, env vars, disk space, memory, network, and bot health. Like 'zeroclaw doctor'.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "verbose": {
                        "type": "boolean",
                        "description": "Show detailed output (default false)"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: DoctorArgs =
            serde_json::from_str(arguments).unwrap_or(DoctorArgs { verbose: false });
        let report = collect_doctor_report(None, Some("apollo.json"), args.verbose).await;
        Ok(ToolResult::success(render_doctor_report(&report)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_doctor_execute_parses_verbose_flag() {
        let tool = DoctorTool::new();

        // We test executing without verbose
        let result = tool.execute(r#"{"verbose": false}"#).await.unwrap();
        assert!(!result.is_error);
        // Just asserting on some expected text output
        assert!(result.output.contains("apollo doctor"));

        // We test executing with verbose
        let result_verbose = tool.execute(r#"{"verbose": true}"#).await.unwrap();
        assert!(!result_verbose.is_error);
        assert!(result_verbose.output.contains("apollo doctor"));
    }

    #[tokio::test]
    async fn test_doctor_execute_handles_invalid_json() {
        let tool = DoctorTool::new();

        // Invalid JSON or missing field falls back to default (verbose = false)
        let result = tool.execute("invalid json").await.unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("apollo doctor"));

        let result2 = tool.execute(r#"{}"#).await.unwrap();
        assert!(!result2.is_error);
        assert!(result2.output.contains("apollo doctor"));
    }
}
