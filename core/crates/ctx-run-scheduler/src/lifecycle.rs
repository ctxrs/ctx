use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant as TokioInstant;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_providers::adapters::{ProviderAdapter, RunHandle};
use ctx_providers::events::NormalizedEvent;

pub struct RunningTurn {
    pub adapter: Arc<dyn ProviderAdapter>,
    pub handle: RunHandle,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub message_id: MessageId,
    pub provider_id: String,
    pub model_id: String,
    pub execution_environment_label: String,
    pub session_root_kind: String,
    pub event_tx: mpsc::Sender<NormalizedEvent>,
    pub events_done: Option<oneshot::Receiver<()>>,
    pub start_progress: watch::Receiver<TurnStartProgress>,
    pub start_deadline: TokioInstant,
    pub mcp_token: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnStartProgress {
    Pending,
    Started,
    Terminal,
}

#[derive(Clone, Copy)]
pub enum StopReason {
    Cancel,
    Interrupt,
    StorageEmergency,
}

impl StopReason {
    pub fn should_emit_interrupt_requested(self) -> bool {
        matches!(self, Self::Interrupt)
    }

    pub fn missing_outcome_reason(self) -> &'static str {
        match self {
            Self::Cancel => "user_cancel_missing_outcome",
            Self::Interrupt => "user_interrupt_missing_outcome",
            Self::StorageEmergency => "storage_exhausted_missing_outcome",
        }
    }

    pub fn outcome_timeout_reason(self) -> &'static str {
        match self {
            Self::Cancel => "user_cancel_outcome_timeout",
            Self::Interrupt => "user_interrupt_outcome_timeout",
            Self::StorageEmergency => "storage_exhausted_outcome_timeout",
        }
    }

    pub fn suspend_queue(self) -> bool {
        matches!(self, Self::Interrupt)
    }
}

#[cfg(test)]
mod tests {
    use super::StopReason;

    #[test]
    fn stop_reason_policies_match_turn_stop_semantics() {
        assert!(!StopReason::Cancel.should_emit_interrupt_requested());
        assert!(StopReason::Interrupt.should_emit_interrupt_requested());
        assert!(StopReason::Interrupt.suspend_queue());
        assert!(!StopReason::StorageEmergency.suspend_queue());
        assert_eq!(
            StopReason::StorageEmergency.missing_outcome_reason(),
            "storage_exhausted_missing_outcome"
        );
        assert_eq!(
            StopReason::Cancel.outcome_timeout_reason(),
            "user_cancel_outcome_timeout"
        );
    }
}
