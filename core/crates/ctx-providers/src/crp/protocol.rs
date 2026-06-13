use serde::de::Error as _;
use serde::Deserialize;
use serde_json::Value;

pub(super) use ctx_crp_protocol::{
    CrpChannel, CrpCommand, CrpCommandEnvelope, CrpEvent as KnownCrpEvent, CrpMcpServerConfig,
    CrpModelInfo, CrpSessionConfig, CrpToolStatus, CrpTurnStatus,
};

#[derive(Debug, Clone)]
pub struct CrpModelsProbe {
    pub models: Vec<CrpModelInfo>,
    pub current_model_id: Option<String>,
    pub catalog_source: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CrpEventEnvelope {
    #[allow(dead_code)]
    pub(super) v: Option<u32>,
    pub(super) seq: u64,
    #[allow(dead_code)]
    pub(super) channel: CrpChannel,
    pub(super) event: CrpEvent,
}

impl<'de> Deserialize<'de> for CrpEventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("CRP event envelope must be a JSON object"))?;

        let v =
            match object.get("v") {
                Some(raw) => Some(serde_json::from_value::<u32>(raw.clone()).map_err(|err| {
                    D::Error::custom(format!("invalid CRP envelope version: {err}"))
                })?),
                None => None,
            };
        let seq = serde_json::from_value::<u64>(
            object
                .get("seq")
                .cloned()
                .ok_or_else(|| D::Error::custom("CRP event envelope missing seq"))?,
        )
        .map_err(|err| D::Error::custom(format!("invalid CRP event seq: {err}")))?;
        let channel = serde_json::from_value::<CrpChannel>(
            object
                .get("channel")
                .cloned()
                .ok_or_else(|| D::Error::custom("CRP event envelope missing channel"))?,
        )
        .map_err(|err| D::Error::custom(format!("invalid CRP event channel: {err}")))?;

        let event = match parse_known_event(value.clone()) {
            Ok(event) => CrpEvent::Known(Box::new(event)),
            Err(err) => {
                let event_type = object
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| D::Error::custom(format!("failed to parse CRP event: {err}")))?;
                CrpEvent::Unknown {
                    event_type: event_type.to_string(),
                    session_id: object
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    turn_id: object
                        .get("turn_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    parse_error: err.to_string(),
                    raw: strip_envelope_fields(object),
                }
            }
        };

        Ok(Self {
            v,
            seq,
            channel,
            event,
        })
    }
}

#[derive(Debug, Clone)]
pub(super) enum CrpEvent {
    Known(Box<KnownCrpEvent>),
    Unknown {
        event_type: String,
        session_id: Option<String>,
        turn_id: Option<String>,
        parse_error: String,
        raw: Value,
    },
}

fn parse_known_event(value: Value) -> Result<KnownCrpEvent, serde_json::Error> {
    let mut compat = value;
    if let Some(object) = compat.as_object_mut() {
        if object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|event_type| event_type == "tool.output.delta")
        {
            object.insert(
                "type".to_string(),
                Value::String("tool.output_delta".to_string()),
            );
        }
    }
    serde_json::from_value(compat)
}

fn strip_envelope_fields(object: &serde_json::Map<String, Value>) -> Value {
    let mut raw = serde_json::Map::with_capacity(object.len());
    for (key, value) in object {
        if matches!(key.as_str(), "v" | "seq" | "channel") {
            continue;
        }
        raw.insert(key.clone(), value.clone());
    }
    Value::Object(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_crp_envelope() {
        let parsed: CrpEventEnvelope = serde_json::from_str(
            r#"{
                "v": 1,
                "seq": 7,
                "channel": "control",
                "type": "message.delta",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "message_id": "message-1",
                "delta": "hello"
            }"#,
        )
        .expect("known envelope should parse");

        assert_eq!(parsed.seq, 7);
        assert!(matches!(parsed.channel, CrpChannel::Control));
        let CrpEvent::Known(event) = parsed.event else {
            panic!("expected known CRP event");
        };
        assert!(matches!(
            event.as_ref(),
            KnownCrpEvent::MessageDelta {
                session_id,
                turn_id,
                message_id,
                delta,
            } if session_id == "session-1"
                && turn_id == "turn-1"
                && message_id == "message-1"
                && delta == "hello"
        ));
    }

    #[test]
    fn preserves_unknown_crp_event_type_as_unknown_variant() {
        let parsed: CrpEventEnvelope = serde_json::from_str(
            r#"{
                "v": 1,
                "seq": 8,
                "channel": "data",
                "type": "tool.progress",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "message": "progress update",
                "percent": 50,
                "nested": { "step": "scan" }
            }"#,
        )
        .expect("unknown envelope should still parse");

        assert_eq!(parsed.seq, 8);
        assert!(matches!(parsed.channel, CrpChannel::Data));
        match parsed.event {
            CrpEvent::Unknown {
                event_type,
                session_id,
                turn_id,
                parse_error,
                raw,
            } => {
                assert_eq!(event_type, "tool.progress");
                assert_eq!(session_id.as_deref(), Some("session-1"));
                assert_eq!(turn_id.as_deref(), Some("turn-1"));
                assert!(!parse_error.is_empty());
                assert_eq!(
                    raw.get("type"),
                    Some(&Value::String("tool.progress".to_string()))
                );
                assert_eq!(raw.get("percent"), Some(&Value::Number(50.into())));
            }
            other => panic!("expected unknown CRP event, got {other:?}"),
        }
    }

    #[test]
    fn parses_tool_output_delta_with_underscore_tag() {
        let parsed: CrpEventEnvelope = serde_json::from_str(
            r#"{
                "v": 1,
                "seq": 9,
                "channel": "data",
                "type": "tool.output_delta",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "tool_call_id": "tool-1",
                "chunk": "line-1\n"
            }"#,
        )
        .expect("underscored tool output delta should parse");

        let CrpEvent::Known(event) = parsed.event else {
            panic!("expected known CRP event");
        };
        assert!(matches!(
            event.as_ref(),
            KnownCrpEvent::ToolOutputDelta {
                session_id,
                turn_id,
                tool_call_id,
                chunk,
                ..
            } if session_id == "session-1"
                && turn_id == "turn-1"
                && tool_call_id == "tool-1"
                && chunk == "line-1\n"
        ));
    }

    #[test]
    fn parses_tool_output_delta_with_dotted_alias() {
        let parsed: CrpEventEnvelope = serde_json::from_str(
            r#"{
                "v": 1,
                "seq": 10,
                "channel": "data",
                "type": "tool.output.delta",
                "session_id": "session-2",
                "turn_id": "turn-2",
                "tool_call_id": "tool-2",
                "chunk": "line-2\n"
            }"#,
        )
        .expect("dotted tool output delta should parse");

        let CrpEvent::Known(event) = parsed.event else {
            panic!("expected known CRP event");
        };
        assert!(matches!(
            event.as_ref(),
            KnownCrpEvent::ToolOutputDelta {
                session_id,
                turn_id,
                tool_call_id,
                chunk,
                ..
            } if session_id == "session-2"
                && turn_id == "turn-2"
                && tool_call_id == "tool-2"
                && chunk == "line-2\n"
        ));
    }

    #[test]
    fn parses_shared_tool_output_delta_contract_envelope() {
        let raw = serde_json::to_string(&ctx_crp_protocol::CrpEventEnvelope {
            v: Some(ctx_crp_protocol::CRP_VERSION),
            seq: 11,
            channel: ctx_crp_protocol::CrpChannel::Data,
            event: ctx_crp_protocol::CrpEvent::ToolOutputDelta {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                tool_call_id: "tool-1".to_string(),
                stream: Some(ctx_crp_protocol::CrpToolOutputStream::Stdout),
                chunk: "line-1\n".to_string(),
            },
        })
        .expect("serialize shared CRP event");

        let parsed: CrpEventEnvelope =
            serde_json::from_str(&raw).expect("shared envelope should parse");

        let CrpEvent::Known(event) = parsed.event else {
            panic!("expected known CRP event");
        };
        assert!(matches!(
            event.as_ref(),
            KnownCrpEvent::ToolOutputDelta {
                session_id,
                turn_id,
                tool_call_id,
                stream,
                chunk,
            } if session_id == "session-1"
                && turn_id == "turn-1"
                && tool_call_id == "tool-1"
                && *stream == Some(ctx_crp_protocol::CrpToolOutputStream::Stdout)
                && chunk == "line-1\n"
        ));
    }
}
