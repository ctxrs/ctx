use serde_json::json;

use ctx_core::models::SessionEventType;

use crate::events::NormalizedEvent;

use super::super::protocol::KnownCrpEvent;
use super::{insert_crp_channel, MappedCrpEvent};

pub(super) fn map_known_event(
    event: KnownCrpEvent,
    crp_channel: Option<&str>,
    seq: u64,
) -> MappedCrpEvent {
    match event {
        KnownCrpEvent::MessageDelta {
            delta, message_id, ..
        } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::AssistantChunk,
                payload_json: json!({
                    "content_fragment": delta,
                    "message_id": message_id,
                    "crp_seq": seq,
                    "crp_channel": crp_channel,
                }),
            }],
            done: false,
        },
        KnownCrpEvent::MessageFinal {
            content,
            message_id,
            ..
        } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::AssistantComplete,
                payload_json: json!({
                    "full_content": content,
                    "message_id": message_id,
                    "crp_seq": seq,
                }),
            }],
            done: false,
        },
        KnownCrpEvent::ReasoningSummary {
            text,
            item_id,
            summary_index,
            ..
        } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::Notice,
                payload_json: json!({
                    "kind": "reasoning_summary",
                    "summary_index": summary_index,
                    "text": text,
                    "item_id": item_id,
                    "crp_seq": seq,
                }),
            }],
            done: false,
        },
        KnownCrpEvent::ReasoningTrace {
            chunk,
            encoding,
            summary_index,
            item_id,
            ..
        } => {
            let mut payload = json!({
                "content_fragment": chunk,
                "encoding": encoding,
                "summary_index": summary_index,
                "item_id": item_id,
                "crp_seq": seq,
            });
            insert_crp_channel(&mut payload, crp_channel);
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::ThoughtChunk,
                    payload_json: payload,
                }],
                done: false,
            }
        }
        KnownCrpEvent::ReasoningTraceFinal {
            content,
            encoding,
            summary_index,
            item_id,
            ..
        } => {
            let mut payload = json!({
                "content_fragment": "",
                "full_content": content,
                "is_final": true,
                "encoding": encoding,
                "summary_index": summary_index,
                "item_id": item_id,
                "crp_seq": seq,
            });
            if let Some(item_id) = item_id {
                if let Some(obj) = payload.as_object_mut() {
                    obj.insert("item_id".to_string(), json!(item_id));
                }
            }
            insert_crp_channel(&mut payload, crp_channel);
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::ThoughtChunk,
                    payload_json: payload,
                }],
                done: false,
            }
        }
        _ => unreachable!("unexpected message normalize event"),
    }
}
