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

tokio::task_local! {
    /// Sink for the turn running in the current task.
    ///
    /// A single global sink on `AgentRunner` cannot serve two clients: the
    /// second connection overwrites the first one's sender, so A's tokens go
    /// to B and A's turn end clears B's sink. Scoping the sink to the task
    /// that runs the turn keeps concurrent turns independent.
    static TURN_STREAM: Option<AgentStreamTx>;
}

/// The sink for the turn running in this task, if any.
pub fn current_turn_sink() -> Option<AgentStreamTx> {
    TURN_STREAM.try_with(|tx| tx.clone()).ok().flatten()
}

/// Run `fut` with `tx` as the sink for this turn.
pub async fn with_turn_sink<F>(tx: Option<AgentStreamTx>, fut: F) -> F::Output
where
    F: std::future::Future,
{
    TURN_STREAM.scope(tx, fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn one_turn(label: &str, tx: AgentStreamTx) {
        with_turn_sink(Some(tx), async {
            for i in 0..50 {
                emit(
                    &current_turn_sink(),
                    AgentStreamEvent::Delta {
                        text: format!("{label}{i}"),
                    },
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn concurrent_turns_do_not_cross_streams() {
        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();

        let a = tokio::spawn(one_turn("a", tx_a));
        let b = tokio::spawn(one_turn("b", tx_b));
        a.await.unwrap();
        b.await.unwrap();

        let drain = |rx: &mut mpsc::UnboundedReceiver<AgentStreamEvent>| {
            let mut out = Vec::new();
            while let Ok(AgentStreamEvent::Delta { text }) = rx.try_recv() {
                out.push(text);
            }
            out
        };

        let a_events = drain(&mut rx_a);
        let b_events = drain(&mut rx_b);
        assert_eq!(a_events.len(), 50);
        assert_eq!(b_events.len(), 50);
        assert!(a_events.iter().all(|t| t.starts_with('a')));
        assert!(b_events.iter().all(|t| t.starts_with('b')));
    }

    #[tokio::test]
    async fn no_scope_means_no_turn_sink() {
        assert!(current_turn_sink().is_none());
    }
}
