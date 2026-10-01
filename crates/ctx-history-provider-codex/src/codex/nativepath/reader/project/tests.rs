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
        facts: vec![ctx_history_core::ProviderDeclaredFact {
            kind: ctx_history_core::LiteralFactKind::File,
            value: "src/witness.rs".to_owned(),
        }],
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

    let mut authority = super::super::CodexTerminalAuthority::default();
    for terminals in 0..=2 {
        let mut projected = row.clone();
        authority.settle_invocation_linkage(&mut projected);
        let mut expected = row.clone();
        if terminals == 2 {
            expected.activity.as_mut().unwrap().provider_call_id = None;
            expected.activity.as_mut().unwrap().invocation = None;
        }
        assert_eq!(
            projected, expected,
            "pending/unique invocations keep linkage; duplicates retain native content and facts"
        );
        authority.observe_record(
            &serde_json::to_vec(&json!({
                "type":"response_item", "payload":{
                    "type":"function_call_output", "call_id":call_id, "output":"result"
                }
            }))
            .unwrap(),
        );
    }
    authority.invalidate_linkage();
    let mut projected = row.clone();
    authority.settle_invocation_linkage(&mut projected);
    let mut expected = row;
    expected.activity.as_mut().unwrap().provider_call_id = None;
    expected.activity.as_mut().unwrap().invocation = None;
    assert_eq!(
        projected, expected,
        "unknown native selectors may conceal a duplicate; this is distinct from no terminal"
    );
}

#[test]
fn patch_lifecycle_notification_preserves_invocation_but_duplicate_outputs_abstain() {
    use super::super::{
        CodexCatalogSource, CodexContextMutation, CodexFileObservation, CodexNativeScanner,
        CodexPhysicalRecordContext,
    };

    #[derive(Clone)]
    struct NoLookup;
    impl crate::provider::source_backed::BaseEventLookup for NoLookup {
        type Error = std::convert::Infallible;
        fn contains(&self, _: uuid::Uuid) -> Result<bool, Self::Error> {
            unreachable!("cold projection has no base lookup")
        }
    }

    for notification_first in [false, true] {
        for duplicate in [None, Some("identical"), Some("conflicting"), Some("failed")] {
            let patch =
                "*** Begin Patch\n*** Update File: src/main.rs\n@@\n- old\n+ new\n*** End Patch";
            let call = json!({"type":"response_item", "payload":{
                "type":"custom_tool_call", "name":"apply_patch", "input":patch,
                "call_id":"patch", "status":"completed"
            }});
            let output = json!({"type":"response_item", "payload":{
                "type":"custom_tool_call_output", "call_id":"patch",
                "output":"Success. Updated files:\nM src/main.rs\n"
            }});
            let notification = json!({"type":"event_msg", "payload":{
                "type":"patch_apply_end", "call_id":"patch", "status":"success",
                "success":true, "stdout":"patched src/main.rs", "stderr":"", "duration_ms":120
            }});
            let mut records = vec![
                json!({"type":"session_meta", "payload":{
                    "id":"patch-session", "timestamp":"2026-06-24T01:00:00Z", "source":"cli"
                }}),
                call,
            ];
            if notification_first {
                records.extend([notification, output.clone()]);
            } else {
                records.extend([output.clone(), notification]);
            }
            if let Some(kind) = duplicate {
                let mut second = output.clone();
                if kind == "conflicting" {
                    second["payload"]["output"] = json!("different result");
                } else if kind == "failed" {
                    second["payload"]["output"] = json!("patch failed");
                    second["payload"]["status"] = json!("failed");
                }
                records.push(second);
            }
            let source = CodexCatalogSource {
                source_path: "patch-session.jsonl".into(),
                source_root_lineage: None,
                catalog_observation: CodexFileObservation {
                    len: 0,
                    modified_at_ms: 0,
                    stable_token: None,
                    change_token: [0; 32],
                },
                carried_jsonl_observation: None,
                catalog_prefix_sha256: None,
                catalog_native_session_id: Some("patch-session".to_owned()),
                authority_root: None,
                authority_relative_path: None,
            };
            let mut scanner = CodexNativeScanner::new_semantic(source, None::<NoLookup>).unwrap();
            let raw: Vec<_> = records
                .iter()
                .map(|record| serde_json::to_vec(record).unwrap())
                .collect();
            for bytes in &raw {
                scanner.terminal_authority.observe_record(bytes);
            }
            for (index, bytes) in raw.iter().enumerate() {
                let projection = scanner
                    .process_record(
                        bytes,
                        CodexPhysicalRecordContext {
                            raw_ordinal: index as u64,
                            start_byte: 0,
                            end_byte: bytes.len() as u64,
                        },
                    )
                    .unwrap();
                if index == 0 {
                    assert!(projection.context_mutation.is_none());
                    continue;
                }
                let mutation = projection
                    .context_mutation
                    .expect("retain every native activity");
                let CodexContextMutation::SourceBackedRow { ref row, .. } = mutation;
                assert_eq!(
                    row.structured_content.as_ref(),
                    Some(&records[index]["payload"])
                );
                assert_eq!(row.provider_event_identity.as_ref().unwrap().value, "patch");
                let is_notification = records[index]["payload"]["type"] == "patch_apply_end";
                if is_notification || duplicate.is_some() {
                    assert!(row.activity.as_ref().is_none_or(|activity| {
                        activity.provider_call_id.is_none()
                            && activity.invocation.is_none()
                            && activity.result.is_none()
                    }));
                } else {
                    let activity = row.activity.as_ref().unwrap();
                    assert_eq!(
                        activity.provider_call_id,
                        Some(TypedKey::Utf8("patch".to_owned()))
                    );
                    if index == 1 {
                        assert_eq!(
                            activity.invocation.as_ref().unwrap().arguments,
                            ActivityJsonCapture::Present {
                                value: json!(patch)
                            }
                        );
                    } else {
                        assert_eq!(
                            activity.result.as_ref().unwrap().text,
                            ctx_history_core::ActivityTextCapture::Present {
                                value: "Success. Updated files:\nM src/main.rs\n".to_owned()
                            }
                        );
                    }
                }
                if index == 1 {
                    assert!(row
                        .activity
                        .as_ref()
                        .unwrap()
                        .facts
                        .iter()
                        .any(|fact| fact.kind == ctx_history_core::LiteralFactKind::File
                            && fact.value == "src/main.rs"));
                }
                scanner
                    .apply_context_mutation_inner(mutation, false)
                    .unwrap();
                if is_notification {
                    assert_eq!(
                        scanner.pending_calls.contains_key("patch"),
                        notification_first,
                        "notification must not consume the pending invocation"
                    );
                }
            }
        }
    }
}
