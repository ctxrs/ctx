use serde_json::{json, Value};

use ctx_core::models::SessionEventType;

use crate::events::NormalizedEvent;

use super::super::protocol::KnownCrpEvent;
use super::MappedCrpEvent;

fn is_auth_notice_code(code: &str) -> bool {
    matches!(
        code,
        "auth_required"
            | "auth_error"
            | "auth_failed"
            | "auth_complete"
            | "auth_completed"
            | "auth_success"
            | "authenticated"
    )
}

fn safe_auth_notice_message(code: &str, message: Option<&str>) -> Option<String> {
    let generic = match code {
        "auth_required" => "Authentication required.",
        "auth_error" | "auth_failed" => "Authentication failed.",
        "auth_complete" | "auth_completed" | "auth_success" | "authenticated" => {
            "Authentication complete."
        }
        _ => "Authentication update.",
    };

    let Some(message) = message.map(str::trim).filter(|value| !value.is_empty()) else {
        return Some(generic.to_string());
    };

    let lower = message.to_ascii_lowercase();
    if lower.contains("://")
        || lower.contains("www.")
        || lower.contains("token=")
        || lower.contains("code=")
        || lower.contains("bearer ")
    {
        return Some(generic.to_string());
    }

    Some(message.to_string())
}

fn safe_auth_methods_value(value: &Value) -> Option<Value> {
    let methods = value
        .as_array()?
        .iter()
        .filter_map(|method| {
            let object = method.as_object()?;
            let id = object
                .get("id")
                .or_else(|| object.get("methodId"))
                .or_else(|| object.get("method_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let name = object
                .get("name")
                .or_else(|| object.get("label"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(id);
            Some(json!({
                "id": id,
                "name": name,
            }))
        })
        .collect::<Vec<_>>();

    (!methods.is_empty()).then_some(Value::Array(methods))
}

pub(super) fn map_known_event(event: KnownCrpEvent, seq: u64) -> MappedCrpEvent {
    match event {
        KnownCrpEvent::SessionNotice {
            code,
            severity,
            message,
            details,
            transient,
            ..
        } => {
            let mut payload = serde_json::Map::new();
            payload.insert("kind".to_string(), json!(code.clone()));
            payload.insert("code".to_string(), json!(code.clone()));
            if let Some(severity) = severity {
                payload.insert("severity".to_string(), json!(severity));
            }
            if is_auth_notice_code(&code) {
                if let Some(message) = safe_auth_notice_message(&code, message.as_deref()) {
                    payload.insert("message".to_string(), json!(message));
                }
                if let Some(Value::Object(map)) = details.as_ref() {
                    if let Some(auth_methods) = map
                        .get("auth_methods")
                        .or_else(|| map.get("authMethods"))
                        .and_then(safe_auth_methods_value)
                    {
                        payload.insert("auth_methods".to_string(), auth_methods);
                    }
                    if let Some(provider) = map
                        .get("provider")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        payload.insert("provider".to_string(), json!(provider));
                    }
                }
            } else {
                if let Some(message) = message {
                    payload.insert("message".to_string(), json!(message));
                }
                if let Some(details) = details {
                    if let Value::Object(map) = &details {
                        if let Some(auth_methods) =
                            map.get("auth_methods").or_else(|| map.get("authMethods"))
                        {
                            payload.insert("auth_methods".to_string(), auth_methods.clone());
                        }
                        if let Some(provider) = map.get("provider") {
                            payload.insert("provider".to_string(), provider.clone());
                        }
                    }
                    payload.insert("details".to_string(), details);
                }
            }
            if let Some(transient) = transient {
                payload.insert("transient".to_string(), json!(transient));
            }
            payload.insert("crp_seq".to_string(), json!(seq));
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::Notice,
                    payload_json: Value::Object(payload),
                }],
                done: false,
            }
        }
        KnownCrpEvent::SessionGap { reason, .. } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::Notice,
                payload_json: json!({
                    "kind": "session_gap",
                    "reason": reason,
                    "crp_seq": seq,
                }),
            }],
            done: false,
        },
        _ => unreachable!("unexpected notice normalize event"),
    }
}

pub(super) fn map_unknown_event(
    _event_type: String,
    _parse_error: String,
    _raw: Value,
    _crp_channel: Option<&str>,
    _seq: u64,
) -> MappedCrpEvent {
    MappedCrpEvent {
        events: Vec::new(),
        done: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::crp::normalize::CachedToolInput;
    use crate::crp::protocol::{CrpChannel, CrpEvent};

    fn known(event: KnownCrpEvent) -> CrpEvent {
        CrpEvent::Known(Box::new(event))
    }

    #[test]
    fn auth_session_notice_persists_only_safe_subset() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let mapped = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::SessionNotice {
                session_id: "session-1".to_string(),
                turn_id: Some("turn-1".to_string()),
                code: "auth_required".to_string(),
                severity: Some("warning".to_string()),
                message: Some("Visit https://auth.example.test/start?token=secret".to_string()),
                details: Some(json!({
                    "provider": "amp",
                    "auth_methods": [
                        {
                            "id": "oauth",
                            "name": "Sign in",
                            "description": "Opens a browser",
                            "_meta": {
                                "authUrl": "https://auth.example.test/start?token=secret"
                            }
                        }
                    ],
                    "auth_url": "https://auth.example.test/start?token=secret",
                    "message": "raw provider payload"
                })),
                transient: Some(false),
            }),
            CrpChannel::Control,
            7,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        assert_eq!(mapped.events.len(), 1);
        assert!(matches!(
            mapped.events[0].event_type,
            SessionEventType::Notice
        ));
        let payload = mapped.events[0]
            .payload_json
            .as_object()
            .expect("notice payload");
        assert_eq!(payload.get("kind"), Some(&json!("auth_required")));
        assert_eq!(payload.get("code"), Some(&json!("auth_required")));
        assert_eq!(payload.get("severity"), Some(&json!("warning")));
        assert_eq!(
            payload.get("message"),
            Some(&json!("Authentication required."))
        );
        assert_eq!(payload.get("provider"), Some(&json!("amp")));
        assert_eq!(
            payload.get("auth_methods"),
            Some(&json!([{ "id": "oauth", "name": "Sign in" }]))
        );
        assert_eq!(payload.get("transient"), Some(&json!(false)));
        assert_eq!(payload.get("crp_seq"), Some(&json!(7)));
        assert!(!payload.contains_key("details"));
        let serialized = Value::Object(payload.clone()).to_string();
        assert!(!serialized.contains("https://auth.example.test/start"));
        assert!(!serialized.contains("token=secret"));
        assert!(!serialized.contains("raw provider payload"));
    }

    #[test]
    fn unknown_data_channel_event_maps_to_no_events() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let mapped = crate::crp::normalize::map_crp_event(
            CrpEvent::Unknown {
                event_type: "tool.progress".to_string(),
                session_id: Some("session-1".to_string()),
                turn_id: Some("turn-1".to_string()),
                parse_error: "unknown variant `tool.progress`".to_string(),
                raw: json!({
                    "type": "tool.progress",
                    "session_id": "session-1",
                    "turn_id": "turn-1",
                    "message": "chunk"
                }),
            },
            CrpChannel::Data,
            8,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        assert!(mapped.events.is_empty());
        assert!(!mapped.done);
    }

    #[test]
    fn non_auth_session_notice_keeps_details() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let mapped = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::SessionNotice {
                session_id: "session-1".to_string(),
                turn_id: None,
                code: "provider_guard_warning".to_string(),
                severity: Some("warning".to_string()),
                message: Some("Provider memory high".to_string()),
                details: Some(json!({
                    "provider": "amp",
                    "memory_mb": 1024
                })),
                transient: Some(false),
            }),
            CrpChannel::Control,
            8,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        let payload = mapped.events[0]
            .payload_json
            .as_object()
            .expect("notice payload");
        assert_eq!(payload.get("message"), Some(&json!("Provider memory high")));
        assert_eq!(
            payload.get("details"),
            Some(&json!({
                "provider": "amp",
                "memory_mb": 1024
            }))
        );
    }
}
