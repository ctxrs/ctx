use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash as core_compute_payload_hash, core_record_leaf_sha256, AgentScope,
    CaptureProvider, CoreRecord, EventType, SessionStatus, TypedKey, MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde_json::{json, Value};

use super::*;
use crate::test_support::NativeJsonlTestRuntime;

fn adapter(provider: CaptureProvider) -> DirectJsonlFamilyAdapter<NativeJsonlTestRuntime> {
    match provider {
        CaptureProvider::Antigravity => {
            super::super::antigravity_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::Tabnine => {
            super::super::tabnine_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::FactoryAiDroid => {
            super::super::factory_droid_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::GrokBuild => {
            super::super::grok_build_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::Qoder => {
            super::super::qoder_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::CopilotCli => {
            super::super::copilot_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        CaptureProvider::QwenCode => {
            super::super::qwen_code_source_backed_adapter::<NativeJsonlTestRuntime>()
        }
        other => panic!("unsupported direct JSONL test provider: {other:?}"),
    }
}

fn session(provider: CaptureProvider) -> DirectJsonlSession {
    let native_session_id = format!("{}-result-contract", provider.as_str());
    DirectJsonlSession {
        provider_session_id: native_session_id.clone(),
        native_session_id,
        parent_provider_session_id: None,
        root_provider_session_id: None,
        external_agent_id: None,
        agent_scope: Some(AgentScope::Primary),
        session_relationship: None,
        status: SessionStatus::Imported,
        started_at: DateTime::<Utc>::UNIX_EPOCH,
        ended_at: None,
        cwd: None,
        metadata: json!({}),
    }
}

fn fallback_identities(
    adapter: DirectJsonlFamilyAdapter<NativeJsonlTestRuntime>,
    source: &SourceKey,
    session_id: StableEntityId,
) -> FallbackEventIdentityState<crate::test_support::NativeJsonlTestLookup, CaptureError> {
    FallbackEventIdentityState::new(
        source.clone(),
        session_id,
        "direct-jsonl-event",
        format!("{}.direct-jsonl-fallback", adapter.provider.as_str()),
        DIRECT_JSONL_EVENT_IDENTITY_REVISION,
        JsonlFamilyProjectionMode::Cold.into(),
        None,
    )
    .unwrap()
}

fn project(provider: CaptureProvider, value: &Value) -> (Vec<CoreRecord>, u64) {
    let adapter = adapter(provider);
    let session = session(provider);
    let (source, session_id) = adapter
        .session_identity(&session.native_session_id)
        .unwrap();
    let direct = DirectJsonlProjector::new(
        provider,
        adapter.source_format,
        Path::new("direct-jsonl-result-contract.jsonl"),
        None,
        DateTime::<Utc>::UNIX_EPOCH,
        Some(session.clone()),
    )
    .unwrap();
    let mut projector = DirectJsonlFamilyProjector {
        adapter,
        fallback_identities: fallback_identities(adapter, &source, session_id),
        source,
        bound_session: session,
        session_id,
        projector: direct,
        rejected_records: 0,
        record_rejections: SourceBackedRecordRejectionDrafts::default(),
        accepted_event_ids: HashSet::new(),
        append_base_event_lookup: None,
        source_selector: "direct-jsonl-result-contract.jsonl".to_owned(),
    };
    let encoded = serde_json::to_vec(value).unwrap();
    let mut records = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    JsonlFamilyProjector::project(
        &mut projector,
        JsonlRecordRef::for_test(&encoded, 7),
        &mut worker,
        &mut |record| {
            records.push(record);
            Ok(())
        },
    )
    .unwrap();
    (records, projector.rejected_records())
}

fn native_subrecord_index(record: &CoreRecord) -> u64 {
    let Some(TypedKey::Composite(parts)) = record.native_event_id.as_ref() else {
        panic!("direct JSONL result has no composite native event identity");
    };
    let Some(TypedKey::U64(index)) = parts.get(1) else {
        panic!("direct JSONL result identity has no subrecord index");
    };
    *index
}

fn project_all(provider: CaptureProvider, values: &[Value]) -> (Vec<CoreRecord>, u64) {
    let adapter = adapter(provider);
    let session = session(provider);
    let source_path = "direct-jsonl-identity-contract.jsonl";
    let (source, session_id) = adapter
        .session_identity(&session.native_session_id)
        .unwrap();
    let direct = DirectJsonlProjector::new(
        provider,
        adapter.source_format,
        Path::new(source_path),
        None,
        DateTime::<Utc>::UNIX_EPOCH,
        Some(session.clone()),
    )
    .unwrap();
    let mut projector = DirectJsonlFamilyProjector {
        adapter,
        fallback_identities: fallback_identities(adapter, &source, session_id),
        source,
        bound_session: session,
        session_id,
        projector: direct,
        rejected_records: 0,
        record_rejections: SourceBackedRecordRejectionDrafts::default(),
        accepted_event_ids: HashSet::new(),
        append_base_event_lookup: None,
        source_selector: source_path.to_owned(),
    };
    let mut records = Vec::new();
    let mut worker = JsonlFamilyWorkerContext::default();
    for (ordinal, value) in values.iter().enumerate() {
        let encoded = serde_json::to_vec(value).unwrap();
        JsonlFamilyProjector::project(
            &mut projector,
            JsonlRecordRef::for_test(&encoded, ordinal as u64),
            &mut worker,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        )
        .unwrap();
    }
    (records, projector.rejected_records())
}

fn event_ids_by_body(records: &[CoreRecord]) -> BTreeMap<String, String> {
    records
        .iter()
        .map(|record| {
            (
                record.content.normalized_body.clone().unwrap(),
                record.event_id.to_string(),
            )
        })
        .collect()
}

#[test]
fn tabnine_fallback_ids_ignore_earlier_position_changes() {
    let event = |body: &str| json!({"type": "user", "content": body});
    let baseline_values = [event("anchor"), event("target"), event("suffix")];
    let (baseline, rejected) = project_all(CaptureProvider::Tabnine, &baseline_values);
    assert_eq!(rejected, 0);
    let baseline = event_ids_by_body(&baseline);

    let (inserted, rejected) = project_all(
        CaptureProvider::Tabnine,
        &[
            event("inserted"),
            event("anchor"),
            event("target"),
            event("suffix"),
        ],
    );
    assert_eq!(rejected, 0);
    let inserted = event_ids_by_body(&inserted);
    for body in ["anchor", "target", "suffix"] {
        assert_eq!(inserted.get(body), baseline.get(body));
    }

    let (deleted, rejected) = project_all(
        CaptureProvider::Tabnine,
        &[event("target"), event("suffix")],
    );
    assert_eq!(rejected, 0);
    let deleted = event_ids_by_body(&deleted);
    assert_eq!(deleted.get("target"), baseline.get("target"));
    assert_eq!(deleted.get("suffix"), baseline.get("suffix"));

    let (rewritten, rejected) = project_all(
        CaptureProvider::Tabnine,
        &[event("anchor"), event("rewritten"), event("suffix")],
    );
    assert_eq!(rejected, 0);
    let rewritten = event_ids_by_body(&rewritten);
    assert_eq!(rewritten.get("anchor"), baseline.get("anchor"));
    assert_eq!(rewritten.get("suffix"), baseline.get("suffix"));
    assert_ne!(rewritten.get("rewritten"), baseline.get("target"));
}

#[test]
fn tabnine_extracted_payload_hash_matches_core_fnv_authority() {
    let native_value = json!({"type": "user", "content": "hash parity body"});
    let extracted_payload = json!({
        "event_type": EventType::Message.as_str(),
        "role": ctx_history_core::EventRole::User.as_str(),
        "native_record_id": Value::Null,
        "stable_retry_discriminator": Value::Null,
        "sub_ordinal": 0,
        "lexical_text": "hash parity body",
        "native_value": native_value,
    });
    let expected = core_compute_payload_hash(&extracted_payload).unwrap();
    let actual = crate::compute_payload_hash(&extracted_payload).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(actual, "fnv1a64:835cd5ad54c53eab");
}

#[test]
fn tabnine_no_native_id_fixture_keeps_exact_fallback_event_ids() {
    let event = |body: &str| json!({"type": "user", "content": body});
    let (records, rejected) = project_all(
        CaptureProvider::Tabnine,
        &[event("anchor"), event("target"), event("suffix")],
    );
    assert_eq!(rejected, 0);

    let actual = event_ids_by_body(&records);
    let expected = BTreeMap::from([
        (
            "anchor".to_owned(),
            "63877b48-f871-876c-8cd0-9157329f5741".to_owned(),
        ),
        (
            "target".to_owned(),
            "934c46b5-fde3-850d-9fea-6da6775a5bf5".to_owned(),
        ),
        (
            "suffix".to_owned(),
            "5b2d45cf-a35d-8a4c-8ef2-19deeff44e04".to_owned(),
        ),
    ]);

    assert_eq!(actual, expected);
}

#[test]
fn direct_provider_revision_matrix_matches_the_neutral_projection_and_identity_inputs() {
    let parser_cases = [
        (
            CaptureProvider::Antigravity,
            "direct-native-jsonl-parser-v6-optional-activity-admission",
        ),
        (
            CaptureProvider::CopilotCli,
            super::copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION,
        ),
        (
            CaptureProvider::FactoryAiDroid,
            "direct-native-jsonl-parser-v6-optional-activity-admission",
        ),
        (
            CaptureProvider::GrokBuild,
            "direct-native-jsonl-parser-v8-grok-closed-content-admission",
        ),
        (
            CaptureProvider::Qoder,
            "direct-native-jsonl-parser-v6-optional-activity-admission",
        ),
        (
            CaptureProvider::QwenCode,
            "direct-native-jsonl-parser-v6-optional-activity-admission",
        ),
        (
            CaptureProvider::Tabnine,
            "direct-native-jsonl-parser-v6-optional-activity-admission",
        ),
    ];
    for (provider, parser_revision) in parser_cases {
        let adapter = adapter(provider);
        assert_eq!(adapter.parser_revision(), parser_revision, "{provider:?}");
        assert_eq!(
            adapter.event_identity_revision(),
            "direct-jsonl-content-occurrence-v2",
            "{provider:?}"
        );
    }
}

#[test]
fn grok_future_typed_result_retains_linkage_without_untrusted_content() {
    let marker = "grokfuturetypedimagemarker8f31";
    let value = json!({
        "timestamp": 1_786_547_762_i64,
        "method": "session/update",
        "params": {
            "sessionId": "grok_build-result-contract",
            "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "future-typed-content",
                "status": "completed",
                "content": [{
                    "type": "content",
                    "content": {
                        "type": "image_resource_vNext",
                        "resource": {"uri": marker, "mimeType": "image/png"}
                    }
                }],
                "rawOutput": {
                    "type": "FutureResourceVNext",
                    "resource": {"uri": marker}
                }
            },
            "_meta": {
                "eventId": "future-typed-content-event",
                "agentTimestampMs": 1_786_547_762_000_i64
            }
        }
    });

    let (records, rejected) = project(CaptureProvider::GrokBuild, &value);

    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.event_type, EventType::ToolOutput.as_str());
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("tool_output")
    );
    let expected_evidence = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": "future-typed-content",
        "status": "completed",
    });
    assert_eq!(
        record.content.structured_content.as_ref(),
        Some(&expected_evidence)
    );
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::utf8("future-typed-content").unwrap())
    );
    assert!(activity.result.is_some());
    assert!(
        !serde_json::to_string(&record.content)
            .unwrap()
            .contains(marker),
        "future ACP content escaped the closed parser projection"
    );
}

#[test]
fn copilot_no_native_id_fixture_keeps_exact_v2_fallback_event_ids() {
    let start_without_id = |call_id: &str| {
        json!({
            "type": "tool.execution_start",
            "timestamp": "2026-08-03T12:00:01Z",
            "data": {
                "toolCallId": call_id,
                "mcpServerName": "fallback-server",
                "mcpToolName": "identical-fallback-tool",
                "arguments": {"query": "identical-fallback-argument"},
            },
        })
    };
    let first = start_without_id("fallback-call-one");
    let second = start_without_id("fallback-call-two");
    let ids_by_call = |values: &[Value]| {
        let (records, rejected) = project_all(CaptureProvider::CopilotCli, values);
        assert_eq!(rejected, 0);
        records
            .into_iter()
            .filter_map(|record| {
                let activity = record.content.activity?;
                activity.invocation.as_ref()?;
                let TypedKey::Utf8(call_id) = activity.provider_call_id? else {
                    return None;
                };
                Some((call_id, record.event_id.to_string()))
            })
            .collect::<BTreeMap<_, _>>()
    };

    let actual = ids_by_call(&[first.clone(), second.clone()]);
    assert_eq!(actual, ids_by_call(&[second, first]));
    assert_eq!(
        actual,
        BTreeMap::from([
            (
                "fallback-call-one".to_owned(),
                "5ec57ba4-b568-841b-bb21-d632652ab537".to_owned(),
            ),
            (
                "fallback-call-two".to_owned(),
                "f9d1c72a-9243-8799-bc2b-86d9f0e3c5e0".to_owned(),
            ),
        ])
    );
}

#[test]
fn copilot_replay_preserves_current_revision_ids_and_records() {
    let cases = [(
        CaptureProvider::CopilotCli,
        json!({
            "id": "copilot-replay-event",
            "type": "user.message",
            "timestamp": "2026-08-03T12:34:56Z",
            "data": {"content": "copilot replay body"}
        }),
        "c1ebd99c-7338-859b-891d-1c7e04d9ae9d",
        "5ff93a01-4aa3-82f8-8d9e-784490016567",
        "8d4627ce-c12c-8d64-af2d-d85ac722121f",
        "242c534cc04212dd8babe58be9810c1c91b3bced3be22056c3bc1ff66e3e9739",
    )];

    for (provider, value, event_id, session_id, source_id, record_leaf) in cases {
        let adapter = adapter(provider);
        let expected_parser_revision = match provider {
            CaptureProvider::CopilotCli => {
                super::copilot::COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION
            }
            CaptureProvider::GrokBuild => {
                "direct-native-jsonl-parser-v8-grok-closed-content-admission"
            }
            _ => "direct-native-jsonl-parser-v6-optional-activity-admission",
        };
        assert_eq!(
            JsonlFamilyAdapter::parser_revision(&adapter),
            expected_parser_revision
        );
        let (initial, rejected) = project_all(provider, std::slice::from_ref(&value));
        assert_eq!(rejected, 0);
        assert_eq!(initial.len(), 1);
        let (replay, replay_rejected) = project_all(provider, std::slice::from_ref(&value));
        assert_eq!(replay_rejected, 0);
        assert_eq!(replay, initial);
        let record = &initial[0];
        assert_eq!(record.parser_revision, expected_parser_revision);
        assert_eq!(record.event_id.to_string(), event_id, "{provider:?}");
        assert_eq!(record.session_id.to_string(), session_id, "{provider:?}");
        assert_eq!(
            record.source.identity().to_string(),
            source_id,
            "{provider:?}"
        );
        assert!(record.native_event_id.is_some(), "{provider:?}");
        assert_eq!(
            core_record_leaf_sha256(record).unwrap(),
            record_leaf,
            "{provider:?}"
        );
    }
}

fn factory_result(id: &str, parent_id: Option<&str>, call_id: &str, body: &str) -> Value {
    let mut value = json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-14T09:30:13Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": body,
                "is_error": false
            }]
        }
    });
    if let Some(parent_id) = parent_id {
        value["parentId"] = json!(parent_id);
    }
    value
}

#[test]
fn factory_retry_identities_use_stable_native_evidence_not_order_or_content() {
    let anchor = factory_result("shared", None, "Execute_anchor", "anchor");
    let retry_one = factory_result("shared", Some("shared"), "Execute_retry_1", "retry one");
    let retry_two = factory_result("shared", Some("shared"), "Execute_retry_2", "retry two");

    let (anchor_only, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        std::slice::from_ref(&anchor),
    );
    assert_eq!(rejected, 0);
    let (initial, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[anchor.clone(), retry_one.clone(), retry_two.clone()],
    );
    assert_eq!(rejected, 0);
    assert_eq!(initial.len(), 3);
    assert_eq!(anchor_only[0].event_id, initial[0].event_id);
    assert_eq!(
        initial[0].native_event_id,
        Some(TypedKey::Composite(vec![
            TypedKey::Utf8("shared".to_owned()),
            TypedKey::U64(0),
        ]))
    );
    assert_eq!(
        initial[1].native_event_id,
        Some(TypedKey::Composite(vec![
            TypedKey::Utf8("shared".to_owned()),
            TypedKey::Composite(vec![
                TypedKey::Utf8("factory-ai-droid.retry-tool-result".to_owned()),
                TypedKey::Utf8("Execute_retry_1".to_owned()),
            ]),
        ]))
    );

    let baseline = event_ids_by_body(&initial);
    let (reordered, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[retry_two.clone(), anchor.clone(), retry_one.clone()],
    );
    assert_eq!(rejected, 0);
    assert_eq!(event_ids_by_body(&reordered), baseline);

    let inserted = factory_result(
        "shared",
        Some("shared"),
        "Execute_inserted",
        "inserted retry",
    );
    let (with_insert, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[
            retry_two.clone(),
            inserted,
            anchor.clone(),
            retry_one.clone(),
        ],
    );
    assert_eq!(rejected, 0);
    let with_insert = event_ids_by_body(&with_insert);
    for (body, event_id) in &baseline {
        assert_eq!(with_insert.get(body), Some(event_id));
    }

    let (after_delete, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[retry_two.clone(), anchor.clone()],
    );
    assert_eq!(rejected, 0);
    let after_delete = event_ids_by_body(&after_delete);
    assert_eq!(after_delete.get("anchor"), baseline.get("anchor"));
    assert_eq!(after_delete.get("retry two"), baseline.get("retry two"));
    assert!(!after_delete.values().any(|id| id == &baseline["retry one"]));

    let rewritten = factory_result(
        "shared",
        Some("shared"),
        "Execute_retry_1",
        "rewritten retry one",
    );
    let (rewritten, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[anchor, rewritten, retry_two],
    );
    assert_eq!(rejected, 0);
    assert_eq!(
        event_ids_by_body(&rewritten)["rewritten retry one"],
        baseline["retry one"]
    );
}

#[test]
fn factory_retry_multi_subrecords_keep_ids_when_content_blocks_reorder() {
    let record = |content: Vec<Value>| {
        json!({
            "type": "message",
            "id": "shared",
            "parentId": "shared",
            "timestamp": "2026-07-14T09:30:23Z",
            "message": {"role": "user", "content": content}
        })
    };
    let first = json!({
        "type": "tool_result",
        "tool_use_id": "Execute_A",
        "content": "result a"
    });
    let second = json!({
        "type": "tool_result",
        "tool_use_id": "Execute_B",
        "content": "result b"
    });
    let text = json!({"type": "text", "text": "interleaved"});
    let (original, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[record(vec![first.clone(), text.clone(), second.clone()])],
    );
    assert_eq!(rejected, 0);
    let (reordered, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[record(vec![second, first, text])],
    );
    assert_eq!(rejected, 0);
    assert_eq!(event_ids_by_body(&original), event_ids_by_body(&reordered));
}

#[test]
fn duplicate_final_event_identities_reject_the_later_physical_record() {
    let duplicate = factory_result("shared", Some("shared"), "Execute_same", "duplicate");
    let (records, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[duplicate.clone(), duplicate],
    );
    assert_eq!(rejected, 1);
    assert_eq!(records.len(), 1);

    let missing_tool_use_id = json!({
        "type": "message",
        "id": "shared",
        "parentId": "shared",
        "timestamp": "2026-07-14T09:30:23Z",
        "message": {
            "role": "user",
            "content": [{"type": "tool_result", "content": "missing linkage"}]
        }
    });
    let (records, rejected) = project_all(CaptureProvider::FactoryAiDroid, &[missing_tool_use_id]);
    assert!(records.is_empty());
    assert_eq!(rejected, 1);

    for parent_id in [None, Some("contradictory-parent")] {
        let anchor = factory_result("shared", None, "Execute_anchor", "anchor");
        let ambiguous = factory_result("shared", parent_id, "Execute_other", "ambiguous");
        let (records, rejected) =
            project_all(CaptureProvider::FactoryAiDroid, &[anchor, ambiguous]);
        assert_eq!(rejected, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].content.normalized_body.as_deref(),
            Some("anchor")
        );
    }

    let qoder = |call_id: &str, body: &str| {
        json!({
            "type": "user",
            "uuid": "shared-qoder-id",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": body
                }]
            }
        })
    };
    let (records, rejected) = project_all(
        CaptureProvider::Qoder,
        &[qoder("call-a", "qoder a"), qoder("call-b", "qoder b")],
    );
    assert_eq!(rejected, 1);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("qoder a")
    );
}

#[test]
fn duplicate_multi_event_record_is_rejected_atomically() {
    let duplicate = json!({
        "type": "message",
        "id": "shared",
        "timestamp": "2026-07-14T09:30:23Z",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "Execute_A",
                    "content": "result a"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "Execute_B",
                    "content": "result b"
                }
            ]
        }
    });
    let rewritten_duplicate = json!({
        "type": "message",
        "id": "shared",
        "timestamp": "2026-07-14T09:30:24Z",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "Execute_A",
                    "content": "rewritten a"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "Execute_B",
                    "content": "rewritten b"
                }
            ]
        }
    });

    let (records, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[duplicate, rewritten_duplicate],
    );

    assert_eq!(rejected, 1);
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.normalized_body.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["result a", "result b"]
    );
}

#[test]
fn supported_family_members_retain_complete_result_bodies_in_core() {
    let cases = [
        (
            CaptureProvider::Tabnine,
            json!({
                "type": "tabnine",
                "toolCalls": [{
                    "id": "tabnine-call",
                    "name": "shell",
                    "result": {"content": "tabnine complete result"},
                    "success": true
                }]
            }),
            "tabnine complete result",
            "tabnine-call",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            json!({
                "type": "message",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "factory-call",
                        "name": "shell",
                        "content": "factory complete result",
                        "is_error": false
                    }]
                }
            }),
            "factory complete result",
            "factory-call",
        ),
        (
            CaptureProvider::Qoder,
            json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "qoder-call",
                        "name": "shell",
                        "content": "qoder complete result",
                        "is_error": false
                    }]
                }
            }),
            "qoder complete result",
            "qoder-call",
        ),
        (
            CaptureProvider::CopilotCli,
            json!({
                "type": "tool.execution_complete",
                "data": {
                    "callId": "copilot-call",
                    "toolName": "shell",
                    "content": "copilot complete result",
                    "success": true
                }
            }),
            "copilot complete result",
            "copilot-call",
        ),
        (
            CaptureProvider::QwenCode,
            json!({
                "type": "tool_result",
                "message": {
                    "role": "tool",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "qwen-call",
                        "name": "shell",
                        "content": "qwen complete result",
                        "is_error": false
                    }]
                }
            }),
            "qwen complete result",
            "qwen-call",
        ),
    ];

    for (provider, value, expected_body, expected_call_id) in cases {
        let (records, rejected) = project(provider, &value);
        assert_eq!(rejected, 0, "{provider:?}");
        assert_eq!(records.len(), 1, "{provider:?}");
        let record = &records[0];
        assert_eq!(record.event_type, "tool_output", "{provider:?}");
        assert_eq!(
            record.content.normalized_body.as_deref(),
            Some(expected_body),
            "{provider:?}"
        );
        let activity = record.content.activity.as_ref().unwrap();
        assert_eq!(
            activity.provider_call_id,
            Some(TypedKey::utf8(expected_call_id).unwrap()),
            "{provider:?}"
        );
        assert_eq!(
            activity.result.as_ref().unwrap().status,
            None,
            "{provider:?}"
        );
    }
}

#[test]
fn unlinked_native_result_preserves_exact_record_without_empty_activity() {
    let value = json!({
        "type": "tool_result",
        "message": {
            "role": "tool",
            "content": [{
                "type": "tool_result",
                "name": "shell",
                "content": "unlinked complete result"
            }]
        }
    });

    let (records, rejected) = project(CaptureProvider::QwenCode, &value);

    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("unlinked complete result")
    );
    assert!(records[0].content.activity.is_none());
}

#[test]
fn native_status_variants_keep_content_indices_and_statusless_activity() {
    let value = json!({
        "type": "tabnine",
        "toolCalls": [
            {
                "id": "success-call",
                "name": "shell",
                "result": {"content": "success native body"},
                "success": true
            },
            {"id": "call-without-result"},
            {
                "id": "failure-call",
                "name": "shell",
                "result": {"content": {"stderr": "failure native body", "code": 17}},
                "success": false,
                "exitCode": 17
            },
            {
                "id": "unknown-call",
                "name": "custom",
                "result": {"content": ["unknown native body", {"detail": 7}]}
            }
        ]
    });

    let (records, rejected) = project(CaptureProvider::Tabnine, &value);
    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .map(native_subrecord_index)
            .collect::<Vec<_>>(),
        vec![0, 2, 3]
    );
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("success native body")
    );
    assert_eq!(
        serde_json::from_str::<Value>(records[1].content.normalized_body.as_deref().unwrap())
            .unwrap(),
        json!({"stderr": "failure native body", "code": 17})
    );
    assert_eq!(
        serde_json::from_str::<Value>(records[2].content.normalized_body.as_deref().unwrap())
            .unwrap(),
        json!(["unknown native body", {"detail": 7}])
    );
    for record in &records {
        assert_eq!(
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.result.as_ref())
                .and_then(|result| result.status.as_deref()),
            None
        );
    }
}

#[test]
fn malformed_shapes_reject_and_ambiguous_descendants_abstain() {
    let malformed = json!({
        "type": "tabnine",
        "toolCalls": [{
            "result": {"content": "first", "output": "second"}
        }]
    });
    let (records, rejected) = project(CaptureProvider::Tabnine, &malformed);
    assert!(records.is_empty());
    assert_eq!(rejected, 1);

    let ambiguous_descendant = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "future_tool_result",
                "callId": "future-call",
                "payload": {"arbitrary": "must not be guessed"},
                "status": "failed"
            }]
        }
    });
    let (records, rejected) = project(CaptureProvider::Qoder, &ambiguous_descendant);
    assert!(records.is_empty());
    assert_eq!(rejected, 0);
}

#[test]
fn antigravity_does_not_invent_result_semantics() {
    let provider = CaptureProvider::Antigravity;
    assert_eq!(
        super::super::super::result_content::native_jsonl_result_content_profile(provider),
        None
    );
    let candidate = json!({
        "type": "tool_result",
        "result": {"content": "unsupported result candidate"}
    });
    let event_type =
        super::super::super::normalization::native_jsonl_event_type(provider, &candidate);
    assert_ne!(event_type, EventType::ToolOutput);
}

#[test]
fn result_expansion_past_page_targets_is_not_an_admission_ceiling() {
    let tool_calls = (0..65)
        .map(|index| {
            json!({
                "id": format!("call-{index}"),
                "result": {"content": format!("result-{index}")},
                "success": true
            })
        })
        .collect::<Vec<_>>();
    let (records, rejected) = project(
        CaptureProvider::Tabnine,
        &json!({"type": "tabnine", "toolCalls": tool_calls}),
    );
    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 65);
    assert_eq!(native_subrecord_index(records.last().unwrap()), 64);
}

#[test]
fn complete_result_larger_than_eight_mib_reaches_core_once() {
    const OLD_FAMILY_CEILING: usize = 8 * 1024 * 1024;
    const TAIL: &str = "native-jsonl-result-tail";

    let body = format!("{}{}", "x".repeat(OLD_FAMILY_CEILING + 257), TAIL);
    let value = json!({
        "type": "tabnine",
        "toolCalls": [{
            "id": "large-call",
            "name": "shell",
            "result": {"content": body},
            "success": true
        }]
    });
    let expected = value
        .pointer("/toolCalls/0/result/content")
        .unwrap()
        .as_str()
        .unwrap();
    let (records, rejected) = project(CaptureProvider::Tabnine, &value);
    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.content.normalized_body.as_deref(), Some(expected));
    assert!(record
        .content
        .normalized_body
        .as_ref()
        .unwrap()
        .ends_with(TAIL));
    let structured = serde_json::to_string(&record.content.structured_content).unwrap();
    assert!(!structured.contains(TAIL));
    let encoded = record.encode_stored().unwrap();
    assert!(encoded.len() > OLD_FAMILY_CEILING);
    assert!(encoded.len() <= MAX_ENCODED_CORE_RECORD_BYTES);
}
