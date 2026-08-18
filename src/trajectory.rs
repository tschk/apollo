//! Trajectory export for RL training data generation.
//!
//! Captures full ReAct steps (thought → action → observation → response)
//! and serializes to JSON for fine-tuning or evaluation.
//!
//! Port from hermes-rs `trajectory.rs`.
//!
//! pontytail: JSON file output only. Parquet/Arrow can be added when
//! large-scale training pipelines need direct export.

use std::collections::HashMap;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A single step in a trajectory (one tool call + result)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    /// Step index
    pub step: usize,
    /// Agent reasoning/thought before the action
    pub thought: Option<String>,
    /// Tool name or action taken
    pub action: Option<String>,
    /// Arguments passed to the tool
    pub action_args: Option<String>,
    /// Direct observation/result from the action
    pub observation: Option<String>,
    /// Final response if this was the terminal step
    pub response: Option<String>,
    /// Whether the step completed successfully
    pub success: bool,
}

/// A complete trajectory (conversation) ready for training.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// Unique identifier
    pub id: String,
    /// Session ID this trajectory came from
    pub session_id: String,
    /// Model used for generation
    pub model: String,
    /// Unix timestamp
    pub timestamp: i64,
    /// Total tokens consumed
    pub total_tokens: usize,
    /// Number of tool calls made
    pub tool_calls: usize,
    /// Number of iterations (LLM rounds)
    pub iterations: usize,
    /// Whether the overall task was successful
    pub success: bool,
    /// The individual ReAct steps
    pub steps: Vec<TrajectoryStep>,
    /// Raw messages for full fidelity
    pub messages: Vec<TrajectoryMessage>,
    /// Arbitrary metadata
    pub metadata: HashMap<String, String>,
    /// Whether recorded content is blanket-redacted.
    ///
    /// `true` (the default) keeps only the shape of a run — tool names, step
    /// order, success flags — which is what a trajectory recorded without
    /// anyone asking should contain. `false` keeps the text, with credentials
    /// scrubbed by [`crate::redaction::redact_text`]; it is what an operator
    /// who turned collection on for training data actually wants, and it is
    /// reached only through explicit configuration.
    ///
    /// Not part of the saved document: a trajectory read back from disk is
    /// already redacted or already not.
    #[serde(skip, default = "redact_content_by_default")]
    pub redact_content: bool,
}

fn redact_content_by_default() -> bool {
    true
}

/// A single message in the conversation for training data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryMessage {
    pub role: String,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
}

impl Trajectory {
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            id: id.into(),
            session_id: session_id.into(),
            model: model.into(),
            timestamp,
            total_tokens: 0,
            tool_calls: 0,
            iterations: 0,
            success: false,
            steps: Vec::new(),
            messages: Vec::new(),
            metadata: HashMap::new(),
            redact_content: true,
        }
    }

    /// Keep step and message content (credentials still scrubbed) instead of
    /// blanket-redacting it.
    pub fn keeping_content(mut self) -> Self {
        self.redact_content = false;
        self
    }

    /// Add a ReAct step to the trajectory.
    pub fn add_step(&mut self, mut step: TrajectoryStep) {
        if self.redact_content {
            redact_step(&mut step);
        } else {
            scrub_step(&mut step);
        }
        if step.action.is_some() {
            self.tool_calls += 1;
        }
        self.steps.push(step);
    }

    /// Record a ReAct tool step directly (used by the agent loop).
    pub fn record_tool_step(
        &mut self,
        thought: Option<String>,
        action: String,
        action_args: String,
        observation: String,
        success: bool,
    ) {
        self.add_step(TrajectoryStep {
            step: self.steps.len() + 1,
            thought,
            action: Some(action),
            action_args: Some(action_args),
            observation: Some(observation),
            response: None,
            success,
        });
    }

    /// Record the final text response for the current step (used by loop_runner).
    pub fn record_response(&mut self, response: String) {
        let response_len = response.len();
        // Record as a message
        self.add_message("assistant", &response, None, None);
        self.total_tokens += response_len;

        // Also update the last step's response if applicable
        if let Some(step) = self.steps.last_mut() {
            step.response = Some("[REDACTED]".to_string());
        }
    }

    /// Add a raw message to the trajectory.
    pub fn add_message(
        &mut self,
        role: &str,
        content: &str,
        tool_call_id: Option<&str>,
        tool_name: Option<&str>,
    ) {
        self.messages.push(TrajectoryMessage {
            role: role.to_string(),
            content: if self.redact_content {
                redacted(content)
            } else {
                crate::redaction::redact_text(content)
            },
            tool_call_id: tool_call_id.map(|s| s.to_string()),
            tool_name: tool_name.map(|s| s.to_string()),
        });
    }

    /// Serialize to pretty JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut trajectory = self.clone();
        if self.redact_content {
            trajectory
                .metadata
                .values_mut()
                .for_each(|value| *value = "[REDACTED]".to_string());
            trajectory.steps.iter_mut().for_each(redact_step);
            trajectory.messages.iter_mut().for_each(|message| {
                if !message.content.is_empty() {
                    message.content = "[REDACTED]".to_string();
                }
            });
        } else {
            trajectory
                .metadata
                .values_mut()
                .for_each(|value| *value = crate::redaction::redact_text(value));
            trajectory.steps.iter_mut().for_each(scrub_step);
            trajectory.messages.iter_mut().for_each(|message| {
                message.content = crate::redaction::redact_text(&message.content);
            });
        }
        Ok(serde_json::to_string_pretty(&trajectory)?)
    }

    /// Save to a JSON file (alias for save). Creates parent directories.
    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        self.save(path)
    }

    /// Save to a JSON file. Creates parent directories.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        save_private(path, self.to_json()?.as_bytes())?;
        tracing::info!(
            "Trajectory saved to {:?} ({} steps, {} tokens, {} msgs)",
            path,
            self.steps.len(),
            self.total_tokens,
            self.messages.len()
        );
        Ok(())
    }
}

/// TrajectoryCollector — collects trajectory data during an agent run.
///
/// Attach to AgentRunner to capture all steps incrementally.
pub struct TrajectoryCollector {
    pub trajectory: Trajectory,
    current_step: Option<TrajectoryStep>,
}

impl TrajectoryCollector {
    pub fn new(session_id: impl Into<String>, model: impl Into<String>) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            trajectory: Trajectory::new(id, session_id, model),
            current_step: None,
        }
    }

    /// Start tracking a new tool call step.
    pub fn start_step(&mut self, action: &str, action_args: &str) {
        let step = TrajectoryStep {
            step: self.trajectory.steps.len() + 1,
            thought: None,
            action: Some(action.to_string()),
            action_args: Some(redacted(action_args)),
            observation: None,
            response: None,
            success: false,
        };
        self.current_step = Some(step);
    }

    /// Complete the current step with an observation.
    pub fn complete_step(&mut self, observation: &str, success: bool) {
        if let Some(mut step) = self.current_step.take() {
            step.observation = Some(redacted(observation));
            step.success = success;
            self.trajectory.add_step(step);
        }
    }

    /// Mark the current step as failed.
    pub fn fail_step(&mut self, error: &str) {
        if let Some(mut step) = self.current_step.take() {
            step.observation = Some(redacted(error));
            step.success = false;
            self.trajectory.add_step(step);
        }
    }

    /// Attach a thought to the current step.
    pub fn set_thought(&mut self, thought: &str) {
        if let Some(ref mut step) = self.current_step {
            step.thought = Some(redacted(thought));
        }
    }

    /// Attach a final response to the current step.
    pub fn set_response(&mut self, response: &str) {
        if let Some(ref mut step) = self.current_step {
            step.response = Some(redacted(response));
        }
    }

    /// Record a message to the trajectory.
    pub fn record_message(
        &mut self,
        role: &str,
        content: &str,
        tool_call_id: Option<&str>,
        tool_name: Option<&str>,
    ) {
        self.trajectory
            .add_message(role, content, tool_call_id, tool_name);
        self.trajectory.total_tokens += content.len();
    }

    /// Finish and save to file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        self.trajectory.save(path)
    }

    /// Mark the overall trajectory as successful and return JSON.
    pub fn finalize(&mut self, success: bool) -> String {
        self.trajectory.success = success;
        self.to_json().unwrap_or_default()
    }

    /// Serialize the trajectory to JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        self.trajectory.to_json()
    }
}

fn redact_step(step: &mut TrajectoryStep) {
    for value in [
        &mut step.thought,
        &mut step.action_args,
        &mut step.observation,
        &mut step.response,
    ] {
        if value.as_deref().is_some_and(|value| !value.is_empty()) {
            *value = Some("[REDACTED]".to_string());
        }
    }
}

/// Scrub credentials out of a step, keeping the text around them.
fn scrub_step(step: &mut TrajectoryStep) {
    for value in [
        &mut step.thought,
        &mut step.action_args,
        &mut step.observation,
        &mut step.response,
    ] {
        if let Some(text) = value.as_deref() {
            *value = Some(crate::redaction::redact_text(text));
        }
    }
}

fn redacted(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        "[REDACTED]".to_string()
    }
}

#[cfg(unix)]
fn save_private(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("trajectory path requires a private parent directory"))?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => validate_private_directory(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            validate_private_directory(&std::fs::symlink_metadata(parent)?)?;
        }
        Err(error) => return Err(error.into()),
    }

    let mut options = OpenOptions::new();
    options
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
        anyhow::bail!("trajectory file is not private");
    }
    file.set_len(0)?;
    file.write_all(contents)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o077 != 0 {
        anyhow::bail!("trajectory directory is not private");
    }
    Ok(())
}

#[cfg(not(unix))]
fn save_private(_path: &Path, _contents: &[u8]) -> anyhow::Result<()> {
    anyhow::bail!("private trajectory persistence is unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trajectory_new() {
        let t = Trajectory::new("test-1", "session-1", "claude-sonnet");
        assert_eq!(t.model, "claude-sonnet");
        assert_eq!(t.tool_calls, 0);
        assert!(!t.id.is_empty());
    }

    #[test]
    fn test_add_step() {
        let mut t = Trajectory::new("test-2", "session-1", "gpt-4");
        t.add_step(TrajectoryStep {
            step: 1,
            thought: Some("I need to read the file".into()),
            action: Some("read".into()),
            action_args: Some(r#"{"path":"test"}"#.into()),
            observation: Some("file content".into()),
            response: None,
            success: true,
        });
        assert_eq!(t.steps.len(), 1);
        assert_eq!(t.tool_calls, 1);
    }

    #[test]
    fn test_trajectory_collector() {
        let mut collector = TrajectoryCollector::new("session-2", "claude-opus");

        collector.start_step("shell", r#"{"command":"ls"}"#);
        collector.set_thought("Check directory contents");
        collector.complete_step("file1.txt\nfile2.txt", true);

        assert_eq!(collector.trajectory.steps.len(), 1);
        assert_eq!(collector.trajectory.tool_calls, 1);

        collector.record_message("user", "list files", None, None);
        collector.record_message("assistant", "done", None, None);

        assert_eq!(collector.trajectory.messages.len(), 2);
    }

    #[test]
    fn test_trajectory_json() {
        let mut t = Trajectory::new("test-3", "session-3", "claude-sonnet");
        t.add_step(TrajectoryStep {
            step: 1,
            thought: None,
            action: Some("read".into()),
            action_args: Some(r#"{"path":"/tmp/test"}"#.into()),
            observation: Some("data".into()),
            response: Some("Here's the data".into()),
            success: true,
        });
        t.total_tokens = 150;
        t.success = true;

        let json = t.to_json().unwrap();
        assert!(json.contains("\"action\": \"read\""));
        assert!(json.contains("\"total_tokens\": 150"));
        assert!(json.contains("\"success\": true"));

        // Round-trip
        let parsed: Trajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.steps.len(), 1);
        assert_eq!(parsed.tool_calls, 1);
    }

    #[test]
    fn trajectory_json_redacts_all_freeform_text() {
        let mut trajectory = Trajectory::new("test-4", "session-4", "model");
        trajectory
            .metadata
            .insert("task".to_string(), "clipboard secret".to_string());
        trajectory.add_step(TrajectoryStep {
            step: 1,
            thought: Some("credential thought".to_string()),
            action: Some("praefectus".to_string()),
            action_args: Some(r#"{"value":"typed secret"}"#.to_string()),
            observation: Some("private selector".to_string()),
            response: Some("repeated secret".to_string()),
            success: false,
        });
        trajectory.add_message("user", "raw task secret", None, None);

        let json = trajectory.to_json().expect("trajectory should serialize");
        for secret in [
            "clipboard secret",
            "credential thought",
            "typed secret",
            "private selector",
            "repeated secret",
            "raw task secret",
        ] {
            assert!(!json.contains(secret));
        }
        assert!(json.contains("[REDACTED]"));
    }

    #[cfg(unix)]
    #[test]
    fn trajectory_save_creates_private_durable_storage() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temporary directory should open");
        let path = root.path().join("private").join("trajectory.json");
        Trajectory::new("test-5", "session-5", "model")
            .save(&path)
            .expect("private trajectory should save");

        assert_eq!(
            std::fs::metadata(path.parent().expect("parent should exist"))
                .expect("directory metadata should load")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("file metadata should load")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn trajectory_save_rejects_public_or_linked_storage() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().expect("temporary directory should open");
        let public = root.path().join("public");
        std::fs::create_dir(&public).expect("public directory should create");
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755))
            .expect("public permissions should set");
        let trajectory = Trajectory::new("test-6", "session-6", "model");
        assert!(trajectory.save(&public.join("trajectory.json")).is_err());

        let private = root.path().join("private");
        std::fs::create_dir(&private).expect("private directory should create");
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
            .expect("private permissions should set");
        let destination = private.join("destination.json");
        std::fs::write(&destination, "existing").expect("destination should create");
        let linked = private.join("trajectory.json");
        symlink(&destination, &linked).expect("link should create");
        assert!(trajectory.save(&linked).is_err());
        assert_eq!(
            std::fs::read_to_string(destination).expect("destination should remain readable"),
            "existing"
        );
    }

    #[test]
    fn keeping_content_survives_the_round_trip_but_credentials_do_not() {
        let mut t = Trajectory::new("id", "chat", "model").keeping_content();
        t.record_tool_step(
            None,
            "shell".to_string(),
            r#"{"command":"deploy --token sk-ant-abcdefghijklmnopqrstuvwx"}"#.to_string(),
            "deployed 日本語".to_string(),
            true,
        );

        let step = &t.steps[0];
        assert_eq!(step.observation.as_deref(), Some("deployed 日本語"));
        let args = step.action_args.as_deref().unwrap();
        assert!(!args.contains("sk-ant-abcdefghijklmnopqrstuvwx"), "{args}");
        assert!(args.contains("deploy"), "{args}");

        let json = t.to_json().unwrap();
        assert!(
            json.contains("deployed"),
            "saving must not re-redact: {json}"
        );
        assert!(!json.contains("sk-ant-abcdefghijklmnopqrstuvwx"));
        assert!(
            !json.contains("redact_content"),
            "the flag is not part of the document"
        );
    }

    #[test]
    fn a_default_trajectory_still_redacts_everything_it_records() {
        let mut t = Trajectory::new("id", "chat", "model");
        t.record_tool_step(
            None,
            "shell".to_string(),
            r#"{"command":"ls"}"#.to_string(),
            "a.txt".to_string(),
            true,
        );
        assert_eq!(t.steps[0].observation.as_deref(), Some("[REDACTED]"));
        assert_eq!(t.steps[0].action.as_deref(), Some("shell"));
        let json = t.to_json().unwrap();
        assert!(!json.contains("a.txt"), "{json}");
    }
}
