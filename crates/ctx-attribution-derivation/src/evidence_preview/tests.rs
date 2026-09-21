use crate::protocol::{
    BlameAttribution, BlameCoverage, BlameCoverageUnit, BlameOutcome, BlameResult,
    EvidenceCitation, GitSnapshot, NumberedEvidence, ResolvedBlameTarget, ResourceKind,
    ResourceRef, WorktreeStatus,
};
use ctx_history_core::{
    ActivityInvocation, ActivityJsonCapture, CORE_ACTIVITY_REVISION, CoreActivity, CoreRecord,
    EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey, ProviderDeclaredFact,
    SessionIdentityInput, SourceAnchor, SourceKey, TypedKey, derive_event_id, derive_session_id,
};
use ctx_repository_evidence::{
    LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION, REPOSITORY_ASSOCIATION_POLICY_REVISION,
    RepositoryBinding, RepositoryEvaluation, RepositoryEvidence, RepositoryEvidenceConfidence,
    RepositoryEvidenceKind, RepositoryFileInvocationEvidence, RepositoryFileInvocationKind,
    RepositoryFileInvocationTextRange, RepositoryFileObservation, RepositoryFileObservationKind,
    RepositoryLocalRootAuthorization,
};
use sha2::{Digest, Sha256};

use super::{
    MAX_EVIDENCE_PREVIEW_CITATIONS, MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES, VerifiedEvidenceRecord,
    project_evidence_previews,
};

const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OID: &str = "0123456789abcdef0123456789abcdef01234567";
const REPOSITORY_ID: &str = "forge:github.com/ctxrs/ctx";
const REPOSITORY_RESOURCE_ID: &str = "repository:opaque-derived-graph-id";

#[derive(Debug, Clone)]
struct EvidenceFixture {
    record: CoreRecord,
    repository_evaluation: RepositoryEvaluation,
}

impl std::ops::Deref for EvidenceFixture {
    type Target = CoreRecord;

    fn deref(&self) -> &Self::Target {
        &self.record
    }
}

impl std::ops::DerefMut for EvidenceFixture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.record
    }
}

fn protocol_snapshot() -> crate::protocol::QuerySnapshotExpectation {
    crate::protocol::QuerySnapshotExpectation::Core {
        receipt: crate::protocol::CoreMaterializationReceiptIdentity {
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

fn empty_outcome(unit: BlameCoverageUnit) -> BlameOutcome {
    BlameOutcome {
        attribution: BlameAttribution::None,
        coverage: BlameCoverage {
            unit,
            evaluated: 0,
            proven: 0,
            possible: 0,
            conflicting: 0,
            none: 0,
        },
    }
}

fn source(provider: &str, seed: u8) -> SourceKey {
    source_contract(
        provider,
        &format!("{provider}_native_history"),
        &format!("{provider}-core-schema-v{seed}"),
        u32::from(seed) + 1,
        seed,
    )
}

fn source_contract(
    provider: &str,
    format: &str,
    schema: &str,
    provider_identity_version: u32,
    seed: u8,
) -> SourceKey {
    SourceKey::derive(
        provider,
        format,
        schema,
        provider_identity_version,
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
        association_policy_revision: REPOSITORY_ASSOCIATION_POLICY_REVISION,
    }
}

fn authorize_binding(binding: &mut RepositoryBinding, local_root: &str, seed: u8) {
    binding.checkout_id = Some(format!("checkout-{seed}"));
    binding.worktree_id = Some(format!("worktree-{seed}"));
    binding.local_root_authorization = Some(RepositoryLocalRootAuthorization {
        local_root: local_root.to_owned(),
        local_root_authorization_fingerprint_revision:
            LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
        local_root_authorization_fingerprint: [seed; 32],
        observed_at_unix_ms: i64::from(seed),
    });
}

fn authorize_local_root(record: &mut EvidenceFixture, local_root: &str) {
    authorize_binding(
        &mut record.repository_evaluation.repository_bindings[0],
        local_root,
        1,
    );
}

fn base_record(
    source: SourceKey,
    seed: u8,
    sequence: u64,
    body: &str,
    parser_revision: &str,
) -> EvidenceFixture {
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
    let mut core = ctx_history_core::CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "tool_call",
        parser_revision,
        body,
    )
    .unwrap();
    core.role = Some("assistant".to_owned());
    EvidenceFixture {
        record: core,
        repository_evaluation: RepositoryEvaluation::default(),
    }
}

fn invocation(
    ordinal: u32,
    binding_id: &str,
    path: &str,
    prior_path: Option<&str>,
    operation: RepositoryFileInvocationKind,
    tool_name: Option<&str>,
    range: Option<RepositoryFileInvocationTextRange>,
) -> RepositoryFileInvocationEvidence {
    RepositoryFileInvocationEvidence {
        operation_ordinal: ordinal,
        repository_binding_id: binding_id.to_owned(),
        relative_path: path.to_owned(),
        prior_relative_path: prior_path.map(str::to_owned),
        kind: operation,
        tool_name: tool_name.map(str::to_owned),
        normalized_text_range: range,
    }
}

fn observation(
    binding_id: &str,
    path: &str,
    prior_path: Option<&str>,
    kind: RepositoryFileObservationKind,
) -> RepositoryFileObservation {
    RepositoryFileObservation {
        repository_binding_id: binding_id.to_owned(),
        relative_path: path.to_owned(),
        kind,
        prior_relative_path: prior_path.map(str::to_owned),
    }
}

const fn observation_kind(kind: RepositoryFileInvocationKind) -> RepositoryFileObservationKind {
    match kind {
        RepositoryFileInvocationKind::Read => RepositoryFileObservationKind::Read,
        RepositoryFileInvocationKind::Create => RepositoryFileObservationKind::Created,
        RepositoryFileInvocationKind::Modify => RepositoryFileObservationKind::Modified,
        RepositoryFileInvocationKind::Delete => RepositoryFileObservationKind::Deleted,
        RepositoryFileInvocationKind::Rename => RepositoryFileObservationKind::Renamed,
        RepositoryFileInvocationKind::Write => RepositoryFileObservationKind::Unknown,
    }
}

fn exact_range(body: &str, excerpt: &str) -> RepositoryFileInvocationTextRange {
    let mut matches = body.match_indices(excerpt);
    let (start, _) = matches.next().expect("excerpt must occur");
    assert!(matches.next().is_none(), "excerpt must be unique");
    RepositoryFileInvocationTextRange {
        start: u32::try_from(start).unwrap(),
        end: u32::try_from(start + excerpt.len()).unwrap(),
    }
}

#[allow(clippy::too_many_arguments)]
fn file_record(
    provider: &str,
    seed: u8,
    sequence: u64,
    body: &str,
    excerpt: Option<&str>,
    path: &str,
    prior_path: Option<&str>,
    operation: RepositoryFileInvocationKind,
    tool_name: Option<&str>,
) -> EvidenceFixture {
    let mut record = base_record(
        source(provider, seed),
        seed,
        sequence,
        body,
        &format!("{provider}-projector-contract-v{seed}"),
    );
    record.repository_evaluation.repository_bindings = vec![binding()];
    record
        .repository_evaluation
        .repository_file_invocation_evidence = vec![invocation(
        0,
        "binding-1",
        path,
        prior_path,
        operation,
        tool_name,
        excerpt.map(|excerpt| exact_range(body, excerpt)),
    )];
    record.repository_evaluation.repository_file_observations = vec![observation(
        "binding-1",
        path,
        prior_path,
        observation_kind(operation),
    )];
    finalize_record(&mut record);
    record
}

fn finalize_record(record: &mut EvidenceFixture) {
    record
        .repository_evaluation
        .repository_file_invocation_evidence
        .sort();
    record.record.validate_contract().unwrap();
}

fn numbered(record: &CoreRecord, number: u32) -> NumberedEvidence {
    let encoded = record.encode_stored().unwrap();
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
        outcome: empty_outcome(BlameCoverageUnit::CommittedLine),
        matches: Vec::new(),
        evidence,
        next: None,
        lineage: None,
    }
}

fn verified<'a>(
    evidence: &'a NumberedEvidence,
    record: &'a EvidenceFixture,
) -> VerifiedEvidenceRecord<'a> {
    VerifiedEvidenceRecord::new_with_repository_evaluation(
        evidence,
        GENERATION,
        &record.record,
        record.repository_evaluation.clone(),
    )
    .unwrap()
}

fn one_file_preview(target: &str, record: &EvidenceFixture) -> super::EvidencePreviewModel {
    let evidence = numbered(record, 1);
    let proof = verified(&evidence, record);
    project_evidence_previews(&file_result(target, vec![evidence.clone()]), &[proof])
}

fn one_neutral_file_preview(target: &str, record: &EvidenceFixture) -> super::EvidencePreviewModel {
    let evidence = numbered(record, 1);
    let proof = VerifiedEvidenceRecord::new(&evidence, GENERATION, &record.record).unwrap();
    project_evidence_previews(&file_result(target, vec![evidence.clone()]), &[proof])
}

fn add_neutral_file_activity(record: &mut EvidenceFixture, path: &str, tool_name: &str) {
    record.record.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("call-1").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: tool_name.to_owned(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::json!({ "path": path }),
            },
            started_at_unix_ms: None,
        }),
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::File,
            value: path.to_owned(),
        }],
    });
    finalize_record(record);
}

#[test]
fn tier_one_sources_project_the_same_typed_contract_without_provider_allowlists() {
    let cases = [
        (
            "codex",
            "apply_patch: *** Update File: src/lib.rs",
            "*** Update File: src/lib.rs",
            "apply_patch",
        ),
        (
            "claude",
            r#"{"type":"tool_use","name":"Edit","input":{"file_path":"src/lib.rs"}}"#,
            r#""src/lib.rs""#,
            "Edit",
        ),
        (
            "cursor",
            r#"{"type":"tool_use","name":"write_file","input":{"path":"src/lib.rs"}}"#,
            r#"{"type":"tool_use","name":"write_file","input":{"path":"src/lib.rs"}}"#,
            "write_file",
        ),
        (
            "gemini",
            r#"{"name":"write_file","args":{"path":"src/lib.rs"}}"#,
            r#"{"name":"write_file","args":{"path":"src/lib.rs"}}"#,
            "write_file",
        ),
        (
            "opencode",
            r#"{"tool":"edit","state":{"input":{"path":"src/lib.rs"}}}"#,
            r#"{"tool":"edit","state":{"input":{"path":"src/lib.rs"}}}"#,
            "edit",
        ),
        (
            "openclaw",
            r#"{"type":"toolCall","name":"write_file","arguments":{"path":"src/lib.rs"}}"#,
            r#"{"type":"toolCall","name":"write_file","arguments":{"path":"src/lib.rs"}}"#,
            "write_file",
        ),
    ];

    for (index, (provider, body, excerpt, tool_name)) in cases.into_iter().enumerate() {
        let mut record = file_record(
            provider,
            u8::try_from(index + 1).unwrap(),
            1,
            body,
            Some(excerpt),
            "src/lib.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some(tool_name),
        );
        record
            .repository_evaluation
            .repository_file_observations
            .clear();
        finalize_record(&mut record);
        let model = one_file_preview("src/lib.rs", &record);
        assert_eq!(model.previews.len(), 1, "{provider}");
        let preview = &model.previews[0];
        assert_eq!(preview.operation, RepositoryFileInvocationKind::Modify);
        assert_eq!(preview.path, "src/lib.rs");
        assert_eq!(preview.prior_path, None);
        assert_eq!(preview.tool_name, tool_name);
        assert_eq!(preview.excerpt, excerpt);
        assert!(
            record
                .repository_evaluation
                .repository_file_observations
                .is_empty()
        );
    }
}

#[test]
fn every_typed_operation_preserves_request_metadata_without_claiming_effect_success() {
    let cases = [
        (RepositoryFileInvocationKind::Read, "read_file"),
        (RepositoryFileInvocationKind::Create, "create_file"),
        (RepositoryFileInvocationKind::Modify, "edit_file"),
        (RepositoryFileInvocationKind::Delete, "delete_file"),
        (RepositoryFileInvocationKind::Write, "write_file"),
    ];
    for (index, (operation, tool_name)) in cases.into_iter().enumerate() {
        let excerpt = format!("{tool_name} request for src/lib.rs");
        let record = file_record(
            "provider",
            u8::try_from(index + 10).unwrap(),
            1,
            &excerpt,
            Some(&excerpt),
            "src/lib.rs",
            None,
            operation,
            Some(tool_name),
        );
        let preview = &one_file_preview("src/lib.rs", &record).previews[0];
        assert_eq!(preview.operation, operation);
        assert_eq!(preview.tool_name, tool_name);
        assert_eq!(preview.excerpt, excerpt);
    }

    let body = "rename_file request: src/old.rs -> src/new.rs";
    let rename = file_record(
        "provider",
        20,
        1,
        body,
        Some(body),
        "src/new.rs",
        Some("src/old.rs"),
        RepositoryFileInvocationKind::Rename,
        Some("rename_file"),
    );
    for target in ["src/old.rs", "src/new.rs"] {
        let preview = &one_file_preview(target, &rename).previews[0];
        assert_eq!(preview.operation, RepositoryFileInvocationKind::Rename);
        assert_eq!(preview.path, "src/new.rs");
        assert_eq!(preview.prior_path.as_deref(), Some("src/old.rs"));
        assert_eq!(preview.tool_name, "rename_file");
        assert_eq!(preview.excerpt, body);
    }
}

#[test]
fn preview_copies_only_the_exact_verified_core_event_time() {
    let mut timed = file_record(
        "provider",
        21,
        1,
        "modify src/lib.rs",
        Some("modify src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    timed.record.occurred_at_unix_ms = Some(1_721_000_000_123);
    assert_eq!(
        one_file_preview("src/lib.rs", &timed).previews[0].event_occurred_at_ms,
        Some(1_721_000_000_123)
    );

    timed.record.occurred_at_unix_ms = None;
    assert_eq!(
        one_file_preview("src/lib.rs", &timed).previews[0].event_occurred_at_ms,
        None
    );
}

#[test]
fn typed_range_and_binding_are_authority_while_ordinary_observations_do_not_gate() {
    let valid = file_record(
        "provider",
        30,
        1,
        "exact request src/lib.rs",
        Some("exact request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    assert_eq!(one_file_preview("src/lib.rs", &valid).previews.len(), 1);

    let mut no_range = valid.clone();
    no_range
        .repository_evaluation
        .repository_file_invocation_evidence[0]
        .normalized_text_range = None;
    finalize_record(&mut no_range);
    assert!(
        one_file_preview("src/lib.rs", &no_range)
            .previews
            .is_empty()
    );

    let mut no_tool = valid.clone();
    no_tool
        .repository_evaluation
        .repository_file_invocation_evidence[0]
        .tool_name = None;
    finalize_record(&mut no_tool);
    assert!(one_file_preview("src/lib.rs", &no_tool).previews.is_empty());

    let mut whitespace_tool = valid.clone();
    whitespace_tool
        .repository_evaluation
        .repository_file_invocation_evidence[0]
        .tool_name = Some(" \t ".to_owned());
    finalize_record(&mut whitespace_tool);
    assert!(
        one_file_preview("src/lib.rs", &whitespace_tool)
            .previews
            .is_empty()
    );

    let mut no_observation = valid.clone();
    no_observation
        .repository_evaluation
        .repository_file_observations
        .clear();
    finalize_record(&mut no_observation);
    assert_eq!(
        one_file_preview("src/lib.rs", &no_observation)
            .previews
            .len(),
        1
    );

    let mut wrong_observation = valid.clone();
    wrong_observation
        .repository_evaluation
        .repository_file_observations[0]
        .kind = RepositoryFileObservationKind::Read;
    finalize_record(&mut wrong_observation);
    assert_eq!(
        one_file_preview("src/lib.rs", &wrong_observation)
            .previews
            .len(),
        1
    );

    let mut unknown_observation = valid;
    unknown_observation
        .repository_evaluation
        .repository_file_observations[0]
        .kind = RepositoryFileObservationKind::Unknown;
    finalize_record(&mut unknown_observation);
    assert_eq!(
        one_file_preview("src/lib.rs", &unknown_observation)
            .previews
            .len(),
        1
    );
}

#[test]
fn neutral_core_file_activity_and_legacy_text_fail_closed_without_exact_private_proof() {
    let mut record = base_record(
        source("codex", 31),
        31,
        1,
        "*** Update File: src/lib.rs",
        "codex-projector-contract-v31",
    );
    record.record.content.structured_content = Some(serde_json::json!({
        "tool_name": "provider_tool",
        "path": "src/lib.rs",
        "command_output": "must never be projected"
    }));
    add_neutral_file_activity(&mut record, "src/lib.rs", "apply_patch");

    assert!(record.repository_evaluation.repository_bindings.is_empty());
    assert!(
        one_neutral_file_preview("src/lib.rs", &record)
            .previews
            .is_empty()
    );
}

#[test]
fn excerpt_limit_is_exact_in_utf8_bytes_and_model_keeps_verbatim_bytes() {
    let exact = format!(
        "src/lib.rs:{}x",
        "é".repeat((512 - "src/lib.rs:x".len()) / 2)
    );
    assert_eq!(exact.len(), 512);
    let exact_record = file_record(
        "provider",
        40,
        1,
        &exact,
        Some(&exact),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Read,
        Some("read_file"),
    );
    assert_eq!(
        one_file_preview("src/lib.rs", &exact_record).previews[0].excerpt,
        exact
    );

    let oversized = format!("{exact}x");
    assert_eq!(oversized.len(), 513);
    let oversized_record = file_record(
        "provider",
        41,
        1,
        &oversized,
        Some(&oversized),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Read,
        Some("read_file"),
    );
    assert!(
        one_file_preview("src/lib.rs", &oversized_record)
            .previews
            .is_empty()
    );

    let unsafe_terminal_text = "\u{1b}[31mrequest src/lib.rs\u{202e}";
    let unsafe_record = file_record(
        "provider",
        42,
        1,
        unsafe_terminal_text,
        Some(unsafe_terminal_text),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Read,
        Some("read_file"),
    );
    assert_eq!(
        one_file_preview("src/lib.rs", &unsafe_record).previews[0].excerpt,
        unsafe_terminal_text
    );
}

#[test]
fn same_path_and_multi_unit_ambiguity_fail_closed() {
    let base = file_record(
        "provider",
        50,
        1,
        "first unit\nsecond unit",
        Some("first unit"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );

    let mut duplicate_target = base.clone();
    duplicate_target
        .repository_evaluation
        .repository_file_invocation_evidence
        .push(invocation(
            1,
            "binding-1",
            "src/lib.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some("edit_file"),
            Some(exact_range("first unit\nsecond unit", "second unit")),
        ));
    finalize_record(&mut duplicate_target);
    assert!(
        one_file_preview("src/lib.rs", &duplicate_target)
            .previews
            .is_empty()
    );

    let mut shared_range = base.clone();
    let selected_range = shared_range
        .repository_evaluation
        .repository_file_invocation_evidence[0]
        .normalized_text_range;
    shared_range
        .repository_evaluation
        .repository_file_invocation_evidence
        .push(invocation(
            1,
            "binding-1",
            "src/other.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some("edit_file"),
            selected_range,
        ));
    shared_range
        .repository_evaluation
        .repository_file_observations
        .push(observation(
            "binding-1",
            "src/other.rs",
            None,
            RepositoryFileObservationKind::Modified,
        ));
    finalize_record(&mut shared_range);
    assert!(
        one_file_preview("src/lib.rs", &shared_range)
            .previews
            .is_empty()
    );

    let mut duplicate_observation = base.clone();
    duplicate_observation
        .repository_evaluation
        .repository_file_observations
        .push(
            duplicate_observation
                .repository_evaluation
                .repository_file_observations[0]
                .clone(),
        );
    finalize_record(&mut duplicate_observation);
    assert_eq!(
        one_file_preview("src/lib.rs", &duplicate_observation)
            .previews
            .len(),
        1
    );

    let mut other_repository = base.clone();
    other_repository
        .repository_evaluation
        .repository_bindings
        .push(binding_for("binding-2", "forge:github.com/fork/ctx"));
    other_repository
        .repository_evaluation
        .repository_file_invocation_evidence
        .push(invocation(
            1,
            "binding-2",
            "src/lib.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some("edit_file"),
            None,
        ));
    other_repository
        .repository_evaluation
        .repository_file_observations
        .push(observation(
            "binding-2",
            "src/lib.rs",
            None,
            RepositoryFileObservationKind::Modified,
        ));
    finalize_record(&mut other_repository);
    assert!(
        one_file_preview("src/lib.rs", &other_repository)
            .previews
            .is_empty()
    );
}

#[test]
fn exact_repository_binding_is_required() {
    let valid = file_record(
        "provider",
        60,
        1,
        "request src/lib.rs",
        Some("request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    assert_ne!(repository().id, repository().display);
    assert_eq!(one_file_preview("src/lib.rs", &valid).previews.len(), 1);

    let mut ambiguous = valid.clone();
    ambiguous
        .repository_evaluation
        .repository_bindings
        .push(binding_for("binding-2", REPOSITORY_ID));
    finalize_record(&mut ambiguous);
    assert!(
        one_file_preview("src/lib.rs", &ambiguous)
            .previews
            .is_empty()
    );

    let mut wrong_binding = valid.clone();
    wrong_binding
        .repository_evaluation
        .repository_bindings
        .push(binding_for("binding-2", "forge:github.com/fork/ctx"));
    wrong_binding
        .repository_evaluation
        .repository_file_invocation_evidence[0]
        .repository_binding_id = "binding-2".to_owned();
    wrong_binding
        .repository_evaluation
        .repository_file_observations[0]
        .repository_binding_id = "binding-2".to_owned();
    finalize_record(&mut wrong_binding);
    assert!(
        one_file_preview("src/lib.rs", &wrong_binding)
            .previews
            .is_empty()
    );
}

#[test]
fn certified_local_root_resolves_exact_absolute_current_and_prior_targets() {
    let body = "rename request: src/old.rs -> src/new.rs";
    let mut record = file_record(
        "provider",
        61,
        1,
        body,
        Some(body),
        "src/new.rs",
        Some("src/old.rs"),
        RepositoryFileInvocationKind::Rename,
        Some("rename_file"),
    );

    assert!(
        one_file_preview("/worktrees/target/src/new.rs", &record)
            .previews
            .is_empty()
    );
    authorize_local_root(&mut record, "/worktrees/target");
    for target in [
        "src/new.rs",
        "/worktrees/target/src/new.rs",
        "/worktrees/target/src/old.rs",
    ] {
        let preview = &one_file_preview(target, &record).previews[0];
        assert_eq!(preview.path, "src/new.rs");
        assert_eq!(preview.prior_path.as_deref(), Some("src/old.rs"));
        assert_eq!(preview.excerpt, body);
    }

    for unsafe_or_inexact in [
        "/other/target/src/new.rs",
        "/worktrees/target/../target/src/new.rs",
        "/worktrees//target/src/new.rs",
        "/worktrees/target/src/new.rs/",
    ] {
        assert!(
            one_file_preview(unsafe_or_inexact, &record)
                .previews
                .is_empty(),
            "{unsafe_or_inexact}"
        );
    }

    authorize_local_root(&mut record, "/worktrees/target/../target");
    assert!(
        one_file_preview("/worktrees/target/src/new.rs", &record)
            .previews
            .is_empty()
    );
}

#[test]
fn certified_windows_drive_and_unc_roots_resolve_only_canonical_exact_targets() {
    let cases = [
        (
            r"C:\worktrees\target",
            r"C:\worktrees\target\src\new.rs",
            r"C:\worktrees\target\src\old.rs",
            r"D:\worktrees\target\src\new.rs",
        ),
        (
            "D:/worktrees/target",
            "D:/worktrees/target/src/new.rs",
            "D:/worktrees/target/src/old.rs",
            "D:/worktrees/other/src/new.rs",
        ),
        (
            r"\\server\share\target",
            r"\\server\share\target\src\new.rs",
            r"\\server\share\target\src\old.rs",
            r"\\server\other\target\src\new.rs",
        ),
    ];

    for (index, (root, current_target, prior_target, wrong_root)) in cases.into_iter().enumerate() {
        let body = "rename request: src/old.rs -> src/new.rs";
        let mut record = file_record(
            "provider",
            63 + u8::try_from(index).unwrap(),
            1,
            body,
            Some(body),
            "src/new.rs",
            Some("src/old.rs"),
            RepositoryFileInvocationKind::Rename,
            Some("rename_file"),
        );
        authorize_local_root(&mut record, root);

        assert_eq!(one_file_preview("src/new.rs", &record).previews.len(), 1);
        for target in [current_target, prior_target] {
            let preview = &one_file_preview(target, &record).previews[0];
            assert_eq!(preview.path, "src/new.rs");
            assert_eq!(preview.prior_path.as_deref(), Some("src/old.rs"));
        }
        assert!(one_file_preview(wrong_root, &record).previews.is_empty());
    }

    let mut drive = file_record(
        "provider",
        66,
        1,
        "modify request src/lib.rs",
        Some("modify request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    authorize_local_root(&mut drive, r"C:\worktrees\target");
    for unsafe_or_noncanonical in [
        r"C:\worktrees\target\src\..\src\lib.rs",
        r"C:\worktrees\target\src\\lib.rs",
        r"C:\worktrees\target/src/lib.rs",
        r"C:\worktrees\target\src\lib.rs\",
    ] {
        assert!(
            one_file_preview(unsafe_or_noncanonical, &drive)
                .previews
                .is_empty(),
            "{unsafe_or_noncanonical}"
        );
    }

    let mut unc = drive;
    authorize_local_root(&mut unc, r"\\server\share\target");
    for unsafe_or_noncanonical in [
        r"\\server\share\target\src\..\src\lib.rs",
        r"\\server\share\target\src\\lib.rs",
        r"\\server/share/target/src/lib.rs",
        r"\\server\share\target\src\lib.rs\",
    ] {
        assert!(
            one_file_preview(unsafe_or_noncanonical, &unc)
                .previews
                .is_empty(),
            "{unsafe_or_noncanonical}"
        );
    }
}

#[test]
fn windows_same_path_competitors_require_distinct_certified_roots() {
    let cases = [
        (
            r"C:\worktrees\target",
            r"C:\worktrees\target\src\lib.rs",
            r"D:\worktrees\fork",
            r"D:\worktrees\fork\src\lib.rs",
        ),
        (
            r"\\server\share\target",
            r"\\server\share\target\src\lib.rs",
            r"\\server\other-share\fork",
            r"\\server\other-share\fork\src\lib.rs",
        ),
    ];

    for (index, (selected_root, selected_target, distinct_root, wrong_root_target)) in
        cases.into_iter().enumerate()
    {
        let mut record = file_record(
            "provider",
            67 + u8::try_from(index).unwrap(),
            1,
            "modify request src/lib.rs",
            Some("modify request src/lib.rs"),
            "src/lib.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some("edit_file"),
        );
        authorize_local_root(&mut record, selected_root);
        let mut competing_binding = binding_for("binding-2", "forge:github.com/fork/ctx");
        authorize_binding(&mut competing_binding, distinct_root, 2);
        record
            .repository_evaluation
            .repository_bindings
            .push(competing_binding);
        record
            .repository_evaluation
            .repository_file_invocation_evidence
            .push(invocation(
                1,
                "binding-2",
                "src/lib.rs",
                None,
                RepositoryFileInvocationKind::Modify,
                Some("edit_file"),
                None,
            ));
        record
            .repository_evaluation
            .repository_file_observations
            .push(observation(
                "binding-2",
                "src/lib.rs",
                None,
                RepositoryFileObservationKind::Modified,
            ));
        finalize_record(&mut record);

        assert_eq!(one_file_preview(selected_target, &record).previews.len(), 1);
        assert!(
            one_file_preview(wrong_root_target, &record)
                .previews
                .is_empty()
        );

        authorize_binding(
            &mut record.repository_evaluation.repository_bindings[1],
            selected_root,
            2,
        );
        assert!(
            one_file_preview(selected_target, &record)
                .previews
                .is_empty()
        );
    }
}

#[test]
fn same_relative_path_across_repositories_requires_distinct_certified_absolute_roots() {
    let mut record = file_record(
        "provider",
        62,
        1,
        "modify request src/lib.rs",
        Some("modify request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    authorize_local_root(&mut record, "/worktrees/target");
    let mut competing_binding = binding_for("binding-2", "forge:github.com/fork/ctx");
    authorize_binding(&mut competing_binding, "/worktrees/fork", 2);
    record
        .repository_evaluation
        .repository_bindings
        .push(competing_binding);
    record
        .repository_evaluation
        .repository_file_invocation_evidence
        .push(invocation(
            1,
            "binding-2",
            "src/lib.rs",
            None,
            RepositoryFileInvocationKind::Modify,
            Some("edit_file"),
            None,
        ));
    record
        .repository_evaluation
        .repository_file_observations
        .push(observation(
            "binding-2",
            "src/lib.rs",
            None,
            RepositoryFileObservationKind::Modified,
        ));
    finalize_record(&mut record);

    assert!(one_file_preview("src/lib.rs", &record).previews.is_empty());
    assert_eq!(
        one_file_preview("/worktrees/target/src/lib.rs", &record)
            .previews
            .len(),
        1
    );

    let mut same_root = record.clone();
    authorize_binding(
        &mut same_root.repository_evaluation.repository_bindings[1],
        "/worktrees/target",
        2,
    );
    assert!(
        one_file_preview("/worktrees/target/src/lib.rs", &same_root)
            .previews
            .is_empty()
    );

    let mut uncertified_competitor = record.clone();
    uncertified_competitor
        .repository_evaluation
        .repository_bindings[1]
        .local_root_authorization = None;
    assert!(
        one_file_preview("/worktrees/target/src/lib.rs", &uncertified_competitor)
            .previews
            .is_empty()
    );

    let mut unsafe_competitor = record;
    authorize_binding(
        &mut unsafe_competitor.repository_evaluation.repository_bindings[1],
        "/worktrees/fork/../fork",
        2,
    );
    assert!(
        one_file_preview("/worktrees/target/src/lib.rs", &unsafe_competitor)
            .previews
            .is_empty()
    );
}

#[test]
fn digest_generation_source_and_full_event_coordinates_are_authenticated() {
    let record = file_record(
        "provider",
        70,
        7,
        "request src/lib.rs",
        Some("request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
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

    let mut missing_digest = evidence.clone();
    missing_digest.citation.evidence_sha256 = None;
    assert!(VerifiedEvidenceRecord::new(&missing_digest, GENERATION, &record).is_none());

    let mut ranged = evidence.clone();
    ranged.citation.byte_range = Some(crate::protocol::ByteRange {
        start: 0,
        end_exclusive: 1,
    });
    assert!(VerifiedEvidenceRecord::new(&ranged, GENERATION, &record).is_none());

    let mut mutated_content = record.clone();
    mutated_content
        .record
        .content
        .normalized_body
        .as_mut()
        .unwrap()
        .push_str(" mutated");
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &mutated_content).is_none());
}

#[test]
fn current_core_contract_accepts_new_descriptors_and_rejects_stale_revisions() {
    let custom_source = source_contract(
        "future-provider",
        "future_sqlite_snapshot",
        "future-tool-schema-v42",
        91,
        80,
    );
    let body = "future request src/lib.rs";
    let mut record = base_record(custom_source, 80, 1, body, "future-parser-v999");
    record.repository_evaluation.repository_bindings = vec![binding()];
    record
        .repository_evaluation
        .repository_file_invocation_evidence = vec![invocation(
        0,
        "binding-1",
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("future_edit"),
        Some(exact_range(body, body)),
    )];
    record.repository_evaluation.repository_file_observations = vec![observation(
        "binding-1",
        "src/lib.rs",
        None,
        RepositoryFileObservationKind::Modified,
    )];
    finalize_record(&mut record);
    let evidence = numbered(&record, 1);
    assert!(VerifiedEvidenceRecord::new(&evidence, GENERATION, &record).is_some());

    let mut stale = record.clone();
    stale.record.normalization_revision += 1;
    let stale_evidence = numbered(&stale, 1);
    assert!(VerifiedEvidenceRecord::new(&stale_evidence, GENERATION, &stale).is_none());
}

#[test]
fn evidence_is_number_ordered_limited_and_grouped_by_the_complete_exact_item() {
    let mut shared = file_record(
        "codex",
        90,
        1,
        "modify request src/lib.rs",
        Some("modify request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    let mut replay = file_record(
        "claude",
        91,
        2,
        "modify request src/lib.rs",
        Some("modify request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    shared.record.occurred_at_unix_ms = Some(1_721_000_000_000);
    replay.record.occurred_at_unix_ms = Some(1_721_000_000_001);
    let deleted = file_record(
        "gemini",
        92,
        3,
        "delete request src/lib.rs",
        Some("delete request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Delete,
        Some("delete_file"),
    );
    let fourth = file_record(
        "cursor",
        93,
        4,
        "fourth request src/lib.rs",
        Some("fourth request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Modify,
        Some("edit_file"),
    );
    let evidence = [
        numbered(&shared, 1),
        numbered(&replay, 2),
        numbered(&deleted, 3),
        numbered(&fourth, 4),
    ];
    let proofs = [
        verified(&evidence[2], &deleted),
        verified(&evidence[1], &replay),
        verified(&evidence[3], &fourth),
        verified(&evidence[0], &shared),
    ];
    let model = project_evidence_previews(&file_result("src/lib.rs", evidence.to_vec()), &proofs);
    assert_eq!(MAX_EVIDENCE_PREVIEW_CITATIONS, 3);
    assert_eq!(model.previews.len(), 2);
    assert_eq!(model.previews[0].citation_numbers, vec![1, 2]);
    assert_eq!(model.previews[0].event_occurred_at_ms, None);
    assert_eq!(model.previews[1].citation_numbers, vec![3]);
    assert!(
        model
            .previews
            .iter()
            .all(|preview| !preview.citation_numbers.contains(&4))
    );
}

#[test]
fn duplicate_verifiers_unverified_records_and_non_file_targets_omit() {
    let record = file_record(
        "provider",
        100,
        1,
        "request src/lib.rs",
        Some("request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Read,
        Some("read_file"),
    );
    let evidence = numbered(&record, 1);
    let proof = verified(&evidence, &record);
    assert!(
        project_evidence_previews(
            &file_result("src/lib.rs", vec![evidence.clone()]),
            &[proof.clone(), proof.clone()],
        )
        .previews
        .is_empty()
    );
    assert!(
        project_evidence_previews(&file_result("src/lib.rs", vec![evidence.clone()]), &[],)
            .previews
            .is_empty()
    );

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
        let unit = match &target {
            ResolvedBlameTarget::Commit { .. } => BlameCoverageUnit::CommitFact,
            ResolvedBlameTarget::PullRequest { .. } => BlameCoverageUnit::PullRequestRelationship,
            ResolvedBlameTarget::File { .. } => unreachable!(),
        };
        let result = BlameResult {
            snapshot: protocol_snapshot(),
            target,
            git_snapshot: None,
            outcome: empty_outcome(unit),
            matches: Vec::new(),
            evidence: vec![evidence.clone()],
            next: None,
            lineage: None,
        };
        assert!(
            project_evidence_previews(&result, std::slice::from_ref(&proof))
                .previews
                .is_empty()
        );
    }
}

#[test]
fn serializable_model_exposes_only_provider_neutral_invocation_fields() {
    let body = "rename request src/old.rs -> src/new.rs";
    let record = file_record(
        "provider",
        110,
        1,
        body,
        Some(body),
        "src/new.rs",
        Some("src/old.rs"),
        RepositoryFileInvocationKind::Rename,
        Some("rename_file"),
    );
    let value = serde_json::to_value(one_file_preview("src/new.rs", &record)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "previews": [{
                "citation_numbers": [1],
                "operation": "rename",
                "path": "src/new.rs",
                "prior_path": "src/old.rs",
                "tool_name": "rename_file",
                "excerpt": body,
            }]
        })
    );
    let encoded = serde_json::to_string(&value).unwrap();
    assert!(!encoded.contains("evidence_numbers"));
    assert!(!encoded.contains("file_kind"));
    assert!(!encoded.contains("structured_content"));

    let read = file_record(
        "provider",
        111,
        1,
        "read request src/lib.rs",
        Some("read request src/lib.rs"),
        "src/lib.rs",
        None,
        RepositoryFileInvocationKind::Read,
        Some("read_file"),
    );
    let read_value = serde_json::to_value(one_file_preview("src/lib.rs", &read)).unwrap();
    assert_eq!(
        read_value,
        serde_json::json!({
            "previews": [{
                "citation_numbers": [1],
                "operation": "read",
                "path": "src/lib.rs",
                "tool_name": "read_file",
                "excerpt": "read request src/lib.rs",
            }]
        })
    );
    assert!(
        !read_value["previews"][0]
            .as_object()
            .unwrap()
            .contains_key("prior_path")
    );
}

#[test]
fn public_projector_limits_are_exact() {
    assert_eq!(MAX_EVIDENCE_PREVIEW_EXCERPT_BYTES, 512);
    assert_eq!(MAX_EVIDENCE_PREVIEW_CITATIONS, 3);
}
