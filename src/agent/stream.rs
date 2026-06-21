use serde::Serialize;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    Status {
        message: String,
    },
    ToolStart {
        name: String,
        hint: String,
    },
    ToolEnd {
        name: String,
        ok: bool,
        elapsed_secs: u64,
    },
    Delta {
        text: String,
    },
    Done {
        response: String,
    },
    Error {
        message: String,
    },
}

pub type AgentStreamTx = mpsc::UnboundedSender<AgentStreamEvent>;

pub fn emit(tx: &Option<AgentStreamTx>, event: AgentStreamEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}
