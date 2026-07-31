use super::*;
use crate::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, TypedKey,
};

fn source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8("core-record-test").unwrap())
            .unwrap(),
    )
    .unwrap()
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
        source,
        provider_session_id: Some("session".to_owned()),
        native_event_id: Some(TypedKey::U64(1)),
        event_sequence: 1,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
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
        },
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings: Vec::new(),
        repository_abstentions: Vec::new(),
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
        constructed.normalization_revision,
        CORE_NORMALIZATION_REVISION
    );
    assert_eq!(
        constructed.content.policy_revision,
        CORE_CONTENT_POLICY_REVISION
    );
    assert_eq!(constructed.parser_revision, "provider-parser-v7");
    assert_eq!(
        constructed.content.normalized_body.as_deref(),
        Some("complete selected body")
    );
    assert!(constructed.metadata.is_empty());
    assert!(constructed.repository_bindings.is_empty());
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
            locator_fingerprint: [7; 32],
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
        association_policy_revision: 1,
    }
}

#[test]
fn complete_record_round_trips_stored_encoding() {
    let record = record();
    let encoded = record.encode_stored().unwrap();
    assert_eq!(CoreRecord::decode_stored(&encoded).unwrap(), record);
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
    prior.repository_bindings.push(binding());
    prior
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    let mut current = record();
    current.repository_abstentions.push(RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
        reason: RepositoryAbstentionReason::CandidateMissingBeforeCertification,
        detail: Some("candidate_missing_before_certification".to_owned()),
        association_policy_revision: 1,
    });

    assert!(current.needs_prior_repository_certificate());
    assert!(current.reuse_prior_repository_certificate(&prior));
    assert_eq!(current.repository_bindings.len(), 1);
    assert!(current.repository_bindings[0]
        .local_root_authorization
        .is_none());
    assert_eq!(current.repository_file_observations.len(), 1);
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
        association_policy_revision: 1,
    }];
    assert!(!wrong_source.reuse_prior_repository_certificate(&prior));
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
            locator_fingerprint: [9; 32],
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
    record.repository_candidate_evidence = RepositoryCandidateEvidence {
        session_cwd: Some("/control/workspace".to_owned()),
        declared_tool_workdir: Some("/code/repo".to_owned()),
        derived_effective_cwd: Some("/code/repo/crates".to_owned()),
        command_specific_repository_path: Some("/code/other".to_owned()),
    };
    record.repository_abstentions.push(RepositoryAbstention {
        evidence_kind: RepositoryEvidenceKind::DerivedEffectiveCwd,
        reason: RepositoryAbstentionReason::AmbiguousCandidates,
        detail: Some("multiple certified boundaries".to_owned()),
        association_policy_revision: 1,
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
            .declared_tool_workdir
            .as_deref(),
        Some("/code/repo")
    );
    assert_eq!(
        decoded
            .repository_candidate_evidence
            .derived_effective_cwd
            .as_deref(),
        Some("/code/repo/crates")
    );
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
