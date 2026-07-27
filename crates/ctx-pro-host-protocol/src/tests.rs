use std::{collections::BTreeSet, io::Cursor};

use base64::Engine as _;
use proptest::prelude::*;
use serde_json::json;

use super::*;

fn host(sequence: u64, message: HostMessage) -> HostEnvelope {
    HostEnvelope {
        sequence,
        request_id: Uuid::from_u128(u128::from(sequence) + 1),
        message,
    }
}

fn hello(sequence: u64) -> HostEnvelope {
    host(
        sequence,
        HostMessage::Hello(HelloRequest::current(
            "test-host",
            BTreeSet::from([
                Capability::Status,
                Capability::JournalSync,
                Capability::OutputMaterialization,
                Capability::Query,
            ]),
        )),
    )
}

fn output_source() -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: "codex".to_owned(),
        namespace_id: "codex-session-jsonl".to_owned(),
        source_id: "fixture/session.jsonl".to_owned(),
    }
}

fn output_cursor(value: &str) -> OutputNativeCursor {
    OutputNativeCursor {
        version: 1,
        payload_base64: base64::engine::general_purpose::STANDARD.encode(value),
    }
}

fn empty_output_page() -> ProOutputMaterializationPage {
    ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 1,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-1".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: output_cursor("cursor-1"),
        terminal: true,
        observations: Vec::new(),
    }
}

fn provenance(kind: JournalEntityKind, id: Uuid) -> JournalProvenanceIdentity {
    JournalProvenanceIdentity {
        entity_kind: kind,
        stable_entity_id: id,
        capture_source_id: Some(Uuid::from_u128(99)),
        provider: Some("codex".to_owned()),
        provider_external_id: Some("session-1".to_owned()),
    }
}

fn record(
    generation: u64,
    sequence: u64,
    revision: u64,
    operation: JournalOperation,
    prior_digest: &str,
) -> JournalRecord {
    let id = Uuid::from_u128(u128::from(sequence) + 100);
    let canonical_payload = match operation {
        JournalOperation::Upsert => Some(json!({"sequence": sequence})),
        JournalOperation::Delete => None,
    };
    let mut record = JournalRecord {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind: JournalEntityKind::Event,
        stable_entity_id: id,
        entity_revision: revision,
        operation,
        payload_sha256: sha256_hex(
            &canonical_payload
                .as_ref()
                .map(canonical_payload_bytes)
                .transpose()
                .expect("canonical payload")
                .unwrap_or_default(),
        ),
        canonical_payload,
        evidence: vec![JournalEvidenceIdentity {
            event_id: id,
            source_id: Some(Uuid::from_u128(99)),
            source_path: Some("fixture/session.jsonl".to_owned()),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(10),
            byte_end_exclusive: Some(20),
        }],
        provenance: provenance(JournalEntityKind::Event, id),
        cumulative_digest: "0".repeat(64),
    };
    record.cumulative_digest = journal_record_digest(prior_digest, &record).expect("record digest");
    record
}

fn full_request(records: Vec<JournalRecord>) -> JournalSyncRequest {
    let generation = records.first().map_or(1, |record| record.generation);
    let initial = initial_journal_digest(generation);
    let frozen = records.last().map_or_else(
        || JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial.clone(),
        },
        |record| JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: record.sequence,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: record.cumulative_digest.clone(),
        },
    );
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "a".repeat(64),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            cumulative_digest: initial,
        },
        frozen_through: frozen,
        authorized_repository_roots: Vec::new(),
        records,
    }
}

fn citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: Some(Uuid::from_u128(1)),
        observation_seq: Some(1),
        observation_kind: Some(ObservationKind::Event),
        session_id: Some(Uuid::from_u128(2)),
        event_id: Some(Uuid::from_u128(1)),
        event_seq: Some(1),
        source_path: Some("fixture/session.jsonl".to_owned()),
        fixture_line: Some(1),
        source_record_ordinal: Some(0),
        source_record_subrecord_index: Some(0),
        byte_range: Some(ByteRange {
            start: 0,
            end_exclusive: 10,
        }),
        source_sha256: Some("b".repeat(64)),
        provider_output: None,
    }
}

fn resource(kind: ResourceKind, id: &str, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn blame_request(target: BlameTarget) -> BlameRequest {
    BlameRequest {
        target,
        limit: 10,
        cursor: None,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint: full_request(Vec::new()).frozen_through,
            projection_pending: false,
        },
    }
}

fn cited_commit_blame_result() -> BlameResult {
    let commit = resource(
        ResourceKind::Commit,
        "commit:1",
        "0123456789abcdef0123456789abcdef01234567",
    );
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: resource(ResourceKind::Repository, "repository:1", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches: vec![BlameMatch::Commit(CommitBlameMatch {
            fact_id: "fact:1".to_owned(),
            fact_type: CommitFactType::Referenced,
            predicate: CommitPredicate::ReferencedBy,
            subject: commit,
            object: Some(resource(ResourceKind::Session, "session:1", "session-1")),
            fact_occurred_at_ms: None,
            confidence: FactConfidence::Explicit,
            state: FactState::Asserted,
            direct_actor: None,
            owning_root: None,
            evidence_numbers: vec![1],
        })],
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: citation(),
        }],
        next: None,
    }
}

fn cited_file_blame_result(requested_lines: Option<LineRange>, lines: LineRange) -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: resource(ResourceKind::Repository, "repository:1", "ctxrs/ctx"),
            requested_lines,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        matches: vec![BlameMatch::File(FileBlameMatch {
            id: "file-match:1".to_owned(),
            lines,
            commit: resource(
                ResourceKind::Commit,
                "commit:1",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            line_evidence_numbers: vec![1],
            production: Vec::new(),
        })],
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: citation(),
        }],
        next: None,
    }
}

fn cited_pull_request_blame_result() -> BlameResult {
    let pull_request = resource(
        ResourceKind::PullRequest,
        "pull_request:1",
        "https://github.com/ctxrs/ctx/pull/42",
    );
    BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "42".to_owned(),
            pull_request: pull_request.clone(),
            repository: resource(ResourceKind::Repository, "repository:1", "ctxrs/ctx"),
        },
        git_snapshot: None,
        matches: vec![BlameMatch::PullRequest(PullRequestBlameMatch {
            pull_request,
            relationship: PullRequestBlameRelationship::Activity(PullRequestActivity {
                fact_id: "pr-fact:1".to_owned(),
                action: PullRequestAction::Referenced,
                session: resource(ResourceKind::Session, "session:1", "session-1"),
                direct_actor: None,
                owning_root: None,
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state: FactState::Asserted,
                evidence_numbers: vec![1],
            }),
        })],
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: citation(),
        }],
        next: None,
    }
}

#[test]
fn exact_v1_frame_round_trips_and_rejects_other_versions() {
    let request = hello(0);
    let mut frame = Vec::new();
    write_frame(&mut frame, &request).unwrap();
    assert_eq!(&frame[..6], FRAME_MAGIC);
    assert_eq!(&frame[6..8], &PROTOCOL_VERSION.to_be_bytes());
    assert_eq!(
        read_frame::<_, HostEnvelope>(&mut Cursor::new(&frame)).unwrap(),
        request
    );
    for draft in [0_u16, 2, 3, 4, u16::MAX] {
        let mut incompatible = frame.clone();
        incompatible[6..8].copy_from_slice(&draft.to_be_bytes());
        assert!(matches!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(incompatible)),
            Err(FrameError::UnsupportedVersion { received, .. }) if received == draft
        ));
    }
}

#[test]
fn frame_payload_limit_is_exact_and_prevents_partial_output() {
    let exact = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 2);
    let mut output = Vec::new();
    write_frame(&mut output, &exact).unwrap();
    assert_eq!(output.len(), FRAME_HEADER_BYTES + MAX_FRAME_PAYLOAD_BYTES);

    let oversized = "x".repeat(MAX_FRAME_PAYLOAD_BYTES - 1);
    let mut rejected = Vec::new();
    assert!(matches!(
        write_frame(&mut rejected, &oversized),
        Err(FrameError::Oversized { .. })
    ));
    assert!(rejected.is_empty());
}

#[test]
fn exact_hello_binds_version_fingerprint_and_capabilities() {
    let mut fake = FakeHelper::default();
    let mut wrong = hello(0);
    let HostMessage::Hello(body) = &mut wrong.message else {
        unreachable!()
    };
    body.protocol_fingerprint = "0".repeat(64);
    assert!(matches!(
        fake.handle(wrong).message,
        HelperMessage::Error(ProtocolError {
            class: ErrorClass::ProtocolMismatch,
            ..
        })
    ));

    let response = fake.handle(hello(1)).message;
    assert!(matches!(
        response,
        HelperMessage::Hello(HelloResult {
            protocol_version: PROTOCOL_VERSION,
            ref protocol_fingerprint,
            ..
        }) if protocol_fingerprint == PROTOCOL_FINGERPRINT
    ));
}

#[test]
fn journal_full_baseline_is_contiguous_digest_bound_and_frozen() {
    let generation = 7;
    let initial = initial_journal_digest(generation);
    let first = record(generation, 1, 1, JournalOperation::Upsert, &initial);
    let second = record(
        generation,
        2,
        2,
        JournalOperation::Delete,
        &first.cumulative_digest,
    );
    let request = full_request(vec![first, second]);
    request.validate().unwrap();
    assert_eq!(request.committed_checkpoint(), request.frozen_through);
}

#[test]
fn journal_rejects_gaps_payload_mutation_and_tombstone_payloads() {
    let generation = 8;
    let initial = initial_journal_digest(generation);
    let mut skipped = record(generation, 2, 1, JournalOperation::Upsert, &initial);
    let request = full_request(vec![skipped.clone()]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Sequence);

    skipped.sequence = 1;
    skipped.canonical_payload = Some(json!({"sequence": 9}));
    let request = full_request(vec![skipped]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Corrupt);

    let mut tombstone = record(generation, 1, 2, JournalOperation::Delete, &initial);
    tombstone.canonical_payload = Some(json!({}));
    let request = full_request(vec![tombstone]);
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Corrupt);
}

#[test]
fn fake_journal_ack_is_idempotent_and_queries_require_exact_checkpoint() {
    let generation = 9;
    let initial = initial_journal_digest(generation);
    let request = full_request(vec![record(
        generation,
        1,
        1,
        JournalOperation::Upsert,
        &initial,
    )]);
    let checkpoint = request.frozen_through.clone();
    let mut fake = FakeHelper::default();
    assert!(matches!(
        fake.handle(hello(0)).message,
        HelperMessage::Hello(_)
    ));
    let first = fake.handle(host(1, HostMessage::SyncJournal(request.clone())));
    assert!(matches!(
        first.message,
        HelperMessage::JournalSynced(JournalSyncResult {
            replayed: false,
            frozen_complete: true,
            ..
        })
    ));
    let replay = fake.handle(host(2, HostMessage::SyncJournal(request)));
    assert!(matches!(
        replay.message,
        HelperMessage::JournalSynced(JournalSyncResult {
            replayed: true,
            accepted_records: 0,
            ..
        })
    ));

    let query = BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint,
            projection_pending: false,
        },
    };
    assert!(matches!(
        fake.handle(host(3, HostMessage::Blame(query))).message,
        HelperMessage::Blame(_)
    ));
}

#[test]
fn fake_output_sink_coordinates_inventory_progress_and_page_cas() {
    let mut fake = FakeHelper::default();
    assert!(matches!(
        fake.handle(hello(0)).message,
        HelperMessage::Hello(_)
    ));
    let began = fake.handle(host(
        1,
        HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation: 1 }),
    ));
    assert!(matches!(
        began.message,
        HelperMessage::OutputInventoryBegan(OutputInventoryBegan { generation: 1, .. })
    ));
    let observed = fake.handle(host(
        2,
        HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
            generation: 1,
            source: output_source(),
            availability: OutputSourceAvailability::Available,
        }),
    ));
    assert!(matches!(
        observed.message,
        HelperMessage::OutputSourceObserved(_)
    ));

    let page = empty_output_page();
    let first = fake.handle(host(3, HostMessage::MaterializeOutputPage(page.clone())));
    assert!(matches!(
        first.message,
        HelperMessage::OutputPageMaterialized(OutputPageMaterialized {
            accepted_outputs: 0,
            replayed: false,
            ..
        })
    ));
    let replay = fake.handle(host(4, HostMessage::MaterializeOutputPage(page)));
    assert!(matches!(
        replay.message,
        HelperMessage::OutputPageMaterialized(OutputPageMaterialized {
            accepted_outputs: 0,
            replayed: true,
            ..
        })
    ));
    let progress = fake.handle(host(
        5,
        HostMessage::GetOutputProgress(OutputProgressRequest {
            sources: vec![output_source()],
        }),
    ));
    assert!(matches!(
        progress.message,
        HelperMessage::OutputProgress(OutputProgressResult {
            ref sources,
            inventory_complete: false,
            ..
        }) if sources.len() == 1 && sources[0].cursor == Some(output_cursor("cursor-1"))
    ));
    let finished = fake.handle(host(
        6,
        HostMessage::FinishOutputInventory(FinishOutputInventoryRequest { generation: 1 }),
    ));
    assert!(matches!(
        finished.message,
        HelperMessage::OutputInventoryFinished(OutputInventoryFinished {
            generation: 1,
            observed_sources: 1,
            unavailable_sources: 0,
        })
    ));
}

#[test]
fn blame_target_file_ranges_are_positive_and_inclusive() {
    let file = BlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
        lines: Some(LineRange { start: 42, end: 60 }),
    };
    file.validate().unwrap();

    for lines in [
        LineRange { start: 0, end: 1 },
        LineRange { start: 2, end: 1 },
    ] {
        let invalid = BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: None,
            lines: Some(lines),
        };
        assert_eq!(
            invalid.validate().unwrap_err().class,
            ErrorClass::InvalidRequest
        );
    }
}

#[test]
fn pull_request_selectors_are_positive_numbers_or_canonical_urls() {
    for selector in [
        "1",
        "42",
        "https://github.com/ctxrs/ctx/pull/42",
        "https://gitlab.com/ctxrs/ctx/-/merge_requests/42",
        "https://gitlab.corp.example/groups/ctxrs/ctx/-/merge_requests/42",
        "https://codeberg.org/ctxrs/ctx/pulls/42",
    ] {
        let target = BlameTarget::PullRequest {
            selector: selector.to_owned(),
            repository: (!selector.starts_with("https://")).then(|| "ctxrs/ctx".to_owned()),
        };
        target.validate().unwrap_or_else(|error| {
            panic!("{selector} should be valid: {}", error.message);
        });
    }
    for selector in [
        "",
        "0",
        "-1",
        "+1",
        "01",
        "1.0",
        "#42",
        "https://github.com/ctxrs/ctx/pull/0",
        "https://github.com/ctxrs/ctx/pulls/42",
        "https://github.com/ctxrs/ctx/-/merge_requests/42",
        "https://gitlab.com/ctxrs/ctx/merge_requests/42",
        "https://gitlab.com/ctxrs/ctx/-/merge_requests/-1",
        "https://gitlab.com/ctxrs/ctx/-/merge_requests/42?token=x",
        "https://GitLab.corp.example/ctxrs/ctx/-/merge_requests/42",
        "https://bitbucket.org/ctxrs/ctx/-/merge_requests/42",
        "https://example.com/arbitrary/42",
    ] {
        let target = BlameTarget::PullRequest {
            selector: selector.to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        };
        assert!(
            target.validate().is_err(),
            "{selector} should be rejected as a PR selector"
        );
    }

    let number_without_repository = BlameTarget::PullRequest {
        selector: "42".to_owned(),
        repository: None,
    };
    let error = number_without_repository.validate().unwrap_err();
    assert_eq!(error.class, ErrorClass::InvalidRequest);
    assert_eq!(
        error.message,
        "pull request number requires a repository selector"
    );
}

#[test]
fn typed_blame_results_require_complete_deduplicated_evidence() {
    let result = cited_commit_blame_result();
    result.validate().unwrap();

    let mut unreferenced = result.clone();
    unreferenced.matches.clear();
    assert_eq!(
        unreferenced.validate().unwrap_err().class,
        ErrorClass::Corrupt
    );

    let mut duplicate_number = result;
    duplicate_number
        .evidence
        .push(duplicate_number.evidence[0].clone());
    assert_eq!(
        duplicate_number.validate().unwrap_err().class,
        ErrorClass::Corrupt
    );
}

#[test]
fn typed_blame_results_bind_matches_to_the_request_context() {
    let commit_request = blame_request(BlameTarget::Commit {
        oid: "0123456789abcdef".to_owned(),
        repository: Some("ctxrs/ctx".to_owned()),
    });
    let commit_result = cited_commit_blame_result();
    commit_result.validate_for_request(&commit_request).unwrap();

    let mut wrong_resolved_commit = commit_result.clone();
    let ResolvedBlameTarget::Commit { commit, .. } = &mut wrong_resolved_commit.target else {
        unreachable!();
    };
    commit.display = "89abcdef0123456789abcdef0123456789abcdef".to_owned();
    assert_eq!(
        wrong_resolved_commit
            .validate_for_request(&commit_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );

    let mut wrong_commit_repository = commit_result.clone();
    let ResolvedBlameTarget::Commit { repository, .. } = &mut wrong_commit_repository.target else {
        unreachable!();
    };
    repository.display = "ctxrs/other".to_owned();
    assert_eq!(
        wrong_commit_repository
            .validate_for_request(&commit_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );

    let mut unrelated_commit = commit_result.clone();
    let BlameMatch::Commit(commit_match) = &mut unrelated_commit.matches[0] else {
        unreachable!();
    };
    commit_match.subject = resource(
        ResourceKind::Commit,
        "commit:other",
        "0123456789abcdef0123456789abcdef01234567",
    );
    assert_eq!(
        unrelated_commit
            .validate_for_request(&commit_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );

    let mut target_as_object = unrelated_commit;
    let ResolvedBlameTarget::Commit {
        commit: resolved_commit,
        ..
    } = &target_as_object.target
    else {
        unreachable!();
    };
    let mut object_identity = resolved_commit.clone();
    object_identity.display = "same exact commit, alternate display".to_owned();
    let BlameMatch::Commit(commit_match) = &mut target_as_object.matches[0] else {
        unreachable!();
    };
    commit_match.object = Some(object_identity);
    target_as_object
        .validate_for_request(&commit_request)
        .unwrap();

    let mut explicit_absence = commit_result;
    explicit_absence.matches.clear();
    explicit_absence.evidence.clear();
    explicit_absence
        .validate_for_request(&commit_request)
        .unwrap();

    let file_request = blame_request(BlameTarget::File {
        path: "src/lib.rs".to_owned(),
        repository: Some("ctxrs/ctx".to_owned()),
        lines: Some(LineRange { start: 42, end: 60 }),
    });
    let file_result = cited_file_blame_result(
        Some(LineRange { start: 42, end: 60 }),
        LineRange { start: 45, end: 50 },
    );
    file_result.validate_for_request(&file_request).unwrap();
    let mut wrong_file_path = file_result.clone();
    let ResolvedBlameTarget::File { path, .. } = &mut wrong_file_path.target else {
        unreachable!();
    };
    *path = "src/other.rs".to_owned();
    assert_eq!(
        wrong_file_path
            .validate_for_request(&file_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );
    assert_eq!(
        cited_file_blame_result(
            Some(LineRange { start: 42, end: 60 }),
            LineRange { start: 41, end: 50 },
        )
        .validate_for_request(&file_request)
        .unwrap_err()
        .class,
        ErrorClass::Corrupt
    );
    assert_eq!(
        cited_file_blame_result(None, LineRange { start: 45, end: 50 })
            .validate_for_request(&file_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );

    let pull_request_request = blame_request(BlameTarget::PullRequest {
        selector: "42".to_owned(),
        repository: Some("ctxrs/ctx".to_owned()),
    });
    let pull_request_result = cited_pull_request_blame_result();
    pull_request_result
        .validate_for_request(&pull_request_request)
        .unwrap();
    let mut wrong_resolved_pull_request = pull_request_result.clone();
    let ResolvedBlameTarget::PullRequest { selector, .. } = &mut wrong_resolved_pull_request.target
    else {
        unreachable!();
    };
    *selector = "43".to_owned();
    assert_eq!(
        wrong_resolved_pull_request
            .validate_for_request(&pull_request_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );
    let mut unrelated_pull_request = pull_request_result;
    let BlameMatch::PullRequest(pull_request_match) = &mut unrelated_pull_request.matches[0] else {
        unreachable!();
    };
    pull_request_match.pull_request.id = "pull_request:other".to_owned();
    assert_eq!(
        unrelated_pull_request
            .validate_for_request(&pull_request_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );

    assert_eq!(
        cited_commit_blame_result()
            .validate_for_request(&file_request)
            .unwrap_err()
            .class,
        ErrorClass::Corrupt
    );
}

#[test]
fn typed_blame_results_obey_the_request_limit() {
    let request = blame_request(BlameTarget::Commit {
        oid: "0123456789abcdef".to_owned(),
        repository: Some("ctxrs/ctx".to_owned()),
    });
    let mut request = request;
    request.limit = 1;

    let mut result = cited_commit_blame_result();
    result.matches.push(result.matches[0].clone());
    assert_eq!(
        result.validate_for_request(&request).unwrap_err().class,
        ErrorClass::Bounds
    );
}

#[test]
fn resource_kind_inventory_round_trips_and_rejects_unknown_values() {
    for kind in ResourceKind::ALL {
        assert_eq!(ResourceKind::from_wire_name(kind.wire_name()), Some(kind));
        assert!(matches!(
            serde_json::from_value::<ResourceKind>(serde_json::json!(kind.wire_name())),
            Ok(decoded) if decoded == kind
        ));
    }
    assert_eq!(ResourceKind::from_wire_name("deployment"), None);
    assert!(serde_json::from_str::<ResourceKind>(r#""deployment""#).is_err());
}

#[test]
fn unknown_and_duplicate_envelope_fields_fail_closed() {
    let unknown = json!({
        "sequence": 0,
        "request_id": Uuid::from_u128(1),
        "message": {"kind": "status", "body": {}},
        "extra": true
    });
    assert!(serde_json::from_value::<HostEnvelope>(unknown).is_err());
    let duplicate = format!(
        "{{\"sequence\":0,\"sequence\":1,\"request_id\":\"{}\",\"message\":{{\"kind\":\"status\",\"body\":{{}}}}}}",
        Uuid::from_u128(1)
    );
    assert!(serde_json::from_str::<HostEnvelope>(&duplicate).is_err());
    let nil = json!({
        "sequence": 0,
        "request_id": Uuid::nil(),
        "message": {"kind": "status", "body": {}}
    });
    assert!(serde_json::from_value::<HostEnvelope>(nil.clone()).is_err());
    assert!(serde_json::from_value::<HelperEnvelope>(json!({
        "sequence": 0,
        "request_id": Uuid::nil(),
        "message": {"kind": "error", "body": {
            "class": "invalid_request", "message": "nil", "retryable": false
        }}
    }))
    .is_err());
}

#[test]
fn journal_roots_are_bounded_sorted_unique_and_control_free() {
    let mut request = full_request(Vec::new());
    request.authorized_repository_roots = vec!["/repo/a".to_owned(), "/repo/b".to_owned()];
    request.validate().unwrap();
    request.authorized_repository_roots.reverse();
    assert_eq!(
        request.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
    request.authorized_repository_roots = vec!["/repo\ncontrol".to_owned()];
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
    request.authorized_repository_roots = (0..=MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| format!("/{index:03}"))
        .collect();
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
}

proptest! {
    #[test]
    fn arbitrary_split_payloads_never_decode_as_complete_frames(split in 0usize..128) {
        let mut frame = Vec::new();
        write_frame(&mut frame, &hello(0)).unwrap();
        let split = split.min(frame.len().saturating_sub(1));
        prop_assert!(read_frame::<_, HostEnvelope>(&mut Cursor::new(&frame[..split])).is_err());
    }

    #[test]
    fn arbitrary_non_v1_headers_fail_before_json(version in any::<u16>().prop_filter("not V1", |v| *v != PROTOCOL_VERSION)) {
        let mut frame = Vec::new();
        frame.extend_from_slice(FRAME_MAGIC);
        frame.extend_from_slice(&version.to_be_bytes());
        frame.extend_from_slice(&2_u32.to_be_bytes());
        frame.extend_from_slice(b"{}");
        let rejected = matches!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(frame)),
            Err(FrameError::UnsupportedVersion { received, .. }) if received == version
        );
        prop_assert!(rejected, "non-V1 frame was not rejected exactly");
    }
}
