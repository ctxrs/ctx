use std::{collections::BTreeMap, io::Cursor};

use crate::{read_frame, write_frame, HostEnvelope, HostMessage};
use ctx_history_core::{
    derive_event_id, derive_session_id, CoreContent, CoreContentPolicyStatus, EventIdentityInput,
    NativeItemKey, NativeSessionKey, RepositoryAlias, RepositoryAliasKind, RepositoryBinding,
    RepositoryCandidateEvidence, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryVcsObservation, RepositoryVcsObservationKind, SessionIdentityInput, SourceAnchor,
    TypedKey, CORE_CONTENT_POLICY_REVISION,
};

use super::*;

fn source(lineage: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([lineage; 32]),
    )
    .unwrap()
}

fn state(source: SourceKey, revision: u8, event_count: u64) -> CoreSourceState {
    CoreSourceState {
        source,
        source_revision_sha256: format!("{revision:064x}"),
        event_count,
    }
}

fn binding(id: &str, repository: &str) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: id.to_owned(),
        logical_repository_id: repository.to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: vec![RepositoryAlias {
            kind: RepositoryAliasKind::Forge,
            host: "github.com".to_owned(),
            namespace: vec!["ctxrs".to_owned()],
            name: repository.to_owned(),
            remote_name: Some("origin".to_owned()),
        }],
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::DeclaredToolWorkdir,
            confidence: RepositoryEvidenceConfidence::High,
        }],
        association_policy_revision: 1,
    }
}

fn record(source: &SourceKey, sequence: u64, body: String, two_repositories: bool) -> CoreRecord {
    let session_key = NativeSessionKey::native_id("fixture-session", TypedKey::U64(1)).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("fixture-event", TypedKey::U64(sequence))
            .unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let repository_bindings = if two_repositories {
        vec![
            binding("binding-a", "repo-a"),
            binding("binding-b", "repo-b"),
        ]
    } else {
        Vec::new()
    };
    let repository_file_observations = if two_repositories {
        vec![
            RepositoryFileObservation {
                repository_binding_id: "binding-a".to_owned(),
                relative_path: "src/a.rs".to_owned(),
                kind: RepositoryFileObservationKind::Modified,
                prior_relative_path: None,
            },
            RepositoryFileObservation {
                repository_binding_id: "binding-b".to_owned(),
                relative_path: "src/b.rs".to_owned(),
                kind: RepositoryFileObservationKind::Read,
                prior_relative_path: None,
            },
        ]
    } else {
        Vec::new()
    };
    let repository_vcs_observations = if two_repositories {
        vec![RepositoryVcsObservation {
            repository_binding_id: "binding-b".to_owned(),
            kind: RepositoryVcsObservationKind::Branch,
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: Some("refs/heads/main".to_owned()),
            relative_path: None,
        }]
    } else {
        Vec::new()
    };
    CoreRecord {
        record_version: CORE_RECORD_VERSION,
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        provider_session_id: Some("provider-session".to_owned()),
        native_event_id: None,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        workspace: None,
        branch: Some("main".to_owned()),
        cwd: None,
        parser_revision: "fixture-parser-v1".to_owned(),
        normalization_revision: CORE_NORMALIZATION_REVISION,
        content: CoreContent {
            policy_revision: CORE_CONTENT_POLICY_REVISION,
            policy_status: CoreContentPolicyStatus::Selected,
            normalized_body: Some(body),
            structured_content: None,
        },
        metadata: BTreeMap::new(),
        repository_candidate_evidence: RepositoryCandidateEvidence::default(),
        repository_bindings,
        repository_abstentions: Vec::new(),
        repository_file_observations,
        repository_vcs_observations,
    }
}

fn head(sources: &[CoreSourceState]) -> CoreGenerationHead {
    CoreGenerationHead::new(
        "a".repeat(64),
        4,
        1,
        "b".repeat(64),
        3,
        2,
        "c".repeat(64),
        sources,
    )
    .unwrap()
}

#[test]
fn complete_core_record_page_transports_long_body_and_two_repository_scopes() {
    let source = source(1);
    let source_state = state(source.clone(), 2, 1);
    let body = format!("{}tail-marker", "x".repeat(20 * 1024));
    let record = record(&source, 1, body.clone(), true);
    let page = CoreRecordPage::new(
        "d".repeat(64),
        "a".repeat(64),
        source_state,
        0,
        0,
        true,
        vec![record],
    )
    .unwrap();

    assert_eq!(
        page.records[0].content.normalized_body.as_deref(),
        Some(body.as_str())
    );
    assert_eq!(page.records[0].repository_bindings.len(), 2);
    assert_eq!(page.records[0].repository_file_observations.len(), 2);
    assert_eq!(page.records[0].repository_vcs_observations.len(), 1);
    let encoded = serde_json::to_string(&page).unwrap();
    assert!(encoded.contains("tail-marker"));
    assert!(!encoded.contains("source_locator"));
    assert!(!encoded.contains("worktree_root_locator"));

    let envelope = HostEnvelope {
        sequence: 1,
        request_id: uuid::Uuid::from_u128(1),
        message: HostMessage::MaterializeCoreRecordPage(MaterializeCoreRecordPageRequest { page }),
    };
    let mut frame = Vec::new();
    write_frame(&mut frame, &envelope).unwrap();
    assert!(frame.len() > 16 * 1024);
    assert_eq!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(frame)).unwrap(),
        envelope
    );
}

#[test]
fn record_page_acknowledgement_uses_compact_identity_after_request_moves() {
    let source = source(1);
    let source_state = state(source.clone(), 2, 1);
    let page = CoreRecordPage::new(
        "d".repeat(64),
        "a".repeat(64),
        source_state,
        3,
        7,
        true,
        vec![record(&source, 1, "x".repeat(1024 * 1024), false)],
    )
    .unwrap();
    let identity = page.acknowledgement_identity();
    let request = MaterializeCoreRecordPageRequest { page };
    let mut acknowledgement = CoreRecordPageMaterialized {
        materialization_id: request.page.materialization_id.clone(),
        core_generation_id: request.page.core_generation_id.clone(),
        source: request.page.source.source.clone(),
        source_revision_sha256: request.page.source.source_revision_sha256.clone(),
        source_index: request.page.source_index,
        page_index: request.page.page_index,
        accepted_records: 1,
        terminal: true,
        replayed: false,
    };
    drop(request);

    acknowledgement.validate_for_identity(&identity).unwrap();
    acknowledgement.page_index = 8;
    assert_eq!(
        acknowledgement
            .validate_for_identity(&identity)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );
}

#[test]
fn delta_ack_selects_only_exact_changed_revisions_for_record_materialization() {
    let unchanged = state(source(1), 1, 10_000);
    let changed = state(source(2), 2, 1);
    let page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        true,
        vec![
            CoreSourceDelta::Present(unchanged),
            CoreSourceDelta::Present(changed.clone()),
        ],
    )
    .unwrap();
    let identity = page.acknowledgement_identity();
    CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        changed_sources: 1,
        removed_sources: 0,
        materialize_sources: vec![changed.clone()],
        replayed: false,
    }
    .validate_for_identity(&identity)
    .unwrap();

    let mut stale = changed;
    stale.source_revision_sha256 = "f".repeat(64);
    let invalid = CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        changed_sources: 1,
        removed_sources: 0,
        materialize_sources: vec![stale],
        replayed: false,
    };
    assert_eq!(
        invalid.validate_for_identity(&identity).unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn source_delta_pages_require_stable_order_and_exact_page_cas() {
    let first = state(source(1), 1, 2);
    let second = state(source(2), 2, 3);
    let page = CoreSourceDeltaPage::new(
        "d".repeat(64),
        "a".repeat(64),
        0,
        true,
        vec![
            CoreSourceDelta::Present(first),
            CoreSourceDelta::Present(second),
        ],
    )
    .unwrap();
    CoreSourceDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        page_index: 0,
        changed_sources: 2,
        removed_sources: 0,
        materialize_sources: page
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                CoreSourceDelta::Present(state) => Some(state.clone()),
                CoreSourceDelta::Removed(_) => None,
            })
            .collect(),
        replayed: false,
    }
    .validate_for(&page)
    .unwrap();

    let mut reversed = page.deltas.clone();
    reversed.reverse();
    assert!(CoreSourceDeltaPage::new("d".repeat(64), "a".repeat(64), 0, true, reversed,).is_err());
}

#[test]
fn generation_begin_and_finish_fail_closed_on_mismatched_cas() {
    let sources = vec![state(source(1), 1, 0)];
    let request = BeginCoreMaterializationRequest {
        head: head(&sources),
        expected_prior_receipt: None,
    };
    let revision = "materializer-v2";
    let identity = request.acknowledgement_identity().unwrap();
    let expected_materialization_id = canonical_sha256(
        &(&request, revision),
        "Core materialization ID encoding failed",
    )
    .unwrap();
    let mut began = CoreMaterializationBegan {
        materialization_id: core_materialization_id(&request, revision).unwrap(),
        core_generation_id: request.head.core_generation_id.clone(),
        materializer_revision: revision.to_owned(),
        expected_prior_receipt: None,
        replayed: false,
    };
    assert_eq!(began.materialization_id, expected_materialization_id);
    drop(request);
    began.validate_for_identity(&identity).unwrap();
    began.core_generation_id = "f".repeat(64);
    assert!(began.validate_for_identity(&identity).is_err());
}

#[test]
fn receipt_identity_binds_generation_sources_and_materializer_revision() {
    let sources = vec![state(source(1), 1, 4), state(source(2), 2, 5)];
    let head = head(&sources);
    let receipt = CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id.clone(),
        core_record_contract_fingerprint: head.core_record_contract_fingerprint.clone(),
        source_snapshot_sha256: head.source_snapshot_sha256.clone(),
        materializer_revision: "materializer-v1".to_owned(),
        source_count: 2,
        event_count: 9,
    };
    receipt.validate_for_head(&head).unwrap();
    let first = CoreMaterializationReceiptIdentity::from_receipt(&receipt).unwrap();
    let mut revised = receipt.clone();
    revised.materializer_revision = "materializer-v2".to_owned();
    let second = CoreMaterializationReceiptIdentity::from_receipt(&revised).unwrap();
    assert_ne!(first.materializer_revision, second.materializer_revision);
}
