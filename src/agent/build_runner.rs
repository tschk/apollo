//! Build runner — orchestrates cargo build/test/run cycles.
//!
//! Wraps `cargo build` and `cargo test` with structured error parsing, retry,
//! and timeout handling. Uses `tokio::process::Command` with the same
//! environment scrubbing as the shell tool so secrets never reach the child.
//!
//! Designed to be driven by the agent loop (via `BuildRunnerTool`) or by the
//! autonomous loop's validation step, replacing the bare `sh -c` test runner
//! with one that returns compiler diagnostics in a structured form the model
//! can act on directly.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::tools::child_proc;

/// Maximum retries before giving up. A retry only makes sense when the build
/// failed for a transient reason (a stale lock, a race with another cargo
/// invocation); compiler errors are deterministic and retrying them just
/// burns time. The runner still retries unconditionally because the cost of a
/// false retry is one wasted build, while the cost of not retrying a transient
/// failure is a spurious red.
const DEFAULT_MAX_RETRIES: usize = 2;

/// Upper bound on a single cargo invocation. A release build of a large
/// workspace can take minutes; an hour is well past any legitimate single
/// compile and signals a stuck process.
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// Configuration for the build runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRunnerConfig {
    /// Maximum number of retries on a failed build/test.
    pub max_retries: usize,
    /// Per-invocation timeout in seconds.
    pub timeout_secs: u64,
    /// Extra arguments appended to every `cargo` invocation
    /// (e.g. `["--release"]`).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Package filter passed as `cargo <cmd> -p <name>`. Empty = whole workspace.
    #[serde(default)]
    pub package: String,
}

impl Default for BuildRunnerConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            extra_args: Vec::new(),
            package: String::new(),
        }
    }
}

/// Severity of a parsed compiler diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Note,
}

/// A single parsed compiler diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileError {
    pub severity: DiagnosticSeverity,
    /// Rust error code when present (e.g. `E0308`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The headline message (first line of the diagnostic).
    pub message: String,
    /// Source file, when the diagnostic carries a `--> path:line:col` span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Result of a single build or test invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub success: bool,
    pub exit_code: i32,
    /// Which attempt this is (1-based); >1 means a retry fired.
    pub attempt: usize,
    pub errors: Vec<CompileError>,
    pub warnings: Vec<CompileError>,
    /// Raw combined stdout+stderr, truncated for size.
    pub raw_output: String,
    /// `cargo build`, `cargo test`, etc.
    pub command: String,
}

impl BuildResult {
    /// A compact, model-friendly summary of the diagnostics.
    pub fn summary(&self) -> String {
        if self.success {
            return format!("{} succeeded (attempt {})", self.command, self.attempt);
        }
        let mut out = format!(
            "{} failed (exit {}, attempt {})\n",
            self.command, self.exit_code, self.attempt
        );
        if !self.errors.is_empty() {
            out.push_str(&format!("--- {} error(s) ---\n", self.errors.len()));
            for e in &self.errors {
                out.push_str(&format_error(e));
                out.push('\n');
            }
        }
        if !self.warnings.is_empty() {
            out.push_str(&format!("--- {} warning(s) ---\n", self.warnings.len()));
            for w in &self.warnings {
                out.push_str(&format_error(w));
                out.push('\n');
            }
        }
        out
    }
}

/// The build/run orchestrator.
pub struct BuildRunner {
    workspace: PathBuf,
    config: BuildRunnerConfig,
}

impl BuildRunner {
    pub fn new(workspace: PathBuf, config: BuildRunnerConfig) -> Self {
        Self { workspace, config }
    }

    pub fn with_workspace(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }

    /// Run `cargo build` with retries. Returns the last attempt's result.
    pub async fn run_build(&self) -> BuildResult {
        self.run_with_retry("build").await
    }

    /// Run `cargo test` with retries. Returns the last attempt's result.
    pub async fn run_test(&self) -> BuildResult {
        self.run_with_retry("test").await
    }

    /// Run `cargo build` then `cargo test`. Stops after the build if it
    /// fails — testing a broken compile is wasted time.
    pub async fn run_cycle(&self) -> (BuildResult, Option<BuildResult>) {
        let build = self.run_build().await;
        if !build.success {
            return (build, None);
        }
        let test = self.run_test().await;
        (build, Some(test))
    }

    /// Run a cargo subcommand, retrying up to `max_retries` times on failure.
    async fn run_with_retry(&self, subcommand: &str) -> BuildResult {
        let max_attempts = self.config.max_retries.saturating_add(1);
        let mut last = BuildResult {
            success: false,
            exit_code: -1,
            attempt: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
            raw_output: String::new(),
            command: format!("cargo {}", subcommand),
        };

        for attempt in 1..=max_attempts {
            let result = self.run_once(subcommand, attempt).await;
            if result.success {
                return result;
            }
            tracing::warn!(
                "[build_runner] {} attempt {} failed (exit {})",
                subcommand,
                attempt,
                result.exit_code
            );
            last = result;
        }
        last
    }

    /// A single cargo invocation with timeout and output parsing.
    async fn run_once(&self, subcommand: &str, attempt: usize) -> BuildResult {
        let mut cmd = tokio::process::Command::new("cargo");
        cmd.arg(subcommand)
            .current_dir(&self.workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if !self.config.package.is_empty() {
            cmd.arg("-p").arg(&self.config.package);
        }
        for arg in &self.config.extra_args {
            cmd.arg(arg);
        }
        child_proc::scrub(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return BuildResult {
                    success: false,
                    exit_code: -1,
                    attempt,
                    errors: vec![CompileError {
                        severity: DiagnosticSeverity::Error,
                        code: None,
                        message: format!("failed to spawn cargo: {e}"),
                        file: None,
                        line: None,
                        column: None,
                    }],
                    warnings: Vec::new(),
                    raw_output: format!("failed to spawn cargo: {e}"),
                    command: format!("cargo {}", subcommand),
                };
            }
        };

        let timeout = Duration::from_secs(self.config.timeout_secs);
        let output = match child_proc::wait_with_timeout(&mut child, timeout).await {
            Ok(Some(o)) => o,
            Ok(None) => {
                return BuildResult {
                    success: false,
                    exit_code: -1,
                    attempt,
                    errors: vec![CompileError {
                        severity: DiagnosticSeverity::Error,
                        code: None,
                        message: format!(
                            "cargo {} timed out after {}s",
                            subcommand,
                            timeout.as_secs()
                        ),
                        file: None,
                        line: None,
                        column: None,
                    }],
                    warnings: Vec::new(),
                    raw_output: format!("timed out after {}s", timeout.as_secs()),
                    command: format!("cargo {}", subcommand),
                };
            }
            Err(e) => {
                return BuildResult {
                    success: false,
                    exit_code: -1,
                    attempt,
                    errors: vec![CompileError {
                        severity: DiagnosticSeverity::Error,
                        code: None,
                        message: format!("cargo {} io error: {e}", subcommand),
                        file: None,
                        line: None,
                        column: None,
                    }],
                    warnings: Vec::new(),
                    raw_output: format!("io error: {e}"),
                    command: format!("cargo {}", subcommand),
                };
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = if stdout.is_empty() && !stderr.is_empty() {
            stderr.to_string()
        } else if !stderr.is_empty() {
            format!("{}\n{}", stdout, stderr)
        } else {
            stdout.to_string()
        };

        let (errors, warnings) = parse_diagnostics(&combined);
        let exit_code = output.status.code().unwrap_or(-1);
        let success = output.status.success();

        let raw = match crate::text::truncate_chars_counted(&combined, 20_000) {
            Some((head, dropped)) => format!("{}...\n[truncated {} chars]", head, dropped),
            None => combined,
        };

        BuildResult {
            success,
            exit_code,
            attempt,
            errors,
            warnings,
            raw_output: raw,
            command: format!("cargo {}", subcommand),
        }
    }
}

/// Format a single diagnostic for the summary.
fn format_error(e: &CompileError) -> String {
    let sev = match e.severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Note => "note",
    };
    let code = e
        .code
        .as_ref()
        .map(|c| format!("[{}]", c))
        .unwrap_or_default();
    let loc = match (&e.file, e.line, e.column) {
        (Some(f), Some(l), Some(c)) => format!(" {}:{}:{}", f, l, c),
        (Some(f), Some(l), None) => format!(" {}:{}", f, l),
        _ => String::new(),
    };
    format!("{}{}: {}{}", sev, code, e.message, loc)
}

/// Parse cargo/rustc diagnostic output into structured errors and warnings.
///
/// Recognises the standard rustc format:
///
/// ```text
/// error[E0308]: mismatched types
///   --> src/main.rs:10:5
/// ```
///
/// and the `warning:` / `note:` variants. Lines without a `-->` span still
/// produce a diagnostic (with `file`/`line`/`column` set to `None`) so the
/// caller sees the headline message.
pub fn parse_diagnostics(output: &str) -> (Vec<CompileError>, Vec<CompileError>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        if let Some((severity, rest)) = parse_diagnostic_header(line) {
            let (code, message) = parse_code_and_message(rest);
            // Look ahead for a `--> path:line:col` span.
            let (file, line_no, col) = look_ahead_for_span(&lines, i + 1);
            let diag = CompileError {
                severity: severity.clone(),
                code,
                message,
                file,
                line: line_no,
                column: col,
            };
            match severity {
                DiagnosticSeverity::Error => errors.push(diag),
                DiagnosticSeverity::Warning => warnings.push(diag),
                DiagnosticSeverity::Note => {}
            }
        }
        i += 1;
    }

    (errors, warnings)
}

/// Detect `error[...]:`, `warning:`, or `note:` headers.
///
/// Returns the severity and the remainder *after* the severity keyword,
/// including any `[CODE]` prefix, so `parse_code_and_message` can extract it.
fn parse_diagnostic_header(line: &str) -> Option<(DiagnosticSeverity, &str)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("error") {
        // `error:` or `error[E0308]: ...` — hand the rest (including the
        // code bracket) to parse_code_and_message.
        return Some((DiagnosticSeverity::Error, rest));
    }
    if let Some(rest) = trimmed.strip_prefix("warning") {
        return Some((DiagnosticSeverity::Warning, rest));
    }
    None
}

/// Split `[E0308]: mismatched types` into `(Some("E0308"), "mismatched types")`
/// when a code is present, otherwise `(None, message)` after stripping the
/// leading `: ` separator.
fn parse_code_and_message(rest: &str) -> (Option<String>, String) {
    if rest.starts_with('[') {
        if let Some(idx) = rest.find(']') {
            let code = rest[1..idx].to_string();
            let after = &rest[idx + 1..];
            let msg = after.strip_prefix(':').unwrap_or(after).trim_start();
            return (Some(code), msg.to_string());
        }
    }
    // `: could not compile` → strip the leading colon.
    let msg = rest.strip_prefix(':').unwrap_or(rest).trim_start();
    (None, msg.to_string())
}

/// Scan forward from `start` for a `--> path:line:col` span line.
fn look_ahead_for_span(lines: &[&str], start: usize) -> (Option<String>, Option<u32>, Option<u32>) {
    for line in &lines[start..] {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("-->") {
            return parse_span(rest.trim());
        }
        // A new diagnostic header before a span means this one had none.
        if trimmed.starts_with("error") || trimmed.starts_with("warning") {
            return (None, None, None);
        }
    }
    (None, None, None)
}

/// Parse `src/main.rs:10:5` into file/line/column.
fn parse_span(span: &str) -> (Option<String>, Option<u32>, Option<u32>) {
    // The span may carry a trailing note like `src/main.rs:10:5:13:20` or
    // `src/main.rs:10:5`. Take the first three colon-separated parts.
    let parts: Vec<&str> = span.splitn(3, ':').collect();
    if parts.is_empty() {
        return (None, None, None);
    }
    let file = Some(parts[0].to_string());
    let line = parts.get(1).and_then(|s| s.parse().ok());
    let column = parts.get(2).and_then(|s| s.parse().ok());
    (file, line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = "\
   Compiling apollo v0.4.0 (/Users/undivisible/projects/apollo)
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     let x: i32 = \"hello\";
   |                 ^^^^^^^ expected `i32`, found `&str`

warning: unused variable: `y`
  --> src/main.rs:5:9
   |
5  |     let y = 1;
   |         ^^^

error[E0425]: cannot find value `z` in this scope
  --> src/main.rs:12:13
   |
12 |     println!(\"{}\", z);
   |                     ^ not found in this scope
";

    #[test]
    fn parses_errors_and_warnings() {
        let (errors, warnings) = parse_diagnostics(SAMPLE_OUTPUT);
        assert_eq!(errors.len(), 2);
        assert_eq!(warnings.len(), 1);

        assert_eq!(errors[0].code.as_deref(), Some("E0308"));
        assert_eq!(errors[0].message, "mismatched types");
        assert_eq!(errors[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(errors[0].line, Some(10));
        assert_eq!(errors[0].column, Some(5));

        assert_eq!(errors[1].code.as_deref(), Some("E0425"));
        assert_eq!(errors[1].file.as_deref(), Some("src/main.rs"));
        assert_eq!(errors[1].line, Some(12));
        assert_eq!(errors[1].column, Some(13));

        assert_eq!(warnings[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(warnings[0].message, "unused variable: `y`");
        assert_eq!(warnings[0].line, Some(5));
    }

    #[test]
    fn handles_error_without_code() {
        let output = "error: could not compile `foo` due to 1 previous error";
        let (errors, _) = parse_diagnostics(output);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].code.is_none());
        assert!(errors[0].file.is_none());
        assert!(errors[0].line.is_none());
    }

    #[test]
    fn handles_error_without_span() {
        let output = "error[E0308]: mismatched types\nnote: run with `RUST_BACKTRACE=1`";
        let (errors, _) = parse_diagnostics(output);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].file.is_none());
        assert!(errors[0].line.is_none());
    }

    #[test]
    fn summary_lists_errors() {
        let (errors, warnings) = parse_diagnostics(SAMPLE_OUTPUT);
        let result = BuildResult {
            success: false,
            exit_code: 101,
            attempt: 1,
            errors,
            warnings,
            raw_output: String::new(),
            command: "cargo build".to_string(),
        };
        let s = result.summary();
        assert!(s.contains("cargo build failed"));
        assert!(s.contains("2 error(s)"));
        assert!(s.contains("1 warning(s)"));
        assert!(s.contains("E0308"));
        assert!(s.contains("src/main.rs:10:5"));
    }

    #[test]
    fn summary_for_success() {
        let result = BuildResult {
            success: true,
            exit_code: 0,
            attempt: 1,
            errors: Vec::new(),
            warnings: Vec::new(),
            raw_output: String::new(),
            command: "cargo test".to_string(),
        };
        assert_eq!(result.summary(), "cargo test succeeded (attempt 1)");
    }

    #[test]
    fn config_defaults_are_sane() {
        let c = BuildRunnerConfig::default();
        assert_eq!(c.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(c.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(c.extra_args.is_empty());
        assert!(c.package.is_empty());
    }

    #[tokio::test]
    async fn run_build_on_nonexistent_workspace_reports_spawn_error() {
        let runner = BuildRunner::new(
            PathBuf::from("/nonexistent/path/that/does/not/exist"),
            BuildRunnerConfig {
                max_retries: 0,
                ..BuildRunnerConfig::default()
            },
        );
        let result = runner.run_build().await;
        assert!(!result.success);
        assert_eq!(result.attempt, 1);
        assert!(!result.errors.is_empty());
    }
}
