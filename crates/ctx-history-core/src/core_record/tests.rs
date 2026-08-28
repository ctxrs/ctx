use serde_json::{json, Value};

use super::*;
use crate::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

fn source() -> SourceKey {
    SourceKey::derive(
        "core-record-test",
        "core-record-jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session", TypedKey::utf8("session.jsonl").unwrap()).unwrap(),
    )
    .unwrap()
}

fn session(source: &SourceKey, native: &str) -> StableEntityId {
    let key = NativeSessionKey::native_id("session", TypedKey::utf8(native).unwrap()).unwrap();
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &key,
    })
    .unwrap()
}

fn event(source: &SourceKey, session_id: StableEntityId, sequence: u64) -> StableEntityId {
    let key = NativeItemKey::native_id("event", TypedKey::U64(sequence)).unwrap();
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap()
}

fn record() -> CoreRecord {
    let source = source();
    let session_id = session(&source, "session");
    CoreRecord::new_selected(
        event(&source, session_id, 1),
        session_id,
        source,
        1,
        "tool_output",
        "core-record-test-v1",
        "complete result text",
    )
    .unwrap()
}

fn activity() -> CoreActivity {
    CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::Utf8("call-01".to_owned())),
        invocation: Some(ActivityInvocation {
            protocol: Some("mcp".to_owned()),
            server: Some("source-server".to_owned()),
            tool: "execute".to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: json!({"command": "tool --native"}),
            },
            started_at_unix_ms: Some(101),
        }),
        result: Some(ActivityResult {
            status: Some("provider::ok".to_owned()),
            completed_at_unix_ms: Some(202),
            duration_ns: Some(303),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Present {
                value: json!({
                    "stdout": "complete output",
                    "nested": {"all": ["provider", "content"]}
                }),
            },
        }),
        facts: [
            (LiteralFactKind::SessionCwd, "/Work/Repo/../ctx"),
            (LiteralFactKind::ToolWorkdir, "./literal-workdir"),
            (LiteralFactKind::Project, "ctxrs/ctx"),
            (LiteralFactKind::Forge, "https://example.invalid/ctxrs/ctx"),
            (LiteralFactKind::Branch, "Feature/MixedCase"),
            (LiteralFactKind::File, "file:///Work/Repo/src/lib.rs"),
            (LiteralFactKind::Vcs, "native-vcs-string"),
            (LiteralFactKind::Commit, "AbCd-provider-literal"),
            (LiteralFactKind::PullRequest, "PR native#17"),
            (LiteralFactKind::File, "file:///Work/Repo/src/lib.rs"),
        ]
        .into_iter()
        .map(|(kind, value)| ProviderDeclaredFact {
            kind,
            value: value.to_owned(),
        })
        .collect(),
    }
}

fn object_keys(value: &Value, keys: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                keys.push(key.clone());
                object_keys(value, keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                object_keys(item, keys);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[test]
fn neutral_record_serializes_no_interpreted_source_semantics() {
    let mut record = record();
    record.provider_session_id = Some("provider-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(44));
    record.occurred_at_unix_ms = Some(1_777_000_000_123);
    record.role = Some("provider-role".to_owned());
    record.content.activity = Some(activity());

    let encoded = record.encode_stored().unwrap();
    let value: Value = serde_json::from_slice(&encoded).unwrap();
    let mut keys = Vec::new();
    object_keys(&value, &mut keys);
    for forbidden in [
        "repository_candidate_evidence",
        "repository_bindings",
        "repository_abstentions",
        "repository_file_invocation_evidence",
        "repository_file_observations",
        "repository_vcs_observations",
        "confidence",
        "effect",
        "change_kind",
        "is_primary",
        "event_origin",
    ] {
        assert!(!keys.iter().any(|key| key == forbidden), "{forbidden}");
    }
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), record);

    let mut old_shape = value;
    old_shape
        .as_object_mut()
        .unwrap()
        .insert("repository_bindings".to_owned(), json!([]));
    assert!(serde_json::from_value::<CoreRecord>(old_shape).is_err());
}

#[test]
fn encoded_json_len_matches_stored_encoding_with_json_escaping() {
    let mut record = record();
    record.content.normalized_body = Some("quoted \"text\"\nwith a NUL: \0".to_owned());
    record.content.structured_content = Some(json!({
        "escaped": ["line\nfeed", "tab\tvalue", "\\path"]
    }));

    let encoded = record.encode_stored().unwrap();
    assert_eq!(record.encoded_json_len().unwrap(), encoded.len());
}

#[test]
fn generic_metadata_cannot_smuggle_repository_or_causal_semantics() {
    for metadata in [
        json!({"repository_bindings": [{"repository": "invented"}]}),
        json!({"nested": {"confidence": "high"}}),
        json!({"nested": [{"effect": "modified"}]}),
    ] {
        let mut value = serde_json::to_value(record()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("metadata".to_owned(), metadata);
        assert!(serde_json::from_value::<CoreRecord>(value).is_err());
    }
}

#[test]
fn provider_activity_retains_exact_order_duplicates_and_complete_result_content() {
    let mut record = record();
    record.content.activity = Some(activity());
    let encoded = record.encode_stored().unwrap();
    let decoded = CoreRecord::decode_stored(&encoded).unwrap();
    let activity = decoded.content.activity.unwrap();

    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("call-01".to_owned()))
    );
    assert_eq!(activity.facts[0].value, "/Work/Repo/../ctx");
    assert_eq!(activity.facts[5].value, "file:///Work/Repo/src/lib.rs");
    assert_eq!(activity.facts[5], activity.facts[9]);
    let invocation = activity.invocation.unwrap();
    assert_eq!(invocation.protocol.as_deref(), Some("mcp"));
    assert_eq!(invocation.server.as_deref(), Some("source-server"));
    assert_eq!(invocation.tool, "execute");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: json!({"command": "tool --native"})
        }
    );
    assert_eq!(invocation.started_at_unix_ms, Some(101));
    let result = activity.result.unwrap();
    assert_eq!(result.status.as_deref(), Some("provider::ok"));
    assert_eq!(result.completed_at_unix_ms, Some(202));
    assert_eq!(result.duration_ns, Some(303));
    assert_eq!(result.text, ActivityTextCapture::NormalizedBody);
    assert_eq!(
        result.structured_content,
        ActivityJsonCapture::Present {
            value: json!({
                "stdout": "complete output",
                "nested": {"all": ["provider", "content"]}
            })
        }
    );
}

#[test]
fn provider_activity_distinguishes_present_empty_result_text_from_absent() {
    let mut record = record();
    let mut activity = activity();
    activity.result.as_mut().unwrap().text = ActivityTextCapture::Present {
        value: String::new(),
    };
    record.content.activity = Some(activity);

    let encoded = record.encode_stored().unwrap();
    let decoded = CoreRecord::decode_stored(&encoded).unwrap();
    assert_eq!(
        decoded.content.activity.unwrap().result.unwrap().text,
        ActivityTextCapture::Present {
            value: String::new()
        }
    );
}

#[test]
fn session_relationship_and_agent_scope_are_optional_direct_claims() {
    let mut record = record();
    let empty = serde_json::to_value(&record).unwrap();
    for absent in [
        "parent_session_id",
        "root_session_id",
        "session_relationship",
        "agent_scope",
    ] {
        assert!(empty.get(absent).is_none(), "{absent}");
    }

    let parent = session(&record.source, "parent");
    record.parent_session_id = Some(parent);
    record.root_session_id = Some(parent);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    record.agent_scope = Some(AgentScope::Subagent);
    record.validate_contract().unwrap();

    let encoded = record.encode_stored().unwrap();
    let value: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(value["session_relationship"], "delegated");
    assert_eq!(value["agent_scope"], "subagent");
    assert!(value.get("is_primary").is_none());
}

#[test]
fn invented_fact_relationship_and_scope_values_are_rejected_by_serde() {
    let admitted = [
        LiteralFactKind::SessionCwd,
        LiteralFactKind::ToolWorkdir,
        LiteralFactKind::File,
        LiteralFactKind::Url,
        LiteralFactKind::Forge,
        LiteralFactKind::Project,
        LiteralFactKind::Vcs,
        LiteralFactKind::Commit,
        LiteralFactKind::PullRequest,
        LiteralFactKind::Command,
        LiteralFactKind::Branch,
        LiteralFactKind::Workspace,
        LiteralFactKind::ProviderDisposition,
    ];
    assert_eq!(
        admitted.map(LiteralFactKind::as_str),
        [
            "session_cwd",
            "tool_workdir",
            "file",
            "url",
            "forge",
            "project",
            "vcs",
            "commit",
            "pull_request",
            "command",
            "branch",
            "workspace",
            "provider_disposition",
        ]
    );
    assert_eq!(
        [
            ProviderNativeSessionRelationship::Root,
            ProviderNativeSessionRelationship::Delegated,
            ProviderNativeSessionRelationship::Forked,
            ProviderNativeSessionRelationship::ResumedFrom,
            ProviderNativeSessionRelationship::WorkflowChild,
        ]
        .map(ProviderNativeSessionRelationship::as_str),
        [
            "root",
            "delegated",
            "forked",
            "resumed_from",
            "workflow_child",
        ]
    );
    assert_eq!(
        [AgentScope::Primary, AgentScope::Subagent].map(AgentScope::as_str),
        ["primary", "subagent"]
    );
    assert!(serde_json::from_value::<ProviderDeclaredFact>(json!({
        "kind": "repository_binding",
        "value": "invented"
    }))
    .is_err());
    assert!(serde_json::from_value::<ProviderDeclaredFact>(json!({
        "kind": "file_effect",
        "value": "modified"
    }))
    .is_err());
    assert!(serde_json::from_value::<ProviderDeclaredFact>(json!({
        "kind": "confidence",
        "value": "high"
    }))
    .is_err());
    assert!(
        serde_json::from_value::<ProviderNativeSessionRelationship>(json!("related_unknown"))
            .is_err()
    );
    assert!(serde_json::from_value::<AgentScope>(json!("reviewer")).is_err());
}

#[test]
fn only_exact_provider_native_copy_proofs_are_admitted() {
    let mut copied = record();
    let ancestor_session = session(&copied.source, "ancestor");
    copied.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: ancestor_session,
        ancestor_event_id: event(&copied.source, ancestor_session, 2),
        proof: ProviderNativeCopyProof::NativeCallResultIdentity,
    });
    copied.validate_contract().unwrap();

    let encoded = copied.encode_stored().unwrap();
    let rendered = String::from_utf8(encoded.clone()).unwrap();
    assert!(rendered.contains("native_call_result_identity"));
    assert!(!rendered.contains("unique_to_session"));
    assert!(!rendered.contains("certified_ordered_prefix"));

    let mut value: Value = serde_json::from_slice(&encoded).unwrap();
    value["event_copy"]["proof"] = json!("certified_ordered_prefix");
    assert!(serde_json::from_value::<CoreRecord>(value).is_err());

    copied.event_copy.as_mut().unwrap().ancestor_session_id = copied.session_id;
    assert!(matches!(
        copied.validate_contract(),
        Err(CoreRecordError::InvalidEventCopy)
    ));
}

#[test]
fn activity_metadata_accepts_exact_boundaries_and_rejects_max_plus_one() {
    let exact = "x".repeat(MAX_TEXT_METADATA_BYTES);
    let mut bounded = activity();
    let invocation = bounded.invocation.as_mut().unwrap();
    invocation.protocol = Some(exact.clone());
    invocation.server = Some(exact.clone());
    invocation.tool = exact.clone();
    bounded.result.as_mut().unwrap().status = Some(exact);

    let mut record = record();
    record.content.activity = Some(bounded.clone());
    record.validate_contract().unwrap();

    for (component, expected_field) in [
        ("protocol", "activity.invocation.protocol"),
        ("server", "activity.invocation.server"),
        ("tool", "activity.invocation.tool"),
        ("status", "activity.result.status"),
    ] {
        let mut oversized = bounded.clone();
        let value = "x".repeat(MAX_TEXT_METADATA_BYTES + 1);
        match component {
            "protocol" => oversized.invocation.as_mut().unwrap().protocol = Some(value),
            "server" => oversized.invocation.as_mut().unwrap().server = Some(value),
            "tool" => oversized.invocation.as_mut().unwrap().tool = value,
            "status" => oversized.result.as_mut().unwrap().status = Some(value),
            _ => unreachable!(),
        }
        record.content.activity = Some(oversized);
        assert!(matches!(
            record.validate_contract(),
            Err(CoreRecordError::FieldTooLarge {
                field,
                actual,
                maximum,
            }) if field == expected_field
                && actual == MAX_TEXT_METADATA_BYTES + 1
                && maximum == MAX_TEXT_METADATA_BYTES
        ));
    }
}

#[test]
fn optional_metadata_text_admission_omits_only_invalid_values() {
    assert_eq!(admit_optional_metadata_text(None), None);
    assert_eq!(admit_optional_metadata_text(Some(String::new())), None);
    assert_eq!(
        admit_optional_metadata_text(Some(" \t".to_owned())),
        Some(" \t".to_owned())
    );

    let exact = "x".repeat(MAX_TEXT_METADATA_BYTES);
    assert_eq!(
        admit_optional_metadata_text(Some(exact.clone())),
        Some(exact)
    );
    assert_eq!(
        admit_optional_metadata_text(Some("x".repeat(MAX_TEXT_METADATA_BYTES + 1))),
        None
    );
}

#[test]
fn optional_provider_call_id_admission_uses_typed_key_boundaries() {
    assert_eq!(admit_optional_provider_call_id(None), None);
    assert_eq!(admit_optional_provider_call_id(Some(String::new())), None);
    assert_eq!(
        admit_optional_provider_call_id(Some(" \t".to_owned())),
        Some(TypedKey::Utf8(" \t".to_owned()))
    );

    // UTF-8 typed keys add a one-byte tag and an eight-byte length prefix.
    let exact = "x".repeat(MAX_TEXT_METADATA_BYTES - 9);
    assert_eq!(
        admit_optional_provider_call_id(Some(exact.clone())),
        Some(TypedKey::Utf8(exact.clone()))
    );
    assert!(TypedKey::Utf8(exact).validate_contract().is_ok());
    assert_eq!(
        admit_optional_provider_call_id(Some("x".repeat(MAX_TEXT_METADATA_BYTES - 8))),
        None
    );
}

#[test]
fn provider_declared_fact_admission_enforces_value_and_count_boundaries() {
    assert_eq!(
        admit_provider_declared_fact(LiteralFactKind::Command, String::new(), 0),
        None
    );
    assert_eq!(
        admit_provider_declared_fact(LiteralFactKind::Command, " \t".to_owned(), 0),
        Some(ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: " \t".to_owned(),
        })
    );

    let exact = "x".repeat(MAX_TEXT_METADATA_BYTES);
    assert_eq!(
        admit_provider_declared_fact(LiteralFactKind::Command, exact.clone(), 0),
        Some(ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: exact.clone(),
        })
    );
    let mut complete = record();
    complete.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: exact,
        }],
    });
    complete.validate_contract().unwrap();
    assert_eq!(
        admit_provider_declared_fact(
            LiteralFactKind::Command,
            "x".repeat(MAX_TEXT_METADATA_BYTES + 1),
            0,
        ),
        None
    );
    assert!(admit_provider_declared_fact(
        LiteralFactKind::Command,
        "command".to_owned(),
        MAX_PROVIDER_DECLARED_FACTS - 1,
    )
    .is_some());
    assert_eq!(
        admit_provider_declared_fact(
            LiteralFactKind::Command,
            "command".to_owned(),
            MAX_PROVIDER_DECLARED_FACTS,
        ),
        None
    );
}

#[test]
fn optional_facts_are_omitted_against_the_complete_selected_content_budget() {
    let mut record = record();
    record.content.normalized_body = Some("b".repeat(MAX_CORE_CONTENT_BYTES - 32));
    record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: "optional command".to_owned(),
        }],
    });

    assert_eq!(
        record
            .content
            .omit_provider_declared_facts_if_aggregate_exceeds_limit()
            .unwrap(),
        1
    );
    assert!(record.content.activity.is_none());
    record.validate_contract().unwrap();
}

#[test]
fn activity_linkage_and_content_policy_fail_closed() {
    let mut record = record();
    let mut unlinked = activity();
    unlinked.provider_call_id = None;
    record.content.activity = Some(unlinked);
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidActivity)
    ));

    record.content.activity = Some(activity());
    record.content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "provider policy".to_owned(),
    };
    record.content.normalized_body = None;
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidContentPolicyState)
    ));
}

#[test]
fn record_leaf_is_byte_exact_and_contract_fingerprint_is_rotated() {
    let mut first = record();
    first.content.activity = Some(activity());
    let mut second = first.clone();
    second.content.activity.as_mut().unwrap().facts.swap(0, 1);

    assert_ne!(
        core_record_leaf_sha256(&first).unwrap(),
        core_record_leaf_sha256(&second).unwrap()
    );
    let fingerprint = core_record_contract_fingerprint();
    assert_eq!(
        fingerprint,
        "ebb5c9b638de184824a6ce141ebf9b70941fb293fc113d29e2851565bad4371e"
    );
    assert_ne!(
        fingerprint,
        "1d0a6cea575cf79eb8dbad9bebed1e54cccce2fd7a59944a4c2ff448cc34ecf3"
    );

    let current = CoreContractRevisions::current();
    for changed in [
        CoreContractRevisions {
            record: current.record + 1,
            ..current
        },
        CoreContractRevisions {
            normalization: current.normalization + 1,
            ..current
        },
        CoreContractRevisions {
            content_policy: current.content_policy + 1,
            ..current
        },
        CoreContractRevisions {
            activity: current.activity + 1,
            ..current
        },
        CoreContractRevisions {
            relationship: current.relationship + 1,
            ..current
        },
        CoreContractRevisions {
            accumulator_identity: b"ctx-core-record-event-binding-v2\0",
            ..current
        },
    ] {
        assert_ne!(
            core_record_contract_fingerprint_for(current),
            core_record_contract_fingerprint_for(changed)
        );
    }
}

#[test]
fn annotation_contains_only_neutral_provider_content() {
    let annotation = CoreRecordAnnotation::default();
    assert!(annotation.activity.is_none());
    assert!(annotation.structured_content.is_none());
}
