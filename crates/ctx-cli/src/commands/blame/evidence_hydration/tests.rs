use std::{cell::Cell, fs};

use anyhow::anyhow;
use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceKey, SourceObservation, TypedKey, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
};
use ctx_history_index::{
    CoreEventRecord, EventRecord, GenerationWriter, IndexError, WriterOptions,
};
use ctx_pro_host_protocol::{
    BlameResult, ByteRange, EvidenceCitation, GitSnapshot, NumberedEvidence, ResolvedBlameTarget,
    ResourceKind, ResourceRef, WorktreeStatus,
};
use sha2::{Digest as _, Sha256};

use super::*;

const PATH: &str = "src/lib.rs";

fn protocol_snapshot() -> ctx_pro_host_protocol::QuerySnapshotExpectation {
    ctx_pro_host_protocol::QuerySnapshotExpectation::Core {
        receipt: ctx_pro_host_protocol::CoreMaterializationReceiptIdentity {
            core_generation_id: "a".repeat(64),
            materializer_revision: "materializer-v1".to_owned(),
        },
    }
}

fn source(seed: u8) -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl",
        "codex-nativepath-jsonl-v0",
        1,
        SourceAnchor::CatalogLineage([seed; 32]),
    )
    .unwrap()
}

fn binding() -> RepositoryBinding {
    RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: None,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn core_record(seed: u8, sequence: u64, body: impl Into<String>) -> CoreRecord {
    let source = source(seed);
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", TypedKey::U64(u64::from(seed)))
            .unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "tool_call",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(sequence)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source,
        sequence,
        "tool_call",
        "primary",
        true,
        "codex-nativepath-core-record-v7",
        body,
    )
    .unwrap();
    record.role = Some("assistant".to_owned());
    record.repository_bindings = vec![binding()];
    record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: PATH.to_owned(),
        kind: RepositoryFileObservationKind::Modified,
        prior_relative_path: None,
    }];
    record.validate_contract().unwrap();
    record
}

fn event_record(core: CoreRecord) -> CoreEventRecord {
    let event = EventRecord {
        event_id: core.event_id,
        session_id: core.session_id,
        parent_session_id: core.parent_session_id,
        root_session_id: core.root_session_id,
        source: core.source.clone(),
        provider: core.source.provider().to_owned(),
        source_format: core.source.source_format().to_owned(),
        provider_session_id: core.provider_session_id.clone(),
        native_event_id: core.native_event_id.clone(),
        branch: core.branch.clone(),
        agent_type: core.agent_type.clone(),
        is_primary: core.is_primary,
        event_sequence: core.event_sequence,
        occurred_at_unix_ms: core.occurred_at_unix_ms,
        event_type: core.event_type.clone(),
        role: core.role.clone(),
        workspace: core.workspace.clone(),
        cwd: core.cwd.clone(),
        touched_files: vec![PATH.to_owned()],
    };
    CoreEventRecord {
        event,
        core_record: core,
    }
}

fn digest(record: &CoreRecord) -> String {
    format!("{:x}", Sha256::digest(record.encode_stored().unwrap()))
}

fn evidence(record: &CoreRecord, generation: &str, number: u32) -> NumberedEvidence {
    NumberedEvidence {
        number,
        citation: EvidenceCitation {
            core_generation_id: generation.to_owned(),
            source: record.source.clone(),
            session_id: record.session_id,
            event_id: record.event_id,
            event_sequence: record.event_sequence,
            byte_range: None,
            evidence_sha256: Some(digest(record)),
        },
    }
}

fn result(evidence: Vec<NumberedEvidence>) -> BlameResult {
    BlameResult {
        snapshot: protocol_snapshot(),
        target: ResolvedBlameTarget::File {
            path: PATH.to_owned(),
            repository: ResourceRef {
                id: "repository:ctxrs-ctx".to_owned(),
                kind: ResourceKind::Repository,
                display: "forge:github.com/ctxrs/ctx".to_owned(),
            },
            requested_lines: None,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        matches: Vec::new(),
        evidence,
        next: None,
    }
}

fn certificate(source: &SourceKey, revision: u8) -> CertifiedSource {
    let observation = SourceObservation::new(
        source.clone(),
        "codex-nativepath-core-record-v7",
        vec![revision],
    )
    .unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "codex-nativepath-core-record-v7",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn publish(index_root: &std::path::Path, record: &CoreRecord, revision: u8) -> String {
    let mut writer = GenerationWriter::open(
        index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1_024 * 1_024,
        },
    )
    .unwrap();
    writer.begin_source(record.source.clone()).unwrap();
    writer.add_core_record(record.clone()).unwrap();
    writer
        .certify_source(certificate(&record.source, revision))
        .unwrap();
    writer.commit(|_| true).unwrap().generation_id
}

#[test]
fn one_strict_bounded_batch_deduplicates_ids_and_caps_citations() {
    let generation = "a".repeat(64);
    let first = core_record(1, 1, "modified: src/lib.rs");
    let second = core_record(2, 1, "modified: src/lib.rs");
    let fourth = core_record(4, 1, "modified: src/lib.rs");
    let result = result(vec![
        evidence(&first, &generation, 2),
        evidence(&second, &generation, 3),
        evidence(&fourth, &generation, 4),
        evidence(&first, &generation, 1),
    ]);
    let calls = Cell::new(0usize);

    let model = hydrate_evidence_previews_with(
        &result,
        |requested_generation, ids, maximum_events, budget| {
            calls.set(calls.get() + 1);
            assert_eq!(requested_generation, generation);
            assert_eq!(ids, &[first.event_id.as_uuid(), second.event_id.as_uuid()]);
            assert_eq!(maximum_events, 2);
            assert_eq!(budget, evidence_hydration_budget(2));
            assert_eq!(budget.aggregate.maximum_encoded_core_bytes, 2 * 64 * 1_024);
            assert_eq!(budget.aggregate.maximum_content_bytes, 2 * 64 * 1_024);
            assert_eq!(budget.per_record.maximum_encoded_core_bytes, 64 * 1_024);
            assert_eq!(budget.per_record.maximum_content_bytes, 64 * 1_024);
            Ok(Some(HydratedEvidenceBatch {
                generation_id: generation.clone(),
                records: vec![event_record(first.clone()), event_record(second.clone())],
            }))
        },
    );

    assert_eq!(calls.get(), 1);
    assert_eq!(model.previews.len(), 1);
    assert_eq!(model.previews[0].evidence_numbers, vec![1, 2, 3]);
    assert!(model
        .previews
        .iter()
        .all(|preview| !preview.evidence_numbers.contains(&4)));
}

#[test]
fn malformed_mixed_or_ranged_citations_stop_before_index_open() {
    let generation = "a".repeat(64);
    let record = core_record(3, 1, "modified: src/lib.rs");
    let cases = {
        let mut mixed = vec![
            evidence(&record, &generation, 1),
            evidence(&record, &"b".repeat(64), 2),
        ];
        let mut ranged = vec![evidence(&record, &generation, 1)];
        ranged[0].citation.byte_range = Some(ByteRange {
            start: 0,
            end_exclusive: 1,
        });
        let mut missing_digest = vec![evidence(&record, &generation, 1)];
        missing_digest[0].citation.evidence_sha256 = None;
        let malformed = vec![evidence(&record, "A", 1)];
        vec![
            std::mem::take(&mut mixed),
            ranged,
            missing_digest,
            malformed,
        ]
    };

    for evidence in cases {
        let model = hydrate_evidence_previews_with(&result(evidence), |_, _, _, _| {
            panic!("invalid evidence reached the index opener")
        });
        assert!(model.previews.is_empty());
    }
}

#[test]
fn read_errors_missing_records_wrong_order_and_wrong_generation_fail_closed() {
    let generation = "a".repeat(64);
    let first = core_record(5, 1, "modified: src/lib.rs");
    let second = core_record(6, 1, "modified: src/lib.rs");
    let result = result(vec![
        evidence(&first, &generation, 1),
        evidence(&second, &generation, 2),
    ]);

    let race = hydrate_evidence_previews_with(&result, |_, _, _, _| {
        Err(anyhow!(IndexError::ConcurrentGenerationChange))
    });
    assert!(race.previews.is_empty());

    let missing = hydrate_evidence_previews_with(&result, |_, _, _, _| {
        Ok(Some(HydratedEvidenceBatch {
            generation_id: generation.clone(),
            records: vec![event_record(first.clone())],
        }))
    });
    assert!(missing.previews.is_empty());

    let wrong_order = hydrate_evidence_previews_with(&result, |_, _, _, _| {
        Ok(Some(HydratedEvidenceBatch {
            generation_id: generation.clone(),
            records: vec![event_record(second.clone()), event_record(first.clone())],
        }))
    });
    assert!(wrong_order.previews.is_empty());

    let wrong_generation = hydrate_evidence_previews_with(&result, |_, _, _, _| {
        Ok(Some(HydratedEvidenceBatch {
            generation_id: "b".repeat(64),
            records: vec![event_record(first.clone()), event_record(second.clone())],
        }))
    });
    assert!(wrong_generation.previews.is_empty());
}

#[test]
fn digest_and_every_event_coordinate_mismatch_are_omitted() {
    let generation = "a".repeat(64);
    let record = core_record(7, 7, "modified: src/lib.rs");
    let mut bad_digest = evidence(&record, &generation, 1);
    bad_digest.citation.evidence_sha256 = Some("f".repeat(64));
    let model = hydrate_evidence_previews_with(&result(vec![bad_digest]), |_, _, _, _| {
        Ok(Some(HydratedEvidenceBatch {
            generation_id: generation.clone(),
            records: vec![event_record(record.clone())],
        }))
    });
    assert!(model.previews.is_empty());

    for mismatch in 0..4 {
        let cited = evidence(&record, &generation, 1);
        let mut hydrated = event_record(record.clone());
        let other = event_record(core_record(8, 8, "modified: src/lib.rs"));
        match mismatch {
            0 => hydrated.event.source = other.source.clone(),
            1 => hydrated.event.session_id = other.session_id,
            2 => hydrated.event.event_id = other.event_id,
            3 => hydrated.event.event_sequence += 1,
            _ => unreachable!(),
        }
        let model = hydrate_evidence_previews_with(&result(vec![cited]), |_, _, _, _| {
            Ok(Some(HydratedEvidenceBatch {
                generation_id: generation.clone(),
                records: vec![hydrated],
            }))
        });
        assert!(model.previews.is_empty(), "mismatch {mismatch}");
    }
}

#[test]
fn non_file_preview_is_rejected_without_any_core_read() {
    let mut result = result(Vec::new());
    let repository = ResourceRef {
        id: "repository:ctxrs-ctx".to_owned(),
        kind: ResourceKind::Repository,
        display: "ctxrs/ctx".to_owned(),
    };
    for target in [
        ResolvedBlameTarget::Commit {
            commit: ResourceRef {
                id: "commit:abc1234".to_owned(),
                kind: ResourceKind::Commit,
                display: "abc1234".to_owned(),
            },
            repository: repository.clone(),
        },
        ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: ResourceRef {
                id: "pull_request:ctxrs/ctx#42".to_owned(),
                kind: ResourceKind::PullRequest,
                display: "ctxrs/ctx#42".to_owned(),
            },
            repository: repository.clone(),
        },
    ] {
        result.target = target;
        result.git_snapshot = None;
        let model = hydrate_evidence_previews_with(&result, |_, _, _, _| {
            panic!("non-file preview reached the Core index")
        });
        assert!(model.previews.is_empty());
    }
}

#[test]
fn active_and_retained_previous_generations_hydrate_from_real_index() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = data_root.join("search/lexical");
    let previous_record = core_record(9, 1, "modified: src/lib.rs");
    let previous = publish(&index_root, &previous_record, 1);

    let active_model = hydrate_evidence_previews(
        &data_root,
        &result(vec![evidence(&previous_record, &previous, 1)]),
    );
    assert_eq!(active_model.previews.len(), 1);

    let active_record = core_record(9, 1, "before\nmodified: src/lib.rs\nafter");
    let active = publish(&index_root, &active_record, 2);
    assert_ne!(active, previous);
    let previous_model = hydrate_evidence_previews(
        &data_root,
        &result(vec![evidence(&previous_record, &previous, 1)]),
    );
    assert_eq!(previous_model.previews.len(), 1);
    assert_eq!(previous_model.previews[0].excerpt, "modified: src/lib.rs");
}

#[test]
fn evicted_real_core_records_are_normal_omissions_without_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = data_root.join("search/lexical");
    fs::create_dir_all(&data_root).unwrap();
    let provider_source = data_root.join("provider-source.jsonl");
    fs::write(&provider_source, b"source authority must stay untouched").unwrap();

    let evicted_record = core_record(10, 1, "modified: src/lib.rs");
    let evicted = publish(&index_root, &evicted_record, 1);
    let replacement = core_record(10, 1, "before\nmodified: src/lib.rs");
    publish(&index_root, &replacement, 2);
    publish(
        &index_root,
        &core_record(10, 1, "modified: src/lib.rs\nafter"),
        3,
    );

    let evicted_model = hydrate_evidence_previews(
        &data_root,
        &result(vec![evidence(&evicted_record, &evicted, 1)]),
    );
    assert!(evicted_model.previews.is_empty());
    assert_eq!(
        fs::read(&provider_source).unwrap(),
        b"source authority must stay untouched"
    );
    assert!(!data_root.join("work.sqlite").exists());
}

#[test]
fn compact_eval_envelope_fits_and_mixed_oversized_records_fail_closed() {
    const MEASURED_MAX_BODY_CHARS: usize = 40_206;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = data_root.join("search/lexical");
    let suffix = "\nmodified: src/lib.rs";
    let compact = core_record(
        11,
        1,
        format!(
            "{}{}",
            "x".repeat(MEASURED_MAX_BODY_CHARS - suffix.len()),
            suffix
        ),
    );
    assert_eq!(
        compact
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .chars()
            .count(),
        MEASURED_MAX_BODY_CHARS
    );
    assert!(compact.encode_stored().unwrap().len() < 64 * 1_024);
    let compact_generation = publish(&index_root, &compact, 1);
    let compact_model = hydrate_evidence_previews(
        &data_root,
        &result(vec![evidence(&compact, &compact_generation, 1)]),
    );
    assert_eq!(compact_model.previews.len(), 1);

    let oversized = core_record(12, 1, format!("{}{}", "x".repeat(64 * 1_024), suffix));
    assert!(oversized.encode_stored().unwrap().len() > 64 * 1_024);
    let mixed_generation = publish(&index_root, &oversized, 2);
    let mixed_model = hydrate_evidence_previews(
        &data_root,
        &result(vec![
            evidence(&compact, &mixed_generation, 1),
            evidence(&oversized, &mixed_generation, 2),
        ]),
    );
    assert!(mixed_model.previews.is_empty());
    assert_eq!(
        evidence_hydration_budget(MAX_EVIDENCE_PREVIEW_CITATIONS),
        EvidenceHydrationBudget {
            aggregate: CoreEventPageBudget::new(3 * 64 * 1_024, 3 * 64 * 1_024),
            per_record: CoreEventPageBudget::new(64 * 1_024, 64 * 1_024),
        }
    );
}
