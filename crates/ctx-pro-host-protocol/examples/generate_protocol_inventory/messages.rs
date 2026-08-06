use super::fixtures::*;
use super::*;

fn capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::CoreMaterialization,
        Capability::Query,
        Capability::GitRead,
    ])
}

fn access(state: ProAccessState) -> ProAccessStatus {
    ProAccessStatus {
        entitlement: state,
        graph_key: state,
        local_repository: state,
    }
}

fn operations() -> BTreeSet<ProOperation> {
    BTreeSet::from([
        ProOperation::FileBlame,
        ProOperation::CommitBlame,
        ProOperation::PullRequestBlame,
    ])
}

fn repository_coverage(event_count: u64) -> RepositoryCoverage {
    RepositoryCoverage {
        repository_candidate_events: event_count,
        logical_binding_events: event_count,
        certified_live_root_access_events: event_count,
        file_evidence_events: event_count,
        exact_commit_evidence_events: event_count,
        exact_pull_request_evidence_events: event_count,
    }
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
        (
            "status",
            HostMessage::Status(StatusRequest {
                requested_core_generation_id: Some("a".repeat(64)),
            }),
        ),
        (
            "begin_core_materialization",
            HostMessage::BeginCoreMaterialization(begin_request()),
        ),
        (
            "apply_core_source_delta_page",
            HostMessage::ApplyCoreSourceDeltaPage(ApplyCoreSourceDeltaPageRequest {
                page: delta_page(),
                acknowledgement_page_index: 0,
            }),
        ),
        (
            "core_event_state_page",
            HostMessage::CoreEventStatePage(event_state_request()),
        ),
        (
            "apply_core_event_delta_page",
            HostMessage::ApplyCoreEventDeltaPage(ApplyCoreEventDeltaPageRequest {
                page: event_delta_page(),
            }),
        ),
        (
            "finish_core_materialization",
            HostMessage::FinishCoreMaterialization(finish_request()),
        ),
        (
            "continue_core_materialization",
            HostMessage::ContinueCoreMaterialization(ContinueCoreMaterializationRequest {
                expected_progress: finalization_progress(
                    CoreMaterializationFinalizationPhase::EmitReplay,
                    'd',
                ),
            }),
        ),
        ("blame", HostMessage::Blame(blame_request())),
        (
            "apply_core_event_delta_pages",
            HostMessage::ApplyCoreEventDeltaPages(event_delta_pages_request()),
        ),
    ]
}

fn helper_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let page = delta_page();
    let mut reconciliations = page
        .deltas
        .iter()
        .cloned()
        .enumerate()
        .map(|(materialize_index, delta)| CoreSourceReconciliation {
            materialize_index: u32::try_from(materialize_index).unwrap_or(u32::MAX),
            delta,
        })
        .collect::<Vec<_>>();
    reconciliations.push(CoreSourceReconciliation {
        materialize_index: 1,
        delta: CoreSourceDelta::Removed(source_removal()),
    });
    let state_page = event_state_page();
    let delta_page = event_delta_page();
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
                currentness: CoreProjectionCurrentness::Current,
                requested_core_generation_id: Some("a".repeat(64)),
                core_receipt: Some(receipt()),
                coverage: MaterializedCoverage::Complete,
                repository_coverage: repository_coverage(1),
                core_preparation_peak_workers: 4,
                access: access(ProAccessState::Available),
                supported_operations: operations(),
                available_operations: operations(),
                finalization_progress: None,
                storage_evidence: Some(ProStorageEvidence {
                    graph_manifest_schema: 3,
                    flat_format_version: 2,
                    materializer_checkpoint_version: 5,
                    journal_pack_format_version: 3,
                    legacy_journals_written: 0,
                    journal_pages_written: 2,
                    journal_packs_written: 1,
                    journal_finish_activity: JournalFinishActivity {
                        worker_limit: 1,
                        peak_workers: 1,
                        started_after_preparation: true,
                    },
                }),
            }),
        ),
        (
            "core_materialization_began",
            HelperMessage::CoreMaterializationBegan(CoreMaterializationBegan {
                materialization_id: materialization_id(),
                core_generation_id: "a".repeat(64),
                materializer_revision: "golden-core-materializer-v1".to_owned(),
                expected_prior_receipt: None,
                replayed: false,
            }),
        ),
        (
            "core_source_delta_page_applied",
            HelperMessage::CoreSourceDeltaPageApplied(CoreSourceDeltaPageApplied {
                materialization_id: page.materialization_id,
                core_generation_id: page.core_generation_id,
                page_index: page.page_index,
                acknowledgement_page_index: 0,
                acknowledgement_terminal: true,
                changed_sources: 1,
                removed_sources: 1,
                reconcile_sources: reconciliations,
                replayed: false,
            }),
        ),
        (
            "core_event_state_page",
            HelperMessage::CoreEventStatePage(state_page),
        ),
        (
            "core_event_delta_page_applied",
            HelperMessage::CoreEventDeltaPageApplied(CoreEventDeltaPageApplied {
                materialization_id: delta_page.materialization_id,
                core_generation_id: delta_page.core_generation_id,
                source: delta_page.reconciliation.delta.source().clone(),
                page_index: delta_page.page_index,
                additions: 1,
                replacements: 0,
                tombstones: 0,
                terminal: true,
                replayed: false,
            }),
        ),
        (
            "core_materialization_finished",
            HelperMessage::CoreMaterializationFinished(CoreMaterializationFinished {
                receipt: receipt(),
                replayed: false,
            }),
        ),
        (
            "core_materialization_finalization_pending",
            HelperMessage::CoreMaterializationFinalizationPending(
                CoreMaterializationFinalizationPending {
                    progress: finalization_progress(
                        CoreMaterializationFinalizationPhase::EmitFlat,
                        'e',
                    ),
                    replayed: false,
                },
            ),
        ),
        ("blame", HelperMessage::Blame(Box::new(blame_result()))),
        (
            "error",
            HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "golden protocol error",
            )),
        ),
        {
            let batch_page = event_delta_page();
            (
                "core_event_delta_pages_applied",
                HelperMessage::CoreEventDeltaPagesApplied(CoreEventDeltaPagesApplied {
                    pages: vec![CoreEventDeltaPageApplied {
                        materialization_id: batch_page.materialization_id,
                        core_generation_id: batch_page.core_generation_id,
                        source: batch_page.reconciliation.delta.source().clone(),
                        page_index: batch_page.page_index,
                        additions: 1,
                        replacements: 0,
                        tombstones: 0,
                        terminal: true,
                        replayed: false,
                    }],
                }),
            )
        },
    ]
}

fn finalization_progress(
    phase: CoreMaterializationFinalizationPhase,
    cursor: char,
) -> CoreMaterializationFinalizationProgress {
    CoreMaterializationFinalizationProgress {
        materialization_id: materialization_id(),
        core_generation_id: "a".repeat(64),
        phase,
        cursor_sha256: cursor.to_string().repeat(64),
    }
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
    let core_record_digests = core_record_digests(&record())
        .unwrap_or_else(|error| panic!("encode golden Core record digests: {error:?}"));
    json!({
        "core_record_digests": {
            "core_record_sha256": core_record_digests.core_record_sha256,
            "core_record_leaf_sha256": core_record_digests.core_record_leaf_sha256
        },
        "host_frames": host,
        "helper_frames": helper
    })
}
