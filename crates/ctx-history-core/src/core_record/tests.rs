use super::*;
use crate::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

fn source() -> SourceKey {
    source_named("core-record-test")
}

fn source_named(native_key: &str) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(native_key).unwrap()).unwrap(),
    )
    .unwrap()
}

fn operation_core_ids(
    source: &SourceKey,
    native_session: &str,
    native_event: u64,
) -> (StableEntityId, StableEntityId) {
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id(
            "session",
            TypedKey::utf8(native_session).unwrap(),
        )
        .unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(native_event)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    (session_id, event_id)
}

fn record() -> CoreRecord {
    let source = source();
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8("session").unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        session_relationship: SessionRelationshipKind::Root,
        event_origin: EventOrigin::Unknown,
        source,
        provider_session_id: Some("session".to_owned()),
        native_event_id: Some(TypedKey::U64(1)),
        event_sequence: 1,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
        mcp_tool_call: None,
        role: Some("user".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        workspace: Some("ctx".to_owned()),
        branch: Some("main".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        parser_revision: "codex-parser-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some("complete body".to_owned()),
            structured_content: Some(serde_json::json!({"type": "message"})),
            discovery_exclusion: None,
            mcp_exchange: None,
        },
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings: Vec::new(),
        repository_abstentions: Vec::new(),
        repository_file_invocation_evidence: Vec::new(),
        repository_file_observations: Vec::new(),
        repository_vcs_observations: Vec::new(),
    }
}

#[test]
fn selected_constructor_defaults_the_active_core_contract() {
    let fixture = record();
    let constructed = CoreRecord::new_selected(
        fixture.event_id,
        fixture.session_id,
        fixture.root_session_id,
        fixture.source.clone(),
        fixture.event_sequence,
        fixture.event_type.clone(),
        fixture.agent_type.clone(),
        fixture.is_primary,
        "provider-parser-v7",
        "complete selected body",
    )
    .unwrap();

    assert_eq!(constructed.record_version, CORE_RECORD_VERSION);
    assert_eq!(
        constructed.session_relationship,
        SessionRelationshipKind::Root
    );
    assert_eq!(constructed.event_origin, EventOrigin::Unknown);
    assert_eq!(constructed.root_session_id, constructed.session_id);
    assert!(constructed.parent_session_id.is_none());
    assert!(constructed.is_primary);
    assert_eq!(
        constructed.normalization_revision,
        CORE_NORMALIZATION_REVISION
    );
    assert_eq!(
        constructed.content.policy_revision,
        CORE_CONTENT_POLICY_REVISION
    );
    assert_eq!(constructed.parser_revision, "provider-parser-v7");
    assert!(constructed.mcp_tool_call.is_none());
    assert!(constructed.content.is_discovery_eligible());
    assert_eq!(
        constructed.content.normalized_body.as_deref(),
        Some("complete selected body")
    );
    assert!(constructed.metadata.is_empty());
    assert!(constructed.repository_bindings.is_empty());
    assert!(constructed.repository_file_invocation_evidence.is_empty());
}

fn related_session_id(label: &str) -> StableEntityId {
    let source = source();
    let native_session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8(label).unwrap()).unwrap();
    derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap()
}

fn related_event_id(session_id: StableEntityId, sequence: u64) -> StableEntityId {
    let source = source();
    let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence)).unwrap();
    derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap()
}

#[test]
fn selected_constructor_preserves_an_existing_non_primary_child_projection() {
    let child_session_id = related_session_id("compatibility-child");
    let parent_session_id = related_session_id("compatibility-parent");
    let event_id = related_event_id(child_session_id, 2);
    let mut child = CoreRecord::new_selected(
        event_id,
        child_session_id,
        child_session_id,
        source(),
        2,
        "message",
        "subagent",
        true,
        "provider-parser-v7",
        "child body",
    )
    .unwrap();

    child
        .set_session_relationship(
            SessionRelationshipKind::Delegated,
            Some(parent_session_id),
            parent_session_id,
        )
        .unwrap();
    child.validate_contract().unwrap();
    assert_eq!(child.parent_session_id, Some(parent_session_id));
    assert_eq!(child.root_session_id, parent_session_id);
    assert!(!child.is_primary);
}

#[test]
fn relationship_setter_updates_every_projection_atomically() {
    let mut record = record();
    let parent = related_session_id("parent");
    let root = related_session_id("root");

    record
        .set_session_relationship(SessionRelationshipKind::Delegated, Some(parent), root)
        .unwrap();
    assert_eq!(record.parent_session_id, Some(parent));
    assert_eq!(record.root_session_id, root);
    assert_eq!(
        record.session_relationship,
        SessionRelationshipKind::Delegated
    );
    assert!(!record.is_primary);
    record.validate_contract().unwrap();

    let before = record.clone();
    assert!(matches!(
        record.set_session_relationship(SessionRelationshipKind::Forked, None, root),
        Err(CoreRecordError::InvalidSessionRelationship)
    ));
    assert_eq!(record, before);
}

#[test]
fn relationship_validation_rejects_drift_and_self_parent_edges() {
    let parent = related_session_id("parent");
    let mut record = record();
    record
        .set_session_relationship(
            SessionRelationshipKind::RelatedUnknown,
            Some(parent),
            parent,
        )
        .unwrap();
    assert!(record.is_primary);

    record.is_primary = false;
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidSessionRelationship)
    ));
    record.is_primary = true;
    record.parent_session_id = Some(record.session_id);
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidSessionRelationship)
    ));
}

#[test]
fn event_origin_has_an_exact_fail_closed_wire_shape() {
    let ancestor_session_id = related_session_id("ancestor");
    let ancestor_event_id = related_event_id(ancestor_session_id, 41);
    let mut record = record();
    record.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor_session_id),
        ancestor_event_id: Box::new(ancestor_event_id),
        proof: EventCopyProofKind::NativeCopiedFromField,
    };
    let encoded = record.encode_stored().unwrap();
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), record);
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        wire["event_origin"],
        serde_json::json!({
            "kind": "copied_from_ancestor",
            "ancestor_session_id": ancestor_session_id,
            "ancestor_event_id": ancestor_event_id,
            "proof": "native_copied_from_field"
        })
    );

    for malformed in [
        serde_json::json!({"kind": "copied_from_ancestor", "ancestor_session_id": ancestor_session_id, "ancestor_event_id": ancestor_event_id, "proof": "body_similarity"}),
        serde_json::json!({"kind": "copied_from_ancestor", "ancestor_session_id": ancestor_session_id, "ancestor_event_id": ancestor_event_id, "proof": "native_event_identity", "provider_payload": true}),
        serde_json::json!({"kind": "assumed_unique"}),
    ] {
        let mut candidate = wire.clone();
        candidate["event_origin"] = malformed;
        assert!(matches!(
            CoreRecord::decode_stored(&serde_json::to_vec(&candidate).unwrap()),
            Err(CoreRecordError::Json(_))
        ));
    }
}

#[test]
fn copied_origin_rejects_self_references() {
    let mut record = record();
    record.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(record.session_id),
        ancestor_event_id: Box::new(record.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidEventOrigin)
    ));
}

#[test]
fn copied_origin_rejects_asserted_repository_outcomes_and_commit_operations() {
    let scope = record();
    let repository = binding();
    let mut operation_outcome = outcome(RepositoryOutcomeKind::Commit);
    let mappings = vec![RepositoryCommitMapping {
        source: oid('b'),
        result: oid('a'),
    }];
    let mut operation = RepositoryCommitOperationEvent::repository_verified_yield(
        &operation_outcome.linkage,
        RepositoryCommitOperationKind::Amend,
        mappings,
        Some(oid('b')),
        None,
        oid('a'),
        [8; 32],
    )
    .unwrap();
    operation
        .bind_scoped_identity(
            &scope.source,
            scope.event_id,
            scope.session_id,
            &repository,
            &operation_outcome.linkage,
        )
        .unwrap();
    operation_outcome.produced_object_ids.clear();
    operation_outcome.commit_operation = Some(operation);

    let ancestor_session_id = related_session_id("copied-outcome-ancestor");
    let ancestor_event_id = related_event_id(ancestor_session_id, 42);
    for asserted_outcome in [outcome(RepositoryOutcomeKind::Commit), operation_outcome] {
        let mut candidate = record();
        candidate.repository_bindings.push(repository.clone());
        candidate
            .repository_vcs_observations
            .push(RepositoryVcsObservation {
                repository_binding_id: repository.binding_id.clone(),
                kind: RepositoryVcsObservationKind::Outcome(Box::new(asserted_outcome)),
                object_id: None,
                parent_object_ids: Vec::new(),
                reference: None,
                relative_path: None,
            });
        candidate.validate_contract().unwrap();

        candidate.event_origin = EventOrigin::CopiedFromAncestor {
            ancestor_session_id: Box::new(ancestor_session_id),
            ancestor_event_id: Box::new(ancestor_event_id),
            proof: EventCopyProofKind::NativeCopiedFromField,
        };
        assert!(matches!(
            candidate.validate_contract(),
            Err(CoreRecordError::InvalidRepositoryOutcome)
        ));
    }
}

fn mcp_tool_call(server: impl Into<String>, tool: impl Into<String>) -> McpToolCallAttribution {
    McpToolCallAttribution {
        server: server.into(),
        tool: tool.into(),
    }
}

#[test]
fn discovery_exclusion_is_optional_typed_and_fail_closed_on_noncanonical_wire_values() {
    let absent = record();
    assert!(absent.content.is_discovery_eligible());
    let absent_wire = serde_json::to_value(&absent).unwrap();
    assert!(!absent_wire["content"]
        .as_object()
        .unwrap()
        .contains_key("discovery_exclusion"));

    let mut excluded = absent;
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    assert!(!excluded.content.is_discovery_eligible());
    let encoded = excluded.encode_stored().unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        wire["content"]["discovery_exclusion"],
        serde_json::Value::String("ctx_retrieval_derived".to_owned())
    );
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), excluded);

    for invalid in [
        serde_json::Value::Null,
        serde_json::Value::String("future_reason".to_owned()),
    ] {
        let mut wire = serde_json::to_value(record()).unwrap();
        wire["content"]
            .as_object_mut()
            .unwrap()
            .insert("discovery_exclusion".to_owned(), invalid);
        assert!(matches!(
            CoreRecord::decode_stored(&serde_json::to_vec(&wire).unwrap()),
            Err(CoreRecordError::Json(_))
        ));
    }
}

#[test]
fn discovery_exclusion_requires_complete_policy_selected_content() {
    for status in [
        CoreContentPolicyStatus::Redacted {
            reason: "sensitive".to_owned(),
        },
        CoreContentPolicyStatus::Omitted {
            reason: "policy".to_owned(),
        },
    ] {
        let mut excluded = record();
        excluded.content.policy_status = status;
        excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
        if matches!(
            excluded.content.policy_status,
            CoreContentPolicyStatus::Omitted { .. }
        ) {
            excluded.content.normalized_body = None;
            excluded.content.structured_content = None;
        }
        assert!(matches!(
            excluded.validate_contract(),
            Err(CoreRecordError::InvalidContentPolicyState)
        ));
    }
}

#[test]
fn discovery_exclusion_changes_stored_content_but_not_stable_identity_or_lineage() {
    let baseline = record();
    let mut excluded = baseline.clone();
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);

    assert_eq!(excluded.event_id, baseline.event_id);
    assert_eq!(excluded.session_id, baseline.session_id);
    assert_eq!(excluded.parent_session_id, baseline.parent_session_id);
    assert_eq!(excluded.root_session_id, baseline.root_session_id);
    assert_eq!(excluded.session_relationship, baseline.session_relationship);
    assert_eq!(excluded.event_origin, baseline.event_origin);
    assert_eq!(excluded.source, baseline.source);
    assert_ne!(
        core_record_leaf_sha256(&excluded).unwrap(),
        core_record_leaf_sha256(&baseline).unwrap()
    );
}

fn mcp_exchange() -> McpExchangeContent {
    McpExchangeContent {
        provider_call_id: "call-1".to_owned(),
        invocation: Some(McpInvocationContent {
            server: "filesystem".to_owned(),
            tool: "read_file".to_owned(),
            arguments: McpJsonCapture::Present {
                value: serde_json::json!({"path": "/work/ctx/README.md"}),
            },
        }),
        response: Some(McpTerminalResponseContent {
            status: McpTerminalStatus::Succeeded,
            failure_kind: None,
            duration_ns: Some(42),
            text: McpTextCapture::NormalizedBody,
            payload: McpJsonCapture::Present {
                value: serde_json::json!({"content": "hello"}),
            },
        }),
    }
}

fn binding() -> RepositoryBinding {
    RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "repo-1".to_owned(),
        checkout_id: Some("checkout-1".to_owned()),
        worktree_id: Some("worktree-1".to_owned()),
        aliases: vec![RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: "ctx".to_owned(),
            remote_name: Some("origin".to_owned()),
        }],
        git_object_format: Some(GitObjectFormat::Sha1),
        local_root_authorization: Some(RepositoryLocalRootAuthorization {
            local_root: "/work/ctx".to_owned(),
            local_root_authorization_fingerprint_revision:
                CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
            local_root_authorization_fingerprint: [7; 32],
            observed_at_unix_ms: 1_700_000_000_000,
        }),
        evidence: vec![
            RepositoryEvidence {
                kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
                confidence: RepositoryEvidenceConfidence::Explicit,
            },
            RepositoryEvidence {
                kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
                confidence: RepositoryEvidenceConfidence::High,
            },
        ],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn invocation(
    operation_ordinal: u32,
    kind: RepositoryFileInvocationKind,
    relative_path: &str,
) -> RepositoryFileInvocationEvidence {
    RepositoryFileInvocationEvidence {
        operation_ordinal,
        repository_binding_id: "binding-1".to_owned(),
        relative_path: relative_path.to_owned(),
        prior_relative_path: None,
        kind,
        tool_name: Some("read_file".to_owned()),
        normalized_text_range: None,
    }
}

#[test]
fn complete_record_round_trips_stored_encoding() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record.content.normalized_body = Some("prefix α suffix".to_owned());
    let mut evidence = invocation(0, RepositoryFileInvocationKind::Read, "src/lib.rs");
    evidence.normalized_text_range = Some(RepositoryFileInvocationTextRange { start: 7, end: 9 });
    record.repository_file_invocation_evidence.push(evidence);
    let encoded = record.encode_stored().unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    let authorization = wire["repository_bindings"][0]["local_root_authorization"]
        .as_object()
        .unwrap();
    assert!(authorization.contains_key("local_root_authorization_fingerprint_revision"));
    assert!(authorization.contains_key("local_root_authorization_fingerprint"));
    assert_eq!(
        wire["repository_file_invocation_evidence"][0]["normalized_text_range"],
        serde_json::json!({"start": 7, "end": 9})
    );
    let invocation = wire["repository_file_invocation_evidence"][0]
        .as_object()
        .unwrap();
    assert!(!invocation.contains_key("body"));
    assert!(!invocation.contains_key("preview"));
    assert!(!invocation.contains_key("text"));
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), record);
}

#[test]
fn mcp_tool_call_uses_the_exact_optional_canonical_wire_shape() {
    let absent = serde_json::to_value(record()).unwrap();
    assert!(!absent.as_object().unwrap().contains_key("mcp_tool_call"));
    assert!(
        CoreRecord::decode_stored(&serde_json::to_vec(&absent).unwrap())
            .unwrap()
            .mcp_tool_call
            .is_none()
    );

    let mut attributed = record();
    attributed.mcp_tool_call = Some(mcp_tool_call("filesystem", "read_file"));
    let encoded = attributed.encode_stored().unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        wire["mcp_tool_call"],
        serde_json::json!({"server": "filesystem", "tool": "read_file"})
    );
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), attributed);

    let mut explicit_null = absent.clone();
    explicit_null
        .as_object_mut()
        .unwrap()
        .insert("mcp_tool_call".to_owned(), serde_json::Value::Null);
    assert!(matches!(
        CoreRecord::decode_stored(&serde_json::to_vec(&explicit_null).unwrap()),
        Err(CoreRecordError::Json(_))
    ));

    let mut unknown_field = absent;
    unknown_field.as_object_mut().unwrap().insert(
        "mcp_tool_call".to_owned(),
        serde_json::json!({
            "server": "filesystem",
            "tool": "read_file",
            "provider": "must-not-be-stored"
        }),
    );
    assert!(matches!(
        CoreRecord::decode_stored(&serde_json::to_vec(&unknown_field).unwrap()),
        Err(CoreRecordError::Json(_))
    ));

    for incomplete in [
        serde_json::json!({"tool": "read_file"}),
        serde_json::json!({"server": "filesystem"}),
    ] {
        let mut wire = serde_json::to_value(record()).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("mcp_tool_call".to_owned(), incomplete);
        assert!(matches!(
            CoreRecord::decode_stored(&serde_json::to_vec(&wire).unwrap()),
            Err(CoreRecordError::Json(_))
        ));
    }
}

#[test]
fn mcp_exchange_uses_an_optional_content_governed_wire_shape() {
    let absent = serde_json::to_value(record()).unwrap();
    assert!(!absent["content"]
        .as_object()
        .unwrap()
        .contains_key("mcp_exchange"));

    let mut captured = record();
    captured.content.mcp_exchange = Some(mcp_exchange());
    let encoded = captured.encode_stored().unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        wire["content"]["mcp_exchange"],
        serde_json::json!({
            "provider_call_id": "call-1",
            "invocation": {
                "server": "filesystem",
                "tool": "read_file",
                "arguments": {
                    "capture_status": "present",
                    "value": {"path": "/work/ctx/README.md"}
                }
            },
            "response": {
                "status": "succeeded",
                "duration_ns": 42,
                "text": {"capture_status": "normalized_body"},
                "payload": {
                    "capture_status": "present",
                    "value": {"content": "hello"}
                }
            }
        })
    );
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), captured);

    let mut explicit_null = absent;
    explicit_null["content"]
        .as_object_mut()
        .unwrap()
        .insert("mcp_exchange".to_owned(), serde_json::Value::Null);
    assert!(matches!(
        CoreRecord::decode_stored(&serde_json::to_vec(&explicit_null).unwrap()),
        Err(CoreRecordError::Json(_))
    ));
}

#[test]
fn mcp_exchange_rejects_noncanonical_explicit_null_optional_members() {
    let record = {
        let mut record = record();
        record.content.mcp_exchange = Some(mcp_exchange());
        serde_json::to_value(record).unwrap()
    };
    for path in [
        &["content", "mcp_exchange", "invocation"][..],
        &["content", "mcp_exchange", "response"][..],
        &["content", "mcp_exchange", "response", "duration_ns"][..],
    ] {
        let mut wire = record.clone();
        let (member, parents) = path.split_last().unwrap();
        let mut parent = &mut wire;
        for key in parents {
            parent = parent.get_mut(*key).unwrap();
        }
        parent
            .as_object_mut()
            .unwrap()
            .insert((*member).to_owned(), serde_json::Value::Null);
        assert!(matches!(
            CoreRecord::decode_stored(&serde_json::to_vec(&wire).unwrap()),
            Err(CoreRecordError::Json(_))
        ));
    }

    let mut failed = record;
    failed["content"]["mcp_exchange"]["response"]["status"] =
        serde_json::Value::String("failed".to_owned());
    failed["content"]["mcp_exchange"]["response"]
        .as_object_mut()
        .unwrap()
        .insert("failure_kind".to_owned(), serde_json::Value::Null);
    assert!(matches!(
        CoreRecord::decode_stored(&serde_json::to_vec(&failed).unwrap()),
        Err(CoreRecordError::Json(_))
    ));
}

#[test]
fn mcp_exchange_requires_a_nonempty_side_and_object_arguments() {
    let mut empty = record();
    empty.content.mcp_exchange = Some(McpExchangeContent {
        provider_call_id: "call-1".to_owned(),
        invocation: None,
        response: None,
    });
    assert!(matches!(
        empty.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));

    let mut scalar_arguments = record();
    let mut exchange = mcp_exchange();
    exchange.invocation.as_mut().unwrap().arguments = McpJsonCapture::Present {
        value: serde_json::json!(["not", "an", "object"]),
    };
    scalar_arguments.content.mcp_exchange = Some(exchange);
    assert!(matches!(
        scalar_arguments.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));
}

#[test]
fn mcp_exchange_terminal_state_and_body_reference_are_consistent() {
    let mut missing_failure_kind = record();
    let mut exchange = mcp_exchange();
    exchange.response.as_mut().unwrap().status = McpTerminalStatus::Failed;
    missing_failure_kind.content.mcp_exchange = Some(exchange);
    assert!(matches!(
        missing_failure_kind.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));

    let mut stray_failure_kind = record();
    let mut exchange = mcp_exchange();
    exchange.response.as_mut().unwrap().failure_kind = Some(McpFailureKind::Unknown);
    stray_failure_kind.content.mcp_exchange = Some(exchange);
    assert!(matches!(
        stray_failure_kind.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));

    let mut missing_body = record();
    missing_body.content.normalized_body = None;
    missing_body.content.mcp_exchange = Some(mcp_exchange());
    assert!(matches!(
        missing_body.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));
}

#[test]
fn mcp_exchange_invocation_agrees_with_projection_independent_attribution() {
    let mut record = record();
    record.mcp_tool_call = Some(mcp_tool_call("other-server", "read_file"));
    record.content.mcp_exchange = Some(mcp_exchange());
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidMcpExchange)
    ));

    record.mcp_tool_call = Some(mcp_tool_call("filesystem", "read_file"));
    record.validate_contract().unwrap();
}

#[test]
fn nonselected_content_cannot_retain_an_mcp_exchange() {
    for status in [
        CoreContentPolicyStatus::Redacted {
            reason: "sensitive".to_owned(),
        },
        CoreContentPolicyStatus::Omitted {
            reason: "policy".to_owned(),
        },
    ] {
        let mut record = record();
        record.content.policy_status = status;
        record.content.normalized_body = None;
        record.content.structured_content = None;
        record.content.mcp_exchange = Some(mcp_exchange());
        assert!(matches!(
            record.validate_contract(),
            Err(CoreRecordError::InvalidContentPolicyState)
        ));
    }
}

#[test]
fn mcp_tool_call_accepts_exact_unicode_control_delimiter_and_whitespace_values() {
    let server = "\u{0000}\u{0001}\u{001f}\t\n\r /:\\@|,;=[]{}()<>?#!$%^&*+~`'\"—服务";
    let tool = "\u{000b}\u{000c}\u{007f} tool::/\\@|,;=[]{}()<>?#!$%^&*+~`'\"—道具";
    let mut record = record();
    record.mcp_tool_call = Some(mcp_tool_call(server, tool));

    let encoded = record.encode_stored().unwrap();
    let decoded = CoreRecord::decode_stored(&encoded).unwrap();
    assert_eq!(decoded.mcp_tool_call, record.mcp_tool_call);

    record.mcp_tool_call = Some(mcp_tool_call(" \t\n", "\u{2003}"));
    record.validate_contract().unwrap();
}

#[test]
fn mcp_tool_call_requires_both_nonempty_components() {
    for (server, tool, field) in [
        ("", "read_file", "mcp_tool_call.server"),
        ("filesystem", "", "mcp_tool_call.tool"),
    ] {
        let mut record = record();
        record.mcp_tool_call = Some(mcp_tool_call(server, tool));
        assert!(matches!(
            record.validate_contract(),
            Err(CoreRecordError::EmptyField { field: actual }) if actual == field
        ));
    }
}

#[test]
fn redacted_and_omitted_core_policy_require_mcp_tool_call_to_be_absent() {
    let mut redacted = record();
    redacted.content.policy_status = CoreContentPolicyStatus::Redacted {
        reason: "sensitive".to_owned(),
    };
    redacted.mcp_tool_call = Some(mcp_tool_call("sensitive-server", "sensitive-tool"));
    assert!(matches!(
        redacted.validate_contract(),
        Err(CoreRecordError::InvalidContentPolicyState)
    ));
    let redacted_wire = serde_json::to_vec(&redacted).unwrap();
    assert!(matches!(
        CoreRecord::decode_stored(&redacted_wire),
        Err(CoreRecordError::InvalidContentPolicyState)
    ));
    redacted.mcp_tool_call = None;
    let redacted_wire = redacted.encode_stored().unwrap();
    assert!(!serde_json::from_slice::<serde_json::Value>(&redacted_wire)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("mcp_tool_call"));

    let mut omitted = record();
    omitted.content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "sensitive".to_owned(),
    };
    omitted.content.normalized_body = None;
    omitted.content.structured_content = None;
    omitted.mcp_tool_call = Some(mcp_tool_call("sensitive-server", "sensitive-tool"));
    assert!(matches!(
        omitted.validate_contract(),
        Err(CoreRecordError::InvalidContentPolicyState)
    ));
    let omitted_wire = serde_json::to_vec(&omitted).unwrap();
    assert!(matches!(
        CoreRecord::decode_stored(&omitted_wire),
        Err(CoreRecordError::InvalidContentPolicyState)
    ));
    omitted.mcp_tool_call = None;
    let omitted_wire = omitted.encode_stored().unwrap();
    assert!(!serde_json::from_slice::<serde_json::Value>(&omitted_wire)
        .unwrap()
        .as_object()
        .unwrap()
        .contains_key("mcp_tool_call"));
}

#[test]
fn mcp_tool_call_bounds_each_decoded_utf8_component_at_exact_64_kib() {
    let exact_unicode = "🧰".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES / 4);
    assert_eq!(
        exact_unicode.len(),
        MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
    );
    let exact_ascii = "x".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES);
    let mut record = record();
    record.mcp_tool_call = Some(mcp_tool_call(exact_unicode.clone(), exact_ascii));
    let encoded = record.encode_stored().unwrap();
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), record);

    record.mcp_tool_call = Some(mcp_tool_call(format!("{exact_unicode}x"), "tool"));
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "mcp_tool_call.server",
            actual,
            maximum: MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        }) if actual == MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES + 1
    ));

    record.mcp_tool_call = Some(mcp_tool_call(
        "server",
        "x".repeat(MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES + 1),
    ));
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "mcp_tool_call.tool",
            actual,
            maximum: MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        }) if actual == MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES + 1
    ));
}

#[test]
fn mcp_tool_call_changes_stored_payload_but_not_stable_identity() {
    assert_eq!(CORE_RECORD_VERSION, 2);
    assert_eq!(crate::IDENTITY_VERSION, 1);
    assert_eq!(CORE_MCP_TOOL_CALL_ATTRIBUTION_REVISION, 1);

    let baseline = record();
    let mut attributed = baseline.clone();
    attributed.mcp_tool_call = Some(mcp_tool_call("filesystem", "read_file"));

    assert_eq!(attributed.event_id, baseline.event_id);
    assert_eq!(attributed.session_id, baseline.session_id);
    assert_eq!(attributed.root_session_id, baseline.root_session_id);
    assert_eq!(attributed.source, baseline.source);
    assert_ne!(
        core_record_leaf_sha256(&attributed).unwrap(),
        core_record_leaf_sha256(&baseline).unwrap()
    );
}

#[test]
fn core_record_annotation_defaults_mcp_tool_call_to_absent() {
    let mut annotation = CoreRecordAnnotation::default();
    assert!(annotation.mcp_tool_call.is_none());
    annotation.mcp_tool_call = Some(mcp_tool_call("server", "tool"));
    annotation
        .mcp_tool_call
        .as_ref()
        .unwrap()
        .validate_contract()
        .unwrap();
}

#[test]
fn old_stored_records_default_missing_invocation_evidence_to_empty() {
    let mut wire = serde_json::to_value(record()).unwrap();
    wire.as_object_mut()
        .unwrap()
        .remove("repository_file_invocation_evidence");
    let decoded = CoreRecord::decode_stored(&serde_json::to_vec(&wire).unwrap()).unwrap();
    assert!(decoded.repository_file_invocation_evidence.is_empty());
}

#[test]
fn core_record_has_no_locator_or_canonical_preview_field() {
    let encoded = serde_json::to_value(record()).unwrap();
    let object = encoded.as_object().unwrap();
    assert!(!object.contains_key("locator"));
    assert!(!object.contains_key("source_path"));
    assert!(!object.contains_key("body_preview"));
    assert!(!object["content"]
        .as_object()
        .unwrap()
        .contains_key("body_preview"));
}

#[test]
fn invocation_evidence_has_closed_action_names_without_unknown() {
    let cases = [
        (RepositoryFileInvocationKind::Read, "read"),
        (RepositoryFileInvocationKind::Create, "create"),
        (RepositoryFileInvocationKind::Modify, "modify"),
        (RepositoryFileInvocationKind::Delete, "delete"),
        (RepositoryFileInvocationKind::Rename, "rename"),
        (RepositoryFileInvocationKind::Write, "write"),
    ];
    for (kind, wire_name) in cases {
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            format!("\"{wire_name}\"")
        );
    }
    assert!(serde_json::from_str::<RepositoryFileInvocationKind>("\"unknown\"").is_err());
}

#[test]
fn invocation_contract_validates_binding_paths_rename_shape_and_tool_bounds() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record.repository_file_invocation_evidence = vec![invocation(
        0,
        RepositoryFileInvocationKind::Modify,
        "src/lib.rs",
    )];
    record.validate_contract().unwrap();

    record.repository_file_invocation_evidence[0].repository_binding_id = "missing".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::UnknownRepositoryBinding(binding)) if binding == "missing"
    ));
    record.repository_file_invocation_evidence[0].repository_binding_id = "binding-1".to_owned();

    record.repository_file_invocation_evidence[0].relative_path = "../outside".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryRelativePath(_))
    ));
    record.repository_file_invocation_evidence[0].relative_path = "src/lib.rs".to_owned();

    record.repository_file_invocation_evidence[0].prior_relative_path =
        Some("src/old.rs".to_owned());
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence)
    ));
    record.repository_file_invocation_evidence[0].kind = RepositoryFileInvocationKind::Rename;
    record.validate_contract().unwrap();
    record.repository_file_invocation_evidence[0].prior_relative_path = None;
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence)
    ));

    record.repository_file_invocation_evidence[0].kind = RepositoryFileInvocationKind::Modify;
    record.repository_file_invocation_evidence[0].tool_name = Some("x".repeat(513));
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "repository_file_invocation_tool_name",
            actual: 513,
            maximum: 512,
        })
    ));
}

#[test]
fn invocation_text_range_is_nonempty_bounded_and_on_utf8_boundaries() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record.content.normalized_body = Some("aαz".to_owned());
    let mut evidence = invocation(0, RepositoryFileInvocationKind::Read, "src/lib.rs");
    evidence.normalized_text_range = Some(RepositoryFileInvocationTextRange { start: 1, end: 3 });
    record.repository_file_invocation_evidence = vec![evidence];
    record.validate_contract().unwrap();

    for range in [
        RepositoryFileInvocationTextRange { start: 1, end: 1 },
        RepositoryFileInvocationTextRange { start: 2, end: 3 },
        RepositoryFileInvocationTextRange { start: 1, end: 2 },
        RepositoryFileInvocationTextRange { start: 3, end: 5 },
    ] {
        record.repository_file_invocation_evidence[0].normalized_text_range = Some(range);
        assert!(matches!(
            record.validate_contract(),
            Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence)
        ));
    }

    record.repository_file_invocation_evidence[0].normalized_text_range =
        Some(RepositoryFileInvocationTextRange { start: 1, end: 3 });
    record.content.normalized_body = None;
    record.content.structured_content = None;
    record.content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "policy".to_owned(),
    };
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryFileInvocationEvidence)
    ));
}

#[test]
fn invocation_evidence_must_be_strictly_sorted_and_unique() {
    let mut record = record();
    record.repository_bindings.push(binding());
    let first = invocation(0, RepositoryFileInvocationKind::Read, "src/a.rs");
    let second = invocation(1, RepositoryFileInvocationKind::Write, "src/b.rs");
    record.repository_file_invocation_evidence = vec![first.clone(), second.clone()];
    record.validate_contract().unwrap();

    record.repository_file_invocation_evidence = vec![second, first.clone()];
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::NonCanonicalRepositoryFileInvocationEvidence)
    ));
    record.repository_file_invocation_evidence = vec![first.clone(), first];
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::NonCanonicalRepositoryFileInvocationEvidence)
    ));
}

#[test]
fn invocation_evidence_count_is_bounded() {
    let mut record = record();
    record.repository_file_invocation_evidence =
        vec![invocation(0, RepositoryFileInvocationKind::Read, "src/a.rs"); 4_097];
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::TooManyItems {
            field: "repository_file_invocation_evidence",
            actual: 4_097,
            maximum: 4_096,
        })
    ));
}

#[test]
fn aggregate_complete_representations_share_one_content_budget() {
    let duplicate = "x".repeat((MAX_CORE_CONTENT_BYTES - 2) / 2);
    let mut record = record();
    record.content.normalized_body = Some(duplicate.clone());
    record.content.structured_content = Some(serde_json::Value::String(duplicate));

    assert_eq!(
        record.content.encoded_content_bytes().unwrap(),
        MAX_CORE_CONTENT_BYTES
    );
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();

    record.content.normalized_body.as_mut().unwrap().push('x');
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "selected_content",
            actual,
            maximum: MAX_CORE_CONTENT_BYTES,
        }) if actual == MAX_CORE_CONTENT_BYTES + 1
    ));
}

#[test]
fn streamed_content_byte_count_matches_canonical_json_for_all_representations() {
    let mut record = record();
    record.content.structured_content = Some(serde_json::json!({
        "escaped": "line one\nline two",
        "unicode": "snowman ☃",
        "items": [1, 2, 3]
    }));
    record.content.mcp_exchange = Some(mcp_exchange());

    let expected = record.content.normalized_body.as_ref().unwrap().len()
        + serde_json::to_vec(record.content.structured_content.as_ref().unwrap())
            .unwrap()
            .len()
        + serde_json::to_vec(record.content.mcp_exchange.as_ref().unwrap())
            .unwrap()
            .len();

    assert_eq!(record.content.encoded_content_bytes().unwrap(), expected);
    assert_eq!(
        record.validate_contract_and_content_bytes().unwrap(),
        expected
    );
}

#[test]
fn projectors_can_omit_duplicate_structured_content_without_losing_body_or_identity() {
    let tail = "aggregate_content_tail_survives";
    let body = format!("{}{tail}", "x".repeat(MAX_CORE_CONTENT_BYTES / 2 + 1_024));
    let mut record = record();
    record.content.normalized_body = Some(body.clone());
    record.content.structured_content = Some(serde_json::json!({
        "provider_native": {"body": &body}
    }));
    record.native_event_id = Some(TypedKey::utf8("stable-native-event").unwrap());
    let event_id = record.event_id;
    let native_event_id = record.native_event_id.clone();

    assert!(record.content.normalized_body.as_ref().unwrap().len() <= MAX_CORE_CONTENT_BYTES);
    assert!(
        serde_json::to_vec(record.content.structured_content.as_ref().unwrap())
            .unwrap()
            .len()
            <= MAX_CORE_CONTENT_BYTES
    );
    assert!(record.content.encoded_content_bytes().unwrap() > MAX_CORE_CONTENT_BYTES);

    assert!(record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()
        .unwrap());
    assert_eq!(record.event_id, event_id);
    assert_eq!(record.native_event_id, native_event_id);
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(body.as_str())
    );
    assert!(record
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .ends_with(tail));
    assert!(record.content.structured_content.is_none());
    assert!(record.content.encoded_content_bytes().unwrap() <= MAX_CORE_CONTENT_BYTES);
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}

#[test]
fn either_complete_representation_over_the_content_ceiling_is_rejected() {
    let mut oversized_body = record();
    oversized_body.content.structured_content = None;
    oversized_body.content.normalized_body = Some("x".repeat(MAX_CORE_CONTENT_BYTES + 1));
    assert!(matches!(
        oversized_body.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "normalized_body",
            actual,
            maximum: MAX_CORE_CONTENT_BYTES,
        }) if actual == MAX_CORE_CONTENT_BYTES + 1
    ));

    let mut oversized_structured = record();
    oversized_structured.content.structured_content = Some(serde_json::Value::String(
        "x".repeat(MAX_CORE_CONTENT_BYTES),
    ));
    assert!(matches!(
        oversized_structured.validate_contract(),
        Err(CoreRecordError::FieldTooLarge {
            field: "structured_content",
            actual,
            maximum: MAX_CORE_CONTENT_BYTES,
        }) if actual == MAX_CORE_CONTENT_BYTES + 2
    ));
}

#[test]
fn repository_contract_scopes_relative_observations_to_known_bindings() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "crates/ctx-history-core/src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: Some(GitObjectId {
                format: GitObjectFormat::Sha1,
                hex: "a".repeat(40),
            }),
            parent_object_ids: Vec::new(),
            reference: Some("refs/heads/main".to_owned()),
            relative_path: None,
        });
    record.validate_contract().unwrap();

    record.repository_file_observations[0].relative_path = "../private".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryRelativePath(_))
    ));
}

#[test]
fn repository_contract_rejects_unscoped_and_mismatched_observations() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "missing-binding".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Read,
            prior_relative_path: None,
        });
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::UnknownRepositoryBinding(binding))
            if binding == "missing-binding"
    ));

    record.repository_file_observations.clear();
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: Some(GitObjectId {
                format: GitObjectFormat::Sha256,
                hex: "b".repeat(64),
            }),
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidGitObjectId)
    ));
}

#[test]
fn missing_candidate_reuses_only_the_exact_prior_certificate_and_revokes_local_access() {
    let mut prior = record();
    prior.repository_candidate_evidence.insert(
        RepositoryCandidateKind::DeclaredToolWorkdir,
        "/old/repo".to_owned(),
    );
    prior.repository_bindings.push(binding());
    let mut prior_invocation = invocation(3, RepositoryFileInvocationKind::Modify, "src/lib.rs");
    prior_invocation.normalized_text_range =
        Some(RepositoryFileInvocationTextRange { start: 0, end: 8 });
    prior
        .repository_file_invocation_evidence
        .push(prior_invocation);
    prior
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    let mut current = record();
    current.repository_candidate_evidence.insert(
        RepositoryCandidateKind::DeclaredToolWorkdir,
        "/old/repo".to_owned(),
    );
    current.repository_abstentions.push(RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
        reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
        detail: Some("candidate_missing_before_certification".to_owned()),
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    });

    assert!(current.needs_prior_repository_certificate());
    assert!(current.reuse_prior_repository_certificate(&prior));
    assert_eq!(current.repository_bindings.len(), 1);
    assert!(current.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert_eq!(current.repository_file_observations.len(), 1);
    assert_eq!(current.repository_file_invocation_evidence.len(), 1);
    assert_eq!(
        current.repository_file_invocation_evidence[0].operation_ordinal,
        3
    );
    assert!(current.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::Unavailable
            && abstention.detail.as_deref()
                == Some("prior_certificate_reused_without_local_authorization")
    }));
    current.validate_contract().unwrap();

    let mut wrong_source = record();
    wrong_source.source = SourceKey::derive(
        "codex",
        "different_codex_format",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("core-record-test").unwrap())
            .unwrap(),
    )
    .unwrap();
    wrong_source.repository_abstentions = vec![RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::SessionCwd,
        reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
        detail: None,
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }];
    assert!(!wrong_source.reuse_prior_repository_certificate(&prior));
}

#[test]
fn prior_repository_certificate_is_bound_to_exact_generation_inputs() {
    let mut prior = record();
    prior.repository_candidate_evidence.insert(
        RepositoryCandidateKind::DeclaredToolWorkdir,
        "/old/repo".to_owned(),
    );
    prior.repository_bindings.push(binding());

    let missing = || {
        let mut current = record();
        current.repository_candidate_evidence.insert(
            RepositoryCandidateKind::DeclaredToolWorkdir,
            "/old/repo".to_owned(),
        );
        current.repository_abstentions.push(RepositoryAbstention {
            evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
            detail: Some("candidate_missing_before_certification".to_owned()),
            association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        });
        current
    };

    let mut moved_after_certification = missing();
    assert!(moved_after_certification.reuse_prior_repository_certificate(&prior));

    let mut changed_parser = missing();
    changed_parser.parser_revision.push_str("-changed");
    assert!(!changed_parser.reuse_prior_repository_certificate(&prior));

    let mut changed_content = missing();
    changed_content.content.normalized_body = Some("changed command".to_owned());
    assert!(!changed_content.reuse_prior_repository_certificate(&prior));

    let mut changed_command_digest = missing();
    changed_command_digest.content.structured_content = Some(serde_json::json!({
        "provider_native_tool": {"command_sha256": "changed"}
    }));
    assert!(!changed_command_digest.reuse_prior_repository_certificate(&prior));

    let mut changed_mcp_tool_call = missing();
    changed_mcp_tool_call.mcp_tool_call = Some(mcp_tool_call("filesystem", "read_file"));
    assert!(!changed_mcp_tool_call.reuse_prior_repository_certificate(&prior));

    let mut changed_candidate = missing();
    changed_candidate.repository_candidate_evidence = RepositoryCandidateEvidence::default();
    changed_candidate.repository_candidate_evidence.insert(
        RepositoryCandidateKind::DeclaredToolWorkdir,
        "/different/repo".to_owned(),
    );
    assert!(!changed_candidate.reuse_prior_repository_certificate(&prior));
}

#[test]
fn streamed_repository_reuse_fingerprint_matches_canonical_json() {
    let mut record = record();
    record.content.structured_content = Some(serde_json::json!({
        "escaped": "line one\nline two",
        "unicode": "snowman ☃"
    }));
    record.content.mcp_exchange = Some(mcp_exchange());

    let encoded = serde_json::to_vec(&RepositoryReuseInput::from(&record)).unwrap();
    let mut expected = Sha256::new();
    expected.update(CORE_REPOSITORY_REUSE_INPUT_DOMAIN);
    expected.update(CORE_REPOSITORY_CONTRACT_REVISION.to_be_bytes());
    expected.update(u64::try_from(encoded.len()).unwrap().to_be_bytes());
    expected.update(encoded);

    assert_eq!(
        record.repository_reuse_input_fingerprint().unwrap(),
        <[u8; 32]>::from(expected.finalize())
    );
}

#[test]
fn logical_repository_binding_can_abstain_from_local_checkout_identity() {
    let mut record = record();
    let mut moved = binding();
    moved.local_root_authorization = None;
    record.repository_bindings.push(moved);
    record.validate_contract().unwrap();
    record.repository_bindings.clear();

    let mut logical_only = binding();
    logical_only.checkout_id = None;
    logical_only.worktree_id = None;
    logical_only.git_object_format = None;
    logical_only.local_root_authorization = None;
    logical_only.evidence = vec![RepositoryEvidence {
        kind: RepositoryEvidenceKind::ProviderNativeProject,
        confidence: RepositoryEvidenceConfidence::Explicit,
    }];
    record.repository_bindings.push(logical_only);
    record.validate_contract().unwrap();

    record.repository_bindings[0].local_root_authorization =
        Some(RepositoryLocalRootAuthorization {
            local_root: "/uncertified/checkout".to_owned(),
            local_root_authorization_fingerprint_revision:
                CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
            local_root_authorization_fingerprint: [9; 32],
            observed_at_unix_ms: 1_700_000_000_000,
        });
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidIdentityRelationship)
    ));
}

#[test]
fn object_observation_requires_binding_object_format() {
    let mut record = record();
    let mut logical_only = binding();
    logical_only.git_object_format = None;
    record.repository_bindings.push(logical_only);
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Commit,
            object_id: Some(GitObjectId {
                format: GitObjectFormat::Sha1,
                hex: "c".repeat(40),
            }),
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidGitObjectId)
    ));
}

#[test]
fn repository_abstention_preserves_evidence_kind() {
    let mut record = record();
    for (kind, path) in [
        (RepositoryCandidateKind::SessionCwd, "/control/workspace"),
        (RepositoryCandidateKind::DeclaredToolWorkdir, "/code/repo"),
        (
            RepositoryCandidateKind::DerivedEffectiveCwd,
            "/code/repo/crates",
        ),
        (
            RepositoryCandidateKind::CommandSpecificRepositoryPath,
            "/code/other",
        ),
        (
            RepositoryCandidateKind::OutcomeOperationRepositoryPath,
            "/code/repo",
        ),
        (
            RepositoryCandidateKind::OutcomeOutputRepositoryPath,
            "/code/repo",
        ),
    ] {
        record
            .repository_candidate_evidence
            .insert(kind, path.to_owned());
    }
    record.repository_abstentions.push(RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
        reason: RepositoryAbstentionReason::AmbiguousCandidates,
        detail: Some("multiple certified boundaries".to_owned()),
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    });
    let encoded = record.encode_stored().unwrap();
    let decoded = CoreRecord::decode_stored(&encoded).unwrap();
    assert_eq!(
        decoded.repository_abstentions[0].evidence_kind,
        RepositoryEvidenceKind::DerivedEffectiveCwd
    );
    assert_eq!(
        decoded
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::DeclaredToolWorkdir)
            .collect::<Vec<_>>(),
        vec!["/code/repo"]
    );
    assert_eq!(
        decoded
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::DerivedEffectiveCwd)
            .collect::<Vec<_>>(),
        vec!["/code/repo/crates"]
    );
}

#[test]
fn repository_candidate_evidence_is_a_complete_order_independent_set() {
    let mut forward = RepositoryCandidateEvidence::default();
    let mut reverse = RepositoryCandidateEvidence::default();
    let candidates = [
        (
            RepositoryCandidateKind::FileActivityPath,
            "/repos/a/src/a.rs",
        ),
        (
            RepositoryCandidateKind::FileActivityPath,
            "/repos/b/src/b.rs",
        ),
        (
            RepositoryCandidateKind::CommandSpecificRepositoryPath,
            "/repos/a",
        ),
        (
            RepositoryCandidateKind::CommandSpecificRepositoryPath,
            "/repos/b",
        ),
    ];
    for (kind, path) in candidates {
        forward.insert(kind, path.to_owned());
    }
    for (kind, path) in candidates.into_iter().rev() {
        reverse.insert(kind, path.to_owned());
    }
    forward.insert(
        RepositoryCandidateKind::FileActivityPath,
        "/repos/a/src/a.rs".to_owned(),
    );

    assert_eq!(forward, reverse);
    assert_eq!(
        forward
            .paths(RepositoryCandidateKind::FileActivityPath)
            .collect::<Vec<_>>(),
        vec!["/repos/a/src/a.rs", "/repos/b/src/b.rs"]
    );
    forward.validate_contract().unwrap();

    let mut noncanonical = forward;
    noncanonical.candidates.reverse();
    assert!(matches!(
        noncanonical.validate_contract(),
        Err(CoreRecordError::NonCanonicalRepositoryCandidateEvidence)
    ));

    let mut stale_policy = reverse;
    stale_policy.association_policy_revision -= 1;
    assert!(matches!(
        stale_policy.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryRevisions)
    ));
}

#[test]
fn repository_spike_abstention_codes_have_stable_wire_names() {
    let cases = [
        (RepositoryAbstentionReason::DynamicPath, "dynamic_path"),
        (
            RepositoryAbstentionReason::UnknownWrapper,
            "unknown_wrapper",
        ),
        (
            RepositoryAbstentionReason::ProfileDependent,
            "profile_dependent",
        ),
        (
            RepositoryAbstentionReason::UnsupportedShell,
            "unsupported_shell",
        ),
        (
            RepositoryAbstentionReason::CommandTooLarge,
            "command_too_large",
        ),
        (
            RepositoryAbstentionReason::CandidateLimitExceeded,
            "candidate_limit_exceeded",
        ),
        (
            RepositoryAbstentionReason::CandidateMissingBeforeCertification,
            "candidate_missing_before_certification",
        ),
        (RepositoryAbstentionReason::UnsafePath, "unsafe_path"),
        (
            RepositoryAbstentionReason::UnscopedFileActivity,
            "unscoped_file_activity",
        ),
        (
            RepositoryAbstentionReason::AmbiguousCandidates,
            "ambiguous_candidates",
        ),
        (
            RepositoryAbstentionReason::AmbiguousRemote,
            "ambiguous_remote",
        ),
        (
            RepositoryAbstentionReason::GitProbeFailed,
            "git_probe_failed",
        ),
        (
            RepositoryAbstentionReason::ProbeBudgetExceeded,
            "probe_budget_exceeded",
        ),
        (
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "provider_output_unjoined",
        ),
        (
            RepositoryAbstentionReason::LinkageCapacityExceeded,
            "linkage_capacity_exceeded",
        ),
        (
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "outcome_result_inadmissible",
        ),
        (
            RepositoryAbstentionReason::HistoryRewriteUnlinked,
            "history_rewrite_unlinked",
        ),
        (
            RepositoryAbstentionReason::OutcomeRepositoryUnbound,
            "outcome_repository_unbound",
        ),
        (
            RepositoryAbstentionReason::ConcurrentDrift,
            "concurrent_drift",
        ),
        (
            RepositoryAbstentionReason::PlatformUnsupported,
            "platform_unsupported",
        ),
    ];

    for (reason, expected) in cases {
        assert_eq!(
            serde_json::to_string(&reason).unwrap(),
            format!("\"{expected}\"")
        );
    }
}

fn oid(hex: char) -> GitObjectId {
    GitObjectId {
        format: GitObjectFormat::Sha1,
        hex: hex.to_string().repeat(40),
    }
}

fn numbered_oid(index: usize) -> GitObjectId {
    GitObjectId {
        format: GitObjectFormat::Sha1,
        hex: format!("{index:040x}"),
    }
}

fn outcome(kind: RepositoryOutcomeKind) -> RepositoryOutcomeObservation {
    RepositoryOutcomeObservation {
        kind,
        produced_object_ids: vec![oid('a')],
        commit_operation: None,
        pull_request: None,
        pull_request_merge_commit: None,
        observed_at_unix_ms: 1_700_000_000_000,
        linkage: RepositoryOutcomeLinkage {
            provider: "codex".to_owned(),
            origin_call_id: "call-origin".to_owned(),
            result_call_id: "call-result".to_owned(),
            origin_event_sequence: 7,
            continuation_call_id_sha256: vec![[3; 32]],
            result_record_sha256: [4; 32],
        },
        outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    }
}

#[test]
fn repository_outcome_requires_one_scoped_binding_and_exact_shape() {
    let mut record = record();
    record.repository_bindings.push(binding());
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(outcome(
                RepositoryOutcomeKind::Commit,
            ))),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();

    record.repository_vcs_observations[0].repository_binding_id = "missing".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::UnknownRepositoryBinding(binding)) if binding == "missing"
    ));
}

#[test]
fn pull_request_association_is_exact_scoped_and_membership_is_atomic() {
    let mut record = record();
    record.repository_bindings.push(binding());
    let mut association = RepositoryPullRequestAssociationObservation {
        pull_request: RepositoryPullRequestIdentity {
            forge_repository: RepositoryAlias {
                kind: RepositoryAliasKind::Forge,
                host: "github.com".to_owned(),
                namespace: vec!["ctxrs".to_owned()],
                name: "ctx".to_owned(),
                remote_name: None,
            },
            number: 203,
            provider_id: None,
        },
        merged_as: oid('d'),
        contains_commits: vec![oid('a')],
        linkage: RepositoryOutcomeLinkage {
            provider: "codex".to_owned(),
            origin_call_id: "origin".to_owned(),
            result_call_id: "result".to_owned(),
            origin_event_sequence: 7,
            continuation_call_id_sha256: Vec::new(),
            result_record_sha256: [7; 32],
        },
        association_capture_revision: CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
    };
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::PullRequestAssociation(Box::new(
                association.clone(),
            )),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();

    association.contains_commits = vec![oid('b'), oid('a')];
    assert!(matches!(
        association.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryPullRequestAssociation)
    ));
    association.contains_commits.clear();
    association.validate_contract().unwrap();
    association.merged_as.hex = "deadbeef".to_owned();
    assert!(matches!(
        association.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryPullRequestAssociation)
    ));
}

#[test]
fn operation_event_and_pull_request_shapes_are_explicit() {
    let scope = record();
    let repository = binding();
    let mut commit = outcome(RepositoryOutcomeKind::Commit);
    let mappings = vec![RepositoryCommitMapping {
        source: oid('b'),
        result: oid('a'),
    }];
    let linkage = commit.linkage.clone();
    commit.produced_object_ids.clear();
    commit.commit_operation = Some(RepositoryCommitOperationEvent {
        event_id: repository_commit_operation_event_id(
            &scope.source,
            scope.event_id,
            scope.session_id,
            &repository,
            GitObjectFormat::Sha1,
            &mappings,
            &linkage,
            RepositoryCommitOperationKind::Amend,
        ),
        receipt_id: repository_outcome_receipt_id(&linkage),
        kind: RepositoryCommitOperationKind::Amend,
        mappings: mappings.clone(),
        unlinked_sources: Vec::new(),
        unlinked_results: Vec::new(),
        mapping_completeness: RepositoryCommitMappingCompleteness::Complete,
        state: RepositoryCommitOperationState::Asserted,
        proof: RepositoryCommitOperationProof::RepositoryVerifiedYield(
            RepositoryVerifiedYieldProof {
                command_pre_head: Some(oid('b')),
                sequencer_pre_head: None,
                exact_source_oids: vec![oid('b')],
                command_post_head: oid('a'),
                repository_geometry_before_sha256: [8; 32],
                repository_geometry_after_sha256: [8; 32],
                exact_result_map_sha256: repository_result_map_sha256(&mappings),
                drift_excluded: true,
                mutation_excluded: true,
            },
        ),
    });
    commit.validate_contract().unwrap();
    assert_eq!(
        commit
            .commit_operation
            .as_ref()
            .unwrap()
            .repository_verified_yields()
            .map(|object_id| object_id.hex.as_str())
            .collect::<Vec<_>>(),
        vec!["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]
    );

    let old_shape = serde_json::json!({
        "kind": "commit",
        "produced_object_ids": [oid('a')],
        "replacement_lineage": [{"replaced": oid('b'), "replacement": oid('a')}],
        "pull_request": null,
        "observed_at_unix_ms": 1_700_000_000_000_i64,
        "linkage": linkage,
        "outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    });
    assert!(serde_json::from_value::<RepositoryOutcomeObservation>(old_shape).is_err());

    let mut partial = commit.clone();
    let operation = partial.commit_operation.as_mut().unwrap();
    operation.state = RepositoryCommitOperationState::Ambiguous;
    operation.mapping_completeness = RepositoryCommitMappingCompleteness::Partial;
    operation.unlinked_results.push(oid('c'));
    operation.proof = RepositoryCommitOperationProof::RecordExact;
    partial.validate_contract().unwrap();
    assert!(partial
        .commit_operation
        .as_ref()
        .unwrap()
        .repository_verified_yields()
        .next()
        .is_none());

    let mut invalid_verified = partial;
    invalid_verified.commit_operation.as_mut().unwrap().proof =
        commit.commit_operation.as_ref().unwrap().proof.clone();
    assert!(matches!(
        invalid_verified.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));

    let mut duplicate_linkage = outcome(RepositoryOutcomeKind::Commit);
    duplicate_linkage
        .linkage
        .continuation_call_id_sha256
        .push([3; 32]);
    assert!(matches!(
        duplicate_linkage.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));

    let pull_request = RepositoryPullRequestIdentity {
        forge_repository: RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: "ctx".to_owned(),
            remote_name: None,
        },
        number: 224,
        provider_id: Some("PR_kwDOexample".to_owned()),
    };
    let mut created = outcome(RepositoryOutcomeKind::PullRequestCreated);
    created.produced_object_ids.clear();
    created.pull_request = Some(pull_request.clone());
    created.validate_contract().unwrap();

    let mut merged = created;
    merged.kind = RepositoryOutcomeKind::PullRequestMerged;
    merged.pull_request_merge_commit = Some(oid('d'));
    merged.validate_contract().unwrap();
}

#[test]
fn plural_exact_operation_mapping_bound_accepts_32_rejects_33_and_leaves_unlinked_unchanged() {
    let linkage = outcome(RepositoryOutcomeKind::Commit).linkage;
    let mappings = |count: usize| {
        (0..count)
            .map(|index| RepositoryCommitMapping {
                source: numbered_oid(index + 1),
                result: numbered_oid(index + 1_001),
            })
            .collect::<Vec<_>>()
    };

    let accepted_mappings = mappings(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS);
    let accepted = RepositoryCommitOperationEvent::repository_verified_yield(
        &linkage,
        RepositoryCommitOperationKind::Rebase,
        accepted_mappings.clone(),
        Some(accepted_mappings[0].source.clone()),
        Some(accepted_mappings[0].source.clone()),
        accepted_mappings[0].result.clone(),
        [8; 32],
    )
    .unwrap();
    assert_eq!(
        accepted.mappings.len(),
        MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS
    );

    let rejected_mappings = mappings(MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS + 1);
    let rejected = RepositoryCommitOperationEvent::repository_verified_yield(
        &linkage,
        RepositoryCommitOperationKind::Rebase,
        rejected_mappings.clone(),
        Some(rejected_mappings[0].source.clone()),
        Some(rejected_mappings[0].source.clone()),
        rejected_mappings[0].result.clone(),
        [8; 32],
    );
    assert!(matches!(
        rejected,
        Err(CoreRecordError::TooManyItems {
            field: "repository_commit_operation_mappings",
            actual: 33,
            maximum: MAX_REPOSITORY_COMMIT_OPERATION_MAPPINGS,
        })
    ));

    let unlinked_sources = (0..33).map(|index| numbered_oid(index + 1)).collect();
    let unlinked = RepositoryCommitOperationEvent::record_exact_unlinked(
        &linkage,
        RepositoryCommitOperationKind::Rebase,
        unlinked_sources,
        Vec::new(),
        RepositoryCommitOperationState::Ambiguous,
    )
    .unwrap();
    assert_eq!(unlinked.unlinked_sources.len(), 33);
}

#[test]
fn plural_asserted_operation_mappings_require_unique_sources_and_results() {
    let linkage = outcome(RepositoryOutcomeKind::Commit).linkage;
    for mappings in [
        vec![
            RepositoryCommitMapping {
                source: numbered_oid(1),
                result: numbered_oid(101),
            },
            RepositoryCommitMapping {
                source: numbered_oid(1),
                result: numbered_oid(102),
            },
        ],
        vec![
            RepositoryCommitMapping {
                source: numbered_oid(1),
                result: numbered_oid(101),
            },
            RepositoryCommitMapping {
                source: numbered_oid(2),
                result: numbered_oid(101),
            },
        ],
    ] {
        let command_pre_head = mappings[0].source.clone();
        let command_post_head = mappings[0].result.clone();
        assert!(matches!(
            RepositoryCommitOperationEvent::repository_verified_yield(
                &linkage,
                RepositoryCommitOperationKind::Rebase,
                mappings,
                Some(command_pre_head.clone()),
                Some(command_pre_head),
                command_post_head,
                [8; 32],
            ),
            Err(CoreRecordError::InvalidRepositoryOutcome)
        ));
    }
}

#[test]
fn plural_operation_id_replay_is_stable_and_mapping_order_is_canonical() {
    let scope = record();
    let repository = binding();
    let linkage = outcome(RepositoryOutcomeKind::Commit).linkage;
    let mappings = vec![
        RepositoryCommitMapping {
            source: numbered_oid(1),
            result: numbered_oid(101),
        },
        RepositoryCommitMapping {
            source: numbered_oid(2),
            result: numbered_oid(102),
        },
    ];
    let mut replay_mappings = mappings.clone();
    replay_mappings.reverse();

    let mut first = RepositoryCommitOperationEvent::repository_verified_yield(
        &linkage,
        RepositoryCommitOperationKind::Rebase,
        mappings.clone(),
        Some(mappings[0].source.clone()),
        Some(mappings[0].source.clone()),
        mappings[0].result.clone(),
        [8; 32],
    )
    .unwrap();
    let mut replay = RepositoryCommitOperationEvent::repository_verified_yield(
        &linkage,
        RepositoryCommitOperationKind::Rebase,
        replay_mappings.clone(),
        Some(mappings[0].source.clone()),
        Some(mappings[0].source.clone()),
        mappings[0].result.clone(),
        [8; 32],
    )
    .unwrap();

    first
        .bind_scoped_identity(
            &scope.source,
            scope.event_id,
            scope.session_id,
            &repository,
            &linkage,
        )
        .unwrap();
    replay
        .bind_scoped_identity(
            &scope.source,
            scope.event_id,
            scope.session_id,
            &repository,
            &linkage,
        )
        .unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.mappings.len(), 2);
    assert_eq!(
        repository_result_map_sha256(&mappings),
        repository_result_map_sha256(&replay_mappings)
    );
}

#[test]
fn operation_ids_bind_core_repository_mapping_kind_and_provider_domains() {
    let source = source_named("operation-source-a");
    let (session_id, event_id) = operation_core_ids(&source, "shared-provider-session", 1);
    let (_, other_event_id) = operation_core_ids(&source, "shared-provider-session", 2);
    let (other_session_id, other_session_event_id) =
        operation_core_ids(&source, "other-core-session", 1);
    let other_source = source_named("operation-source-b");
    let (other_source_session_id, other_source_event_id) =
        operation_core_ids(&other_source, "shared-provider-session", 1);
    let repository = binding();
    let mut equivalent_checkout = repository.clone();
    equivalent_checkout.binding_id = "binding-2".to_owned();
    equivalent_checkout.checkout_id = Some("checkout-2".to_owned());
    equivalent_checkout.worktree_id = Some("worktree-2".to_owned());
    let mut other_repository = repository.clone();
    other_repository.logical_repository_id = "repo-2".to_owned();
    let linkage = outcome(RepositoryOutcomeKind::Commit).linkage;
    let mappings = vec![
        RepositoryCommitMapping {
            source: numbered_oid(1),
            result: numbered_oid(101),
        },
        RepositoryCommitMapping {
            source: numbered_oid(2),
            result: numbered_oid(102),
        },
    ];
    let mut changed_mappings = mappings.clone();
    changed_mappings[1].result = numbered_oid(103);
    let sha256_mappings = vec![RepositoryCommitMapping {
        source: GitObjectId {
            format: GitObjectFormat::Sha256,
            hex: "1".repeat(64),
        },
        result: GitObjectId {
            format: GitObjectFormat::Sha256,
            hex: "2".repeat(64),
        },
    }];
    let mut changed_linkage = linkage.clone();
    changed_linkage.origin_call_id = "other-provider-call".to_owned();
    let id = |source: &SourceKey,
              event_id: StableEntityId,
              session_id: StableEntityId,
              repository: &RepositoryBinding,
              mappings: &[RepositoryCommitMapping],
              kind: RepositoryCommitOperationKind| {
        repository_commit_operation_event_id(
            source,
            event_id,
            session_id,
            repository,
            GitObjectFormat::Sha1,
            mappings,
            &linkage,
            kind,
        )
    };
    let baseline = id(
        &source,
        event_id,
        session_id,
        &repository,
        &mappings,
        RepositoryCommitOperationKind::Rebase,
    );

    assert_eq!(
        baseline,
        id(
            &source,
            event_id,
            session_id,
            &equivalent_checkout,
            &mappings,
            RepositoryCommitOperationKind::Rebase,
        ),
        "checkout and binding coordinates are not logical repository identity"
    );
    assert_ne!(
        baseline,
        id(
            &source,
            other_event_id,
            session_id,
            &repository,
            &mappings,
            RepositoryCommitOperationKind::Rebase,
        )
    );
    assert_ne!(
        baseline,
        id(
            &source,
            other_session_event_id,
            other_session_id,
            &repository,
            &mappings,
            RepositoryCommitOperationKind::Rebase,
        ),
        "reused provider-local IDs must not collide across Core sessions"
    );
    assert_ne!(
        baseline,
        id(
            &other_source,
            other_source_event_id,
            other_source_session_id,
            &repository,
            &mappings,
            RepositoryCommitOperationKind::Rebase,
        )
    );
    assert_ne!(
        baseline,
        id(
            &source,
            event_id,
            session_id,
            &other_repository,
            &mappings,
            RepositoryCommitOperationKind::Rebase,
        ),
        "reused provider-local IDs must not collide across logical repositories"
    );
    assert_ne!(
        baseline,
        id(
            &source,
            event_id,
            session_id,
            &repository,
            &changed_mappings,
            RepositoryCommitOperationKind::Rebase,
        )
    );
    assert_ne!(
        baseline,
        id(
            &source,
            event_id,
            session_id,
            &repository,
            &mappings,
            RepositoryCommitOperationKind::CherryPick,
        )
    );
    assert_ne!(
        baseline,
        repository_commit_operation_event_id(
            &source,
            event_id,
            session_id,
            &repository,
            GitObjectFormat::Sha256,
            &sha256_mappings,
            &linkage,
            RepositoryCommitOperationKind::Rebase,
        )
    );
    assert_ne!(
        baseline,
        repository_commit_operation_event_id(
            &source,
            event_id,
            session_id,
            &repository,
            GitObjectFormat::Sha1,
            &mappings,
            &changed_linkage,
            RepositoryCommitOperationKind::Rebase,
        )
    );
}

#[test]
fn core_record_rejects_operation_id_transplanted_to_another_repository() {
    let mut record = record();
    let repository = binding();
    let linkage = outcome(RepositoryOutcomeKind::Commit).linkage;
    let mapping = RepositoryCommitMapping {
        source: oid('b'),
        result: oid('a'),
    };
    let operation = RepositoryCommitOperationEvent::repository_verified_yield(
        &linkage,
        RepositoryCommitOperationKind::Amend,
        vec![mapping.clone()],
        Some(mapping.source),
        None,
        mapping.result,
        [8; 32],
    )
    .unwrap();
    record.repository_bindings.push(repository);
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(RepositoryOutcomeObservation {
                kind: RepositoryOutcomeKind::Commit,
                produced_object_ids: Vec::new(),
                commit_operation: Some(operation),
                pull_request: None,
                pull_request_merge_commit: None,
                observed_at_unix_ms: 1_700_000_000_000,
                linkage,
                outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            })),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record
        .bind_repository_commit_operation_identities()
        .unwrap();
    record.validate_contract().unwrap();

    record.repository_bindings[0].logical_repository_id = "repo-2".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));
}

#[test]
fn pull_request_outcome_must_match_its_referenced_repository_binding() {
    let pull_request = RepositoryPullRequestIdentity {
        forge_repository: RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: "ctx".to_owned(),
            remote_name: None,
        },
        number: 224,
        provider_id: None,
    };
    let mut created = outcome(RepositoryOutcomeKind::PullRequestCreated);
    created.produced_object_ids.clear();
    created.pull_request = Some(pull_request);

    let mut record = record();
    record.repository_bindings.push(binding());
    record
        .repository_vcs_observations
        .push(RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(created)),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        });
    record.validate_contract().unwrap();

    let RepositoryVcsObservationKind::Outcome(outcome) =
        &mut record.repository_vcs_observations[0].kind
    else {
        unreachable!();
    };
    outcome.pull_request.as_mut().unwrap().forge_repository.name = "other".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));

    let RepositoryVcsObservationKind::Outcome(outcome) =
        &mut record.repository_vcs_observations[0].kind
    else {
        unreachable!();
    };
    outcome.pull_request.as_mut().unwrap().forge_repository.name = "ctx".to_owned();
    record.repository_bindings[0].aliases.clear();
    record.repository_bindings[0].logical_repository_id = "repo-1".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));

    record.repository_bindings[0].logical_repository_id = "forge:github.com/ctxrs/ctx".to_owned();
    record.validate_contract().unwrap();

    record.repository_bindings[0].logical_repository_id = "forge:github.com/ctxrs/other".to_owned();
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));

    record.repository_bindings[0].logical_repository_id = "local:certified-checkout".to_owned();
    record.repository_bindings[0].aliases.push(RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host: "github.com".to_owned(),
        namespace: vec!["ctxrs".to_owned()],
        name: "other".to_owned(),
        remote_name: Some("upstream".to_owned()),
    });
    assert!(matches!(
        record.validate_contract(),
        Err(CoreRecordError::InvalidRepositoryOutcome)
    ));
    record.repository_bindings[0].aliases.push(RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host: "GITHUB.COM".to_owned(),
        namespace: vec!["ctxrs".to_owned()],
        name: "ctx".to_owned(),
        remote_name: Some("origin".to_owned()),
    });
    record.validate_contract().unwrap();
}

#[test]
fn every_bound_revision_and_accumulator_identity_changes_the_core_contract_fingerprint() {
    let current = CoreContractRevisions::current();
    let expected = core_record_contract_fingerprint_for(current);
    assert_eq!(
        expected,
        "0610be9a5810ce742b505dc4c3b3db24e9ab795126a6b62e0ef2172e04a85cc3"
    );
    assert_eq!(
        core_record_contract_fingerprint_for(CoreContractRevisions {
            accumulator_identity: b"",
            ..current
        }),
        "fc61723d453f4e953145517891e8c9c7129ec09068a821182c77b52a006df982"
    );
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
            mcp_tool_call_attribution: current.mcp_tool_call_attribution + 1,
            ..current
        },
        CoreContractRevisions {
            session_lineage: current.session_lineage + 1,
            ..current
        },
        CoreContractRevisions {
            mcp_exchange: current.mcp_exchange + 1,
            ..current
        },
        CoreContractRevisions {
            accumulator_identity: b"ctx-core-record-event-binding-v2\0",
            ..current
        },
        CoreContractRevisions {
            repository_contract: current.repository_contract + 1,
            ..current
        },
        CoreContractRevisions {
            repository_observation: current.repository_observation + 1,
            ..current
        },
        CoreContractRevisions {
            bounded_shell_subset: current.bounded_shell_subset + 1,
            ..current
        },
        CoreContractRevisions {
            repository_association_policy: current.repository_association_policy + 1,
            ..current
        },
        CoreContractRevisions {
            repository_pull_request_association_capture: current
                .repository_pull_request_association_capture
                + 1,
            ..current
        },
        CoreContractRevisions {
            repository_outcome_capture: current.repository_outcome_capture + 1,
            ..current
        },
        CoreContractRevisions {
            repository_local_root_authorization_fingerprint: current
                .repository_local_root_authorization_fingerprint
                + 1,
            ..current
        },
    ] {
        assert_ne!(core_record_contract_fingerprint_for(changed), expected);
    }
}
