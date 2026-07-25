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
