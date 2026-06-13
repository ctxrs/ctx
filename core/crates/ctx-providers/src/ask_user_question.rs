use std::collections::HashMap;

use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskUserQuestionOutcome {
    Submitted,
    Cancelled,
}

impl AskUserQuestionOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            AskUserQuestionOutcome::Submitted => "submitted",
            AskUserQuestionOutcome::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AskUserQuestionAnswer {
    pub outcome: AskUserQuestionOutcome,
    pub answers: HashMap<String, String>,
}

#[derive(Debug, Default)]
pub struct AskUserQuestionBroker {
    pending: Mutex<HashMap<(String, String), oneshot::Sender<AskUserQuestionAnswer>>>,
}

impl AskUserQuestionBroker {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn begin(
        &self,
        session_id: String,
        tool_call_id: String,
    ) -> oneshot::Receiver<AskUserQuestionAnswer> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.pending.lock().await;
        map.insert((session_id, tool_call_id), tx);
        rx
    }

    pub async fn submit(
        &self,
        session_id: &str,
        tool_call_id: &str,
        answer: AskUserQuestionAnswer,
    ) -> bool {
        let key = (session_id.to_string(), tool_call_id.to_string());
        let tx = { self.pending.lock().await.remove(&key) };
        if let Some(tx) = tx {
            let _ = tx.send(answer);
            true
        } else {
            false
        }
    }

    pub async fn abandon(&self, session_id: &str, tool_call_id: &str) {
        let key = (session_id.to_string(), tool_call_id.to_string());
        self.pending.lock().await.remove(&key);
    }
}
