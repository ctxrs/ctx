use serde_json::{json, Value};

use super::support::{
    assistant_turn, authority, canonical_state, cold, frame, history_payload, id, started,
    watermark,
};
use crate::{replay_suffix, BoundaryIntent, RecoveryCheckpoint, ReplayLimits, SuffixDisposition};

fn checkpoint(version: u8) -> Value {
    let mut value = json!({
        "version": version, "turn_id": 1,
        "user": {"text": "saved request", "images": []},
        "assistant_source": "saved partial",
        "execution": {"schema_version": 3, "tool_steps": [], "files": []},
        "cause": "response_interrupted", "action": "paused", "tool_state": "uncertain",
        "requested_fast_mode": false, "fast_mode": false,
        "max_provider_attempts": 3, "consumed_provider_attempts": 0,
        "outstanding_reservation": false
    });
    if version == 1 {
        value["route_model"] = json!("test/model");
    } else {
        value["authority"] = json!({"provider": "gateway", "model": "test/model"});
    }
    value
}

fn validate(value: Value) -> bool {
    serde_json::from_value::<RecoveryCheckpoint>(value)
        .is_ok_and(|checkpoint| crate::limits::validate_recovery_checkpoint(&checkpoint).is_ok())
}

#[test]
fn recovery_versions_accept_only_their_native_route_shape() {
    for version in [1, 2] {
        let value = checkpoint(version);
        assert!(validate(value.clone()));
        for (field, invalid) in [
            ("version", json!(3)),
            ("action", json!("future_action")),
            ("max_provider_attempts", json!(0)),
            ("consumed_provider_attempts", json!(4)),
        ] {
            let mut bad = value.clone();
            bad[field] = invalid;
            assert!(!validate(bad), "accepted invalid {field}");
        }
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(if version == 1 {
            "route_model"
        } else {
            "authority"
        });
        assert!(!validate(missing));
        let mut mixed = value.clone();
        mixed["route_model"] = json!("other/model");
        mixed["authority"] = json!({"provider": "gateway", "model": "test/model"});
        assert!(!validate(mixed));
        for (cause, action) in [
            ("provider_stream_timeout", "waiting_for_connectivity"),
            ("response_interrupted", "paused"),
        ] {
            let mut known = value.clone();
            known["cause"] = json!(cause);
            known["action"] = json!(action);
            assert!(validate(known));
        }
    }
    let mut value = checkpoint(1);
    value["route_provider"] = json!("codex");
    assert!(validate(value.clone()));
    value["route_provider"] = json!("unknown");
    assert!(!validate(value));
    let mut value = checkpoint(2);
    value["route_provider"] = json!("gateway");
    assert!(!validate(value));
}

#[test]
fn recovery_checkpoints_preserve_history_in_cold_and_suffix_replay() {
    for version in [1, 2] {
        let prefix = started(1, id(1));
        let initial = cold(&prefix, &watermark(&prefix, 1, id(1)));
        let mut suffix = frame(
            id(0x11),
            2,
            id(2),
            2,
            "recovery_checkpoint_set",
            json!({"checkpoint": checkpoint(version)}),
        );
        suffix.extend(frame(
            id(0x11),
            3,
            id(3),
            3,
            "history_turn_committed",
            history_payload(assistant_turn("retained request", "retained answer")),
        ));
        let mut log = prefix.clone();
        log.extend(&suffix);
        let commit = watermark(&log, 3, id(3));
        let replay = cold(&log, &commit);
        assert_eq!(replay.state.history.len(), 1);
        let result = replay_suffix(
            &authority(),
            &initial.checkpoint,
            &commit,
            &mut std::io::Cursor::new(suffix),
            BoundaryIntent::Stable,
            ReplayLimits::default(),
        )
        .unwrap();
        let SuffixDisposition::AppendNewTurns(replay) = result else {
            panic!("expected append")
        };
        assert_eq!(replay.new_turns.len(), 1);
    }
}

#[test]
fn legacy_permission_state_keeps_validation_and_version() {
    for version in [1, 2] {
        let mut value = canonical_state(vec![], 2);
        value["permission_state"]["schema_version"] = json!(version);
        let state: crate::CanonicalState = serde_json::from_value(value.clone()).unwrap();
        crate::validate_canonical_state(&state, ReplayLimits::default()).unwrap();
        assert_eq!(state.permission_state.schema_version, version);
        value["permission_state"]["next_generation"] = json!(0);
        let state = serde_json::from_value(value).unwrap();
        assert!(crate::validate_canonical_state(&state, ReplayLimits::default()).is_err());
    }
    let mut value = canonical_state(vec![], 2);
    value["permission_state"]["schema_version"] = json!(3);
    let state = serde_json::from_value(value).unwrap();
    assert!(crate::validate_canonical_state(&state, ReplayLimits::default()).is_err());
}

#[test]
fn upstream_history_only_recovery_routes_accept_exact_v2_v3_v4_shapes() {
    for version in [2, 3, 4] {
        let mut value = checkpoint(1);
        value["version"] = json!(version);
        value["delivery"] = json!("possibly_sent");
        let mut route = json!({"connection_id":"vercel","adapter_kind":"vercel_ai_gateway","permission_review_model_id":"review"});
        if version >= 3 {
            route["vision_model_id"] = json!("vision");
            route["subagent_model_id"] = json!("child");
        }
        if version == 4 {
            route["version"] = json!(1);
            route["endpoint"] = json!("https://example.invalid");
            route["protocol"] = json!("vercel_ai_gateway");
            route["credential_ref"] = json!("fixture-reference");
        }
        value["route_identity"] = route;
        assert!(validate(value.clone()), "version {version}");
        value["delivery"] = json!("definitely_unsent");
        assert!(validate(value.clone()));
        let mut bad = value.clone();
        bad["route_identity"]["unexpected"] = json!("field");
        assert!(!validate(bad));
        let mut bad = value.clone();
        bad["delivery"] = json!("future");
        assert!(!validate(bad));
        let mut bad = value.clone();
        bad["route_identity"]["connection_id"] = json!("other");
        assert!(!validate(bad));
        let mut bad = value.clone();
        bad["authority"] = json!({"provider":"gateway","model":"mixed"});
        assert!(!validate(bad));
    }
}
