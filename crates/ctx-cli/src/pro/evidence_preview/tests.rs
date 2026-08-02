use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, GitObjectFormat, GitObjectId,
    NativeItemKey, NativeSessionKey, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryOutcomeKind, RepositoryOutcomeLinkage,
    RepositoryOutcomeObservation, RepositoryVcsObservation, RepositoryVcsObservationKind,
    SessionIdentityInput, SourceAnchor, SourceKey, TypedKey,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};
use ctx_history_index::{CoreEventRecord, EventRecord};
use ctx_pro_host_protocol::{
    BlameResult, EvidenceCitation, GitSnapshot, NumberedEvidence, ResolvedBlameTarget,
    ResourceKind, ResourceRef, WorktreeStatus,
};

use super::{
    project_evidence_previews, EvidencePreviewKind, VerifiedEvidenceRecord,
    MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES, MAX_EVIDENCE_PREVIEW_CITATIONS,
    MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};

const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const OID: &str = "0123456789abcdef0123456789abcdef01234567";

fn resource(id: &str, kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn repository() -> ResourceRef {
    resource(
        "repository:ctxrs-ctx",
        ResourceKind::Repository,
        "ctxrs/ctx",
    )
}

fn source(provider: &str, seed: u8) -> SourceKey {
    SourceKey::derive(
        provider,
        format!("{provider}_session_jsonl"),
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([seed; 32]),
    )
    .unwrap()
}

fn binding(format: Option<GitObjectFormat>) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: "binding-1".to_owned(),
        logical_repository_id: "forge:github.com/ctxrs/ctx".to_owned(),
        checkout_id: None,
        worktree_id: None,
        aliases: Vec::new(),
        git_object_format: format,
        local_root_authorization: None,
        evidence: vec![RepositoryEvidence {
            kind: RepositoryEvidenceKind::FileActivity,
            confidence: RepositoryEvidenceConfidence::Explicit,
        }],
        association_policy_revision: CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn base_record(provider: &str, seed: u8, sequence: u64, body: &str) -> CoreEventRecord {
    let source = source(provider, seed);
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
        logical_item_kind: "tool_result",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(sequence)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let mut core = ctx_history_core::CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "tool_result",
        "codex",
        true,
        "fixture-v1",
        body,
    )
    .unwrap();
    core.role = Some("assistant".to_owned());
    let event = EventRecord {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        provider: provider.to_owned(),
        source_format: source.source_format().to_owned(),
        provider_session_id: None,
        native_event_id: None,
        branch: None,
        agent_type: "codex".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: None,
        event_type: "tool_result".to_owned(),
        role: Some("assistant".to_owned()),
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    };
    CoreEventRecord {
        event,
        core_record: core,
    }
}

fn file_record(
    seed: u8,
    sequence: u64,
    body: &str,
    path: &str,
    kind: RepositoryFileObservationKind,
    prior_path: Option<&str>,
) -> CoreEventRecord {
    let mut record = base_record("codex", seed, sequence, body);
    record.core_record.repository_bindings = vec![binding(None)];
    record.core_record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: path.to_owned(),
        kind,
        prior_relative_path: prior_path.map(str::to_owned),
    }];
    record
}

fn commit_record(seed: u8, sequence: u64, body: &str, outcomes: usize) -> CoreEventRecord {
    let mut record = base_record("codex", seed, sequence, body);
    record.core_record.repository_bindings = vec![binding(Some(GitObjectFormat::Sha1))];
    record.core_record.repository_vcs_observations = (0..outcomes)
        .map(|index| RepositoryVcsObservation {
            repository_binding_id: "binding-1".to_owned(),
            kind: RepositoryVcsObservationKind::Outcome(Box::new(RepositoryOutcomeObservation {
                kind: RepositoryOutcomeKind::Commit,
                produced_object_ids: vec![GitObjectId {
                    format: GitObjectFormat::Sha1,
                    hex: OID.to_owned(),
                }],
                replacement_lineage: Vec::new(),
                pull_request: None,
                observed_at_unix_ms: i64::try_from(index).unwrap(),
                linkage: RepositoryOutcomeLinkage {
                    provider: "codex".to_owned(),
                    origin_call_id: format!("origin-{index}"),
                    result_call_id: format!("result-{index}"),
                    origin_event_sequence: sequence.saturating_sub(1),
                    continuation_call_id_sha256: Vec::new(),
                    result_record_sha256: [index as u8 + 1; 32],
                },
                outcome_capture_revision: CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            })),
            object_id: None,
            parent_object_ids: Vec::new(),
            reference: None,
            relative_path: None,
        })
        .collect();
    record
}

fn numbered(record: &CoreEventRecord, number: u32) -> NumberedEvidence {
    NumberedEvidence {
        number,
        citation: EvidenceCitation {
            core_generation_id: GENERATION.to_owned(),
            source: record.source.clone(),
            session_id: record.session_id,
            event_id: record.event_id,
            event_sequence: record.event_sequence,
            byte_range: None,
            evidence_sha256: Some(DIGEST.to_owned()),
        },
    }
}

fn file_result(path: &str, evidence: Vec<NumberedEvidence>) -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::File {
            path: path.to_owned(),
            repository: repository(),
            requested_lines: None,
        },
        git_snapshot: Some(GitSnapshot {
            head_oid: OID.to_owned(),
            worktree_status: WorktreeStatus::Clean,
        }),
        matches: Vec::new(),
        evidence,
        next: None,
    }
}

fn commit_result(oid: &str, evidence: Vec<NumberedEvidence>) -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: resource(&format!("commit:{oid}"), ResourceKind::Commit, oid),
            repository: repository(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence,
        next: None,
    }
}

fn verified<'a>(
    evidence: &'a NumberedEvidence,
    record: &'a CoreEventRecord,
) -> VerifiedEvidenceRecord<'a> {
    VerifiedEvidenceRecord::new(evidence, GENERATION, DIGEST, record).unwrap()
}

fn one_file_preview(target: &str, record: &CoreEventRecord) -> super::EvidencePreviewModel {
    let evidence = numbered(record, 1);
    let proof = verified(&evidence, record);
    project_evidence_previews(&file_result(target, vec![evidence.clone()]), &[proof])
}

#[test]
fn file_operations_require_exact_typed_agreement() {
    let cases = [
        (
            "*** Begin Patch\n*** Add File: src/new.rs\n+body\n*** End Patch",
            "src/new.rs",
            RepositoryFileObservationKind::Created,
            "*** Add File: src/new.rs",
        ),
        (
            "before\n*** Update File: src/lib.rs\n@@\n-old\n+new\nafter",
            "src/lib.rs",
            RepositoryFileObservationKind::Modified,
            "*** Update File: src/lib.rs",
        ),
        (
            "*** Delete File: src/old.rs\n-old body",
            "src/old.rs",
            RepositoryFileObservationKind::Deleted,
            "*** Delete File: src/old.rs",
        ),
        (
            "read: src/read.rs\ncontents must stay adjacent",
            "src/read.rs",
            RepositoryFileObservationKind::Read,
            "read: src/read.rs",
        ),
    ];
    for (index, (body, path, kind, expected)) in cases.into_iter().enumerate() {
        let record = file_record(index as u8 + 1, 1, body, path, kind, None);
        let model = one_file_preview(path, &record);
        assert_eq!(model.previews.len(), 1, "{kind:?}");
        assert_eq!(model.previews[0].kind, EvidencePreviewKind::File(kind));
        assert_eq!(model.previews[0].excerpt, expected);
    }
}

#[test]
fn rename_old_and_new_paths_use_complete_boundary_safe_units() {
    let body = "before\n*** Update File: src/old.rs\n*** Move to: src/new.rs\nafter";
    let record = file_record(
        5,
        1,
        body,
        "src/new.rs",
        RepositoryFileObservationKind::Renamed,
        Some("src/old.rs"),
    );
    assert_eq!(
        one_file_preview("src/old.rs", &record).previews[0].excerpt,
        "*** Update File: src/old.rs\n*** Move to: src/new.rs"
    );
    assert_eq!(
        one_file_preview("src/new.rs", &record).previews[0].excerpt,
        "*** Move to: src/new.rs"
    );

    let boundary = file_record(
        6,
        1,
        "*** Update File: src/old.rs.bak\n*** Move to: src/new.rs.bak",
        "src/new.rs",
        RepositoryFileObservationKind::Renamed,
        Some("src/old.rs"),
    );
    assert!(one_file_preview("src/old.rs", &boundary)
        .previews
        .is_empty());
}

#[test]
fn unified_diff_markers_cover_created_modified_deleted_and_renamed() {
    let cases = [
        (
            "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644\n--- /dev/null",
            "src/new.rs",
            RepositoryFileObservationKind::Created,
            None,
            "diff --git a/src/new.rs b/src/new.rs\nnew file mode 100644",
        ),
        (
            "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs",
            "src/lib.rs",
            RepositoryFileObservationKind::Modified,
            None,
            "diff --git a/src/lib.rs b/src/lib.rs",
        ),
        (
            "diff --git a/src/old.rs b/src/old.rs\ndeleted file mode 100644\n--- a/src/old.rs",
            "src/old.rs",
            RepositoryFileObservationKind::Deleted,
            None,
            "diff --git a/src/old.rs b/src/old.rs\ndeleted file mode 100644",
        ),
        (
            "diff --git a/src/old.rs b/src/new.rs\nsimilarity index 100%\nrename from src/old.rs\nrename to src/new.rs",
            "src/new.rs",
            RepositoryFileObservationKind::Renamed,
            Some("src/old.rs"),
            "rename to src/new.rs",
        ),
    ];
    for (index, (body, path, kind, prior, expected)) in cases.into_iter().enumerate() {
        let record = file_record(index as u8 + 10, 1, body, path, kind, prior);
        let model = one_file_preview(path, &record);
        assert_eq!(model.previews.len(), 1, "{kind:?}");
        assert_eq!(model.previews[0].excerpt, expected);
    }
}

#[test]
fn exact_path_matching_rejects_basename_case_and_token_boundary_lookalikes() {
    for (index, body) in [
        "*** Update File: lib.rs",
        "*** Update File: src/Lib.rs",
        "*** Update File: src/lib.rs.bak",
        "*** Update File: prefixsrc/lib.rs",
    ]
    .into_iter()
    .enumerate()
    {
        let record = file_record(
            index as u8 + 20,
            1,
            body,
            "src/lib.rs",
            RepositoryFileObservationKind::Modified,
            None,
        );
        assert!(one_file_preview("src/lib.rs", &record).previews.is_empty());
    }

    let absolute = file_record(
        24,
        1,
        "*** Update File: /tmp/worktree/src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_eq!(
        one_file_preview("src/lib.rs", &absolute).previews[0].excerpt,
        "*** Update File: /tmp/worktree/src/lib.rs"
    );
}

#[test]
fn duplicate_conflicting_and_multiple_decisive_file_units_omit() {
    let mut duplicate = file_record(
        30,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    duplicate
        .core_record
        .repository_file_observations
        .push(duplicate.core_record.repository_file_observations[0].clone());
    assert!(one_file_preview("src/lib.rs", &duplicate)
        .previews
        .is_empty());

    let mut conflicting = file_record(
        31,
        1,
        "*** Add File: src/lib.rs\n*** Delete File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Deleted,
        None,
    );
    conflicting
        .core_record
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-1".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Created,
            prior_relative_path: None,
        });
    assert!(one_file_preview("src/lib.rs", &conflicting)
        .previews
        .is_empty());

    let repeated = file_record(
        32,
        1,
        "*** Update File: src/lib.rs\n*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(one_file_preview("src/lib.rs", &repeated)
        .previews
        .is_empty());
}

#[test]
fn commit_requires_one_certified_success_outcome_and_one_exact_oid_unit() {
    let success = commit_record(
        40,
        2,
        &format!("Process exited with code 0\nOutput:\n[main 0123456] exact\n{OID}\nadjacent"),
        1,
    );
    let evidence = numbered(&success, 1);
    let model = project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &success)],
    );
    assert_eq!(model.previews[0].kind, EvidencePreviewKind::Commit);
    assert_eq!(model.previews[0].excerpt, OID);

    let failed = commit_record(
        41,
        2,
        &format!("Process exited with code 1\nOutput:\n{OID}"),
        1,
    );
    let evidence = numbered(&failed, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &failed)],
    )
    .previews
    .is_empty());

    let mention_only = commit_record(42, 2, &format!("git show {OID}"), 0);
    let evidence = numbered(&mention_only, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &mention_only)],
    )
    .previews
    .is_empty());

    let certified_but_textually_unproven = commit_record(47, 2, OID, 1);
    let evidence = numbered(&certified_but_textually_unproven, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &certified_but_textually_unproven)],
    )
    .previews
    .is_empty());
}

#[test]
fn commit_case_token_duplicate_unit_and_duplicate_outcome_ambiguity_omit() {
    let uppercase = OID.to_ascii_uppercase();
    let case = commit_record(
        43,
        2,
        &format!("Process exited with code 0\nOutput:\n{uppercase}"),
        1,
    );
    let evidence = numbered(&case, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &case)],
    )
    .previews
    .is_empty());

    let boundary = commit_record(
        44,
        2,
        &format!("Process exited with code 0\nOutput:\na{OID}"),
        1,
    );
    let evidence = numbered(&boundary, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &boundary)],
    )
    .previews
    .is_empty());

    let repeated = commit_record(
        45,
        2,
        &format!("Process exited with code 0\nOutput:\n{OID}\n{OID}"),
        1,
    );
    let evidence = numbered(&repeated, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &repeated)],
    )
    .previews
    .is_empty());

    let outcomes = commit_record(
        46,
        2,
        &format!("Process exited with code 0\nOutput:\n{OID}"),
        2,
    );
    let evidence = numbered(&outcomes, 1);
    assert!(project_evidence_previews(
        &commit_result(OID, vec![evidence.clone()]),
        &[verified(&evidence, &outcomes)],
    )
    .previews
    .is_empty());
}

#[test]
fn utf8_exact_512_byte_unit_is_kept_and_oversized_unit_is_omitted() {
    let prefix = "*** Update File: ";
    let exact_path = format!("{}x", "é".repeat((512 - prefix.len() - 1) / 2));
    let exact_body = format!("{prefix}{exact_path}");
    assert_eq!(exact_body.len(), 512);
    let exact = file_record(
        50,
        1,
        &exact_body,
        &exact_path,
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_eq!(
        one_file_preview(&exact_path, &exact).previews[0].excerpt,
        exact_body
    );

    let oversized_path = format!("{exact_path}a");
    let oversized_body = format!("{prefix}{oversized_path}");
    assert_eq!(oversized_body.len(), 513);
    let oversized = file_record(
        51,
        1,
        &oversized_body,
        &oversized_path,
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(one_file_preview(&oversized_path, &oversized)
        .previews
        .is_empty());
}

#[test]
fn evidence_is_number_ordered_limited_and_deduplicated_by_event_and_excerpt() {
    let shared = file_record(
        60,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let second = file_record(
        61,
        2,
        "*** Update File: src/lib.rs\nsecond adjacent",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let fourth = file_record(
        62,
        3,
        "*** Update File: src/lib.rs\nfourth adjacent",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let evidence = [
        numbered(&shared, 1),
        numbered(&shared, 2),
        numbered(&second, 3),
        numbered(&fourth, 4),
    ];
    let proofs = [
        verified(&evidence[2], &second),
        verified(&evidence[1], &shared),
        verified(&evidence[3], &fourth),
        verified(&evidence[0], &shared),
    ];
    let result = file_result("src/lib.rs", evidence.to_vec());
    let model = project_evidence_previews(&result, &proofs);
    assert_eq!(MAX_EVIDENCE_PREVIEW_CITATIONS, 3);
    assert_eq!(model.previews.len(), 2);
    assert_eq!(model.previews[0].evidence_numbers, vec![1, 2]);
    assert_eq!(model.previews[1].evidence_numbers, vec![3]);
    assert!(model
        .previews
        .iter()
        .all(|preview| preview.event_id != fourth.event_id));
}

#[test]
fn digest_generation_and_all_coordinates_must_match() {
    let record = file_record(
        70,
        7,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let evidence = numbered(&record, 1);
    assert!(VerifiedEvidenceRecord::new(&evidence, "b", DIGEST, &record).is_none());
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, "f", &record).is_none());

    let mut missing = evidence.clone();
    missing.citation.evidence_sha256 = None;
    assert!(VerifiedEvidenceRecord::new(&missing, GENERATION, DIGEST, &record).is_none());

    let mut ranged = evidence.clone();
    ranged.citation.byte_range = Some(ctx_pro_host_protocol::ByteRange {
        start: 0,
        end_exclusive: 1,
    });
    assert!(VerifiedEvidenceRecord::new(&ranged, GENERATION, DIGEST, &record).is_none());

    let other = file_record(
        71,
        8,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, DIGEST, &other).is_none());

    for mutate in [0, 1, 2] {
        let mut mismatch = record.clone();
        match mutate {
            0 => mismatch.event.event_sequence += 1,
            1 => mismatch.core_record.event_sequence += 1,
            2 => mismatch.event.session_id = other.session_id,
            _ => unreachable!(),
        }
        assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, DIGEST, &mismatch).is_none());
    }
}

#[test]
fn pull_requests_non_codex_and_unverified_records_are_normal_omissions() {
    let record = file_record(
        80,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let evidence = numbered(&record, 1);
    let proof = verified(&evidence, &record);
    let pr = BlameResult {
        target: ResolvedBlameTarget::PullRequest {
            selector: "1".to_owned(),
            pull_request: resource("pr:1", ResourceKind::PullRequest, "#1"),
            repository: repository(),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: vec![evidence.clone()],
        next: None,
    };
    assert!(project_evidence_previews(&pr, &[proof]).previews.is_empty());
    assert!(
        project_evidence_previews(&file_result("src/lib.rs", vec![evidence]), &[])
            .previews
            .is_empty()
    );

    let mut non_codex = file_record(
        81,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    non_codex.event.provider = "claude".to_owned();
    let evidence = numbered(&non_codex, 1);
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, DIGEST, &non_codex).is_none());
}

#[test]
fn excerpt_never_expands_to_adjacent_content_and_repeats_deterministically() {
    let record = file_record(
        90,
        1,
        "SECRET BEFORE\n*** Update File: src/lib.rs\nSECRET AFTER",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let evidence = numbered(&record, 1);
    let proof = verified(&evidence, &record);
    let result = file_result("src/lib.rs", vec![evidence.clone()]);
    let expected = project_evidence_previews(&result, &[proof]);
    assert_eq!(expected.previews[0].excerpt, "*** Update File: src/lib.rs");
    for _ in 0..20 {
        assert_eq!(project_evidence_previews(&result, &[proof]), expected);
    }
}

#[test]
fn public_byte_limits_are_exact_and_aggregate_output_obeys_them() {
    assert_eq!(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES, 512);
    assert_eq!(MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES, 4_096);

    let records = (0..3)
        .map(|index| {
            file_record(
                100 + index,
                u64::from(index),
                &format!("modified: src/{index}.rs"),
                &format!("src/{index}.rs"),
                RepositoryFileObservationKind::Modified,
                None,
            )
        })
        .collect::<Vec<_>>();
    let evidence = records
        .iter()
        .enumerate()
        .map(|(index, record)| numbered(record, u32::try_from(index + 1).unwrap()))
        .collect::<Vec<_>>();
    let proofs = evidence
        .iter()
        .zip(&records)
        .map(|(citation, record)| verified(citation, record))
        .collect::<Vec<_>>();
    let model = project_evidence_previews(&file_result("src/0.rs", evidence.clone()), &proofs);
    assert!(
        model
            .previews
            .iter()
            .map(|preview| preview.excerpt.len())
            .sum::<usize>()
            <= MAX_EVIDENCE_PREVIEW_AGGREGATE_BYTES
    );
}
