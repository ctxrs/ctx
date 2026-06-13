use std::collections::HashMap;

use ctx_core::ids::TurnId;
use ctx_core::models::SessionEventType;
use serde_json::Value;

#[derive(Debug)]
pub struct OrderSeqState {
    next_seq: i64,
    by_key: HashMap<String, i64>,
}

impl OrderSeqState {
    pub fn new(next_seq: i64) -> Self {
        Self {
            next_seq: next_seq.max(1),
            by_key: HashMap::new(),
        }
    }

    pub fn get_or_assign(&mut self, key: String, existing: Option<i64>) -> i64 {
        if let Some(seq) = self.by_key.get(&key) {
            return *seq;
        }
        if let Some(seq) = existing {
            self.bump_next(seq);
            self.by_key.insert(key, seq);
            return seq;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.by_key.insert(key, seq);
        seq
    }

    fn bump_next(&mut self, seq: i64) {
        if seq >= self.next_seq {
            self.next_seq = seq.saturating_add(1);
        }
    }
}

fn read_payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn read_payload_i64(payload: &Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(num) = value.as_i64() {
                return Some(num);
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = text.trim().parse::<i64>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

pub fn read_order_seq(payload: &Value) -> Option<i64> {
    read_payload_i64(payload, &["order_seq", "orderSeq"])
}

fn insert_order_seq(payload: &mut Value, order_seq: i64) {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("order_seq".to_string(), serde_json::json!(order_seq));
    }
}

fn build_order_seq_key(
    event_type: &SessionEventType,
    payload: &Value,
    turn_id: Option<&TurnId>,
    assistant_sequence: i64,
) -> Option<String> {
    match event_type {
        SessionEventType::UserMessage => {
            let message_id = read_payload_string(payload, &["message_id", "messageId"]);
            message_id.map(|id| format!("message:{id}"))
        }
        SessionEventType::AssistantChunk | SessionEventType::AssistantComplete => {
            let message_id = read_payload_string(
                payload,
                &[
                    "message_id",
                    "messageId",
                    "provider_message_id",
                    "providerMessageId",
                ],
            );
            if let Some(id) = message_id {
                return Some(format!("message:{id}"));
            }
            turn_id.map(|turn_id| {
                format!(
                    "message:turn:{}:{}",
                    turn_id.0,
                    assistant_sequence.saturating_add(1)
                )
            })
        }
        SessionEventType::AssistantMessageInserted => {
            if let Some(id) =
                read_payload_string(payload, &["provider_message_id", "providerMessageId"])
            {
                return Some(format!("message:{id}"));
            }
            if let Some(id) = read_payload_string(payload, &["message_id", "messageId"]) {
                return Some(format!("message:{id}"));
            }
            turn_id.map(|turn_id| {
                format!(
                    "message:turn:{}:{}",
                    turn_id.0,
                    assistant_sequence.saturating_add(1)
                )
            })
        }
        SessionEventType::ThoughtChunk => {
            let item_id = read_payload_string(payload, &["item_id", "itemId"]);
            let summary_index =
                read_payload_i64(payload, &["summary_index", "summaryIndex"]).unwrap_or(0);
            if let Some(id) = item_id {
                return Some(format!("thought:{id}:{summary_index}"));
            }
            turn_id.map(|turn_id| format!("thought:turn:{}:{summary_index}", turn_id.0))
        }
        SessionEventType::Notice => {
            let kind = read_payload_string(payload, &["kind"]);
            if kind.as_deref() == Some("reasoning_summary") {
                let item_id = read_payload_string(payload, &["item_id", "itemId"]);
                let summary_index =
                    read_payload_i64(payload, &["summary_index", "summaryIndex"]).unwrap_or(0);
                if let Some(id) = item_id {
                    return Some(format!("thought:{id}:{summary_index}"));
                }
                return turn_id
                    .map(|turn_id| format!("thought:turn:{}:{summary_index}", turn_id.0));
            }
            if kind.as_deref() == Some("ask_user_question") {
                if let Some(tool_call_id) =
                    read_payload_string(payload, &["tool_call_id", "toolCallId"])
                {
                    return Some(format!("tool:{tool_call_id}"));
                }
            }
            None
        }
        SessionEventType::ToolCall
        | SessionEventType::ToolCallUpdate
        | SessionEventType::ToolResult => {
            read_payload_string(payload, &["tool_call_id", "toolCallId"])
                .map(|tool_call_id| format!("tool:{tool_call_id}"))
        }
        _ => None,
    }
}

pub fn attach_order_seq(
    order_seq_state: &mut OrderSeqState,
    event_type: &SessionEventType,
    payload: &mut Value,
    turn_id: Option<&TurnId>,
    assistant_sequence: i64,
) {
    if !payload.is_object() {
        return;
    }
    let key = match build_order_seq_key(event_type, payload, turn_id, assistant_sequence) {
        Some(key) => key,
        None => return,
    };
    let existing = read_order_seq(payload);
    let seq = order_seq_state.get_or_assign(key, existing);
    if existing.is_none() {
        insert_order_seq(payload, seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assigns_stable_order_seq_for_tool_call_lifecycle() {
        let mut state = OrderSeqState::new(7);
        let mut call = json!({"tool_call_id": "tool-1"});
        let mut update = json!({"toolCallId": "tool-1"});
        let mut result = json!({"tool_call_id": "tool-1"});

        attach_order_seq(&mut state, &SessionEventType::ToolCall, &mut call, None, 0);
        attach_order_seq(
            &mut state,
            &SessionEventType::ToolCallUpdate,
            &mut update,
            None,
            0,
        );
        attach_order_seq(
            &mut state,
            &SessionEventType::ToolResult,
            &mut result,
            None,
            0,
        );

        assert_eq!(read_order_seq(&call), Some(7));
        assert_eq!(read_order_seq(&update), Some(7));
        assert_eq!(read_order_seq(&result), Some(7));
    }

    #[test]
    fn preserves_existing_order_seq_and_advances_next_assignment() {
        let mut state = OrderSeqState::new(1);
        let mut existing = json!({"message_id": "m1", "orderSeq": 42});
        let mut next = json!({"message_id": "m2"});

        attach_order_seq(
            &mut state,
            &SessionEventType::UserMessage,
            &mut existing,
            None,
            0,
        );
        attach_order_seq(
            &mut state,
            &SessionEventType::UserMessage,
            &mut next,
            None,
            0,
        );

        assert_eq!(read_order_seq(&existing), Some(42));
        assert_eq!(read_order_seq(&next), Some(43));
    }
}
