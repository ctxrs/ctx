use super::blame::{
    commit_blame_result, file_blame_result, pull_request_activity_result,
    pull_request_membership_result,
};
use super::fixtures::{
    authorization, blame, blame_request, source_manifest_admission_receipt, source_manifest_header,
    source_manifest_page, source_progress, source_receipt, source_record, source_removal,
};
use super::*;

fn capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::SourceMaterialization,
        Capability::Query,
        Capability::GitRead,
    ])
}

fn host_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    vec![
        (
            "hello",
            HostMessage::Hello(HelloRequest {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
                host_version: "golden-host".to_owned(),
                capabilities: capabilities(),
            }),
        ),
        ("authorize", HostMessage::Authorize(authorization())),
        (
            "prepare_graph_key_deletion",
            HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
                installation_key_thumbprint: base64url(&[1; 32]),
            }),
        ),
        (
            "confirm_graph_key_deletion",
            HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest {
                authorization: authorization(),
            }),
        ),
        ("status", HostMessage::Status(StatusRequest {})),
        (
            "begin_source_manifest_admission",
            HostMessage::BeginSourceManifestAdmission(BeginSourceManifestAdmissionRequest {
                header: source_manifest_header(),
            }),
        ),
        (
            "admit_source_manifest_page",
            HostMessage::AdmitSourceManifestPage(AdmitSourceManifestPageRequest {
                page: source_manifest_page(),
            }),
        ),
        (
            "finish_source_manifest_admission",
            HostMessage::FinishSourceManifestAdmission(FinishSourceManifestAdmissionRequest {
                header: source_manifest_header(),
            }),
        ),
        (
            "prepare_source",
            HostMessage::PrepareSource(PrepareSourceRequest {
                core_generation_id: "a".repeat(64),
                source: source_progress(false).source,
                certified_revision_sha256: source_progress(false).certified_revision_sha256,
                materializer_revision: "golden-source-materializer-v1".to_owned(),
                disposition: SourceDisposition::NewSource,
                expected_prior: None,
            }),
        ),
        (
            "materialize_source_page",
            HostMessage::MaterializeSourcePage(MaterializeSourcePageRequest {
                core_generation_id: "a".repeat(64),
                expected_prior: source_progress(false),
                next_frontier: source_progress(true).frontier,
                terminal: true,
                records: vec![source_record()],
            }),
        ),
        (
            "delete_source",
            HostMessage::DeleteSource(DeleteSourceRequest {
                core_generation_id: "a".repeat(64),
                removal: source_removal(),
                expected_prior: source_progress(true),
            }),
        ),
        (
            "finish_admitted_source_manifest",
            HostMessage::FinishAdmittedSourceManifest(FinishAdmittedSourceManifestRequest {
                admission: source_manifest_admission_receipt(),
                expected_progress: vec![source_progress(true)],
            }),
        ),
        ("blame", HostMessage::Blame(blame(None, fingerprint))),
    ]
}

fn helper_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    vec![
        (
            "hello",
            HelperMessage::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
                helper_version: "golden-helper".to_owned(),
                capabilities: capabilities(),
                authorization_challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
            }),
        ),
        (
            "authorized",
            HelperMessage::Authorized(AuthorizationResult {
                state: EntitlementAccessState::Trial,
                refresh_required: false,
                expires_at_unix: 175,
                access_deadline_unix: 200,
                grace_deadline_unix: 250,
                capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
            }),
        ),
        (
            "graph_key_deletion_prepared",
            HelperMessage::GraphKeyDeletionPrepared(GraphKeyDeletionPrepared {
                challenge_base64url: base64url(&[6; GRAPH_KEY_DELETION_CHALLENGE_BYTES]),
                expires_at_unix: 200,
                key_present: true,
            }),
        ),
        (
            "graph_key_deleted",
            HelperMessage::GraphKeyDeleted(GraphKeyDeleted { deleted: true }),
        ),
        (
            "status",
            HelperMessage::Status(StatusResult {
                state: GraphState::NotMaterialized,
                authority: MaterializationAuthority::Source,
                source_receipt: None,
            }),
        ),
        (
            "source_manifest_admission_began",
            HelperMessage::SourceManifestAdmissionBegan(SourceManifestAdmissionBegan {
                cursor: SourceManifestAdmissionCursor::initial(&source_manifest_header()),
                replayed: false,
            }),
        ),
        (
            "source_manifest_page_admitted",
            HelperMessage::SourceManifestPageAdmitted(SourceManifestPageAdmitted {
                cursor: SourceManifestAdmissionCursor {
                    core_generation_id: "a".repeat(64),
                    aggregate_sha256: source_manifest_header().aggregate_sha256,
                    next_page_index: 1,
                    next_source_index: 1,
                    next_removal_index: 0,
                },
                replayed: false,
            }),
        ),
        (
            "source_manifest_admitted",
            HelperMessage::SourceManifestAdmitted(SourceManifestAdmitted {
                receipt: source_manifest_admission_receipt(),
                materializer_revision: "golden-source-materializer-v1".to_owned(),
                progress: Vec::new(),
                replayed: false,
            }),
        ),
        (
            "source_prepared",
            HelperMessage::SourcePrepared(SourcePrepared {
                core_generation_id: "a".repeat(64),
                progress: source_progress(false),
                replayed: false,
            }),
        ),
        (
            "source_page_materialized",
            HelperMessage::SourcePageMaterialized(SourcePageMaterialized {
                core_generation_id: "a".repeat(64),
                progress: source_progress(true),
                accepted_records: 1,
                materialized_facts: 3,
                replayed: false,
            }),
        ),
        (
            "source_deleted",
            HelperMessage::SourceDeleted(SourceDeleted {
                core_generation_id: "a".repeat(64),
                source: source_progress(true).source,
                removed_source_epoch: 1,
                replayed: false,
            }),
        ),
        (
            "source_manifest_finished",
            HelperMessage::SourceManifestFinished(SourceManifestFinished {
                receipt: source_receipt(),
                replayed: false,
            }),
        ),
        ("blame", HelperMessage::Blame(commit_blame_result())),
        (
            "error",
            HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "golden protocol error",
            )),
        ),
    ]
}

fn error_classes() -> Vec<ErrorClass> {
    vec![
        ErrorClass::EntitlementExpired,
        ErrorClass::KeyStoreUnavailable,
        ErrorClass::KeyStoreLocked,
        ErrorClass::NotMaterialized,
        ErrorClass::ProtocolMismatch,
        ErrorClass::MissingSource,
        ErrorClass::MissingRepository,
        ErrorClass::StaleFact,
        ErrorClass::LineOutOfRange,
        ErrorClass::StaleSnapshot,
        ErrorClass::Ambiguous,
        ErrorClass::Corrupt,
        ErrorClass::InvalidRequest,
        ErrorClass::Bounds,
        ErrorClass::Sequence,
        ErrorClass::Internal,
    ]
}

fn error_name(error: ErrorClass) -> String {
    serde_json::to_value(error)
        .expect("error class")
        .as_str()
        .expect("error wire name")
        .to_owned()
}

fn host_operation_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    let authorize = |access_kind: EntitlementAccessKind, name: &str| {
        let mut request = authorization();
        request.entitlement.grant.access_kind = access_kind;
        request.entitlement.grant.grant_id = name.to_owned();
        HostMessage::Authorize(request)
    };
    vec![
        (
            "authorize_trial",
            authorize(EntitlementAccessKind::Trial, "trial"),
        ),
        (
            "authorize_active",
            authorize(EntitlementAccessKind::Active, "active"),
        ),
        (
            "authorize_canceling_paid",
            authorize(EntitlementAccessKind::CancelingPaid, "canceling"),
        ),
        (
            "blame_file",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: None,
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_file_line",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: Some(LineRange { start: 42, end: 42 }),
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_file_range",
            HostMessage::Blame(blame_request(
                BlameTarget::File {
                    path: "src/lib.rs".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                    lines: Some(LineRange { start: 42, end: 60 }),
                },
                None,
                fingerprint,
            )),
        ),
        ("blame_commit", HostMessage::Blame(blame(None, fingerprint))),
        (
            "blame_pull_request_number",
            HostMessage::Blame(blame_request(
                BlameTarget::PullRequest {
                    selector: "42".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                },
                None,
                fingerprint,
            )),
        ),
        (
            "blame_pull_request_url",
            HostMessage::Blame(blame_request(
                BlameTarget::PullRequest {
                    selector: "https://github.com/ctxrs/ctx/pull/42".to_owned(),
                    repository: None,
                },
                None,
                fingerprint,
            )),
        ),
    ]
}

fn helper_operation_messages() -> Vec<(&'static str, HelperMessage)> {
    let authorized = |state| {
        HelperMessage::Authorized(AuthorizationResult {
            state,
            refresh_required: false,
            expires_at_unix: 175,
            access_deadline_unix: 200,
            grace_deadline_unix: 250,
            capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
        })
    };
    let status = |state| {
        HelperMessage::Status(StatusResult {
            state,
            authority: MaterializationAuthority::Source,
            source_receipt: (state == GraphState::Ready).then(source_receipt),
        })
    };
    vec![
        (
            "authorized_trial",
            authorized(EntitlementAccessState::Trial),
        ),
        (
            "authorized_active",
            authorized(EntitlementAccessState::Active),
        ),
        (
            "authorized_canceling_paid",
            authorized(EntitlementAccessState::CancelingPaid),
        ),
        (
            "authorized_offline_grace",
            authorized(EntitlementAccessState::OfflineGrace),
        ),
        (
            "authorized_locked",
            authorized(EntitlementAccessState::Locked),
        ),
        (
            "status_not_materialized",
            status(GraphState::NotMaterialized),
        ),
        ("status_needs_rebuild", status(GraphState::NeedsRebuild)),
        ("status_partial", status(GraphState::Partial)),
        ("status_needs_resume", status(GraphState::NeedsResume)),
        ("status_ready", status(GraphState::Ready)),
        (
            "blame_file",
            HelperMessage::Blame(file_blame_result(
                None,
                LineRange { start: 1, end: 20 },
                WorktreeStatus::Clean,
                ProductionRelationship::ProducedBy,
                None,
            )),
        ),
        (
            "blame_file_line",
            HelperMessage::Blame(file_blame_result(
                Some(LineRange { start: 42, end: 42 }),
                LineRange { start: 42, end: 42 },
                WorktreeStatus::Differs,
                ProductionRelationship::PossiblyProducedBy,
                None,
            )),
        ),
        (
            "blame_file_range",
            HelperMessage::Blame(file_blame_result(
                Some(LineRange { start: 42, end: 60 }),
                LineRange { start: 42, end: 60 },
                WorktreeStatus::Clean,
                ProductionRelationship::ProducedBy,
                None,
            )),
        ),
        ("blame_commit", HelperMessage::Blame(commit_blame_result())),
        (
            "blame_pull_request_activity_without_commit_membership",
            HelperMessage::Blame(pull_request_activity_result()),
        ),
        (
            "blame_pull_request_commit_membership",
            HelperMessage::Blame(pull_request_membership_result()),
        ),
    ]
}

fn operation_frames(fingerprint: &str) -> Value {
    let request_id = Uuid::from_u128(2);
    let host = host_operation_messages(fingerprint)
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HostEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let helper = helper_operation_messages()
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HelperEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    json!({
        "host_request_frames": host,
        "helper_response_frames": helper
    })
}

pub(super) fn golden_vectors(fingerprint: &str) -> Value {
    let request_id = Uuid::from_u128(1);
    let host = host_messages(fingerprint)
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HostEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let helper = helper_messages(fingerprint)
        .into_iter()
        .enumerate()
        .map(|(sequence, (name, message))| {
            (
                name,
                frame_hex(&HelperEnvelope {
                    sequence: sequence as u64,
                    request_id,
                    message,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let errors = error_classes()
        .into_iter()
        .map(|class| {
            let name = error_name(class);
            let frame = frame_hex(&HelperEnvelope {
                sequence: 0,
                request_id,
                message: HelperMessage::Error(ProtocolError::new(class, "golden error")),
            });
            (name, frame)
        })
        .collect::<BTreeMap<_, _>>();
    let max_cursor = frame_hex(&HostEnvelope {
        sequence: u64::MAX,
        request_id,
        message: HostMessage::Blame(blame(Some("c".repeat(MAX_BLAME_CURSOR_BYTES)), fingerprint)),
    });
    json!({
        "host_frames": host,
        "helper_frames": helper,
        "operation_frames": operation_frames(fingerprint),
        "error_frames": errors,
        "cursor_frames": {"blame_cursor_max": max_cursor}
    })
}
