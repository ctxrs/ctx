use std::collections::HashMap;

use serde_json::json;

use ctx_core::models::SessionEventType;

use crate::events::NormalizedEvent;

use super::super::normalize_tool_payload::{
    build_tool_completed_payload, build_tool_started_payload,
};
use super::super::protocol::KnownCrpEvent;
use super::{insert_crp_channel, CachedToolInput, MappedCrpEvent};

pub(super) fn map_known_event(
    event: KnownCrpEvent,
    crp_channel: Option<&str>,
    seq: u64,
    tool_output_cache: &mut HashMap<String, String>,
    tool_input_cache: &mut HashMap<String, CachedToolInput>,
) -> MappedCrpEvent {
    match event {
        KnownCrpEvent::ToolStarted {
            tool_call_id,
            tool_name,
            tool_label,
            input,
            input_preview,
            ..
        } => {
            if input.is_some() || input_preview.is_some() {
                tool_input_cache.insert(
                    tool_call_id.clone(),
                    CachedToolInput {
                        input: input.clone(),
                        input_preview: input_preview.clone(),
                    },
                );
            }
            let payload = build_tool_started_payload(
                tool_call_id,
                tool_name,
                tool_label,
                input,
                input_preview,
                seq,
            );
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::ToolCall,
                    payload_json: payload,
                }],
                done: false,
            }
        }
        KnownCrpEvent::ToolOutputDelta {
            tool_call_id,
            chunk,
            ..
        } => {
            let output_text = {
                let entry = tool_output_cache.entry(tool_call_id.clone()).or_default();
                entry.push_str(&chunk);
                entry.clone()
            };
            let mut payload = json!({
                "tool_call_id": tool_call_id,
                "outputText": output_text,
                "status": "running",
                "crp_seq": seq,
            });
            insert_crp_channel(&mut payload, crp_channel);
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::ToolCallUpdate,
                    payload_json: payload,
                }],
                done: false,
            }
        }
        KnownCrpEvent::ToolCompleted {
            tool_call_id,
            tool_name,
            tool_label,
            status,
            output,
            error,
            input_preview,
            ..
        } => {
            let tool_call_id_for_cache = tool_call_id.clone();
            let cached = tool_input_cache.remove(&tool_call_id_for_cache);
            let (input, cached_preview) = cached
                .map(|cached| (cached.input, cached.input_preview))
                .unwrap_or((None, None));
            let input_preview = input_preview.or(cached_preview);
            let payload = build_tool_completed_payload(
                tool_call_id,
                tool_name,
                tool_label,
                status,
                output,
                error,
                input,
                input_preview,
                seq,
            );
            tool_output_cache.remove(&tool_call_id_for_cache);
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::ToolResult,
                    payload_json: payload,
                }],
                done: false,
            }
        }
        _ => unreachable!("unexpected tool normalize event"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crp::protocol::{CrpChannel, CrpEvent, CrpToolStatus};

    fn known(event: KnownCrpEvent) -> CrpEvent {
        CrpEvent::Known(Box::new(event))
    }

    #[test]
    fn tool_completed_retains_started_preview_when_completed_omits_it() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let started_preview = json!({"summary":"Read: foo.txt"});
        let started = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::ToolStarted {
                session_id: "s".to_string(),
                turn_id: "t".to_string(),
                tool_call_id: "call1".to_string(),
                tool_name: "read_file".to_string(),
                tool_label: None,
                input: None,
                input_preview: Some(started_preview.clone()),
            }),
            CrpChannel::Control,
            1,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );
        assert_eq!(started.events.len(), 1);
        assert!(matches!(
            &started.events[0].event_type,
            SessionEventType::ToolCall
        ));

        let completed = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::ToolCompleted {
                session_id: "s".to_string(),
                turn_id: "t".to_string(),
                tool_call_id: "call1".to_string(),
                tool_name: "read_file".to_string(),
                tool_label: None,
                status: CrpToolStatus::Success,
                output: None,
                error: None,
                input_preview: None,
            }),
            CrpChannel::Control,
            2,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );
        assert_eq!(completed.events.len(), 1);
        assert!(matches!(
            &completed.events[0].event_type,
            SessionEventType::ToolResult
        ));

        let payload = &completed.events[0].payload_json;
        assert_eq!(payload.get("input_preview"), Some(&started_preview));
        assert_eq!(payload.get("rawInput"), Some(&started_preview));
    }
}
