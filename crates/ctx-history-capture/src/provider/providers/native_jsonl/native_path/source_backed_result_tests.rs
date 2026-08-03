use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CoreRecord, EventType, SessionStatus, TypedKey,
    MAX_ENCODED_CORE_RECORD_BYTES,
};
use serde_json::{json, Value};

use super::*;

fn adapter(provider: CaptureProvider) -> DirectJsonlFamilyAdapter {
    match provider {
        CaptureProvider::Antigravity => super::super::antigravity_source_backed_adapter(),
        CaptureProvider::Tabnine => super::super::tabnine_source_backed_adapter(),
        CaptureProvider::FactoryAiDroid => super::super::factory_droid_source_backed_adapter(),
        CaptureProvider::Windsurf => super::super::windsurf_source_backed_adapter(),
        CaptureProvider::Qoder => super::super::qoder_source_backed_adapter(),
        CaptureProvider::CopilotCli => super::super::copilot_source_backed_adapter(),
        CaptureProvider::QwenCode => super::super::qwen_code_source_backed_adapter(),
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
        agent_type: AgentType::Primary,
        role_hint: None,
        is_primary: true,
        status: SessionStatus::Imported,
        started_at: DateTime::<Utc>::UNIX_EPOCH,
        ended_at: None,
        cwd: None,
        metadata: json!({}),
    }
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
        source,
        bound_session: session,
        session_id,
        projector: direct,
        rejected_records: 0,
    };
    let encoded = serde_json::to_vec(value).unwrap();
    let mut records = Vec::new();
    JsonlFamilyProjector::project(
        &mut projector,
        JsonlRecordRef::for_test(&encoded, 7),
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
    let (source, session_id) = adapter
        .session_identity(&session.native_session_id)
        .unwrap();
    let direct = DirectJsonlProjector::new(
        provider,
        adapter.source_format,
        Path::new("direct-jsonl-identity-contract.jsonl"),
        None,
        DateTime::<Utc>::UNIX_EPOCH,
        Some(session.clone()),
    )
    .unwrap();
    let mut projector = DirectJsonlFamilyProjector {
        adapter,
        source,
        bound_session: session,
        session_id,
        projector: direct,
        rejected_records: 0,
    };
    let mut records = Vec::new();
    for (ordinal, value) in values.iter().enumerate() {
        let encoded = serde_json::to_vec(value).unwrap();
        JsonlFamilyProjector::project(
            &mut projector,
            JsonlRecordRef::for_test(&encoded, ordinal as u64),
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
fn factory_retry_ambiguity_and_missing_evidence_fail_closed() {
    let duplicate = factory_result("shared", Some("shared"), "Execute_same", "duplicate");
    let (records, rejected) = project_all(
        CaptureProvider::FactoryAiDroid,
        &[duplicate.clone(), duplicate],
    );
    assert_eq!(rejected, 0);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].event_id, records[1].event_id);

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
        assert_eq!(rejected, 0);
        assert_eq!(records[0].event_id, records[1].event_id);
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
    assert_eq!(rejected, 0);
    assert_eq!(records[0].event_id, records[1].event_id);
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
        let linkage = &record.content.structured_content.as_ref().unwrap()["tool_result"];
        assert_eq!(linkage["call_id"], expected_call_id, "{provider:?}");
        assert_eq!(linkage["outcome"], "success", "{provider:?}");
        assert!(
            !serde_json::to_string(linkage)
                .unwrap()
                .contains(expected_body),
            "{provider:?} duplicated its result body in structured content"
        );
    }
}

#[test]
fn success_failure_and_unknown_results_keep_native_content_and_indices() {
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
    assert_eq!(
        records[0].content.structured_content.as_ref().unwrap()["tool_result"]["outcome"],
        "success"
    );
    assert_eq!(
        records[1].content.structured_content.as_ref().unwrap()["tool_result"]["outcome"],
        "failure"
    );
    assert_eq!(
        records[2].content.structured_content.as_ref().unwrap()["tool_result"]["outcome"],
        "unknown"
    );
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
fn antigravity_and_windsurf_do_not_invent_result_semantics() {
    for provider in [CaptureProvider::Antigravity, CaptureProvider::Windsurf] {
        assert_eq!(
            super::super::super::result_content::native_jsonl_result_content_profile(provider),
            None
        );
        let candidate = json!({
            "type": "tool_result",
            "result": {"content": "unsupported result candidate"}
        });
        let event_type = match provider {
            CaptureProvider::Antigravity => {
                super::super::super::normalization::native_jsonl_event_type(provider, &candidate)
            }
            CaptureProvider::Windsurf => super::super::windsurf_event_type(&candidate),
            _ => unreachable!(),
        };
        assert_ne!(event_type, EventType::ToolOutput, "{provider:?}");
    }
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
