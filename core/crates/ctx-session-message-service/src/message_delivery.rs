use ctx_core::ids::{MessageId, TurnId};
use ctx_core::models::{Message, MessageDelivery, SessionTurn, SessionTurnStatus};

const QUEUED_MESSAGES_DISABLED_MESSAGE: &str = "Queued messages are disabled.";
const TURN_ALREADY_RUNNING_MESSAGE: &str =
    "A turn is already running. Stop it or wait for it to finish.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageDeliveryResolutionError {
    QueuedMessagesDisabled,
    TurnAlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageClientIdResolutionError {
    PartialClientIds,
}

impl MessageClientIdResolutionError {
    pub fn message(self) -> &'static str {
        match self {
            MessageClientIdResolutionError::PartialClientIds => {
                "Message id and turn id must either both be provided or both be omitted."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageClientIds {
    pub message_id: MessageId,
    pub turn_id: TurnId,
    pub client_supplied: bool,
}

impl MessageDeliveryResolutionError {
    pub fn message(self) -> &'static str {
        match self {
            MessageDeliveryResolutionError::QueuedMessagesDisabled => {
                QUEUED_MESSAGES_DISABLED_MESSAGE
            }
            MessageDeliveryResolutionError::TurnAlreadyRunning => TURN_ALREADY_RUNNING_MESSAGE,
        }
    }
}

pub fn resolve_message_client_ids(
    message_id: Option<MessageId>,
    turn_id: Option<TurnId>,
) -> Result<MessageClientIds, MessageClientIdResolutionError> {
    match (message_id, turn_id) {
        (Some(message_id), Some(turn_id)) => Ok(MessageClientIds {
            message_id,
            turn_id,
            client_supplied: true,
        }),
        (None, None) => Ok(MessageClientIds {
            message_id: MessageId::new(),
            turn_id: TurnId::new(),
            client_supplied: false,
        }),
        _ => Err(MessageClientIdResolutionError::PartialClientIds),
    }
}

pub fn resolve_message_delivery(
    requested_delivery: Option<MessageDelivery>,
    session_running: bool,
    queued_enabled: bool,
) -> Result<MessageDelivery, MessageDeliveryResolutionError> {
    match requested_delivery {
        Some(MessageDelivery::Queued) if queued_enabled => Ok(MessageDelivery::Queued),
        Some(MessageDelivery::Queued) => {
            Err(MessageDeliveryResolutionError::QueuedMessagesDisabled)
        }
        None if session_running && queued_enabled => Ok(MessageDelivery::Queued),
        Some(MessageDelivery::Immediate) | None if session_running => {
            Err(MessageDeliveryResolutionError::TurnAlreadyRunning)
        }
        Some(MessageDelivery::Immediate) | None => Ok(MessageDelivery::Immediate),
    }
}

pub fn delivery_matches(left: &MessageDelivery, right: &MessageDelivery) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

pub fn initial_turn_status(delivery: &MessageDelivery) -> SessionTurnStatus {
    match delivery {
        MessageDelivery::Queued => SessionTurnStatus::Queued,
        MessageDelivery::Immediate => SessionTurnStatus::Starting,
    }
}

pub fn build_user_message_turn(
    message: &Message,
    turn_id: TurnId,
    start_seq: Option<i64>,
) -> SessionTurn {
    SessionTurn {
        turn_id,
        session_id: message.session_id,
        run_id: message.run_id,
        user_message_id: Some(message.id),
        status: initial_turn_status(&message.delivery),
        start_seq,
        end_seq: None,
        started_at: message.created_at,
        updated_at: message.created_at,
        assistant_partial: None,
        thought_partial: None,
        metrics_json: None,
        failure: None,
        tool_total: 0,
        tool_pending: 0,
        tool_running: 0,
        tool_completed: 0,
        tool_failed: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_queueing_when_queue_feature_is_disabled() {
        assert!(matches!(
            resolve_message_delivery(None, true, false),
            Err(MessageDeliveryResolutionError::TurnAlreadyRunning)
        ));
        assert!(matches!(
            resolve_message_delivery(Some(MessageDelivery::Queued), false, false),
            Err(MessageDeliveryResolutionError::QueuedMessagesDisabled)
        ));
    }

    #[test]
    fn queues_only_when_feature_enabled() {
        assert!(matches!(
            resolve_message_delivery(None, true, true),
            Ok(MessageDelivery::Queued)
        ));
        assert!(matches!(
            resolve_message_delivery(Some(MessageDelivery::Queued), true, true),
            Ok(MessageDelivery::Queued)
        ));
        assert!(matches!(
            resolve_message_delivery(None, false, false),
            Ok(MessageDelivery::Immediate)
        ));
        assert!(matches!(
            resolve_message_delivery(Some(MessageDelivery::Immediate), true, true),
            Err(MessageDeliveryResolutionError::TurnAlreadyRunning)
        ));
    }

    #[test]
    fn errors_expose_user_facing_messages() {
        assert_eq!(
            MessageDeliveryResolutionError::QueuedMessagesDisabled.message(),
            "Queued messages are disabled."
        );
        assert_eq!(
            MessageDeliveryResolutionError::TurnAlreadyRunning.message(),
            "A turn is already running. Stop it or wait for it to finish."
        );
    }

    #[test]
    fn client_ids_must_be_paired_or_generated_together() {
        let message_id = MessageId::new();
        let turn_id = TurnId::new();
        let supplied = resolve_message_client_ids(Some(message_id), Some(turn_id))
            .expect("paired ids should be accepted");
        assert_eq!(supplied.message_id, message_id);
        assert_eq!(supplied.turn_id, turn_id);
        assert!(supplied.client_supplied);

        let generated =
            resolve_message_client_ids(None, None).expect("missing ids should be generated");
        assert_ne!(generated.message_id, message_id);
        assert_ne!(generated.turn_id, turn_id);
        assert!(!generated.client_supplied);

        let err = resolve_message_client_ids(Some(message_id), None)
            .expect_err("partial ids should be rejected");
        assert_eq!(
            err.message(),
            "Message id and turn id must either both be provided or both be omitted."
        );
    }

    #[test]
    fn delivery_matching_and_initial_turn_status_follow_delivery_variant() {
        assert!(delivery_matches(
            &MessageDelivery::Queued,
            &MessageDelivery::Queued
        ));
        assert!(!delivery_matches(
            &MessageDelivery::Queued,
            &MessageDelivery::Immediate
        ));
        assert!(matches!(
            initial_turn_status(&MessageDelivery::Queued),
            SessionTurnStatus::Queued
        ));
        assert!(matches!(
            initial_turn_status(&MessageDelivery::Immediate),
            SessionTurnStatus::Starting
        ));
    }

    #[test]
    fn user_message_turn_builder_sets_initial_status_and_defaults() {
        use ctx_core::ids::{RunId, SessionId, TaskId};
        use ctx_core::models::MessageRole;

        let message = Message {
            id: MessageId::new(),
            session_id: SessionId::new(),
            task_id: TaskId::new(),
            run_id: Some(RunId::new()),
            turn_id: None,
            turn_sequence: None,
            order_seq: None,
            role: MessageRole::User,
            content: "hello".to_string(),
            attachments: Vec::new(),
            delivery: MessageDelivery::Queued,
            delivered_at: None,
            created_at: "2026-01-01T00:00:00Z".parse().expect("created_at"),
        };
        let turn_id = TurnId::new();
        let turn = build_user_message_turn(&message, turn_id, Some(42));

        assert_eq!(turn.turn_id, turn_id);
        assert_eq!(turn.session_id, message.session_id);
        assert_eq!(turn.run_id, message.run_id);
        assert_eq!(turn.user_message_id, Some(message.id));
        assert_eq!(turn.status, SessionTurnStatus::Queued);
        assert_eq!(turn.start_seq, Some(42));
        assert_eq!(turn.end_seq, None);
        assert_eq!(turn.started_at, message.created_at);
        assert_eq!(turn.updated_at, message.created_at);
        assert_eq!(turn.tool_total, 0);
        assert_eq!(turn.tool_failed, 0);
    }
}
