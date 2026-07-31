use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};
use praefectus::semantic::{SemanticObservation, SemanticTargetRef};
use praefectus::{
    canonical_authority_bytes, normalized_action_hash, AckState, Action, ActionRequest,
    AuthorityGrant, CancellationToken, Ed25519AuthorityVerifier, Engine, ExecuteReport,
    InteractionMode, NativeExecutor, SafetyClass, SignedAuthority, TargetRef, Terminal,
    VerificationPolicy, PROTOCOL_VERSION,
};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{Tool, ToolResult, ToolSpec};
use crate::policy::ExecutionPolicy;

const ACTION_WINDOW_MS: i64 = 30_000;

#[cfg(test)]
type BeforeCore = Arc<dyn Fn(&CancellationToken) + Send + Sync>;

pub struct PraefectusTool {
    runtime: Arc<PraefectusRuntime>,
    policy: Arc<ExecutionPolicy>,
    #[cfg(test)]
    before_core: Option<BeforeCore>,
}

struct PraefectusRuntime {
    engine: Engine<NativeExecutor>,
    observer: NativeExecutor,
    signing_key: SigningKey,
    session_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum PraefectusArgs {
    Capabilities {},
    Observe {},
    Execute {
        operation_id: String,
        deadline_at_ms: i64,
        interaction_mode: InteractionMode,
        desktop_action: Box<Action>,
        target: Box<SemanticTargetRef>,
    },
}

#[derive(Serialize)]
struct SemanticToolObservation {
    protocol_version: u16,
    observation_id: String,
    generation: u64,
    provenance_hash: String,
    observed_at_ms: i64,
    expires_at_ms: i64,
    truncated: bool,
    elements: Vec<SemanticToolElement>,
}

#[derive(Serialize)]
struct SemanticToolElement {
    tag: String,
    role: String,
    name: Option<String>,
    actionability: praefectus::semantic::Actionability,
    target: SemanticTargetRef,
}

struct RuntimeOutput {
    output: String,
    is_error: bool,
}

struct HostCancellation(Option<CancellationToken>);

impl HostCancellation {
    fn new(token: CancellationToken) -> Self {
        Self(Some(token))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for HostCancellation {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

async fn run_cancellable_operation<F>(operation: F) -> anyhow::Result<RuntimeOutput>
where
    F: FnOnce(&CancellationToken) -> anyhow::Result<RuntimeOutput> + Send + 'static,
{
    let cancellation = CancellationToken::default();
    let mut host_cancellation = HostCancellation::new(cancellation.clone());
    let result = tokio::task::spawn_blocking(move || operation(&cancellation)).await;
    if result.is_ok() {
        host_cancellation.disarm();
    }
    result?
}

impl PraefectusTool {
    pub fn new(workspace: &Path, policy: Arc<ExecutionPolicy>) -> anyhow::Result<Self> {
        let workspace = std::fs::canonicalize(workspace)?;
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifier = Ed25519AuthorityVerifier::new([(
            "apollo".to_string(),
            "host-process".to_string(),
            "computer-use-enabled-v2".to_string(),
            signing_key.verifying_key(),
        )])?;
        Ok(Self {
            runtime: Arc::new(PraefectusRuntime {
                engine: Engine::new(
                    NativeExecutor::default(),
                    workspace
                        .join(".apollo")
                        .join("praefectus-operations.jsonl"),
                    verifier,
                ),
                observer: NativeExecutor::default(),
                signing_key,
                session_id: format!("workspace:{}", value_hash(&workspace.to_string_lossy())),
            }),
            policy,
            #[cfg(test)]
            before_core: None,
        })
    }
}

impl PraefectusRuntime {
    fn execute(
        &self,
        args: PraefectusArgs,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<RuntimeOutput> {
        match args {
            PraefectusArgs::Capabilities {} => Ok(RuntimeOutput {
                output: serde_json::to_string(&self.engine.capabilities()?)?,
                is_error: false,
            }),
            PraefectusArgs::Observe {} => {
                let deadline_at_ms = now_ms()?.saturating_add(ACTION_WINDOW_MS);
                let observation = self
                    .observer
                    .observe_semantic(cancellation, deadline_at_ms)?;
                Ok(RuntimeOutput {
                    output: serde_json::to_string(&tool_observation(&observation)?)?,
                    is_error: false,
                })
            }
            PraefectusArgs::Execute {
                operation_id,
                deadline_at_ms,
                interaction_mode,
                desktop_action,
                target,
            } => {
                if !matches!(*desktop_action, Action::Invoke | Action::SetValue { .. }) {
                    anyhow::bail!(
                        "praefectus execution only permits semantic invoke and set_value actions"
                    );
                }
                let request = signed_request(
                    *desktop_action,
                    *target,
                    operation_id,
                    deadline_at_ms,
                    interaction_mode,
                    &self.signing_key,
                    &self.session_id,
                )?;
                let report = self.engine.execute(&request, cancellation)?;
                let is_error = !report_succeeded(&report);
                let output = serde_json::to_string(&json!({
                    "report": report,
                    "retry_safe": report_is_retry_safe(&report),
                }))?;
                Ok(RuntimeOutput { output, is_error })
            }
        }
    }
}

fn tool_observation(observation: &SemanticObservation) -> anyhow::Result<SemanticToolObservation> {
    observation.validate(now_ms()?)?;
    Ok(SemanticToolObservation {
        protocol_version: observation.protocol_version,
        observation_id: observation.observation_id.clone(),
        generation: observation.generation,
        provenance_hash: observation.provenance_hash()?,
        observed_at_ms: observation.observed_at_ms,
        expires_at_ms: observation.expires_at_ms,
        truncated: observation.truncated,
        elements: observation
            .elements
            .iter()
            .map(|element| {
                Ok(SemanticToolElement {
                    tag: element.tag.clone(),
                    role: element.role.clone(),
                    name: element.name.clone(),
                    actionability: element.actionability,
                    target: observation.target(&element.tag)?,
                })
            })
            .collect::<Result<_, praefectus::semantic::SemanticError>>()?,
    })
}

fn signed_request(
    action: Action,
    target: SemanticTargetRef,
    operation_id: String,
    deadline_at_ms: i64,
    interaction_mode: InteractionMode,
    signing_key: &SigningKey,
    session_id: &str,
) -> anyhow::Result<ActionRequest> {
    let safety = SafetyClass::External;
    let subject = "local-host-user".to_string();
    let authority_expires_at_ms = deadline_at_ms.min(now_ms()?.saturating_add(ACTION_WINDOW_MS));
    let mut request = ActionRequest {
        protocol_version: PROTOCOL_VERSION,
        action_version: PROTOCOL_VERSION,
        target_version: PROTOCOL_VERSION,
        verification_version: PROTOCOL_VERSION,
        operation_id: operation_id.clone(),
        subject: subject.clone(),
        session_id: session_id.to_string(),
        authority: SignedAuthority {
            grant: AuthorityGrant {
                protocol_version: PROTOCOL_VERSION,
                issuer: "apollo".to_string(),
                key_id: "host-process".to_string(),
                operation_id,
                subject,
                session_id: session_id.to_string(),
                risk: safety,
                expires_at_ms: authority_expires_at_ms,
                policy_generation: "computer-use-enabled-v2".to_string(),
                action_hash: String::new(),
            },
            signature: String::new(),
        },
        verification: match &action {
            Action::SetValue { value } => VerificationPolicy::TargetValueHash {
                sha256: value_hash(value),
            },
            Action::Invoke => VerificationPolicy::None,
            _ => anyhow::bail!("action has no authorized semantic route"),
        },
        action,
        target: TargetRef::Element { target },
        interaction_mode,
        deadline_at_ms,
        safety,
    };
    request.authority.grant.action_hash = normalized_action_hash(&request)?;
    request.authority.signature = encode_hex(
        &signing_key
            .sign(&canonical_authority_bytes(&request.authority.grant)?)
            .to_bytes(),
    );
    Ok(request)
}

fn report_succeeded(report: &ExecuteReport) -> bool {
    let mut terminals = report
        .acknowledgements
        .iter()
        .filter_map(|acknowledgement| match &acknowledgement.state {
            AckState::Terminal { terminal } => Some(&**terminal),
            _ => None,
        });
    matches!(terminals.next(), Some(Terminal::Succeeded { .. })) && terminals.next().is_none()
}

fn report_is_retry_safe(report: &ExecuteReport) -> bool {
    let mut terminals = report
        .acknowledgements
        .iter()
        .filter_map(|acknowledgement| match &acknowledgement.state {
            AckState::Terminal { terminal } => Some(&**terminal),
            _ => None,
        });
    let Some(first) = terminals.next() else {
        return false;
    };
    terminal_proves_no_effect(first) && terminals.all(terminal_proves_no_effect)
}

fn terminal_proves_no_effect(terminal: &Terminal) -> bool {
    matches!(
        terminal,
        Terminal::Rejected { .. } | Terminal::CancelledBeforeEffect | Terminal::ExpiredBeforeEffect
    )
}

fn value_hash(value: &str) -> String {
    encode_hex(&Sha256::digest(value.as_bytes()))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn now_ms() -> anyhow::Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

#[async_trait]
impl Tool for PraefectusTool {
    fn name(&self) -> &str {
        "praefectus"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Inspect desktop capabilities, observe semantic elements, and request host-authorized fenced actions through Praefectus.".to_string(),
            parameters: json!({
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "action": { "const": "capabilities" } },
                        "required": ["action"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": { "action": { "const": "observe" } },
                        "required": ["action"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": { "const": "execute" },
                            "operation_id": { "type": "string", "minLength": 1, "maxLength": 256 },
                            "deadline_at_ms": { "type": "integer" },
                            "interaction_mode": { "type": "string", "enum": ["interactive", "background_only"] },
                            "desktop_action": {
                                "oneOf": [
                                    {
                                        "type": "object",
                                        "properties": { "kind": { "const": "invoke" } },
                                        "required": ["kind"],
                                        "additionalProperties": false
                                    },
                                    {
                                        "type": "object",
                                        "properties": {
                                            "kind": { "const": "set_value" },
                                            "value": { "type": "string", "maxLength": 16384 }
                                        },
                                        "required": ["kind", "value"],
                                        "additionalProperties": false
                                    }
                                ]
                            },
                            "target": {
                                "type": "object",
                                "properties": {
                                    "observation_id": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                                    "generation": { "type": "integer", "minimum": 1 },
                                    "provenance_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                                    "element_id": { "type": "string", "pattern": "^[0-9a-f]{64}$" },
                                    "fingerprint_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$" }
                                },
                                "required": ["observation_id", "generation", "provenance_hash", "element_id", "fingerprint_hash"],
                                "additionalProperties": false
                            }
                        },
                        "required": ["action", "operation_id", "deadline_at_ms", "interaction_mode", "desktop_action", "target"],
                        "additionalProperties": false
                    }
                ]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_computer_use {
            return ExecutionPolicy::deny("Computer use is disabled by host policy");
        }
        let args: PraefectusArgs = serde_json::from_str(arguments)?;
        let runtime = Arc::clone(&self.runtime);
        #[cfg(test)]
        let before_core = self.before_core.clone();
        match run_cancellable_operation(move |cancellation| {
            #[cfg(test)]
            if let Some(before_core) = before_core {
                before_core(cancellation);
            }
            runtime.execute(args, cancellation)
        })
        .await
        {
            Ok(output) if output.is_error => Ok(ToolResult::error(output.output)),
            Ok(output) => Ok(ToolResult::success(output.output)),
            Err(error) => Ok(ToolResult::error(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use praefectus::{
        ActionAck, ContextPreservation, DeliveryRoute, Effect, FailureCode, Receipt,
        SessionIsolation,
    };

    use super::*;

    fn target() -> SemanticTargetRef {
        SemanticTargetRef {
            observation_id: "1".repeat(64),
            generation: 1,
            provenance_hash: "2".repeat(64),
            element_id: "3".repeat(64),
            fingerprint_hash: "4".repeat(64),
        }
    }

    fn receipt(effect: Effect) -> Receipt {
        Receipt {
            protocol_version: PROTOCOL_VERSION,
            action_name: "invoke".to_string(),
            action_hash: "0".repeat(64),
            started_at_ms: 1,
            finished_at_ms: 2,
            backend: "test".to_string(),
            fallback_chain: Vec::new(),
            delivery_route: DeliveryRoute::TargetAddressed,
            session_isolation: SessionIsolation::SharedDesktop,
            interaction_mode: InteractionMode::Interactive,
            context_preservation: ContextPreservation::NotApplicable,
            effect,
            before: None,
            after: None,
            warnings: Vec::new(),
        }
    }

    fn report(terminal: Terminal) -> ExecuteReport {
        ExecuteReport {
            acknowledgements: vec![ActionAck {
                protocol_version: PROTOCOL_VERSION,
                operation_id: "operation:1".to_string(),
                sequence: 2,
                action_hash: "0".repeat(64),
                replayed: false,
                state: AckState::Terminal {
                    terminal: Box::new(terminal),
                },
            }],
        }
    }

    #[tokio::test]
    async fn host_policy_can_disable_computer_use() {
        let dir = tempfile::tempdir().expect("temporary directory should open");
        let policy = ExecutionPolicy {
            allow_computer_use: false,
            ..ExecutionPolicy::default()
        };
        let tool =
            PraefectusTool::new(dir.path(), Arc::new(policy)).expect("tool should initialize");
        let result = tool
            .execute(r#"{"action":"capabilities"}"#)
            .await
            .expect("policy denial should be a tool result");
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn aborted_host_execution_cancels_the_active_core_token() {
        let dir = tempfile::tempdir().expect("temporary directory should open");
        let mut tool = PraefectusTool::new(dir.path(), Arc::new(ExecutionPolicy::default()))
            .expect("tool should initialize");
        let (token_tx, mut token_rx) = tokio::sync::mpsc::unbounded_channel();
        tool.before_core = Some(Arc::new(move |cancellation| {
            token_tx
                .send(cancellation.clone())
                .expect("host should receive core token");
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
        }));
        let tool = Arc::new(tool);
        let execution =
            tokio::spawn(async move { tool.execute(r#"{"action":"capabilities"}"#).await });
        let core_token = token_rx
            .recv()
            .await
            .expect("blocking execution should expose its core token");

        execution.abort();
        let join_error = match execution.await {
            Ok(_) => panic!("host execution should abort"),
            Err(error) => error,
        };
        assert!(join_error.is_cancelled(), "host future must be cancelled");
        assert!(core_token.is_cancelled());
    }

    #[test]
    fn host_cancellation_reaches_praefectus_before_effect() {
        let dir = tempfile::tempdir().expect("temporary directory should open");
        let tool = PraefectusTool::new(dir.path(), Arc::new(ExecutionPolicy::default()))
            .expect("tool should initialize");
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let result = tool
            .runtime
            .execute(
                PraefectusArgs::Execute {
                    operation_id: "cancelled:operation".to_string(),
                    deadline_at_ms: now_ms()
                        .expect("time should resolve")
                        .saturating_add(ACTION_WINDOW_MS),
                    interaction_mode: InteractionMode::Interactive,
                    desktop_action: Box::new(Action::Invoke),
                    target: Box::new(target()),
                },
                &cancellation,
            )
            .expect("cancellation should produce a terminal report");

        assert!(result.is_error);
        assert!(result.output.contains("cancelled_before_effect"));
    }

    #[test]
    fn caller_cannot_supply_authority_or_isolation() {
        for input in [
            r#"{"action":"capabilities","authority":{}}"#,
            r#"{"action":"observe","session_isolation":"host_isolated"}"#,
        ] {
            assert!(serde_json::from_str::<PraefectusArgs>(input).is_err());
        }
    }

    #[test]
    fn authority_hash_binds_interaction_mode_and_operation() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let interactive = signed_request(
            Action::Invoke,
            target(),
            "operation:1".to_string(),
            i64::MAX,
            InteractionMode::Interactive,
            &signing_key,
            "session:1",
        )
        .expect("interactive request should sign");
        let background = signed_request(
            Action::Invoke,
            target(),
            "operation:1".to_string(),
            i64::MAX,
            InteractionMode::BackgroundOnly,
            &signing_key,
            "session:1",
        )
        .expect("background request should sign");
        let other_operation = signed_request(
            Action::Invoke,
            target(),
            "operation:2".to_string(),
            i64::MAX,
            InteractionMode::Interactive,
            &signing_key,
            "session:1",
        )
        .expect("other operation should sign");

        assert_ne!(
            interactive.authority.grant.action_hash,
            background.authority.grant.action_hash
        );
        assert_eq!(
            interactive.authority.grant.action_hash,
            other_operation.authority.grant.action_hash
        );
        assert_ne!(
            interactive.authority.grant.operation_id,
            other_operation.authority.grant.operation_id
        );
    }

    #[test]
    fn set_value_verification_binds_the_exact_value() {
        let request = signed_request(
            Action::SetValue {
                value: "private value".to_string(),
            },
            target(),
            "operation:1".to_string(),
            i64::MAX,
            InteractionMode::Interactive,
            &SigningKey::from_bytes(&[7; 32]),
            "session:1",
        )
        .expect("set value request should sign");

        assert!(matches!(
            request.verification,
            VerificationPolicy::TargetValueHash { sha256 }
                if sha256 == value_hash("private value")
        ));
    }

    #[test]
    fn uncertain_and_failed_effects_are_never_retry_safe() {
        for terminal in [
            Terminal::OutcomeUnknown {
                receipt: receipt(Effect::Unknown),
                message: "outcome unknown".to_string(),
            },
            Terminal::Failed {
                code: FailureCode::VerificationFailed,
                message: "verification failed".to_string(),
            },
        ] {
            let report = report(terminal);
            assert!(!report_succeeded(&report));
            assert!(!report_is_retry_safe(&report));
        }
    }

    #[test]
    fn only_proven_no_effect_terminals_are_retry_safe() {
        assert!(report_is_retry_safe(&report(Terminal::Rejected {
            code: FailureCode::PermissionDenied,
            message: "denied".to_string(),
        })));
        assert!(!report_is_retry_safe(&report(Terminal::Succeeded {
            receipt: receipt(Effect::Verified),
        })));
    }
}
