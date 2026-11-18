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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn execute_reports_diagnostic_outcomes() {
        // Isolate cwd, PATH and credentials without mutating the test runner's globals.
        const CHILD: &str = "APOLLO_DOCTOR_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            for name in ["which", "git"] {
                let found = Command::new("which").arg(name).output().unwrap();
                assert!(found.status.success(), "{name} is required for this test");
                let path = String::from_utf8(found.stdout).unwrap();
                std::os::unix::fs::symlink(path.trim(), bin.join(name)).unwrap();
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tools::doctor::tests::execute_reports_diagnostic_outcomes",
                    "--nocapture",
                ])
                .current_dir(dir.path())
                .env_clear()
                .env(CHILD, "1")
                .env("HOME", dir.path())
                .env("PATH", &bin)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "doctor subprocess failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // A misspelled filter must not turn this into a zero-test success.
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }

        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(check_outcomes());
    }

    async fn check_outcomes() {
        let tool = DoctorTool::new();
        let mut cfg = crate::config::Config::default();
        cfg.provider.api_key = Some("doctor-fixture-credential".into());
        cfg.policy.allow_shell = false;
        cfg.policy.allow_dynamic_tools = false;
        cfg.policy.allow_plugin_shell = false;
        cfg.policy.allow_plugin_git = false;
        std::fs::write("apollo.json", serde_json::to_string(&cfg).unwrap()).unwrap();

        let normal = tool.execute(r#"{"verbose":false}"#).await.unwrap();
        assert!(!normal.is_error);
        assert!(normal.output.starts_with("apollo doctor\n\nChecks:\n"));
        assert!(normal
            .output
            .contains("- [OK] Config file: apollo.json parses OK"));
        assert!(normal.output.contains("- [OK] Git: git is available"));
        assert!(normal.output.contains("- [OK] Workspace: .\n"));
        assert!(normal
            .output
            .contains("- [OK] Workspace writable: workspace is writable"));
        assert!(normal
            .output
            .contains("- [OK] Provider credentials: API credentials detected"));
        assert!(normal
            .output
            .contains("- [WARN] ~/.local/bin on PATH: ~/.local/bin is not on PATH (optional)"));
        assert!(normal.output.ends_with("Audit:\nNo audit findings."));
        assert!(!normal.output.contains("doctor-fixture-credential"));

        for arguments in ["{}", "invalid json", "null", "[]", r#"{"verbose":"true"}"#] {
            let result = tool.execute(arguments).await.unwrap();
            assert!(!result.is_error, "{arguments}");
            assert_eq!(result.output, normal.output, "{arguments}");
        }

        let verbose = tool.execute(r#"{"verbose":true}"#).await.unwrap();
        assert!(!verbose.is_error);
        assert!(verbose.output.contains("- [OK] Git: git is available"));
        for (label, bin) in [
            ("Rust toolchain", "cargo"),
            ("FFmpeg", "ffmpeg"),
            ("Docker", "docker"),
            ("Node.js", "node"),
        ] {
            assert!(!normal.output.contains(&format!("{label}:")));
            assert!(verbose
                .output
                .contains(&format!("- [FAIL] {label}: {bin} is not on PATH")));
        }

        cfg.workspace = "missing-workspace".into();
        cfg.provider.api_key = None;
        cfg.policy.allow_shell = true;
        std::fs::write("apollo.json", serde_json::to_string(&cfg).unwrap()).unwrap();
        let unhealthy = tool.execute("{}").await.unwrap();
        // A completed diagnosis is a successful tool call even when checks fail.
        assert!(!unhealthy.is_error);
        for expected in [
            "- [FAIL] Workspace: missing-workspace",
            "- [FAIL] Workspace writable: workspace does not exist",
            "- [FAIL] Provider credentials: No provider credentials detected",
            "(workspace_missing)",
            "(provider_credentials_missing)",
            "(policy_shell_enabled)",
        ] {
            assert!(unhealthy.output.contains(expected), "{expected}");
        }

        // A regular file exists but cannot serve as a writable workspace,
        // even when the test runs as root and permission bits are ineffective.
        std::fs::write("missing-workspace", "not a directory").unwrap();
        let unwritable = tool.execute("{}").await.unwrap();
        assert!(!unwritable.is_error);
        assert!(unwritable
            .output
            .contains("- [FAIL] Workspace writable: workspace is not writable"));

        std::fs::write("apollo.json", "not json").unwrap();
        let invalid = tool.execute("{}").await.unwrap();
        assert!(!invalid.is_error);
        assert!(invalid
            .output
            .contains("- [FAIL] Config file: apollo.json invalid:"));
        assert!(invalid.output.contains("- [OK] Workspace: .\n"));

        std::fs::remove_file("apollo.json").unwrap();
        let missing = tool.execute("{}").await.unwrap();
        assert!(!missing.is_error);
        assert!(missing
            .output
            .contains("- [FAIL] Config file: apollo.json not found"));
        assert!(missing.output.contains("- [OK] Workspace: .\n"));
        assert!(!std::fs::read_dir(".").unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".apollo-write-test-")));
    }
}
