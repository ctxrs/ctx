use std::{fs, path::Path};

use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord,
    LiteralFactKind, ProviderDeclaredFact, ProviderNativeCopyProof,
    ProviderNativeSessionRelationship, TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
    MAX_PROVIDER_DECLARED_FACTS,
};
use serde_json::{json, Value};

use super::source_backed::*;
use crate::{test_support_paths::tempdir, CaptureError, ProviderSourceFailureKind};

fn manifest(lineage: bool) -> Value {
    let mut record = json!({
        "record_type": "manifest",
        "schema_version": "ctx-history-jsonl-v2",
        "producer": "source-backed-v2-test",
    });
    if lineage {
        record["lineage_contract"] = json!("provider_native_v1");
    }
    record
}

fn source() -> Value {
    json!({
        "record_type": "source",
        "source_id": "source-a",
        "provider_key": "demo-agent",
        "source_format": "demo-jsonl",
    })
}

fn session(
    provider_session_id: &str,
    parent: Option<&str>,
    relationship: ProviderNativeSessionRelationship,
    scope: AgentScope,
) -> Value {
    json!({
        "record_type": "session",
        "source_id": "source-a",
        "provider_session_id": provider_session_id,
        "parent_provider_session_id": parent,
        "root_provider_session_id": if parent.is_some() { "root" } else { provider_session_id },
        "session_relationship": relationship,
        "agent_scope": scope,
        "started_at": "2026-07-28T12:00:00Z",
        "cwd": "/work/./literal",
    })
}

fn event(index: u64, id: &str, provider_session_id: &str, payload: Value) -> Value {
    json!({
        "record_type": "event",
        "source_id": "source-a",
        "provider_session_id": provider_session_id,
        "event_index": index,
        "event_id": id,
        "event_type": "message",
        "role": "assistant",
        "occurred_at": "2026-07-28T12:00:01Z",
        "payload": payload,
    })
}

fn file_reference(index: u64, event_index: u64, value: &str) -> Value {
    json!({
        "record_type": "file_reference",
        "source_id": "source-a",
        "provider_session_id": "child",
        "reference_index": index,
        "event_index": event_index,
        "value": value,
        "occurred_at": "2026-07-28T12:00:02Z",
    })
}

fn write_records(path: &Path, records: &[Value]) {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn collect(input: &CustomHistorySourceBackedInput) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    let outcome = scan_custom_history_source_backed_explicit(input, None, |_, page| {
        records.extend(page.records);
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        outcome,
        CustomHistorySourceBackedOutcome::Present(_)
    ));
    records
}

fn collect_with_certificate(
    input: &CustomHistorySourceBackedInput,
) -> (Vec<CoreRecord>, ctx_history_core::CertifiedSource) {
    let mut records = Vec::new();
    let outcome = scan_custom_history_source_backed_explicit(input, None, |_, page| {
        records.extend(page.records);
        Ok(())
    })
    .unwrap();
    let CustomHistorySourceBackedOutcome::Present(receipt) = outcome else {
        panic!("expected present custom history source");
    };
    (records, receipt.certificate)
}

fn assert_single_event_line_rejected(
    records: &[CoreRecord],
    certificate: &ctx_history_core::CertifiedSource,
) {
    assert!(records.is_empty());
    assert_eq!(certificate.counts().complete_records, 4);
    assert_eq!(certificate.counts().retained_records, 0);
    assert_eq!(certificate.counts().rejected_records, 1);
    assert_eq!(certificate.counts().ignored_records, 3);
    assert_eq!(certificate.counts().indexed_documents, 0);
}

#[test]
fn v2_projects_exact_activity_payload_and_ordered_duplicate_facts() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("neutral.jsonl");
    let native_activity = CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8("call/01".to_owned())),
        invocation: Some(ActivityInvocation {
            protocol: Some("native/protocol".to_owned()),
            server: None,
            tool: "Read File".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({"path": "./src/../src/lib.rs"}),
            },
            started_at_unix_ms: None,
        }),
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: "cat  ./src/lib.rs".to_owned(),
        }],
    };
    let payload = json!({
        "text": "literal body",
        "activity": serde_json::to_value(&native_activity).unwrap(),
        "provider_extra": {"preserve": [2, 1, 2]},
    });
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "child",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Subagent,
            ),
            event(0, "event-0", "child", payload.clone()),
            file_reference(0, 0, "./src/../src/lib.rs"),
            file_reference(1, 0, "./src/../src/lib.rs"),
        ],
    );

    let records = collect(&CustomHistorySourceBackedInput::explicit(&path, [7; 32]));
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some("literal body")
    );
    assert_eq!(record.content.structured_content.as_ref(), Some(&payload));
    assert_eq!(record.agent_scope, Some(AgentScope::Subagent));
    assert_eq!(record.session_relationship, None);
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(activity.provider_call_id, native_activity.provider_call_id);
    assert_eq!(activity.invocation, native_activity.invocation);
    assert_eq!(
        activity.facts,
        vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: "/work/./literal".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::Command,
                value: "cat  ./src/lib.rs".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "./src/../src/lib.rs".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "./src/../src/lib.rs".to_owned(),
            },
        ]
    );
}

#[test]
fn v2_preserves_declared_provider_session_id_and_file_reference_fact() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("provider-session.jsonl");
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "child",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            event(0, "event-0", "child", json!({"text": "update parser"})),
            file_reference(0, 0, "tests/parser.rs"),
        ],
    );

    let (records, certificate) =
        collect_with_certificate(&CustomHistorySourceBackedInput::explicit(&path, [14; 32]));
    assert_eq!(records.len(), 1);
    assert_eq!(certificate.counts().complete_records, 5);
    assert_eq!(certificate.counts().retained_records, 1);
    assert_eq!(certificate.counts().rejected_records, 0);
    assert_eq!(certificate.counts().ignored_records, 4);
    assert_eq!(certificate.counts().indexed_documents, 1);
    assert_eq!(records[0].agent_scope, Some(AgentScope::Primary));
    assert_eq!(records[0].provider_session_id.as_deref(), Some("child"));
    assert_eq!(
        records[0].content.activity.as_ref().unwrap().facts,
        vec![
            ProviderDeclaredFact {
                kind: LiteralFactKind::SessionCwd,
                value: "/work/./literal".to_owned(),
            },
            ProviderDeclaredFact {
                kind: LiteralFactKind::File,
                value: "tests/parser.rs".to_owned(),
            },
        ]
    );
}

#[test]
fn v2_rejects_wrong_activity_revision_at_the_event_line() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("wrong-activity-revision.jsonl");
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "child",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            event(
                0,
                "event-0",
                "child",
                json!({
                    "text": "literal body",
                    "activity": {
                        "revision": CORE_ACTIVITY_REVISION + 1,
                        "facts": [{"kind": "command", "value": "literal"}],
                    },
                }),
            ),
        ],
    );

    let (records, certificate) =
        collect_with_certificate(&CustomHistorySourceBackedInput::explicit(&path, [11; 32]));
    assert_single_event_line_rejected(&records, &certificate);
}

#[test]
fn v2_rejects_complete_merged_activity_fact_overflow_at_the_event_line() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("merged-activity-overflow.jsonl");
    let facts = vec![
        ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: "literal".to_owned(),
        };
        MAX_PROVIDER_DECLARED_FACTS
    ];
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "child",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            event(
                0,
                "event-0",
                "child",
                json!({
                    "text": "literal body",
                    "activity": CoreActivity {
                        revision: CORE_ACTIVITY_REVISION,
                        provider_call_id: None,
                        invocation: None,
                        result: None,
                        facts,
                    },
                }),
            ),
        ],
    );

    let (records, certificate) =
        collect_with_certificate(&CustomHistorySourceBackedInput::explicit(&path, [12; 32]));
    assert_single_event_line_rejected(&records, &certificate);
}

#[test]
fn v2_rejects_an_oversized_activity_fact_as_one_bounded_line() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("oversized-activity-fact.jsonl");
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "child",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            event(
                0,
                "event-0",
                "child",
                json!({
                    "text": "literal body",
                    "activity": {
                        "revision": CORE_ACTIVITY_REVISION,
                        "facts": [{
                            "kind": "command",
                            "value": "x".repeat(MAX_CORE_CONTENT_BYTES + 1),
                        }],
                    },
                }),
            ),
        ],
    );

    let (records, certificate) =
        collect_with_certificate(&CustomHistorySourceBackedInput::explicit(&path, [13; 32]));
    assert_single_event_line_rejected(&records, &certificate);
}

#[test]
fn v2_retains_only_explicit_provider_native_lineage_and_copy_claims() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("lineage.jsonl");
    let mut copied = event(1, "child-event", "child", json!({"text": "copied"}));
    copied["copied_from"] = json!({
        "ancestor_provider_session_id": "root",
        "ancestor_event_id": "root-event",
        "proof": "native_copied_from_field",
    });
    write_records(
        &path,
        &[
            manifest(true),
            source(),
            session(
                "root",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            session(
                "child",
                Some("root"),
                ProviderNativeSessionRelationship::Delegated,
                AgentScope::Subagent,
            ),
            event(0, "root-event", "root", json!({"text": "root"})),
            copied,
        ],
    );

    let records = collect(&CustomHistorySourceBackedInput::explicit(&path, [8; 32]));
    let root = &records[0];
    let child = &records[1];
    assert_eq!(child.parent_session_id, Some(root.session_id));
    assert_eq!(child.root_session_id, Some(root.session_id));
    assert_eq!(
        child.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    let copy = child.event_copy.as_ref().unwrap();
    assert_eq!(copy.ancestor_session_id, root.session_id);
    assert_eq!(copy.ancestor_event_id, root.event_id);
    assert_eq!(copy.proof, ProviderNativeCopyProof::NativeCopiedFromField);
}

#[test]
fn v2_accepts_grandchild_roots_and_target_independent_ancestor_copy_claims() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("grandchild-lineage.jsonl");
    let mut copied = event(0, "grand-event", "grand", json!({"text": "copied"}));
    copied["copied_from"] = json!({
        "ancestor_provider_session_id": "root",
        "ancestor_event_id": "unresolved-root-event",
        "proof": "native_copied_from_field",
    });
    write_records(
        &path,
        &[
            manifest(true),
            source(),
            session(
                "root",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            session(
                "child",
                Some("root"),
                ProviderNativeSessionRelationship::Delegated,
                AgentScope::Subagent,
            ),
            session(
                "grand",
                Some("child"),
                ProviderNativeSessionRelationship::Delegated,
                AgentScope::Subagent,
            ),
            event(0, "root-event", "root", json!({"text": "root"})),
            event(0, "child-event", "child", json!({"text": "child"})),
            copied,
        ],
    );

    let records = collect(&CustomHistorySourceBackedInput::explicit(&path, [17; 32]));
    let root = &records[0];
    let child = &records[1];
    let grand = &records[2];
    assert_eq!(grand.parent_session_id, Some(child.session_id));
    assert_eq!(grand.root_session_id, Some(root.session_id));
    let copy = grand.event_copy.as_ref().unwrap();
    assert_eq!(copy.ancestor_session_id, root.session_id);
    assert_ne!(copy.ancestor_event_id, root.event_id);
}

#[test]
fn absent_lineage_contract_omits_relationship_and_copy_claims() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("unclaimed-lineage.jsonl");
    let mut copied = event(1, "child-event", "child", json!({"text": "copied"}));
    copied["copied_from"] = json!({
        "ancestor_provider_session_id": "root",
        "ancestor_event_id": "root-event",
        "proof": "native_copied_from_field",
    });
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            session(
                "root",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            session(
                "child",
                Some("root"),
                ProviderNativeSessionRelationship::Delegated,
                AgentScope::Subagent,
            ),
            event(0, "root-event", "root", json!({"text": "root"})),
            copied,
        ],
    );

    let records = collect(&CustomHistorySourceBackedInput::explicit(&path, [9; 32]));
    assert_eq!(records[1].parent_session_id, Some(records[0].session_id));
    assert_eq!(records[1].session_relationship, None);
    assert_eq!(records[1].event_copy, None);
}

#[test]
fn v1_manifest_is_a_schema_incompatible_source_failure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("legacy.jsonl");
    write_records(
        &path,
        &[json!({
            "record_type": "manifest",
            "schema_version": "ctx-history-jsonl-v1",
        })],
    );
    let input = CustomHistorySourceBackedInput::explicit(&path, [10; 32]);
    let error = scan_custom_history_source_backed_explicit(&input, None, |_, _| Ok(()))
        .expect_err("v1 must not be translated");
    let CustomHistorySourceBackedError::Capture(CaptureError::ProviderSource {
        provider,
        kind,
        detail,
        ..
    }) = error
    else {
        panic!("expected typed source error, got {error:?}");
    };
    assert_eq!(provider, CaptureProvider::Custom.as_str());
    assert_eq!(kind, ProviderSourceFailureKind::SchemaIncompatible);
    assert!(detail.contains("ctx-history-jsonl-v1"), "{detail}");
}

#[test]
fn v2_preserves_distinct_routes_with_the_same_provider_session_id() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("multiple-routes.jsonl");
    let mut second_session = session(
        "shared-session",
        None,
        ProviderNativeSessionRelationship::Root,
        AgentScope::Primary,
    );
    second_session["source_id"] = json!("source-b");
    let mut second_event = event(
        0,
        "second-event",
        "shared-session",
        json!({"text":"second route"}),
    );
    second_event["source_id"] = json!("source-b");
    write_records(
        &path,
        &[
            manifest(false),
            source(),
            json!({
                "record_type":"source",
                "source_id":"source-b",
                "provider_key":"second-agent",
                "source_format":"second-jsonl",
            }),
            session(
                "shared-session",
                None,
                ProviderNativeSessionRelationship::Root,
                AgentScope::Primary,
            ),
            event(
                0,
                "first-event",
                "shared-session",
                json!({"text":"first route"}),
            ),
            second_session,
            second_event,
        ],
    );

    let records = collect(&CustomHistorySourceBackedInput::explicit(&path, [16; 32]));
    assert_eq!(records.len(), 2);
    assert_eq!(
        records
            .iter()
            .map(|record| (
                record.provider_session_id.as_deref(),
                record.native_event_id.as_ref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                Some("shared-session"),
                Some(&TypedKey::Composite(vec![
                    TypedKey::Utf8("demo-agent".to_owned()),
                    TypedKey::Utf8("source-a".to_owned()),
                    TypedKey::Utf8("event_id:first-event".to_owned()),
                ])),
            ),
            (
                Some("shared-session"),
                Some(&TypedKey::Composite(vec![
                    TypedKey::Utf8("second-agent".to_owned()),
                    TypedKey::Utf8("source-b".to_owned()),
                    TypedKey::Utf8("event_id:second-event".to_owned()),
                ])),
            ),
        ]
    );
}
