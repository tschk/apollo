use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ed25519_dalek::{Signer, SigningKey};
use praefectus::{
    canonical_authority_bytes, normalized_action_hash, Action, ActionRequest, AuthorityGrant,
    CancellationToken, Ed25519AuthorityVerifier, Engine, NativeExecutor, SafetyClass,
    SignedAuthority, TargetRef, VerificationPolicy, PROTOCOL_VERSION,
};
use rand::rngs::OsRng;
use serde::Deserialize;
use serde_json::Value;

use super::{Tool, ToolResult, ToolSpec};
use crate::policy::ExecutionPolicy;

pub struct PraefectusTool {
    runtime: Arc<PraefectusRuntime>,
    policy: Arc<ExecutionPolicy>,
}

struct PraefectusRuntime {
    engine: Engine<NativeExecutor>,
    observer: NativeExecutor,
    signing_key: SigningKey,
}

#[derive(Deserialize)]
struct PraefectusArgs {
    action: String,
    x: Option<i64>,
    y: Option<i64>,
    operation_id: Option<String>,
    desktop_action: Option<Value>,
    target: Option<Value>,
    verification: Option<Value>,
}

impl PraefectusTool {
    pub fn new(workspace: &Path, policy: Arc<ExecutionPolicy>) -> anyhow::Result<Self> {
        let state_dir = workspace.join(".unthinkclaw");
        std::fs::create_dir_all(&state_dir)?;
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifier = Ed25519AuthorityVerifier::new([(
            "unthinkclaw".to_string(),
            "runtime".to_string(),
            "1".to_string(),
            signing_key.verifying_key(),
        )]);
        Ok(Self {
            runtime: Arc::new(PraefectusRuntime {
                engine: Engine::new(
                    NativeExecutor::default(),
                    state_dir.join("praefectus-operations.jsonl"),
                    verifier,
                ),
                observer: NativeExecutor::default(),
                signing_key,
            }),
            policy,
        })
    }
}

impl PraefectusRuntime {
    fn execute(&self, args: PraefectusArgs) -> anyhow::Result<String> {
        match args.action.as_str() {
            "capabilities" => Ok(serde_json::to_string_pretty(&self.engine.capabilities()?)?),
            "observe_coordinates" => Ok(serde_json::to_string_pretty(
                &self.observer.observe_coordinates()?,
            )?),
            "observe_element" => {
                let target = self.observer.observe_element_at(
                    args.x.ok_or_else(|| anyhow::anyhow!("x is required"))?,
                    args.y.ok_or_else(|| anyhow::anyhow!("y is required"))?,
                )?;
                Ok(serde_json::to_string_pretty(&target)?)
            }
            "execute" => {
                let action: Action = serde_json::from_value(
                    args.desktop_action
                        .ok_or_else(|| anyhow::anyhow!("desktop_action is required"))?,
                )?;
                if !matches!(
                    action,
                    Action::Click { .. } | Action::Move | Action::SetValue { .. }
                ) {
                    anyhow::bail!("praefectus native execution only permits fenced click, move, and set_value actions");
                }
                let safety = match &action {
                    Action::Move => SafetyClass::Reversible,
                    Action::Click { .. } | Action::SetValue { .. } => SafetyClass::External,
                    _ => unreachable!(),
                };
                let target: TargetRef = serde_json::from_value(
                    args.target
                        .ok_or_else(|| anyhow::anyhow!("target is required"))?,
                )?;
                let verification = args
                    .verification
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or(VerificationPolicy::SnapshotChanged);
                let now = chrono::Utc::now().timestamp_millis();
                let operation_id = args
                    .operation_id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let mut request = ActionRequest {
                    protocol_version: PROTOCOL_VERSION,
                    action_version: 1,
                    target_version: 1,
                    verification_version: PROTOCOL_VERSION,
                    operation_id: operation_id.clone(),
                    subject: "local-user".to_string(),
                    session_id: "unthinkclaw".to_string(),
                    authority: SignedAuthority {
                        grant: AuthorityGrant {
                            protocol_version: PROTOCOL_VERSION,
                            issuer: "unthinkclaw".to_string(),
                            key_id: "runtime".to_string(),
                            operation_id,
                            subject: "local-user".to_string(),
                            session_id: "unthinkclaw".to_string(),
                            risk: safety,
                            expires_at_ms: now + 30_000,
                            policy_generation: "1".to_string(),
                            action_hash: String::new(),
                        },
                        signature: String::new(),
                    },
                    action,
                    target,
                    deadline_at_ms: now + 30_000,
                    verification,
                    safety,
                };
                request.authority.grant.action_hash = normalized_action_hash(&request)?;
                let signature = self
                    .signing_key
                    .sign(&canonical_authority_bytes(&request.authority.grant)?);
                request.authority.signature = encode_hex(&signature.to_bytes());
                Ok(serde_json::to_string_pretty(
                    &self
                        .engine
                        .execute(&request, &CancellationToken::default())?,
                )?)
            }
            other => anyhow::bail!("unknown praefectus action: {other}"),
        }
    }
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

#[async_trait]
impl Tool for PraefectusTool {
    fn name(&self) -> &str {
        "praefectus"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Inspect desktop capabilities and execute signed, durable, fenced desktop actions through Praefectus.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["capabilities", "observe_coordinates", "observe_element", "execute"] },
                    "x": { "type": "integer" },
                    "y": { "type": "integer" },
                    "operation_id": { "type": "string" },
                    "desktop_action": { "type": "object" },
                    "target": { "type": "object" },
                    "verification": { "type": "object" }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        if !self.policy.allow_computer_use {
            return ExecutionPolicy::deny("Computer use is disabled by policy");
        }
        let args: PraefectusArgs = serde_json::from_str(arguments)?;
        let runtime = Arc::clone(&self.runtime);
        match tokio::task::spawn_blocking(move || runtime.execute(args)).await? {
            Ok(output) => Ok(ToolResult::success(output)),
            Err(error) => Ok(ToolResult::error(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn policy_can_disable_computer_use() {
        let dir = tempfile::tempdir().unwrap();
        let policy = ExecutionPolicy {
            allow_computer_use: false,
            ..ExecutionPolicy::default()
        };
        let tool = PraefectusTool::new(dir.path(), Arc::new(policy)).unwrap();
        let result = tool.execute(r#"{"action":"capabilities"}"#).await.unwrap();
        assert!(result.is_error);
    }

    #[test]
    fn hex_encoding_is_stable() {
        assert_eq!(encode_hex(&[0, 15, 16, 255]), "000f10ff");
    }
}
