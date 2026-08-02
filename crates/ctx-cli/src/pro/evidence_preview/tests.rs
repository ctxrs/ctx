use ctx_history_core::{
    derive_event_id, derive_session_id, EventIdentityInput, NativeItemKey, NativeSessionKey,
    RepositoryAlias, RepositoryAliasKind, RepositoryBinding, RepositoryEvidence,
    RepositoryEvidenceConfidence, RepositoryEvidenceKind, RepositoryFileObservation,
    RepositoryFileObservationKind, RepositoryLocalRootAuthorization, SessionIdentityInput,
    SourceAnchor, SourceKey, TypedKey, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
};
use ctx_history_index::{CoreEventRecord, EventRecord};
use ctx_pro_host_protocol::{
    BlameResult, EvidenceCitation, GitSnapshot, NumberedEvidence, ResolvedBlameTarget,
    ResourceKind, ResourceRef, WorktreeStatus,
};
use sha2::{Digest, Sha256};

use super::{
    project_evidence_previews, VerifiedEvidenceRecord, MAX_EVIDENCE_PREVIEW_BODY_BYTES,
    MAX_EVIDENCE_PREVIEW_BODY_LINES, MAX_EVIDENCE_PREVIEW_CITATIONS,
    MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES,
};

const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OID: &str = "0123456789abcdef0123456789abcdef01234567";
const REPOSITORY_ID: &str = "forge:github.com/ctxrs/ctx";
const REPOSITORY_RESOURCE_ID: &str = "repository:opaque-derived-graph-id";

fn protocol_snapshot() -> ctx_pro_host_protocol::QuerySnapshotExpectation {
    ctx_pro_host_protocol::QuerySnapshotExpectation::Core {
        receipt: ctx_pro_host_protocol::CoreMaterializationReceiptIdentity {
            core_generation_id: GENERATION.to_owned(),
            materializer_revision: "materializer-v1".to_owned(),
        },
    }
}

fn resource(id: &str, kind: ResourceKind, display: &str) -> ResourceRef {
    ResourceRef {
        id: id.to_owned(),
        kind,
        display: display.to_owned(),
    }
}

fn repository() -> ResourceRef {
    resource(
        REPOSITORY_RESOURCE_ID,
        ResourceKind::Repository,
        REPOSITORY_ID,
    )
}

fn source(provider: &str, seed: u8) -> SourceKey {
    source_contract(
        provider,
        if provider == "codex" {
            "codex_session_jsonl"
        } else {
            "unsupported_session_jsonl"
        },
        "codex-nativepath-jsonl-v0",
        seed,
    )
}

fn source_contract(provider: &str, format: &str, schema: &str, seed: u8) -> SourceKey {
    SourceKey::derive(
        provider,
        format,
        schema,
        1,
        SourceAnchor::CatalogLineage([seed; 32]),
    )
    .unwrap()
}

fn binding() -> RepositoryBinding {
    binding_for("binding-1", REPOSITORY_ID)
}

fn binding_for(binding_id: &str, logical_repository_id: &str) -> RepositoryBinding {
    RepositoryBinding {
        binding_id: binding_id.to_owned(),
        logical_repository_id: logical_repository_id.to_owned(),
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

fn authorize_local_root(record: &mut CoreEventRecord, local_root: &str) {
    authorize_binding(
        &mut record.core_record.repository_bindings[0],
        local_root,
        "1",
    );
}

fn authorize_binding(binding: &mut RepositoryBinding, local_root: &str, identity: &str) {
    binding.checkout_id = Some(format!("checkout-{identity}"));
    binding.worktree_id = Some(format!("worktree-{identity}"));
    binding.local_root_authorization = Some(RepositoryLocalRootAuthorization {
        local_root: local_root.to_owned(),
        local_root_authorization_fingerprint_revision:
            CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
        local_root_authorization_fingerprint: [7; 32],
        observed_at_unix_ms: 1,
    });
}

fn base_record(
    provider: &str,
    seed: u8,
    sequence: u64,
    body: &str,
    event_type: &str,
    role: &str,
) -> CoreEventRecord {
    base_record_for_source(
        source(provider, seed),
        seed,
        sequence,
        body,
        event_type,
        role,
        "codex-nativepath-core-record-v7",
    )
}

fn base_record_for_source(
    source: SourceKey,
    seed: u8,
    sequence: u64,
    body: &str,
    event_type: &str,
    role: &str,
    parser_revision: &str,
) -> CoreEventRecord {
    let provider = source.provider().to_owned();
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
        logical_item_kind: event_type,
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
        event_type,
        "primary",
        true,
        parser_revision,
        body,
    )
    .unwrap();
    core.role = Some(role.to_owned());
    let event = EventRecord {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        provider,
        source_format: source.source_format().to_owned(),
        provider_session_id: None,
        native_event_id: None,
        branch: None,
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: None,
        event_type: event_type.to_owned(),
        role: Some(role.to_owned()),
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
    let record = base_record("codex", seed, sequence, body, "tool_call", "assistant");
    file_record_from_base(record, path, kind, prior_path)
}

fn file_record_from_base(
    mut record: CoreEventRecord,
    path: &str,
    kind: RepositoryFileObservationKind,
    prior_path: Option<&str>,
) -> CoreEventRecord {
    record.core_record.repository_bindings = vec![binding()];
    record.core_record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: path.to_owned(),
        kind,
        prior_relative_path: prior_path.map(str::to_owned),
    }];
    record
}

fn numbered(record: &CoreEventRecord, number: u32) -> NumberedEvidence {
    let encoded = record.core_record.encode_stored().unwrap();
    let digest = format!("{:x}", Sha256::digest(encoded));
    NumberedEvidence {
        number,
        citation: EvidenceCitation {
            core_generation_id: GENERATION.to_owned(),
            source: record.source.clone(),
            session_id: record.session_id,
            event_id: record.event_id,
            event_sequence: record.event_sequence,
            byte_range: None,
            evidence_sha256: Some(digest),
        },
    }
}

fn file_result(path: &str, evidence: Vec<NumberedEvidence>) -> BlameResult {
    BlameResult {
        snapshot: protocol_snapshot(),
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

fn verified<'a>(
    evidence: &'a NumberedEvidence,
    record: &'a CoreEventRecord,
) -> VerifiedEvidenceRecord<'a> {
    VerifiedEvidenceRecord::new(evidence, GENERATION, record).unwrap()
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
        assert_eq!(model.previews[0].file_kind, kind);
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
        "*** Update File: src/old.rs\n*** Move to: src/new.rs"
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
    assert!(one_file_preview("src/lib.rs", &absolute)
        .previews
        .is_empty());
}

#[test]
fn certified_local_root_allows_only_the_exact_authorized_absolute_path() {
    let root = "/worktrees/validated";
    let mut authorized = file_record(
        25,
        1,
        "*** Update File: /worktrees/validated/src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    authorize_local_root(&mut authorized, root);
    assert_eq!(
        one_file_preview("src/lib.rs", &authorized).previews[0].excerpt,
        "*** Update File: /worktrees/validated/src/lib.rs"
    );

    let mut other_root = file_record(
        26,
        1,
        "*** Update File: /other/repository/src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    authorize_local_root(&mut other_root, root);
    assert!(one_file_preview("src/lib.rs", &other_root)
        .previews
        .is_empty());

    let mut traversal = file_record(
        27,
        1,
        "*** Update File: /worktrees/validated/src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    authorize_local_root(&mut traversal, "/worktrees/validated/../validated");
    assert!(one_file_preview("src/lib.rs", &traversal)
        .previews
        .is_empty());
}

#[test]
fn certified_local_root_preserves_exact_absolute_rename_old_and_new_paths() {
    let root =
        "/worktrees/validated/evidence-preview-release-fixture-source-backed-ingestion-20260728";
    let old_path = "products/ctx-pro/src/graph/store/checkpoints.rs";
    let new_path = "products/ctx-pro/src/graph/store/generation.rs";
    let body = format!("*** Update File: {root}/{old_path}\n*** Move to: {root}/{new_path}");
    assert_eq!(body.len(), 298);
    let mut record = file_record(
        28,
        1,
        &body,
        new_path,
        RepositoryFileObservationKind::Renamed,
        Some(old_path),
    );
    authorize_local_root(&mut record, root);
    assert_eq!(
        one_file_preview(old_path, &record).previews[0].excerpt,
        body
    );
    assert_eq!(
        one_file_preview(new_path, &record).previews[0].excerpt,
        body
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
fn codex_contract_allowlist_rejects_unknown_format_schema_revision_and_event_shape() {
    let valid = file_record(
        33,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    let evidence = numbered(&valid, 1);
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &valid).is_some());

    for variant in 0..5 {
        let mut record = if matches!(variant, 0 | 1) {
            file_record_from_base(
                base_record_for_source(
                    source_contract(
                        "codex",
                        if variant == 0 {
                            "unknown_codex_jsonl"
                        } else {
                            "codex_session_jsonl"
                        },
                        if variant == 1 {
                            "unknown-schema"
                        } else {
                            "codex-nativepath-jsonl-v0"
                        },
                        34 + variant,
                    ),
                    34 + variant,
                    1,
                    "*** Update File: src/lib.rs",
                    "tool_call",
                    "assistant",
                    "codex-nativepath-core-record-v7",
                ),
                "src/lib.rs",
                RepositoryFileObservationKind::Modified,
                None,
            )
        } else {
            valid.clone()
        };
        match variant {
            0 | 1 => {}
            2 => record.core_record.parser_revision = "codex-nativepath-core-record-v8".to_owned(),
            3 => record.core_record.normalization_revision += 1,
            4 => {
                record.event.event_type = "message".to_owned();
                record.core_record.event_type = "message".to_owned();
            }
            _ => unreachable!(),
        }
        let evidence = numbered(&record, 1);
        assert!(
            VerifiedEvidenceRecord::new(&evidence, GENERATION, &record).is_none(),
            "variant {variant}"
        );
    }
}

#[test]
fn unsupported_and_mixed_file_grammars_omit() {
    for (index, body) in [
        "updated file src/lib.rs",
        "*** Update File: src/lib.rs\ndiff --git a/src/lib.rs b/src/lib.rs",
        "*** Update File: src/lib.rs\nmodified: src/lib.rs",
        "diff --git a/src/lib.rs b/src/lib.rs\nmodified: src/lib.rs",
    ]
    .into_iter()
    .enumerate()
    {
        let record = file_record(
            34 + u8::try_from(index).unwrap(),
            1,
            body,
            "src/lib.rs",
            RepositoryFileObservationKind::Modified,
            None,
        );
        assert!(
            one_file_preview("src/lib.rs", &record).previews.is_empty(),
            "{body}"
        );
    }

    let wrong_shape = base_record(
        "codex",
        38,
        1,
        "*** Update File: src/lib.rs",
        "command_output",
        "tool",
    );
    let mut wrong_shape = wrong_shape;
    wrong_shape.core_record.repository_bindings = vec![binding()];
    wrong_shape.core_record.repository_file_observations = vec![RepositoryFileObservation {
        repository_binding_id: "binding-1".to_owned(),
        relative_path: "src/lib.rs".to_owned(),
        kind: RepositoryFileObservationKind::Modified,
        prior_relative_path: None,
    }];
    let evidence = numbered(&wrong_shape, 1);
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &wrong_shape).is_none());
}

#[test]
fn production_repository_display_mapping_scopes_file_observations_exactly() {
    let exact = file_record(
        39,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_ne!(repository().id, repository().display);
    assert_eq!(
        one_file_preview("src/lib.rs", &exact).previews[0].excerpt,
        "*** Update File: src/lib.rs"
    );

    let mut other_repository = file_record(
        40,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    other_repository
        .core_record
        .repository_bindings
        .push(binding_for("binding-2", "forge:github.com/fork/ctx"));
    other_repository.core_record.repository_file_observations[0].repository_binding_id =
        "binding-2".to_owned();
    assert!(one_file_preview("src/lib.rs", &other_repository)
        .previews
        .is_empty());

    let mut ambiguous = file_record(
        41,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    ambiguous
        .core_record
        .repository_bindings
        .push(binding_for("binding-2", REPOSITORY_ID));
    assert!(one_file_preview("src/lib.rs", &ambiguous)
        .previews
        .is_empty());

    let mut exact_alias = file_record(
        42,
        1,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    exact_alias.core_record.repository_bindings[0].logical_repository_id =
        "local:certified".to_owned();
    exact_alias.core_record.repository_bindings[0].aliases = vec![RepositoryAlias {
        kind: RepositoryAliasKind::Forge,
        host: "github.com".to_owned(),
        namespace: vec!["ctxrs".to_owned()],
        name: "ctx".to_owned(),
        remote_name: Some("origin".to_owned()),
    }];
    assert!(one_file_preview("src/lib.rs", &exact_alias)
        .previews
        .is_empty());
}

#[test]
fn same_path_other_repositories_require_distinct_certified_absolute_roots() {
    for (index, logical_repository_id) in [
        "forge:github.com/fork/ctx",
        "local:certified-other-repository",
    ]
    .into_iter()
    .enumerate()
    {
        let mut record = file_record(
            43 + u8::try_from(index).unwrap(),
            1,
            "*** Update File: src/lib.rs",
            "src/lib.rs",
            RepositoryFileObservationKind::Modified,
            None,
        );
        record
            .core_record
            .repository_bindings
            .push(binding_for("binding-2", logical_repository_id));
        record
            .core_record
            .repository_file_observations
            .push(RepositoryFileObservation {
                repository_binding_id: "binding-2".to_owned(),
                relative_path: "src/lib.rs".to_owned(),
                kind: RepositoryFileObservationKind::Modified,
                prior_relative_path: None,
            });
        assert!(
            one_file_preview("src/lib.rs", &record).previews.is_empty(),
            "{logical_repository_id}"
        );
    }

    let mut prior_path = file_record(
        45,
        1,
        "*** Update File: src/old.rs\n*** Move to: src/new.rs",
        "src/new.rs",
        RepositoryFileObservationKind::Renamed,
        Some("src/old.rs"),
    );
    prior_path
        .core_record
        .repository_bindings
        .push(binding_for("binding-2", "local:prior-path-repository"));
    prior_path
        .core_record
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-2".to_owned(),
            relative_path: "other/new.rs".to_owned(),
            kind: RepositoryFileObservationKind::Renamed,
            prior_relative_path: Some("src/old.rs".to_owned()),
        });
    assert!(one_file_preview("src/old.rs", &prior_path)
        .previews
        .is_empty());

    let absolute_body = "*** Update File: /worktrees/target/src/lib.rs";
    let mut distinct_roots = file_record(
        46,
        1,
        absolute_body,
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    authorize_local_root(&mut distinct_roots, "/worktrees/target");
    let mut competing = binding_for("binding-2", "forge:github.com/fork/ctx");
    authorize_binding(&mut competing, "/worktrees/fork", "2");
    distinct_roots
        .core_record
        .repository_bindings
        .push(competing);
    distinct_roots
        .core_record
        .repository_file_observations
        .push(RepositoryFileObservation {
            repository_binding_id: "binding-2".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            kind: RepositoryFileObservationKind::Modified,
            prior_relative_path: None,
        });
    assert_eq!(
        one_file_preview("src/lib.rs", &distinct_roots).previews[0].excerpt,
        absolute_body
    );

    let mut same_roots = distinct_roots.clone();
    authorize_binding(
        &mut same_roots.core_record.repository_bindings[1],
        "/worktrees/target",
        "2",
    );
    assert!(one_file_preview("src/lib.rs", &same_roots)
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
fn parser_body_and_line_ceilings_are_checked_before_projection() {
    let marker = "*** Update File: src/lib.rs";
    let bounded_body = format!(
        "{marker}\n{}",
        "x".repeat(MAX_EVIDENCE_PREVIEW_BODY_BYTES - marker.len() - 1)
    );
    assert_eq!(bounded_body.len(), MAX_EVIDENCE_PREVIEW_BODY_BYTES);
    let bounded = file_record(
        52,
        1,
        &bounded_body,
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_eq!(
        one_file_preview("src/lib.rs", &bounded).previews[0].excerpt,
        marker
    );

    let oversized_body = format!("{bounded_body}x");
    let oversized = file_record(
        53,
        1,
        &oversized_body,
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(one_file_preview("src/lib.rs", &oversized)
        .previews
        .is_empty());

    let bounded_lines = format!(
        "{marker}\n{}",
        "\n".repeat(MAX_EVIDENCE_PREVIEW_BODY_LINES - 1)
    );
    let bounded = file_record(
        54,
        1,
        &bounded_lines,
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_eq!(
        one_file_preview("src/lib.rs", &bounded).previews[0].excerpt,
        marker
    );

    let newline_dense = format!("{bounded_lines}\n");
    assert!(newline_dense.len() < MAX_EVIDENCE_PREVIEW_BODY_BYTES);
    let dense = file_record(
        55,
        1,
        &newline_dense,
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(one_file_preview("src/lib.rs", &dense).previews.is_empty());
}

#[test]
fn evidence_is_number_ordered_limited_and_deduplicated_by_exact_replay_content() {
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
        "*** Delete File: src/lib.rs\nsecond adjacent",
        "src/lib.rs",
        RepositoryFileObservationKind::Deleted,
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
    let replay = file_record(
        63,
        4,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert_ne!(shared.event_id, replay.event_id);
    let evidence = [
        numbered(&shared, 1),
        numbered(&replay, 2),
        numbered(&second, 3),
        numbered(&fourth, 4),
    ];
    let proofs = [
        verified(&evidence[2], &second),
        verified(&evidence[1], &replay),
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
        .all(|preview| !preview.evidence_numbers.contains(&4)));
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
    assert!(VerifiedEvidenceRecord::new(&evidence, "b", &record).is_none());

    for malformed_generation in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
        let mut malformed = evidence.clone();
        malformed.citation.core_generation_id = malformed_generation.clone();
        assert!(VerifiedEvidenceRecord::new(&malformed, &malformed_generation, &record).is_none());
    }

    let mut wrong_digest = evidence.clone();
    wrong_digest.citation.evidence_sha256 = Some("f".repeat(64));
    assert!(VerifiedEvidenceRecord::new(&wrong_digest, GENERATION, &record).is_none());

    let mut missing = evidence.clone();
    missing.citation.evidence_sha256 = None;
    assert!(VerifiedEvidenceRecord::new(&missing, GENERATION, &record).is_none());

    let mut ranged = evidence.clone();
    ranged.citation.byte_range = Some(ctx_pro_host_protocol::ByteRange {
        start: 0,
        end_exclusive: 1,
    });
    assert!(VerifiedEvidenceRecord::new(&ranged, GENERATION, &record).is_none());

    let mut mutated_content = record.clone();
    mutated_content
        .core_record
        .content
        .normalized_body
        .as_mut()
        .unwrap()
        .push_str("\nmutated");
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &mutated_content).is_none());

    let other = file_record(
        71,
        8,
        "*** Update File: src/lib.rs",
        "src/lib.rs",
        RepositoryFileObservationKind::Modified,
        None,
    );
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &other).is_none());

    for mutate in [0, 1, 2] {
        let mut mismatch = record.clone();
        match mutate {
            0 => mismatch.event.event_sequence += 1,
            1 => mismatch.core_record.event_sequence += 1,
            2 => mismatch.event.session_id = other.session_id,
            _ => unreachable!(),
        }
        assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &mismatch).is_none());
    }
}

#[test]
fn non_file_targets_non_codex_and_unverified_records_are_normal_omissions() {
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
    for target in [
        ResolvedBlameTarget::Commit {
            commit: resource("commit:1", ResourceKind::Commit, OID),
            repository: repository(),
        },
        ResolvedBlameTarget::PullRequest {
            selector: "1".to_owned(),
            pull_request: resource("pr:1", ResourceKind::PullRequest, "#1"),
            repository: repository(),
        },
    ] {
        let result = BlameResult {
            snapshot: protocol_snapshot(),
            target,
            git_snapshot: None,
            matches: Vec::new(),
            evidence: vec![evidence.clone()],
            next: None,
        };
        assert!(project_evidence_previews(&result, &[proof])
            .previews
            .is_empty());
    }
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
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &non_codex).is_none());
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
fn public_projector_limits_are_exact() {
    assert_eq!(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES, 512);
    assert_eq!(MAX_EVIDENCE_PREVIEW_BODY_BYTES, 64 * 1_024);
    assert_eq!(MAX_EVIDENCE_PREVIEW_BODY_LINES, 4_096);
}
