//! Shared diagnostics and security audit helpers.

use crate::config::Config;
use serde::Serialize;
use std::path::Path;

pub const DEFAULT_GATEWAY_HTTP_TOOL_DENY: &[&str] = &[
    "exec",
    "create_tool",
    "browser",
    "mcp",
    "vibemania",
    "message",
    "Write",
    "Edit",
];

pub const APPROVAL_REQUIRED_TOOLS: &[&str] = &[
    "exec",
    "create_tool",
    "Write",
    "Edit",
    "browser",
    "vibemania",
];

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
pub struct ToolClassification {
    pub name: String,
    pub risk: Severity,
    pub denied_over_gateway_http_by_default: bool,
    pub approval_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub findings: Vec<Finding>,
    pub checks: Vec<Check>,
}

pub fn classify_tool(name: &str) -> ToolClassification {
    let denied = DEFAULT_GATEWAY_HTTP_TOOL_DENY
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(name));
    let approval = APPROVAL_REQUIRED_TOOLS
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(name));
    let risk = if denied {
        Severity::Critical
    } else if approval {
        Severity::Warn
    } else {
        Severity::Info
    };

    ToolClassification {
        name: name.to_string(),
        risk,
        denied_over_gateway_http_by_default: denied,
        approval_required: approval,
    }
}

pub fn audit_config(cfg: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();
    let bind = cfg.gateway.bind.trim();
    let is_loopback = is_loopback_bind(bind);
    let auth_token = cfg.gateway.auth_token.as_deref().unwrap_or("").trim();
    let has_auth = !auth_token.is_empty();

    if !is_loopback && !has_auth {
        findings.push(Finding {
            code: "gateway_bind_no_auth",
            severity: Severity::Critical,
            title: "Gateway binds beyond loopback without auth",
            detail: format!("gateway.bind=\"{bind}\" but no gateway.auth_token is configured."),
            remediation: Some("Bind to localhost or configure a long random bearer token.".into()),
        });
    } else if is_loopback && !has_auth {
        findings.push(Finding {
            code: "gateway_loopback_no_auth",
            severity: Severity::Warn,
            title: "Gateway auth missing on loopback",
            detail: "The gateway is loopback-only, but any local process can call it without a bearer token."
                .into(),
            remediation: Some("Set gateway.auth_token even for local-only deployments.".into()),
        });
    }

    if has_auth && auth_token.len() < 24 {
        findings.push(Finding {
            code: "gateway_token_short",
            severity: Severity::Warn,
            title: "Gateway token looks short",
            detail: format!(
                "gateway.auth_token is only {} characters; use a long random token.",
                auth_token.len()
            ),
            remediation: Some("Use at least 24 random characters.".into()),
        });
    }

    if cfg.gateway.enable_admin_api {
        findings.push(Finding {
            code: "gateway_admin_api_enabled",
            severity: if is_loopback {
                Severity::Warn
            } else {
                Severity::Critical
            },
            title: "Gateway admin API is enabled",
            detail: "Admin endpoints for memory/tools/swarm/plugins are enabled. This should stay disabled unless you are intentionally exposing a control-plane surface.".into(),
            remediation: Some("Keep gateway.enable_admin_api=false unless you are actively wiring and securing those endpoints.".into()),
        });
    }

    if cfg.gateway.rate_limit_per_minute == 0 {
        findings.push(Finding {
            code: "gateway_rate_limit_disabled",
            severity: Severity::Warn,
            title: "Gateway rate limiting is disabled",
            detail: "gateway.rate_limit_per_minute is 0, so authenticated clients can send unlimited requests.".into(),
            remediation: Some("Set a sane per-minute request limit for hosted deployments.".into()),
        });
    }

    if cfg.gateway.request_timeout_secs > 300 {
        findings.push(Finding {
            code: "gateway_timeout_high",
            severity: Severity::Warn,
            title: "Gateway request timeout is unusually high",
            detail: format!(
                "gateway.request_timeout_secs is set to {} seconds.",
                cfg.gateway.request_timeout_secs
            ),
            remediation: Some(
                "Keep request timeouts bounded to reduce stuck sessions and resource exhaustion."
                    .into(),
            ),
        });
    }

    if !is_loopback && cfg.gateway.allowed_origins.is_empty() {
        findings.push(Finding {
            code: "gateway_origins_unrestricted",
            severity: Severity::Warn,
            title: "Gateway allowed origins are unrestricted",
            detail: "The gateway is not loopback-only and gateway.allowed_origins is empty.".into(),
            remediation: Some("Set explicit allowed origins when exposing the gateway behind a browser-facing frontend.".into()),
        });
    }

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

    if cfg.provider.api_key.is_none()
        && std::env::var("ANTHROPIC_API_KEY").is_err()
        && std::env::var("OPENAI_API_KEY").is_err()
    {
        findings.push(Finding {
            code: "provider_credentials_missing",
            severity: Severity::Warn,
            title: "No provider credentials detected",
            detail: "No API key is configured in the config or common environment variables."
                .into(),
            remediation: Some(
                "Set provider.api_key or export a provider API key before starting chat/gateway."
                    .into(),
            ),
        });
    }

    findings
}

pub async fn collect_doctor_report(cfg: Option<&Config>, verbose: bool) -> DoctorReport {
    let mut checks = Vec::new();
    let cfg = cfg.cloned().unwrap_or_else(Config::default_config);

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
            });
        }
    }

    checks.push(Check {
        name: "Workspace".into(),
        ok: cfg.workspace.exists(),
        detail: cfg.workspace.display().to_string(),
    });

    let provider_key_present = cfg.provider.api_key.is_some()
        || std::env::var("ANTHROPIC_API_KEY").is_ok()
        || std::env::var("OPENAI_API_KEY").is_ok();
    checks.push(Check {
        name: "Provider credentials".into(),
        ok: provider_key_present,
        detail: if provider_key_present {
            "API credentials detected".into()
        } else {
            "No provider credentials detected".into()
        },
    });

    checks.push(Check {
        name: "Gateway bind".into(),
        ok: is_loopback_bind(cfg.gateway.bind.trim()),
        detail: cfg.gateway.bind.clone(),
    });

    checks.push(Check {
        name: "Gateway rate limit".into(),
        ok: cfg.gateway.rate_limit_per_minute > 0,
        detail: format!("{} req/min", cfg.gateway.rate_limit_per_minute),
    });

    checks.push(Check {
        name: "Gateway timeout".into(),
        ok: cfg.gateway.request_timeout_secs > 0,
        detail: format!("{}s", cfg.gateway.request_timeout_secs),
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
        "unthinkclaw doctor".to_string(),
        String::new(),
        "Checks:".to_string(),
    ];
    for check in &report.checks {
        let icon = if check.ok { "OK" } else { "FAIL" };
        out.push(format!("- [{icon}] {}: {}", check.name, check.detail));
    }
    out.push(String::new());
    out.push("Audit:".to_string());
    out.push(render_findings(&report.findings));
    out.join("\n")
}

fn is_loopback_bind(bind: &str) -> bool {
    let host = if let Some(stripped) = bind.strip_prefix('[') {
        stripped.split(']').next().unwrap_or(bind)
    } else {
        bind.rsplit_once(':').map(|(host, _)| host).unwrap_or(bind)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
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
    use crate::config::Config;

    #[test]
    fn audit_flags_non_loopback_gateway_without_auth() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "0.0.0.0:8080".into();
        cfg.gateway.auth_token = None;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_bind_no_auth"));
    }

    #[test]
    fn classify_exec_as_high_risk() {
        let tool = classify_tool("exec");
        assert_eq!(tool.risk, Severity::Critical);
        assert!(tool.denied_over_gateway_http_by_default);
        assert!(tool.approval_required);
    }

    #[test]
    fn audit_flags_loopback_gateway_without_auth() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "127.0.0.1:8080".into();
        cfg.gateway.auth_token = None;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_loopback_no_auth" && f.severity == Severity::Warn));
    }

    #[test]
    fn audit_flags_short_auth_token() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "127.0.0.1:8080".into();
        cfg.gateway.auth_token = Some("short".into());
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_token_short" && f.severity == Severity::Warn));
    }

    #[test]
    fn audit_flags_admin_api_enabled_loopback() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "127.0.0.1:8080".into();
        cfg.gateway.auth_token = Some("a_very_long_auth_token_string_here_for_testing".into());
        cfg.gateway.enable_admin_api = true;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_admin_api_enabled" && f.severity == Severity::Warn));
    }

    #[test]
    fn audit_flags_admin_api_enabled_non_loopback() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "0.0.0.0:8080".into();
        cfg.gateway.auth_token = Some("a_very_long_auth_token_string_here_for_testing".into());
        cfg.gateway.enable_admin_api = true;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_admin_api_enabled" && f.severity == Severity::Critical));
    }

    #[test]
    fn audit_flags_rate_limit_disabled() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "127.0.0.1:8080".into();
        cfg.gateway.auth_token = Some("a_very_long_auth_token_string_here_for_testing".into());
        cfg.gateway.rate_limit_per_minute = 0;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_rate_limit_disabled" && f.severity == Severity::Warn));
    }

    #[test]
    fn audit_flags_timeout_high() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "127.0.0.1:8080".into();
        cfg.gateway.auth_token = Some("a_very_long_auth_token_string_here_for_testing".into());
        cfg.gateway.request_timeout_secs = 301;
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_timeout_high" && f.severity == Severity::Warn));
    }

    #[test]
    fn audit_flags_origins_unrestricted() {
        let mut cfg = Config::default_config();
        cfg.gateway.bind = "0.0.0.0:8080".into();
        cfg.gateway.auth_token = Some("a_very_long_auth_token_string_here_for_testing".into());
        cfg.gateway.allowed_origins = vec![];
        let findings = audit_config(&cfg);
        assert!(findings.iter().any(|f| f.code == "gateway_origins_unrestricted" && f.severity == Severity::Warn));
    }
}
