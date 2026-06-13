use serde_json::{json, Value};

use ctx_core::models::SessionEventType;

use crate::events::NormalizedEvent;

use super::super::protocol::{CrpTurnStatus, KnownCrpEvent};
use super::MappedCrpEvent;

pub(super) fn map_known_event(
    event: KnownCrpEvent,
    crp_channel: Option<&str>,
    seq: u64,
) -> MappedCrpEvent {
    match event {
        KnownCrpEvent::SessionOpened {
            session_id,
            provider_session_id,
            supports_session_status,
            commands,
            slash_commands,
            models,
            current_model_id,
            agents,
            output_style,
            available_output_styles,
            skills,
            plugins,
            tools,
            permission_mode,
            mcp_servers,
            account,
            fast_mode_state,
        } => {
            let mut payload = serde_json::Map::new();
            payload.insert("session_id".to_string(), json!(session_id));
            if let Some(provider_session_id) = provider_session_id {
                payload.insert(
                    "provider_session_id".to_string(),
                    json!(provider_session_id),
                );
            }
            if let Some(supports_session_status) = supports_session_status {
                payload.insert(
                    "supports_session_status".to_string(),
                    json!(supports_session_status),
                );
            }
            if let Some(commands) = commands {
                payload.insert("commands".to_string(), commands);
            }
            if let Some(slash_commands) = slash_commands {
                payload.insert("slash_commands".to_string(), json!(slash_commands));
            }
            if let Some(models) = models {
                payload.insert("models".to_string(), models);
            }
            if let Some(current_model_id) = current_model_id {
                payload.insert("current_model_id".to_string(), json!(current_model_id));
            }
            if let Some(agents) = agents {
                payload.insert("agents".to_string(), agents);
            }
            if let Some(output_style) = output_style {
                payload.insert("output_style".to_string(), json!(output_style));
            }
            if let Some(available_output_styles) = available_output_styles {
                payload.insert(
                    "available_output_styles".to_string(),
                    json!(available_output_styles),
                );
            }
            if let Some(skills) = skills {
                payload.insert("skills".to_string(), json!(skills));
            }
            if let Some(plugins) = plugins {
                payload.insert("plugins".to_string(), plugins);
            }
            if let Some(tools) = tools {
                payload.insert("tools".to_string(), json!(tools));
            }
            if let Some(permission_mode) = permission_mode {
                payload.insert("permission_mode".to_string(), json!(permission_mode));
            }
            if let Some(mcp_servers) = mcp_servers {
                payload.insert("mcp_servers".to_string(), mcp_servers);
            }
            if let Some(account) = account {
                payload.insert("account".to_string(), *account);
            }
            if let Some(fast_mode_state) = fast_mode_state {
                payload.insert("fast_mode_state".to_string(), json!(fast_mode_state));
            }
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type: SessionEventType::Init,
                    payload_json: Value::Object(payload),
                }],
                done: false,
            }
        }
        KnownCrpEvent::TurnStarted {
            session_id,
            turn_id,
        } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::TurnStarted,
                payload_json: json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "crp_seq": seq,
                    "crp_channel": crp_channel,
                }),
            }],
            done: false,
        },
        KnownCrpEvent::TurnContextWindowUpdated { context_window, .. } => MappedCrpEvent {
            events: vec![NormalizedEvent {
                event_type: SessionEventType::ContextWindowUpdate,
                payload_json: json!({
                    "context_window": context_window,
                    "crp_seq": seq,
                }),
            }],
            done: false,
        },
        KnownCrpEvent::TurnCompleted {
            status,
            context_window,
            error,
            ..
        } => {
            let (event_type, payload) = match status {
                CrpTurnStatus::Success => {
                    let mut payload = serde_json::Map::new();
                    payload.insert("status".to_string(), json!("completed"));
                    payload.insert("crp_seq".to_string(), json!(seq));
                    if let Some(context_window) = context_window {
                        payload.insert("context_window".to_string(), context_window);
                    }
                    (SessionEventType::Done, Value::Object(payload))
                }
                CrpTurnStatus::Error => {
                    let message = error
                        .as_ref()
                        .map(|err| err.message.clone())
                        .unwrap_or_else(|| "crp_turn_error".to_string());
                    let kind = error.as_ref().and_then(|err| err.kind.clone());
                    let details = error.as_ref().and_then(|err| err.details.clone());
                    let mut payload = serde_json::Map::new();
                    payload.insert("status".to_string(), json!("failed"));
                    payload.insert("message".to_string(), json!(message));
                    payload.insert("reason".to_string(), json!("crp_turn_error"));
                    if let Some(kind) = kind {
                        payload.insert("kind".to_string(), json!(kind));
                    }
                    if let Some(details) = details {
                        payload.insert("details".to_string(), json!(details));
                    }
                    payload.insert("crp_seq".to_string(), json!(seq));
                    (SessionEventType::TurnFinished, Value::Object(payload))
                }
                CrpTurnStatus::Canceled | CrpTurnStatus::Interrupted => (
                    SessionEventType::TurnInterrupted,
                    json!({"reason": "crp_turn_interrupted", "crp_seq": seq}),
                ),
            };
            MappedCrpEvent {
                events: vec![NormalizedEvent {
                    event_type,
                    payload_json: payload,
                }],
                done: true,
            }
        }
        KnownCrpEvent::ModelsList { .. } => MappedCrpEvent {
            events: Vec::new(),
            done: false,
        },
        _ => unreachable!("unexpected session normalize event"),
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
    fn turn_started_maps_to_canonical_lifecycle_event() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let mapped = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::TurnStarted {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
            }),
            CrpChannel::Control,
            42,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        assert_eq!(mapped.events.len(), 1);
        assert!(!mapped.done);
        assert!(matches!(
            mapped.events[0].event_type,
            SessionEventType::TurnStarted
        ));
        assert_eq!(
            mapped.events[0].payload_json,
            json!({
                "session_id": "session-1",
                "turn_id": "turn-1",
                "crp_seq": 42,
                "crp_channel": null,
            })
        );
    }

    #[test]
    fn session_opened_preserves_claude_supported_command_metadata() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let mapped = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::SessionOpened {
                session_id: "session-1".to_string(),
                provider_session_id: Some("provider-session-1".to_string()),
                supports_session_status: Some(true),
                commands: Some(json!([
                    {
                        "name": "compact",
                        "description": "Summarize conversation to save context",
                        "argument_hint": "<focus>"
                    }
                ])),
                slash_commands: Some(vec!["compact".to_string(), "review".to_string()]),
                models: Some(json!([
                    {
                        "id": "sonnet",
                        "name": "Sonnet"
                    }
                ])),
                current_model_id: Some("sonnet".to_string()),
                agents: Some(json!([
                    {
                        "name": "Explore",
                        "description": "Research the repo"
                    }
                ])),
                output_style: Some("default".to_string()),
                available_output_styles: Some(vec!["default".to_string(), "brief".to_string()]),
                skills: Some(vec!["simplify".to_string()]),
                plugins: Some(json!([
                    {
                        "name": "plugin-a",
                        "path": "/tmp/plugin-a"
                    }
                ])),
                tools: Some(vec!["Read".to_string(), "Write".to_string()]),
                permission_mode: Some("default".to_string()),
                mcp_servers: Some(json!([{ "name": "github", "status": "connected" }])),
                account: Some(Box::new(json!({ "email": "dev@example.com" }))),
                fast_mode_state: Some("off".to_string()),
            }),
            CrpChannel::Control,
            1,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        assert_eq!(mapped.events.len(), 1);
        assert!(matches!(
            mapped.events[0].event_type,
            SessionEventType::Init
        ));
        let payload = &mapped.events[0].payload_json;
        assert_eq!(payload.get("session_id"), Some(&json!("session-1")));
        assert_eq!(
            payload.get("provider_session_id"),
            Some(&json!("provider-session-1"))
        );
        assert_eq!(payload.get("supports_session_status"), Some(&json!(true)));
        assert_eq!(payload.pointer("/commands/0/name"), Some(&json!("compact")));
        assert_eq!(
            payload.pointer("/commands/0/description"),
            Some(&json!("Summarize conversation to save context"))
        );
        assert_eq!(
            payload.get("slash_commands"),
            Some(&json!(["compact", "review"]))
        );
        assert_eq!(payload.get("current_model_id"), Some(&json!("sonnet")));
        assert_eq!(payload.get("output_style"), Some(&json!("default")));
        assert_eq!(
            payload.get("available_output_styles"),
            Some(&json!(["default", "brief"]))
        );
        assert_eq!(payload.get("skills"), Some(&json!(["simplify"])));
        assert_eq!(payload.get("permission_mode"), Some(&json!("default")));
        assert_eq!(payload.get("fast_mode_state"), Some(&json!("off")));
    }

    #[test]
    fn context_window_update_maps_to_partial_session_event() {
        let mut tool_output_cache: HashMap<String, String> = HashMap::new();
        let mut tool_input_cache: HashMap<String, CachedToolInput> = HashMap::new();

        let metrics = json!({
            "context_tokens_estimate": 4200,
            "context_window_tokens": 128000,
            "remaining_tokens_estimate": 123800,
            "remaining_fraction": 0.9671875,
        });
        let mapped = crate::crp::normalize::map_crp_event(
            known(KnownCrpEvent::TurnContextWindowUpdated {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                context_window: metrics.clone(),
            }),
            CrpChannel::Control,
            9,
            &mut tool_output_cache,
            &mut tool_input_cache,
        );

        assert_eq!(mapped.events.len(), 1);
        assert!(!mapped.done);
        assert!(matches!(
            mapped.events[0].event_type,
            SessionEventType::ContextWindowUpdate
        ));
        assert_eq!(
            mapped.events[0].payload_json,
            json!({
                "context_window": metrics,
                "crp_seq": 9,
            })
        );
    }
}
