//! Fail-closed `confine(argv)` — layered sandbox around exec.
//!
//! Two dialects, never mixed:
//! - `Denial` — the argv must not run
//! - `Runner` — the argv may run, only under an explicit runner kind
//!
//! A missing or failed runner is a denial. There is no silent pass-through.

use std::path::{Path, PathBuf};

use crate::policy::ExecutionPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfineDialect {
    Runner(RunnerKind),
    Denial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    Host,
    Boxlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Isolation {
    Off,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinePolicy {
    pub isolation: Isolation,
    pub isolator_bin: Option<PathBuf>,
}

impl ConfinePolicy {
    pub fn host() -> Self {
        Self {
            isolation: Isolation::Off,
            isolator_bin: None,
        }
    }

    pub fn runtime_default() -> Self {
        Self {
            isolation: if cfg!(feature = "boxlite") {
                Isolation::Required
            } else {
                Isolation::Off
            },
            isolator_bin: None,
        }
    }

    pub fn required(isolator_bin: Option<PathBuf>) -> Self {
        Self {
            isolation: Isolation::Required,
            isolator_bin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfineOutcome {
    Runner { kind: RunnerKind, argv: Vec<String> },
    Denial { reason: String },
}

impl ConfineOutcome {
    pub fn dialect(&self) -> ConfineDialect {
        match self {
            Self::Runner { kind, .. } => ConfineDialect::Runner(*kind),
            Self::Denial { .. } => ConfineDialect::Denial,
        }
    }

    pub fn argv(&self) -> Option<&[String]> {
        match self {
            Self::Runner { argv, .. } => Some(argv),
            Self::Denial { .. } => None,
        }
    }
}

pub fn confine(argv: &[String], policy: &ConfinePolicy) -> ConfineOutcome {
    if let Some(reason) = deny_argv(argv) {
        return ConfineOutcome::Denial { reason };
    }

    match policy.isolation {
        Isolation::Off => ConfineOutcome::Runner {
            kind: RunnerKind::Host,
            argv: argv.to_vec(),
        },
        Isolation::Required => match resolve_isolator(policy) {
            Some(isolator) => {
                let wrapped = wrap_isolator(&isolator, argv);
                if wrapped.len() <= argv.len() || wrapped.first().is_none_or(|p| p.is_empty()) {
                    return ConfineOutcome::Denial {
                        reason: "isolator wrap produced an unusable argv".to_string(),
                    };
                }
                ConfineOutcome::Runner {
                    kind: RunnerKind::Boxlite,
                    argv: wrapped,
                }
            }
            None => ConfineOutcome::Denial {
                reason: "isolator missing; confinement is fail-closed".to_string(),
            },
        },
    }
}

fn deny_argv(argv: &[String]) -> Option<String> {
    if argv.is_empty() {
        return Some("empty argv".to_string());
    }
    if argv.iter().any(|part| part.is_empty()) {
        return Some("argv contains an empty element".to_string());
    }
    let command = command_from_argv(argv);
    if command.trim().is_empty() {
        return Some("empty command".to_string());
    }
    ExecutionPolicy::default()
        .check_shell_command(&command)
        .err()
}

fn command_from_argv(argv: &[String]) -> String {
    let program = argv[0].rsplit('/').next().unwrap_or(&argv[0]);
    if argv.len() >= 3 && matches!(program, "bash" | "sh" | "dash" | "zsh") && argv[1] == "-c" {
        argv[2].clone()
    } else {
        argv.join(" ")
    }
}

fn resolve_isolator(policy: &ConfinePolicy) -> Option<PathBuf> {
    if let Some(path) = &policy.isolator_bin {
        return path.is_file().then(|| path.clone());
    }
    match std::env::var_os("APOLLO_BOXLITE") {
        Some(value) => {
            let path = PathBuf::from(value);
            path.is_file().then_some(path)
        }
        None => look_on_path("boxlite"),
    }
}

fn look_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn wrap_isolator(isolator: &Path, argv: &[String]) -> Vec<String> {
    let mut wrapped = vec![
        isolator.to_string_lossy().into_owned(),
        "exec".to_string(),
        "--".to_string(),
    ];
    wrapped.extend(argv.iter().cloned());
    wrapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_argv_is_denial() {
        let out = confine(&[], &ConfinePolicy::host());
        assert_eq!(out.dialect(), ConfineDialect::Denial);
        assert!(out.argv().is_none());
    }

    #[test]
    fn empty_element_is_denial() {
        let out = confine(
            &["bash".into(), "-c".into(), String::new()],
            &ConfinePolicy::host(),
        );
        assert_eq!(out.dialect(), ConfineDialect::Denial);
    }

    #[test]
    fn host_runner_is_explicit() {
        let argv = vec!["echo".into(), "ok".into()];
        let out = confine(&argv, &ConfinePolicy::host());
        assert_eq!(out.dialect(), ConfineDialect::Runner(RunnerKind::Host));
        assert_eq!(out.argv(), Some(argv.as_slice()));
    }

    #[test]
    fn catastrophic_argv_is_denial_not_host() {
        let out = confine(
            &["bash".into(), "-c".into(), "rm -rf /".into()],
            &ConfinePolicy::host(),
        );
        assert_eq!(out.dialect(), ConfineDialect::Denial);
        assert!(out.argv().is_none());
    }

    #[test]
    fn required_isolation_without_isolator_is_denial() {
        let policy = ConfinePolicy::required(Some(PathBuf::from("/definitely/missing/boxlite")));
        let out = confine(&["echo".into(), "hi".into()], &policy);
        match out {
            ConfineOutcome::Denial { reason } => {
                assert!(reason.contains("isolator missing"), "{reason}");
            }
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[test]
    fn specified_missing_isolator_does_not_search_path() {
        let policy = ConfinePolicy::required(Some(PathBuf::from("/definitely/missing/boxlite")));
        let out = confine(&["true".into()], &policy);
        assert_eq!(out.dialect(), ConfineDialect::Denial);
    }

    #[test]
    fn isolator_wrap_is_runner_boxlite() {
        let isolator = tempfile::NamedTempFile::new().unwrap();
        let policy = ConfinePolicy::required(Some(isolator.path().to_path_buf()));
        let out = confine(&["echo".into(), "hi".into()], &policy);
        match out {
            ConfineOutcome::Runner {
                kind: RunnerKind::Boxlite,
                argv,
            } => {
                assert_eq!(argv[1], "exec");
                assert_eq!(argv[2], "--");
                assert_eq!(&argv[3..], ["echo", "hi"]);
            }
            other => panic!("expected boxlite runner, got {other:?}"),
        }
    }

    #[test]
    fn runtime_default_matches_feature() {
        let policy = ConfinePolicy::runtime_default();
        if cfg!(feature = "boxlite") {
            assert_eq!(policy.isolation, Isolation::Required);
        } else {
            assert_eq!(policy.isolation, Isolation::Off);
        }
    }
}
