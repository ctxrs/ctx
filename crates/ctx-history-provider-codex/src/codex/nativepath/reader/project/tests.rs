use chrono::{DateTime, Utc};
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, CoreActivity, CoreDiscoveryExclusion, EventRole,
    EventType, ProviderNativeSessionRelationship, TypedKey, CORE_ACTIVITY_REVISION,
};
use serde_json::json;

use super::{pending_call_for_row, pending_call_origin, valid_local_turn_boundary};
use crate::provider::codex::nativepath::{
    checkpoint::CodexPendingCallOriginV0,
    rows::{
        CodexCoreRecordDraft, CodexProviderEventIdentityKindV0, CodexProviderEventIdentityV0,
        CodexSessionRow,
    },
};

fn forked_owner() -> CodexSessionRow {
    CodexSessionRow {
        native_session_id: "019fb100-0000-7000-8000-000000000002".to_owned(),
        parent_native_session_id: Some("019fb100-0000-7000-8000-000000000001".to_owned()),
        root_native_session_id: None,
        session_relationship: Some(ProviderNativeSessionRelationship::Forked),
        started_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        cwd: None,
        originator: None,
        cli_version: None,
        source_kind: None,
        external_agent_id: None,
        role_hint: None,
        model_provider: None,
        git: None,
    }
}

#[test]
fn uuid_v7_boundary_uses_only_strictly_later_embedded_timestamps() {
    let session = "019fb000-0000-7000-8000-0000000000ff";
    let later = "019fb000-0001-7000-8000-000000000000";
    let same_time_higher_randomness = "019fb000-0000-7fff-bfff-ffffffffffff";

    assert!(valid_local_turn_boundary(session, later));
    assert!(!valid_local_turn_boundary(
        session,
        same_time_higher_randomness
    ));
    assert!(!valid_local_turn_boundary(
        session,
        "550e8400-e29b-41d4-a716-446655440000"
    ));
    assert!(!valid_local_turn_boundary(
        "550e8400-e29b-41d4-a716-446655440000",
        later
    ));
}

#[test]
fn cold_forked_call_before_local_turn_is_copied_from_exact_parent() {
    let owner = forked_owner();
    assert_eq!(
        pending_call_origin(&owner, false),
        CodexPendingCallOriginV0::CopiedFromAncestor {
            ancestor_native_session_id: owner.parent_native_session_id.unwrap(),
        }
    );
}

#[test]
fn post_local_turn_forked_call_is_not_a_copy_near_miss() {
    assert_eq!(
        pending_call_origin(&forked_owner(), true),
        CodexPendingCallOriginV0::CurrentSession
    );
}

#[test]
fn pending_call_carries_retrieval_exclusion_without_changing_activity() {
    let call_id = "pending-ctx-retrieval";
    let activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8(call_id).unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: "exec_command".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({"cmd": "ctx search pending"}),
            },
            started_at_unix_ms: Some(1),
        }),
        result: None,
        facts: Vec::new(),
    };
    let row = CodexCoreRecordDraft {
        raw_ordinal: 7,
        provider_event_identity: Some(CodexProviderEventIdentityV0 {
            kind: CodexProviderEventIdentityKindV0::CallId,
            value: call_id.to_owned(),
        }),
        provider_event_copy: None,
        occurred_at: DateTime::<Utc>::from_timestamp_millis(1).unwrap(),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        session_cwd: None,
        lexical_body: "retained invocation".to_owned(),
        structured_content: Some(json!({"call_id": call_id})),
        discovery_exclusion: Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
        activity: Some(activity.clone()),
        optional_argument_facts: Vec::new(),
    };

    let (_, pending) = pending_call_for_row(&forked_owner(), true, 7, &row).unwrap();
    assert_eq!(pending.raw_ordinal, 7);
    assert_eq!(
        pending.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(row.activity, Some(activity));
}
