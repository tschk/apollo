//! Shared diagnostics and security audit helpers.

use crate::config::Config;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub title: &'static str,
    pub detail: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub soft_warn: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub findings: Vec<Finding>,
    pub checks: Vec<Check>,
}

pub fn audit_config(cfg: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();

    if cfg.policy.allow_shell {
        findings.push(Finding {
            code: "policy_shell_enabled",
            severity: Severity::Info,
            title: "Shell execution is enabled",
            detail: "The exec tool can run host commands when invoked by the agent.".into(),
            remediation: Some(
                "Leave enabled only if this deployment is intended to be a full-computer agent."
                    .into(),
            ),
        });
    }

    if cfg.policy.allow_dynamic_tools {
        findings.push(Finding {
            code: "policy_dynamic_tools_enabled",
            severity: Severity::Warn,
            title: "Dynamic tool creation/execution is enabled",
            detail: "The agent can create and execute custom tools at runtime.".into(),
            remediation: Some("Disable policy.allow_dynamic_tools for deployments that do not need self-extending tools.".into()),
        });
    }

    if cfg.policy.allow_plugin_shell {
        findings.push(Finding {
            code: "policy_plugin_shell_enabled",
            severity: Severity::Warn,
            title: "Plugin shell execution is enabled",
            detail: "Plugins are allowed to spawn shell commands.".into(),
            remediation: Some(
                "Disable policy.allow_plugin_shell unless plugins are trusted.".into(),
            ),
        });
    }

    if cfg.policy.allow_plugin_git {
        findings.push(Finding {
            code: "policy_plugin_git_enabled",
            severity: Severity::Warn,
            title: "Plugin git execution is enabled",
            detail: "Plugins are allowed to run git operations directly.".into(),
            remediation: Some("Disable policy.allow_plugin_git unless plugins are trusted.".into()),
        });
    }

    if !Path::new(&cfg.workspace).exists() {
        findings.push(Finding {
            code: "workspace_missing",
            severity: Severity::Warn,
            title: "Configured workspace does not exist",
            detail: format!(
                "workspace=\"{}\" does not exist on disk.",
                cfg.workspace.display()
            ),
            remediation: Some(
                "Set workspace to an existing directory before starting the agent.".into(),
            ),
        });
    }

    if cfg.provider.api_key.is_none() && std::env::var("OPENAI_API_KEY").is_err() {
        findings.push(Finding {
            code: "provider_credentials_missing",
            severity: Severity::Warn,
            title: "No provider credentials detected",
            detail: "No API key is configured in the config or common environment variables."
                .into(),
            remediation: Some(
                "Set provider.api_key or export a provider API key before starting chat.".into(),
            ),
        });
    }

    findings
}

pub async fn collect_doctor_report(
    cfg: Option<&Config>,
    config_path: Option<&str>,
    verbose: bool,
) -> DoctorReport {
    let mut checks = Vec::new();
    if let Some(path) = config_path {
        checks.push(check_config_file(path));
    }
    let cfg = cfg.cloned().unwrap_or_else(|| {
        config_path
            .and_then(|path| Config::load(path).ok())
            .unwrap_or_else(Config::default_config)
    });

    for (bin, label) in [
        ("git", "Git"),
        ("cargo", "Rust toolchain"),
        ("ffmpeg", "FFmpeg"),
        ("docker", "Docker"),
        ("node", "Node.js"),
    ] {
        let found = check_cmd(bin).await;
        if found || verbose {
            checks.push(Check {
                name: label.to_string(),
                ok: found,
                detail: if found {
                    format!("{bin} is available")
                } else {
                    format!("{bin} is not on PATH")
                },
                soft_warn: false,
            });
        }
    }

    let workspace_exists = cfg.workspace.exists();
    checks.push(Check {
        name: "Workspace".into(),
        ok: workspace_exists,
        detail: cfg.workspace.display().to_string(),
        soft_warn: false,
    });

    let workspace_writable = workspace_exists && is_workspace_writable(&cfg.workspace);
    checks.push(Check {
        name: "Workspace writable".into(),
        ok: workspace_writable,
        detail: if !workspace_exists {
            "workspace does not exist".into()
        } else if workspace_writable {
            "workspace is writable".into()
        } else {
            "workspace is not writable".into()
        },
        soft_warn: false,
    });

    checks.push(local_bin_path_check());
    checks.push(shared_login_check());

    let provider_key_present =
        cfg.provider.api_key.is_some() || std::env::var("OPENAI_API_KEY").is_ok();
    checks.push(Check {
        name: "Provider credentials".into(),
        ok: provider_key_present,
        detail: if provider_key_present {
            "API credentials detected".into()
        } else {
            "No provider credentials detected".into()
        },
        soft_warn: false,
    });

    DoctorReport {
        findings: audit_config(&cfg),
        checks,
    }
}

pub fn render_findings(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "No audit findings.".into();
    }

    findings
        .iter()
        .map(|finding| {
            let severity = match finding.severity {
                Severity::Info => "INFO",
                Severity::Warn => "WARN",
                Severity::Critical => "CRITICAL",
            };
            match &finding.remediation {
                Some(remediation) => format!(
                    "[{severity}] {} ({})\n{}\nRemediation: {}",
                    finding.title, finding.code, finding.detail, remediation
                ),
                None => format!(
                    "[{severity}] {} ({})\n{}",
                    finding.title, finding.code, finding.detail
                ),
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn render_doctor_report(report: &DoctorReport) -> String {
    let mut out = vec![
        "apollo doctor".to_string(),
        String::new(),
        "Checks:".to_string(),
    ];
    for check in &report.checks {
        let icon = if check.ok {
            "OK"
        } else if check.soft_warn {
            "WARN"
        } else {
            "FAIL"
        };
        out.push(format!("- [{icon}] {}: {}", check.name, check.detail));
    }
    out.push(String::new());
    out.push("Audit:".to_string());
    out.push(render_findings(&report.findings));
    out.join("\n")
}

pub(crate) fn check_config_file(path: &str) -> Check {
    let file = Path::new(path);
    if !file.exists() {
        return Check {
            name: "Config file".into(),
            ok: false,
            detail: format!("{path} not found"),
            soft_warn: false,
        };
    }
    match Config::load(path) {
        Ok(_) => Check {
            name: "Config file".into(),
            ok: true,
            detail: format!("{path} parses OK"),
            soft_warn: false,
        },
        Err(err) => Check {
            name: "Config file".into(),
            ok: false,
            detail: format!("{path} invalid: {err}"),
            soft_warn: false,
        },
    }
}

fn is_workspace_writable(path: &Path) -> bool {
    let probe = path.join(format!(".apollo-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

/// Which providers have a usable credential in the store shared with
/// telekinesis, so "am I logged in?" is answerable without guessing.
///
/// Reports provider names and expiry only — never a token.
#[cfg(feature = "rs-ai")]
fn shared_login_check() -> Check {
    let logins = crate::providers::shared_credentials::logins();
    if logins.is_empty() {
        return Check {
            name: "Shared login (rs_ai)".into(),
            ok: false,
            detail: "no provider logged in to the shared credential store".into(),
            soft_warn: true,
        };
    }

    let detail = logins
        .iter()
        .map(|login| match (login.expired, login.refreshable) {
            (false, _) => login.provider.to_string(),
            (true, true) => format!("{} (expired, refreshable)", login.provider),
            (true, false) => format!("{} (expired)", login.provider),
        })
        .collect::<Vec<_>>()
        .join(", ");

    // Expired-but-refreshable is a working login: the provider refreshes it on
    // first use. Only a dead, unrefreshable token means the user must log in
    // again.
    let usable = logins.iter().any(|l| !l.expired || l.refreshable);
    Check {
        name: "Shared login (rs_ai)".into(),
        ok: usable,
        detail,
        soft_warn: !usable,
    }
}

#[cfg(not(feature = "rs-ai"))]
fn shared_login_check() -> Check {
    Check {
        name: "Shared login (rs_ai)".into(),
        ok: false,
        detail: "built without the rs-ai feature".into(),
        soft_warn: true,
    }
}

fn local_bin_path_check() -> Check {
    let on_path = local_bin_on_path();
    Check {
        name: "~/.local/bin on PATH".into(),
        ok: on_path,
        detail: if on_path {
            "~/.local/bin is on PATH".into()
        } else {
            "~/.local/bin is not on PATH (optional)".into()
        },
        soft_warn: true,
    }
}

fn local_bin_on_path() -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let local_bin = Path::new(&home).join(".local/bin");
    let Ok(path_env) = std::env::var("PATH") else {
        return false;
    };
    path_env
        .split(':')
        .any(|entry| Path::new(entry) == local_bin.as_path())
}

async fn check_cmd(cmd: &str) -> bool {
    tokio::process::Command::new("which")
        .arg(cmd)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_doctor_report_marks_soft_warn_and_fail() {
        let report = DoctorReport {
            findings: vec![],
            checks: vec![
                Check {
                    name: "passing".into(),
                    ok: true,
                    detail: "all good".into(),
                    soft_warn: false,
                },
                Check {
                    name: "optional missing".into(),
                    ok: false,
                    detail: "not on PATH (optional)".into(),
                    soft_warn: true,
                },
                Check {
                    name: "required missing".into(),
                    ok: false,
                    detail: "not found".into(),
                    soft_warn: false,
                },
            ],
        };
        let rendered = render_doctor_report(&report);
        assert!(rendered.contains("- [OK] passing: all good"));
        assert!(rendered.contains("- [WARN] optional missing: not on PATH (optional)"));
        assert!(rendered.contains("- [FAIL] required missing: not found"));
    }

    #[test]
    fn check_config_file_reports_missing_path() {
        let path = "/tmp/apollo-doctor-missing-config-test-xyz123.json";
        let check = check_config_file(path);
        assert!(!check.ok);
        assert!(!check.soft_warn);
        assert_eq!(check.name, "Config file");
        assert!(check.detail.contains("not found"));
    }

    #[test]
    fn check_config_file_reports_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apollo.json");
        std::fs::write(&path, "{}").unwrap();
        let check = check_config_file(path.to_str().unwrap());
        assert!(check.ok);
        assert!(!check.soft_warn);
        assert!(check.detail.contains("parses OK"));
    }

    #[test]
    fn audit_config_detects_missing_api_keys() {
        temp_env::with_var("OPENAI_API_KEY", None::<&str>, || {
            let mut cfg = Config::default_config();
            cfg.provider.api_key = None;

            let findings = audit_config(&cfg);
            assert!(findings
                .iter()
                .any(|f| f.code == "provider_credentials_missing"));
        });
    }

    #[test]
    fn audit_config_ok_with_env_api_key() {
        temp_env::with_var("OPENAI_API_KEY", Some("sk-test"), || {
            let mut cfg = Config::default_config();
            cfg.provider.api_key = None;

            let findings = audit_config(&cfg);
            assert!(!findings
                .iter()
                .any(|f| f.code == "provider_credentials_missing"));
        });
    }

    #[test]
    fn audit_config_ok_with_config_api_key() {
        temp_env::with_var("OPENAI_API_KEY", None::<&str>, || {
            let mut cfg = Config::default_config();
            cfg.provider.api_key = Some("sk-test".into());

            let findings = audit_config(&cfg);
            assert!(!findings
                .iter()
                .any(|f| f.code == "provider_credentials_missing"));
        });
    }
}
