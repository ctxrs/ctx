use std::io::Cursor;

use serde_json::{json, Value};

use crate::{
    decode_authority, replay_committed, validate_canonical_state, BoundaryIntent, CanonicalState,
    FxProviderError, ReplayLimits, TempFileScratch,
};

use super::support::{assistant_turn, authority, canonical_state, id, started, watermark};

fn decode_state(value: Value) -> crate::FxProviderResult<CanonicalState> {
    let state: CanonicalState = serde_json::from_value(value)?;
    validate_canonical_state(&state, ReplayLimits::default())?;
    Ok(state)
}

fn mutate_state(mutator: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
    let mut value = canonical_state(vec![], 2);
    mutator(value.as_object_mut().expect("state object"));
    value
}

fn mutate_started(mutator: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Vec<u8> {
    let original = started(1, id(1));
    let mut value: Value =
        serde_json::from_slice(&original[..original.len() - 1]).expect("started frame JSON");
    mutator(value.as_object_mut().expect("frame object"));
    let mut encoded = serde_json::to_vec(&value).expect("mutated frame encodes");
    encoded.push(b'\n');
    encoded
}

fn cold_result(log: &[u8]) -> crate::FxProviderResult<crate::ColdReplayDisposition> {
    replay_committed(
        &authority(),
        &watermark(log, 1, id(1)),
        &mut Cursor::new(log),
        BoundaryIntent::Stable,
        &TempFileScratch,
        ReplayLimits::default(),
    )
}

#[test]
fn session_ids_reject_traversal_components() {
    let oversized = "x".repeat(256);
    for invalid in [".", "..", "slash/id", "", oversized.as_str()] {
        let mut value = serde_json::to_value(authority()).expect("authority JSON");
        value["session_id"] = json!(invalid);
        assert!(decode_authority(
            &serde_json::to_vec(&value).expect("authority encodes"),
            ReplayLimits::default()
        )
        .is_err());
    }
}

#[test]
fn absolute_roots_model_language_effort_and_provider_are_strict() {
    let invalid_states = [
        mutate_state(|state| {
            state.insert("workspace_root".to_owned(), json!("relative/path"));
        }),
        mutate_state(|state| {
            state.insert(
                "workspace_root".to_owned(),
                json!(format!("/{}", "x".repeat(4096))),
            );
        }),
        mutate_state(|state| state["preferences"]["model"] = json!(" leading-space")),
        mutate_state(|state| state["preferences"]["model"] = json!("trailing-space ")),
        mutate_state(|state| state["preferences"]["model"] = json!("control\u{0007}")),
        mutate_state(|state| state["preferences"]["model"] = json!("x".repeat(1025))),
        mutate_state(|state| state["conversation_language"] = json!(" en")),
        mutate_state(|state| state["conversation_language"] = json!("x".repeat(25))),
        mutate_state(|state| state["preferences"]["effort"] = json!("not valid!")),
        mutate_state(|state| state["preferences"]["provider"] = json!("future-provider")),
    ];
    for state in invalid_states {
        assert!(decode_state(state).is_err());
    }

    for accepted in ["auto", "Adaptive", "DEFAULT", "high", "reasoning-2.5"] {
        let value = mutate_state(|state| state["preferences"]["effort"] = json!(accepted));
        decode_state(value).expect("accepted upstream effort vocabulary");
    }
}

#[test]
fn v3_protocol_objects_reject_unknown_fields_at_every_boundary() {
    let authority_unknown = {
        let mut value = serde_json::to_value(authority()).expect("authority JSON");
        value["future"] = json!(true);
        value
    };
    assert!(decode_authority(
        &serde_json::to_vec(&authority_unknown).expect("authority encodes"),
        ReplayLimits::default()
    )
    .is_err());

    let envelope_unknown = mutate_started(|frame| {
        frame.insert("future".to_owned(), json!(true));
    });
    assert!(cold_result(&envelope_unknown).is_err());

    let payload_unknown = mutate_started(|frame| {
        frame["payload"]["future"] = json!(true);
    });
    assert!(cold_result(&payload_unknown).is_err());

    let state_unknown = mutate_state(|state| {
        state.insert("future".to_owned(), json!(true));
    });
    assert!(serde_json::from_value::<CanonicalState>(state_unknown).is_err());

    let turn_unknown = mutate_state(|state| {
        state["history"] = json!([{
            "kind": "assistant",
            "user": {"text": "u", "images": []},
            "assistant": "a",
            "execution": {"schema_version": 1, "tool_steps": [], "files": []},
            "future": true
        }]);
    });
    assert!(serde_json::from_value::<CanonicalState>(turn_unknown).is_err());

    let execution_unknown = mutate_state(|state| {
        let mut turn = assistant_turn("u", "a");
        turn["execution"]["future"] = json!(true);
        state["history"] = json!([turn]);
    });
    assert!(serde_json::from_value::<CanonicalState>(execution_unknown).is_err());
}

fn result_for_schema(schema: u64) -> Value {
    let mut result = json!({
        "tool_call_id": "call-1",
        "tool_name": "shell",
        "status": "success",
        "output": "authentic output",
        "output_handle": null,
        "preview": null,
        "output_bytes": 16,
        "stored_output_bytes": 16,
        "truncated": false,
        "provider_native": false,
        "created_at_ms": 2
    });
    if schema >= 2 {
        result["permission_feedback"] = json!([]);
    }
    if schema >= 3 {
        result["committed_file_presentation"] = Value::Null;
        result["command_output_replay"] = json!({
            "kind": "available",
            "handle": "artifact-1",
            "framed_bytes": 16
        });
        result["command_process_presentation"] = json!({
            "kind": "exit_code",
            "value": 0
        });
    }
    if schema >= 4 {
        result["terminal_action_presentation"] = json!({
            "kind": "returned",
            "outcome": {"kind": "exited", "value": 0}
        });
    }
    result
}

fn state_with_execution(schema: u64) -> Value {
    canonical_state(
        vec![json!({
            "kind": "assistant",
            "user": {"text": "u", "images": []},
            "assistant": "a",
            "execution": {
                "schema_version": schema,
                "tool_steps": [{
                    "assistant": null,
                    "tool_calls": [],
                    "tool_results": [result_for_schema(schema)]
                }],
                "files": []
            }
        })],
        2,
    )
}

#[test]
fn authentic_v006_execution_schemas_and_intentional_main_v4_are_supported() {
    for schema in 1..=3 {
        decode_state(state_with_execution(schema)).expect("pinned v0.0.6 execution shape");
    }
    decode_state(state_with_execution(4)).expect("tested compatible current-main v4 shape");
    assert!(decode_state(state_with_execution(5)).is_err());

    let mut mismatched = state_with_execution(3);
    mismatched["history"][0]["execution"]["tool_steps"][0]["tool_results"][0]
        .as_object_mut()
        .expect("result object")
        .remove("command_output_replay");
    assert!(decode_state(mismatched).is_err());
}

#[test]
fn aggregate_image_and_json_amplification_limits_fail_bounded() {
    let state: CanonicalState = serde_json::from_value(canonical_state(
        vec![json!({
            "kind": "assistant",
            "user": {
                "text": "u",
                "images": [
                    {"id": 1, "path": "/a", "media_type": "image/png"},
                    {"id": 2, "path": "/b", "media_type": "image/png"}
                ]
            },
            "assistant": "a",
            "execution": {"schema_version": 1, "tool_steps": [], "files": []}
        })],
        2,
    ))
    .expect("amplification state parses");
    let limits = ReplayLimits {
        max_images: 1,
        ..ReplayLimits::default()
    };
    assert!(matches!(
        validate_canonical_state(&state, limits),
        Err(FxProviderError::LimitExceeded {
            resource: "images",
            actual: 2,
            maximum: 1
        })
    ));

    let deeply_nested = mutate_started(|frame| {
        frame.insert("future".to_owned(), json!([[[[[true]]]]]));
    });
    let limits = ReplayLimits {
        max_json_depth: 3,
        ..ReplayLimits::default()
    };
    let result = replay_committed(
        &authority(),
        &watermark(&deeply_nested, 1, id(1)),
        &mut Cursor::new(&deeply_nested),
        BoundaryIntent::Stable,
        &TempFileScratch,
        limits,
    );
    assert!(matches!(
        result,
        Err(FxProviderError::LimitExceeded {
            resource: "JSON depth",
            ..
        })
    ));
}

#[test]
fn summary_permission_feedback_requires_the_complete_root_shape() {
    let invalid = mutate_state(|state| {
        state["history"] = json!([{
            "kind": "compacted_summary",
            "summary": "s",
            "removed_turn_count": 1,
            "compaction_count": 1,
            "permission_feedback": [],
            "permission_feedback_complete": true
        }]);
    });
    assert!(decode_state(invalid).is_err());

    let valid = mutate_state(|state| {
        state["history"] = json!([{
            "kind": "compacted_summary",
            "summary": "s",
            "removed_turn_count": 1,
            "compaction_count": 1,
            "root_user_messages": [],
            "root_user_messages_complete": true,
            "permission_feedback": [],
            "permission_feedback_complete": true
        }]);
    });
    decode_state(valid).expect("complete pinned summary shape");
}

#[test]
fn committed_event_byte_and_count_limits_are_aggregate() {
    let log = started(1, id(1));
    let bytes_limit = ReplayLimits {
        max_committed_bytes: log.len() as u64 - 1,
        ..ReplayLimits::default()
    };
    assert!(matches!(
        replay_committed(
            &authority(),
            &watermark(&log, 1, id(1)),
            &mut Cursor::new(&log),
            BoundaryIntent::Stable,
            &TempFileScratch,
            bytes_limit,
        ),
        Err(FxProviderError::LimitExceeded {
            resource: "committed event bytes",
            ..
        })
    ));

    let event_limit = ReplayLimits {
        max_events: 0,
        ..ReplayLimits::default()
    };
    assert!(matches!(
        replay_committed(
            &authority(),
            &watermark(&log, 1, id(1)),
            &mut Cursor::new(&log),
            BoundaryIntent::Stable,
            &TempFileScratch,
            event_limit,
        ),
        Err(FxProviderError::LimitExceeded {
            resource: "committed events",
            ..
        })
    ));
}
