use super::blame::{
    commit_blame_result, file_blame_result, pull_request_activity_result,
    pull_request_membership_result,
};
use super::fixtures::{
    authorization, blame, blame_request, checkpoint, journal_operation_requests, journal_request,
    output_cursor, output_operation_pages, output_page, output_source,
    provider_output_blame_result,
};
use super::*;

fn host_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HostMessage::Hello(HelloRequest {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
                host_version: "golden-host".to_owned(),
                capabilities,
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
            "sync_journal",
            HostMessage::SyncJournal(journal_request(Vec::new(), fingerprint)),
        ),
        (
            "begin_output_inventory",
            HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation: 1 }),
        ),
        (
            "observe_output_source",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 1,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "materialize_output_page",
            HostMessage::MaterializeOutputPage(output_page()),
        ),
        (
            "finish_output_inventory",
            HostMessage::FinishOutputInventory(FinishOutputInventoryRequest { generation: 1 }),
        ),
        (
            "get_output_progress",
            HostMessage::GetOutputProgress(OutputProgressRequest {
                sources: vec![output_source()],
            }),
        ),
        ("blame", HostMessage::Blame(blame(None, fingerprint))),
    ]
}

fn helper_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        (
            "hello",
            HelperMessage::Hello(HelloResult {
                protocol_version: PROTOCOL_VERSION,
                protocol_fingerprint: fingerprint.to_owned(),
                helper_version: "golden-helper".to_owned(),
                capabilities,
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
                checkpoint: None,
            }),
        ),
        (
            "journal_synced",
            HelperMessage::JournalSynced(JournalSyncResult {
                committed_through: checkpoint(fingerprint),
                accepted_records: 0,
                replayed: false,
                frozen_complete: true,
            }),
        ),
        (
            "output_inventory_began",
            HelperMessage::OutputInventoryBegan(OutputInventoryBegan {
                generation: 1,
                materializer_revision: "fixture-materializer-1".to_owned(),
            }),
        ),
        (
            "output_source_observed",
            HelperMessage::OutputSourceObserved(OutputSourceObserved {
                generation: 1,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "output_page_materialized",
            HelperMessage::OutputPageMaterialized(OutputPageMaterialized {
                inventory_generation: 1,
                source: output_source(),
                source_epoch: 0,
                committed_cursor: output_cursor(),
                accepted_outputs: 1,
                materialized_facts: 1,
                materialized_evidence: 1,
                replayed: false,
            }),
        ),
        (
            "output_inventory_finished",
            HelperMessage::OutputInventoryFinished(OutputInventoryFinished {
                generation: 1,
                observed_sources: 1,
                unavailable_sources: 0,
            }),
        ),
        (
            "output_progress",
            HelperMessage::OutputProgress(OutputProgressResult {
                inventory_generation: 1,
                inventory_complete: true,
                sources: vec![OutputSourceProgress {
                    source: output_source(),
                    source_epoch: 0,
                    observed_revision: "revision-1".to_owned(),
                    cursor: Some(output_cursor()),
                    parser_revision: "parser-1".to_owned(),
                    materializer_revision: "materializer-1".to_owned(),
                    terminal: true,
                    availability: OutputSourceAvailability::Available,
                    last_seen_inventory: Some(1),
                }],
            }),
        ),
        (
            "blame",
            HelperMessage::Blame(provider_output_blame_result()),
        ),
        (
            "error",
            HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "exact Protocol V1 mismatch",
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
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "invalid".to_owned())
}

fn maximum_escaping_roots() -> Vec<String> {
    (0..MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| {
            let prefix = format!("/{index:03}/");
            format!(
                "{prefix}{}",
                "\\".repeat(2048_usize.saturating_sub(prefix.len()))
            )
        })
        .collect()
}

fn host_operation_messages(fingerprint: &str) -> Vec<(&'static str, HostMessage)> {
    let authorize = |access_kind| {
        let mut request = authorization();
        request.entitlement.grant.access_kind = access_kind;
        request.entitlement.grant.capabilities = BTreeSet::from([
            EntitlementCapability::GraphRead,
            EntitlementCapability::GraphWrite,
            EntitlementCapability::Export,
            EntitlementCapability::Migrate,
            EntitlementCapability::Update,
        ]);
        HostMessage::Authorize(request)
    };
    let [full_baseline, incremental] = journal_operation_requests(fingerprint);
    let [new_source, append_or_resume, rewrite] = output_operation_pages();
    vec![
        ("authorize_trial", authorize(EntitlementAccessKind::Trial)),
        ("authorize_active", authorize(EntitlementAccessKind::Active)),
        (
            "authorize_canceling_paid",
            authorize(EntitlementAccessKind::CancelingPaid),
        ),
        (
            "sync_journal_full_baseline_upsert",
            HostMessage::SyncJournal(full_baseline),
        ),
        (
            "sync_journal_incremental_delete",
            HostMessage::SyncJournal(incremental),
        ),
        (
            "observe_output_source_available",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Available,
            }),
        ),
        (
            "observe_output_source_unavailable",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Unavailable,
            }),
        ),
        (
            "observe_output_source_error",
            HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
                generation: 2,
                source: output_source(),
                availability: OutputSourceAvailability::Error,
            }),
        ),
        (
            "materialize_output_page_new_source_command_success",
            HostMessage::MaterializeOutputPage(new_source),
        ),
        (
            "materialize_output_page_append_or_resume_tool_failure",
            HostMessage::MaterializeOutputPage(append_or_resume),
        ),
        (
            "materialize_output_page_rewrite_command_timeout_and_tool_unknown",
            HostMessage::MaterializeOutputPage(rewrite),
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
        (
            "blame_commit",
            HostMessage::Blame(blame_request(
                BlameTarget::Commit {
                    oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                    repository: Some("ctxrs/ctx".to_owned()),
                },
                Some("commit-page-2".to_owned()),
                fingerprint,
            )),
        ),
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

fn helper_operation_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let authorized = |state| {
        HelperMessage::Authorized(AuthorizationResult {
            state,
            refresh_required: matches!(
                state,
                EntitlementAccessState::OfflineGrace | EntitlementAccessState::Locked
            ),
            expires_at_unix: 175,
            access_deadline_unix: 200,
            grace_deadline_unix: 250,
            capabilities: BTreeSet::from([
                EntitlementCapability::GraphRead,
                EntitlementCapability::GraphWrite,
                EntitlementCapability::Export,
                EntitlementCapability::Migrate,
                EntitlementCapability::Update,
            ]),
        })
    };
    let status = |state| {
        HelperMessage::Status(StatusResult {
            state,
            checkpoint: matches!(state, GraphState::Ready).then(|| checkpoint(fingerprint)),
        })
    };
    let source_observed = |availability| {
        HelperMessage::OutputSourceObserved(OutputSourceObserved {
            generation: 2,
            source: output_source(),
            availability,
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
            "output_source_observed_available",
            source_observed(OutputSourceAvailability::Available),
        ),
        (
            "output_source_observed_unavailable",
            source_observed(OutputSourceAvailability::Unavailable),
        ),
        (
            "output_source_observed_error",
            source_observed(OutputSourceAvailability::Error),
        ),
        (
            "blame_file",
            HelperMessage::Blame(file_blame_result(
                None,
                LineRange { start: 1, end: 100 },
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
                Some(BlameContinuation {
                    cursor: "file-line-next".to_owned(),
                    reason: ContinuationReason::MoreCommittedLines,
                }),
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
    let helper = helper_operation_messages(fingerprint)
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
    let max_roots = HostEnvelope {
        sequence: u64::MAX,
        request_id,
        message: HostMessage::SyncJournal(journal_request(maximum_escaping_roots(), fingerprint)),
    };
    let max_roots_bytes = serde_json::to_vec(&max_roots)
        .unwrap_or_else(|error| panic!("max roots envelope: {error}"));
    json!({
        "host_frames": host,
        "helper_frames": helper,
        "operation_frames": operation_frames(fingerprint),
        "error_frames": errors,
        "cursor_frames": {"blame_cursor_max": max_cursor},
        "boundary_frames": {
            "maximum_escaping_roots": {
                "payload_bytes": max_roots_bytes.len(),
                "sha256": hex(&Sha256::digest(&max_roots_bytes)),
                "root_count": MAX_AUTHORIZED_REPOSITORY_ROOTS,
                "root_total_unescaped_bytes": MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES
            }
        }
    })
}
