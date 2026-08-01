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
        ("blame", HostMessage::Blame(blame_request())),
    ]
}

fn helper_messages(fingerprint: &str) -> Vec<(&'static str, HelperMessage)> {
    let page = delta_page();
    let reconciliations = page
        .deltas
        .iter()
        .cloned()
        .map(|delta| CoreSourceReconciliation { delta })
        .collect();
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
                access: access(ProAccessState::Available),
                supported_operations: operations(),
                available_operations: operations(),
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
        ("blame", HelperMessage::Blame(blame_result())),
        (
            "error",
            HelperMessage::Error(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "golden protocol error",
            )),
        ),
    ]
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
    json!({"host_frames": host, "helper_frames": helper})
}
