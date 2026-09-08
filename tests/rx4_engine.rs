//! End-to-end cover for the rx4 engine.
//!
//! The rx4 (rotary) harness owns the loop. Everything apollo owns around that
//! loop — the assembled context going in, the tool set, the permission hooks,
//! the reply coming back out and being persisted — is only exercised by driving
//! a real turn. Before this test the rx4 path had no coverage at all, which is
//! how it silently shipped without permission hooks, lifecycle events or stream
//! events.
//!
//! Deliberately a real `AgentRunner` over a real `SurrealMemory`, with only the
//! provider and channel stubbed, so the assertions fail if the engine branch,
//! the bridge, the tool registration or the reply plumbing regress.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use apollo::agent::hooks::PermissionHook;
use apollo::agent::mode::NullChannel;
use apollo::agent::rotary_bridge::{
    record_recovery_action, record_rx4_event, record_rx4_event_value, record_spill_notice,
    runtime_pty_worker, RotaryAgentBridge, RotaryBridgeConfig, Rx4TrajectoryRecorder,
    ToolHookContext,
};
use apollo::agent::AgentRunner;
use apollo::channels::IncomingMessage;
use apollo::memory::surreal::SurrealMemory;
use apollo::providers::traits::{
    ChatRequest, ChatResponse, Provider, ProviderCapabilities, ToolCall,
};
use apollo::tools::confine::{confine, ConfineDialect, ConfinePolicy, RunnerKind};
use apollo::tools::shell::ShellTool;
use apollo::tools::{Tool, ToolResult, ToolSpec};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;

const FINAL_REPLY: &str = "the file says hello";

/// A provider that calls `probe` once, then answers with text.
///
/// It also records the system prompt and tool specs it was handed, so the test
/// can assert apollo's context assembly still reaches the model under rx4.
struct ToolThenTextProvider {
    calls: AtomicUsize,
    seen_system: Mutex<Vec<String>>,
    seen_tools: Mutex<Vec<Vec<String>>>,
}

impl ToolThenTextProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            seen_system: Mutex::new(Vec::new()),
            seen_tools: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Provider for ToolThenTextProvider {
    fn name(&self) -> &str {
        "tool-then-text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: false,
            vision: false,
            max_context: 32_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        self.seen_system.lock().unwrap().extend(
            request
                .messages
                .iter()
                .filter(|m| m.role == "system")
                .map(|m| m.content.clone()),
        );
        self.seen_tools.lock().unwrap().push(
            request
                .tools
                .unwrap_or(&[])
                .iter()
                .map(|t| t.name.clone())
                .collect(),
        );

        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_1".to_string(),
                    name: "probe".to_string(),
                    arguments: r#"{"path":"hello.txt"}"#.to_string(),
                }],
                usage: None,
            })
        } else {
            Ok(ChatResponse {
                text: Some(FINAL_REPLY.to_string()),
                tool_calls: vec![],
                usage: None,
            })
        }
    }
}

/// Records that it ran, so a blocked call is distinguishable from an allowed
/// one that happened to return an error.
struct ProbeTool {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for ProbeTool {
    fn name(&self) -> &str {
        "probe"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "probe".to_string(),
            description: "read a path".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
            }),
        }
    }

    async fn execute(&self, _arguments: &str) -> anyhow::Result<ToolResult> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("hello"))
    }
}

/// Owns the tempdir for the lifetime of the runner.
///
/// `tests/reply_delivery.rs` leaks its tempdir with `std::mem::forget` to keep
/// the SurrealDB files alive; holding the handle in a struct achieves the same
/// without leaking, so the RocksDB directory is removed when the test ends.
struct Harness {
    runner: AgentRunner,
    provider: Arc<ToolThenTextProvider>,
    tool_runs: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let memory = SurrealMemory::new(dir.path()).await.unwrap();
    let provider = Arc::new(ToolThenTextProvider::new());
    let tool_runs = Arc::new(AtomicUsize::new(0));
    let tool: Arc<dyn Tool> = Arc::new(ProbeTool {
        runs: Arc::clone(&tool_runs),
    });

    let runner = AgentRunner::new(
        Arc::clone(&provider) as Arc<dyn Provider>,
        vec![tool],
        Arc::new(memory),
        "you are a test agent",
        "test-model",
    )
    .with_config(apollo::config::AgentConfig {
        ..Default::default()
    })
    .with_workspace(dir.path().to_path_buf());

    Harness {
        runner,
        provider,
        tool_runs,
        _dir: dir,
    }
}

fn message(chat_id: &str, text: &str) -> IncomingMessage {
    IncomingMessage {
        id: "m1".to_string(),
        sender_id: "test".to_string(),
        sender_name: None,
        chat_id: chat_id.to_string(),
        text: text.to_string(),
        is_group: false,
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

/// rx4 must actually cycle a tool, and the final text must come back out
/// through apollo's `finish_execution`.
#[tokio::test]
async fn rx4_engine_runs_a_tool_and_returns_the_reply() {
    let h = harness().await;

    let response = h
        .runner
        .handle_message(
            &message("rx4-turn", "read hello.txt"),
            &NullChannel::new("test"),
        )
        .await
        .unwrap();

    assert_eq!(response, FINAL_REPLY, "rx4 turn did not return the reply");
    assert_eq!(
        h.tool_runs.load(Ordering::SeqCst),
        1,
        "rx4 did not execute the registered apollo tool"
    );
    assert!(
        h.provider.calls.load(Ordering::SeqCst) >= 2,
        "rx4 did not cycle back to the model after the tool"
    );

    // apollo still owns context assembly: the system prompt and the tool set
    // must survive the hand-off to the harness.
    let system = h.provider.seen_system.lock().unwrap().join("\n");
    assert!(
        system.contains("you are a test agent"),
        "system prompt lost crossing the bridge: {system:?}"
    );
    let tools = h.provider.seen_tools.lock().unwrap().clone();
    assert!(
        tools.iter().all(|t| t.iter().any(|n| n == "probe")),
        "tool specs lost crossing the bridge: {tools:?}"
    );
}

/// A denied tool must not execute under rx4. This is the assertion that was
/// missing when the rx4 path ran without permission hooks: the turn still
/// completes, but the tool body never runs.
#[tokio::test]
async fn rx4_engine_enforces_permission_hooks() {
    let h = harness().await;
    h.runner.add_hook(Arc::new(PermissionHook::new(
        vec!["probe".to_string()],
        vec![],
    )));

    let response = h
        .runner
        .handle_message(
            &message("rx4-denied", "read hello.txt"),
            &NullChannel::new("test"),
        )
        .await
        .unwrap();

    assert_eq!(
        h.tool_runs.load(Ordering::SeqCst),
        0,
        "a denied tool executed under rx4"
    );
    assert_eq!(
        response, FINAL_REPLY,
        "the turn must still finish after a blocked tool"
    );
}

/// The rx4 turn must persist through apollo's memory backend, not rx4's own
/// session store — history is what apollo feeds back in on the next turn.
#[tokio::test]
async fn rx4_engine_persists_the_turn() {
    let h = harness().await;
    h.runner
        .handle_message(
            &message("rx4-persist", "read hello.txt"),
            &NullChannel::new("test"),
        )
        .await
        .unwrap();

    let history = h
        .runner
        .memory()
        .get_conversation_history("rx4-persist", 20)
        .await
        .unwrap();
    assert!(
        history
            .iter()
            .any(|(_, content)| content.contains(FINAL_REPLY)),
        "rx4 reply not persisted: {history:?}"
    );
}

#[tokio::test]
async fn rx4_engine_records_tool_steps_on_the_trajectory() {
    let h = harness().await;
    h.runner
        .handle_message(
            &message("rx4-traj", "read hello.txt"),
            &NullChannel::new("test"),
        )
        .await
        .unwrap();

    let traj = h
        .runner
        .get_trajectory("rx4-traj")
        .await
        .expect("trajectory must be collected");
    assert!(
        traj.tool_calls >= 1,
        "rx4 events were not subscribed into the trajectory: {traj:?}"
    );
    assert!(
        traj.steps
            .iter()
            .any(|step| step.action.as_deref() == Some("probe")),
        "probe tool step missing: {:?}",
        traj.steps
    );
}

struct ExecThenTextProvider {
    calls: AtomicUsize,
}

impl ExecThenTextProvider {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl Provider for ExecThenTextProvider {
    fn name(&self) -> &str {
        "exec-then-text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: false,
            vision: false,
            max_context: 32_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, _request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "call_exec".to_string(),
                    name: "exec".to_string(),
                    arguments: r#"{"command":"printf confined-ok"}"#.to_string(),
                }],
                usage: None,
            })
        } else {
            Ok(ChatResponse {
                text: Some(FINAL_REPLY.to_string()),
                tool_calls: vec![],
                usage: None,
            })
        }
    }
}

#[tokio::test]
async fn rx4_engine_confines_exec_and_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let memory = SurrealMemory::new(dir.path()).await.unwrap();
    let provider = Arc::new(ExecThenTextProvider::new());
    let tool: Arc<dyn Tool> = Arc::new(
        ShellTool::new(
            dir.path().to_path_buf(),
            Arc::new(apollo::policy::ExecutionPolicy::default()),
        )
        .with_confine(ConfinePolicy::host()),
    );
    let runner = AgentRunner::new(
        Arc::clone(&provider) as Arc<dyn Provider>,
        vec![tool],
        Arc::new(memory),
        "you are a test agent",
        "test-model",
    )
    .with_workspace(dir.path().to_path_buf());

    let response = runner
        .handle_message(&message("rx4-exec", "run it"), &NullChannel::new("test"))
        .await
        .unwrap();
    assert_eq!(response, FINAL_REPLY);

    let traj = runner
        .get_trajectory("rx4-exec")
        .await
        .expect("trajectory must be collected");
    assert!(
        traj.steps
            .iter()
            .any(|step| step.action.as_deref() == Some("exec")),
        "exec step missing: {:?}",
        traj.steps
    );
}

#[test]
fn confine_host_runner_is_not_a_silent_pass() {
    let argv = vec!["printf".into(), "ok".into()];
    let out = confine(&argv, &ConfinePolicy::host());
    assert_eq!(out.dialect(), ConfineDialect::Runner(RunnerKind::Host));
    assert_eq!(out.argv(), Some(argv.as_slice()));
}

#[test]
fn confine_denies_instead_of_passing_an_unusable_isolator() {
    let out = confine(
        &["echo".into(), "hi".into()],
        &ConfinePolicy::required(Some(PathBuf::from("/definitely/missing/boxlite"))),
    );
    assert_eq!(out.dialect(), ConfineDialect::Denial);
    assert!(out.argv().is_none());
}

struct SilentProvider;

#[async_trait]
impl Provider for SilentProvider {
    fn name(&self) -> &str {
        "silent"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tools: true,
            streaming: false,
            vision: false,
            max_context: 32_000,
            native_web_search: false,
        }
    }

    async fn chat(&self, _request: &ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        Ok(ChatResponse {
            text: Some(String::new()),
            tool_calls: vec![],
            usage: None,
        })
    }
}

fn rotary_bridge_for_tools(tools: Vec<Arc<dyn Tool>>) -> RotaryAgentBridge {
    RotaryAgentBridge::new(RotaryBridgeConfig {
        provider: Arc::new(SilentProvider),
        tools,
        system_prompt: String::new(),
        model: "test".into(),
        workspace: PathBuf::from("."),
        max_tool_iterations: 4,
        auto_compact_after: 0,
        cost_tracker: None,
        hook_ctx: ToolHookContext::default(),
    })
}

#[tokio::test]
async fn rx4_bridge_write_stdin_uses_the_pty_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let shell = ShellTool::new(
        tmp.path().to_path_buf(),
        Arc::new(apollo::policy::ExecutionPolicy::default()),
    )
    .with_confine(ConfinePolicy::host());
    let worker = shell.pty_worker();
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(shell)];
    let attached = runtime_pty_worker(&tools);
    assert!(
        Arc::ptr_eq(&attached, &worker),
        "rotary run path must attach the shell worker"
    );

    let bridge = rotary_bridge_for_tools(tools).with_pty_worker(attached);
    assert!(
        bridge
            .pty_worker()
            .is_some_and(|pty| Arc::ptr_eq(&pty, &worker)),
        "pty must be populated on the rotary run path"
    );

    let id = worker
        .spawn(
            confine(
                &["sh".into(), "-c".into(), "read x; printf %s \"$x\"".into()],
                &ConfinePolicy::host(),
            ),
            tmp.path(),
            false,
        )
        .await
        .unwrap();

    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ToolExecutionStart(rx4::ToolCall {
            id: id.clone(),
            name: "exec".into(),
            arguments: "{}".into(),
        }),
    );

    bridge.write_stdin(&id, b"bridge-hi\n").await.unwrap();
    worker.close_stdin(&id).await.unwrap();
    let output = worker.wait(&id, Duration::from_secs(5)).await.unwrap();
    assert!(output.stdout.contains("bridge-hi"), "{}", output.stdout);
    assert!(!id.is_empty());
}

#[test]
fn rx4_new_events_are_recorded_on_the_trajectory() {
    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event(
        &mut recorder,
        &rx4::Event::RetryReason {
            retry_reason: "sandbox deny".into(),
            layer: "NestedFs".into(),
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ProcessStdin {
            process_id: "p1".into(),
            bytes: 4,
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::RequestPermissions {
            tool: "write".into(),
            paths: vec!["src/lib.rs".into()],
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::PatchHunk {
            path: "src/lib.rs".into(),
            hunk: "@@ -1 +1 @@".into(),
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::SelfHealing {
            attempt: 1,
            max_attempts: 3,
            errors: vec!["timeout".into()],
        },
    );
    let (steps, _) = recorder.take_steps();
    let actions: Vec<_> = steps
        .iter()
        .filter_map(|step| step.action.as_deref())
        .collect();
    assert_eq!(
        actions,
        ["retry", "stdin", "permissions", "patch", "recovery"]
    );
}

#[test]
fn rx4_sandbox_escalate_records_retry_and_stays_fail_closed() {
    let deny = rx4::SandboxError::PathDenied("/etc/passwd".into());
    let retry = rx4::escalate_on_deny(rx4::SandboxLayer::Userspace, &deny).unwrap();
    assert_eq!(retry.to, rx4::SandboxLayer::NestedFs);

    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event(
        &mut recorder,
        &rx4::Event::RetryReason {
            retry_reason: retry.retry_reason,
            layer: format!("{:?}", retry.to),
        },
    );
    let (steps, _) = recorder.take_steps();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].action.as_deref(), Some("retry"));
    assert_eq!(steps[0].action_args.as_deref(), Some("NestedFs"));
    assert!(!steps[0].success);

    assert!(
        rx4::escalate_on_deny(rx4::SandboxLayer::GitReadOnly, &deny).is_err(),
        "top layer must deny instead of silently passing"
    );
}

#[test]
fn rx4_spilled_tool_result_records_a_spill_step() {
    let dir = tempfile::tempdir().unwrap();
    let body = "x".repeat(20_000);
    let spilled = rx4::tools::spill::bound_tool_output(&body, 1024, dir.path());
    assert!(spilled.spilled);
    assert!(rx4::tools::spill::locator_is_file(&spilled.locator));

    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ToolExecutionStart(rx4::ToolCall {
            id: "c-spill".into(),
            name: "exec".into(),
            arguments: "{}".into(),
        }),
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ToolSpill {
            status: spilled.status,
            locator: spilled.locator.clone(),
            original_bytes: spilled.original_bytes,
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ToolExecutionEnd(rx4::ToolResult {
            id: "c-spill".into(),
            content: spilled.preview.clone(),
            is_error: false,
            error_kind: None,
            spill: Some(spilled.notice()),
        }),
    );
    let (steps, _) = recorder.take_steps();
    assert!(
        steps
            .iter()
            .any(|step| step.action.as_deref() == Some("exec")),
        "tool step missing: {:?}",
        steps
    );
    let spill = steps
        .iter()
        .find(|step| step.action.as_deref() == Some("spill"))
        .expect("spill step missing");
    assert_eq!(spill.observation.as_deref(), Some(spilled.locator.as_str()));
}

#[test]
fn rx4_recovery_actions_are_recorded_on_the_trajectory() {
    let mut recorder = Rx4TrajectoryRecorder::default();
    record_recovery_action(&mut recorder, &rx4::recover_empty_turn(0, 3), "empty_turn");
    record_recovery_action(&mut recorder, &rx4::recover_empty_turn(1, 3), "empty_turn");
    record_recovery_action(&mut recorder, &rx4::recover_stuck_tool(0, 3), "stuck_tool");
    record_recovery_action(&mut recorder, &rx4::RecoveryAction::Retry, "stuck_tool");
    let (steps, _) = recorder.take_steps();
    let actions: Vec<_> = steps
        .iter()
        .filter_map(|step| step.action.as_deref())
        .collect();
    assert_eq!(actions, ["prefill", "nudge", "stuck_tool", "stuck_tool"]);
    assert_eq!(steps[2].action_args.as_deref(), Some("nudge"));
    assert_eq!(steps[3].action_args.as_deref(), Some("retry"));
}

#[test]
fn rx4_typed_notice_values_are_recorded_on_the_trajectory() {
    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event_value(
        &mut recorder,
        &serde_json::json!({"type": "Prefill", "text": "continue"}),
    );
    record_rx4_event_value(
        &mut recorder,
        &serde_json::json!({"type": "Nudge", "text": "answer"}),
    );
    record_rx4_event_value(
        &mut recorder,
        &serde_json::json!({
            "type": "StuckTool",
            "action": "Halt",
            "text": "stuck tool repeated 2 times (halt after 3)"
        }),
    );
    record_rx4_event_value(
        &mut recorder,
        &serde_json::json!({"type": "SpillNotice", "locator": ".rx4/spill/out.txt"}),
    );
    record_rx4_event_value(
        &mut recorder,
        &serde_json::json!({"type": "FailureNotice", "message": "bounded write failed"}),
    );
    record_spill_notice(&mut recorder, "file://already-mapped");
    let (steps, _) = recorder.take_steps();
    let actions: Vec<_> = steps
        .iter()
        .filter_map(|step| step.action.as_deref())
        .collect();
    assert_eq!(
        actions,
        [
            "prefill",
            "nudge",
            "stuck_tool",
            "spill",
            "failure",
            "spill"
        ]
    );
    assert_eq!(steps[2].action_args.as_deref(), Some("halt"));
    assert!(!steps[2].success);
    assert_eq!(steps[3].observation.as_deref(), Some(".rx4/spill/out.txt"));
}

#[test]
fn rx4_typed_recovery_spill_and_process_events_are_recorded() {
    let mut recorder = Rx4TrajectoryRecorder::default();
    record_rx4_event(
        &mut recorder,
        &rx4::Event::Recovery {
            action: rx4::RecoveryKind::Prefill,
            reason: "Continue from where you left off.".into(),
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::Recovery {
            action: rx4::RecoveryKind::Nudge,
            reason: "change args".into(),
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ToolSpill {
            status: rx4::SpillStatus::Spilled,
            locator: ".rx4/spill/out.txt".into(),
            original_bytes: 2048,
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ProcessStart {
            process_id: "p1".into(),
            program: "cat".into(),
        },
    );
    record_rx4_event(
        &mut recorder,
        &rx4::Event::ProcessEnd {
            process_id: "p1".into(),
            exit_code: Some(0),
        },
    );
    let (steps, _) = recorder.take_steps();
    let actions: Vec<_> = steps
        .iter()
        .filter_map(|step| step.action.as_deref())
        .collect();
    assert_eq!(
        actions,
        ["prefill", "nudge", "spill", "process_start", "process_end"]
    );
    assert_eq!(steps[2].action_args.as_deref(), Some("spilled:2048"));
    assert_eq!(steps[2].observation.as_deref(), Some(".rx4/spill/out.txt"));
}
