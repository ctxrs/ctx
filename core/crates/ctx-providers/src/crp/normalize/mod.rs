use std::collections::HashMap;

use serde_json::Value;

use crate::events::{NormalizedEvent, ProviderUnknownEventObservation};

use super::protocol::{CrpChannel, CrpEvent, KnownCrpEvent};
use super::unknown_event::bound_unknown_crp_payload;

mod ids;
mod message_events;
mod notices;
mod session_events;
mod tool_events;

pub(super) struct MappedCrpEvent {
    pub(super) events: Vec<NormalizedEvent>,
    pub(super) done: bool,
}

#[derive(Debug, Clone)]
pub(super) struct CachedToolInput {
    pub(super) input: Option<Value>,
    pub(super) input_preview: Option<Value>,
}

pub(super) fn map_crp_event(
    event: CrpEvent,
    channel: CrpChannel,
    seq: u64,
    tool_output_cache: &mut HashMap<String, String>,
    tool_input_cache: &mut HashMap<String, CachedToolInput>,
) -> MappedCrpEvent {
    let crp_channel = crp_channel_value(channel);
    match event {
        CrpEvent::Known(event) => match *event {
            event @ KnownCrpEvent::SessionOpened { .. }
            | event @ KnownCrpEvent::TurnStarted { .. }
            | event @ KnownCrpEvent::TurnContextWindowUpdated { .. }
            | event @ KnownCrpEvent::TurnCompleted { .. }
            | event @ KnownCrpEvent::ModelsList { .. } => {
                session_events::map_known_event(event, crp_channel, seq)
            }
            event @ KnownCrpEvent::MessageDelta { .. }
            | event @ KnownCrpEvent::MessageFinal { .. }
            | event @ KnownCrpEvent::ReasoningSummary { .. }
            | event @ KnownCrpEvent::ReasoningTrace { .. }
            | event @ KnownCrpEvent::ReasoningTraceFinal { .. } => {
                message_events::map_known_event(event, crp_channel, seq)
            }
            event @ KnownCrpEvent::ToolStarted { .. }
            | event @ KnownCrpEvent::ToolOutputDelta { .. }
            | event @ KnownCrpEvent::ToolCompleted { .. } => tool_events::map_known_event(
                event,
                crp_channel,
                seq,
                tool_output_cache,
                tool_input_cache,
            ),
            event @ KnownCrpEvent::SessionNotice { .. }
            | event @ KnownCrpEvent::SessionGap { .. } => notices::map_known_event(event, seq),
        },
        CrpEvent::Unknown {
            event_type,
            parse_error,
            raw,
            ..
        } => notices::map_unknown_event(event_type, parse_error, raw, crp_channel, seq),
    }
}

pub(super) fn unknown_event_observation(
    event: &CrpEvent,
    channel: CrpChannel,
    seq: u64,
) -> Option<ProviderUnknownEventObservation> {
    let CrpEvent::Unknown {
        event_type,
        parse_error,
        raw,
        ..
    } = event
    else {
        return None;
    };
    let (raw, raw_truncated) = bound_unknown_crp_payload(raw.clone());
    Some(ProviderUnknownEventObservation {
        protocol: "crp",
        event_type: event_type.clone(),
        parse_error: parse_error.clone(),
        raw,
        raw_truncated,
        crp_channel: crp_channel_value(channel).map(str::to_string),
        crp_seq: seq,
        timeline_notice_emitted: false,
    })
}

pub(super) fn event_turn_id(event: &CrpEvent) -> Option<&str> {
    ids::event_turn_id(event)
}

pub(super) fn event_matches_session(event: &CrpEvent, session_id: &str) -> bool {
    ids::event_matches_session(event, session_id)
}

pub(super) fn insert_crp_channel(payload: &mut Value, crp_channel: Option<&str>) {
    if let Some(channel) = crp_channel {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("crp_channel".to_string(), serde_json::json!(channel));
        }
    }
}

fn crp_channel_value(channel: CrpChannel) -> Option<&'static str> {
    match channel {
        CrpChannel::Data => Some("data"),
        CrpChannel::Control => None,
    }
}

#[cfg(all(test, feature = "fuzz_tests"))]
#[path = "tests/fuzz_tests.rs"]
mod fuzz_tests;
