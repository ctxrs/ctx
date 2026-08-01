use super::*;
use ctx_history_core::{
    core_record_contract_fingerprint, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION, CORE_REPOSITORY_CONTRACT_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
};

fn wire_names<T: Copy>(values: &[T], name: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    values.iter().copied().map(name).collect()
}

pub(super) fn inventory() -> Value {
    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::CoreMaterialization,
        Capability::Query,
        Capability::GitRead,
    ];
    let request_id = Uuid::from_u128(1);
    let status = HostEnvelope {
        sequence: 0,
        request_id,
        message: HostMessage::Status(StatusRequest {
            requested_core_generation_id: Some("a".repeat(64)),
        }),
    };
    let error = HelperEnvelope {
        sequence: 0,
        request_id,
        message: HelperMessage::Error(ProtocolError::new(
            ErrorClass::ProtocolMismatch,
            "exact Protocol V1 mismatch",
        )),
    };
    json!({
        "inventory_schema": 1,
        "protocol_version": PROTOCOL_VERSION,
        "fingerprint": {
            "algorithm": "sha256",
            "encoding": "lowercase_hex",
            "scope": "exact_compact_utf8_bytes_of_this_inventory_without_a_fingerprint_value",
            "runtime_value": FINGERPRINT_PLACEHOLDER
        },
        "framing": {
            "magic_ascii": std::str::from_utf8(FRAME_MAGIC).unwrap_or("CTXPRO"),
            "header_bytes": FRAME_HEADER_BYTES,
            "version_bytes": 2,
            "length_bytes": 4,
            "byte_order": "big_endian",
            "payload": "strict_json",
            "maximum_payload_bytes": MAX_FRAME_PAYLOAD_BYTES
        },
        "bounds": {
            "core_source_states": MAX_CORE_SOURCE_STATES,
            "core_source_delta_page_items": MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
            "core_event_state_page_items": MAX_CORE_EVENT_STATE_PAGE_ITEMS,
            "core_event_delta_page_items": MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
            "core_event_delta_page_content_bytes": MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
            "core_source_delta_page_wire_bytes": MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
            "core_event_state_page_wire_bytes": MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
            "core_event_delta_page_wire_bytes": MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
            "core_control_wire_bytes": MAX_CORE_CONTROL_WIRE_BYTES,
            "core_materializer_revision_bytes": MAX_CORE_MATERIALIZER_REVISION_BYTES,
            "blame_results": MAX_BLAME_RESULTS,
            "blame_cursor_bytes": MAX_BLAME_CURSOR_BYTES,
            "blame_evidence": MAX_BLAME_EVIDENCE,
            "blame_attributions_per_match": MAX_BLAME_ATTRIBUTIONS_PER_MATCH,
            "citations_per_fact": MAX_CITATIONS_PER_FACT,
            "blame_target_bytes": MAX_BLAME_TARGET_BYTES
        },
        "host_message_kinds": [
            "hello", "authorize", "prepare_graph_key_deletion",
            "confirm_graph_key_deletion", "status",
            "begin_core_materialization", "apply_core_source_delta_page",
            "core_event_state_page", "apply_core_event_delta_page",
            "finish_core_materialization", "blame"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "core_materialization_began", "core_source_delta_page_applied",
            "core_event_state_page", "core_event_delta_page_applied",
            "core_materialization_finished", "blame", "error"
        ],
        "capabilities": wire_names(&capabilities, Capability::wire_name),
        "enums": {
            "core_projection_currentness": [
                "not_materialized", "partial", "stale", "needs_rebuild", "current"
            ],
            "materialized_coverage": [
                "not_materialized", "partial", "complete", "empty", "abstained"
            ],
            "pro_access_state": ["available", "locked", "unavailable"],
            "pro_operation": ["file_blame", "commit_blame", "pull_request_blame"],
            "core_source_delta_kind": ["present", "removed"],
            "core_event_delta_kind": ["added", "replaced", "tombstoned"],
            "repository_candidate_kind": [
                "session_cwd", "declared_tool_workdir", "derived_effective_cwd",
                "command_specific_repository_path", "file_activity_path", "vcs_activity_path",
                "outcome_operation_repository_path", "outcome_output_repository_path"
            ],
            "query_snapshot_expectation_kind": ["core"],
            "resource_kind": ResourceKind::ALL.map(ResourceKind::wire_name)
        },
        "dto_fields": {
            "StatusRequest": fields(&["requested_core_generation_id"], &[]),
            "StatusResult": fields(&[
                "currentness", "requested_core_generation_id", "core_receipt", "coverage",
                "repository_coverage", "access", "supported_operations",
                "available_operations"
            ], &[]),
            "RepositoryCoverage": fields(&[
                "repository_candidate_events", "logical_binding_events",
                "certified_live_root_access_events", "file_evidence_events",
                "exact_commit_evidence_events", "exact_pull_request_evidence_events"
            ], &[]),
            "ProAccessStatus": fields(&["entitlement", "graph_key", "local_repository"], &[]),
            "CoreSourceState": fields(
                &["source", "core_record_accumulator", "event_count"], &[]),
            "CoreSourceRemoval": fields(&["source", "removal_revision_sha256"], &[]),
            "CoreGenerationHead": fields(&[
                "contract_version", "core_generation_id", "generation_manifest_version",
                "identity_version", "core_record_version", "core_record_contract_fingerprint",
                "normalization_revision", "content_policy_revision",
                "repository_contract_revision", "lexical_schema_version",
                "lexical_analyzer_version", "policy_schema_hash", "source_snapshot_sha256",
                "source_count", "event_count"
            ], &[]),
            "CoreMaterializationReceipt": fields(&[
                "core_generation_id", "core_record_contract_fingerprint",
                "source_snapshot_sha256", "materializer_revision", "source_count", "event_count"
            ], &[]),
            "CoreMaterializationReceiptIdentity": fields(
                &["core_generation_id", "materializer_revision"], &[]),
            "BeginCoreMaterializationRequest": fields(
                &["head", "expected_prior_receipt"], &[]),
            "CoreMaterializationBegan": fields(&[
                "materialization_id", "core_generation_id", "materializer_revision",
                "expected_prior_receipt", "replayed"
            ], &[]),
            "CoreSourceDeltaPage": fields(&[
                "materialization_id", "core_generation_id", "page_index", "terminal", "deltas"
            ], &[]),
            "ApplyCoreSourceDeltaPageRequest": fields(&["page"], &[]),
            "CoreSourceDeltaPageApplied": fields(&[
                "materialization_id", "core_generation_id", "page_index", "changed_sources",
                "removed_sources", "reconcile_sources", "replayed"
            ], &[]),
            "CoreSourceReconciliation": fields(&["delta"], &[]),
            "CoreEventState": fields(
                &["event_id", "core_record_sha256", "requires_replacement"], &[]),
            "CoreEventStatePageRequest": fields(&[
                "materialization_id", "core_generation_id", "reconciliation", "page_index",
                "after_event_id", "maximum_items"
            ], &[]),
            "CoreEventStatePage": fields(&[
                "materialization_id", "core_generation_id", "reconciliation", "page_index",
                "after_event_id", "states", "terminal", "replayed"
            ], &[]),
            "CoreEventReplacement": fields(&["prior_core_record_sha256", "record"], &[]),
            "CoreEventTombstone": fields(&["event_id", "prior_core_record_sha256"], &[]),
            "CoreEventDeltaPage": fields(&[
                "materialization_id", "core_generation_id", "reconciliation", "page_index",
                "terminal", "deltas"
            ], &[]),
            "ApplyCoreEventDeltaPageRequest": fields(&["page"], &[]),
            "CoreEventDeltaPageApplied": fields(&[
                "materialization_id", "core_generation_id", "source", "page_index",
                "additions", "replacements", "tombstones", "terminal", "replayed"
            ], &[]),
            "FinishCoreMaterializationRequest": fields(&[
                "materialization_id", "head", "expected_prior_receipt", "source_delta_pages",
                "changed_sources", "removed_sources", "event_delta_pages", "event_mutations"
            ], &[]),
            "CoreMaterializationFinished": fields(&["receipt", "replayed"], &[]),
            "QuerySnapshotExpectation.core": fields(&["kind", "receipt"], &[]),
            "EvidenceCitation": fields(&[
                "core_generation_id", "source", "session_id", "event_id", "event_sequence",
                "byte_range", "evidence_sha256"
            ], &[]),
            "CoreRecord": fields(&[
                "record_version", "event_id", "session_id", "parent_session_id",
                "root_session_id", "source", "provider_session_id", "native_event_id",
                "event_sequence", "occurred_at_unix_ms", "event_type", "role", "agent_type",
                "is_primary", "workspace", "branch", "cwd", "parser_revision",
                "normalization_revision", "content", "metadata", "repository_candidate_evidence",
                "repository_bindings", "repository_abstentions", "repository_file_observations",
                "repository_vcs_observations"
            ], &[]),
            "RepositoryCandidateEvidence": fields(&[
                "repository_observation_revision", "bounded_shell_subset_revision",
                "association_policy_revision", "outcome_capture_revision", "candidates"
            ], &[]),
            "RepositoryCandidate": fields(&["kind", "path"], &[])
        },
        "core_record_contract": {
            "fingerprint": core_record_contract_fingerprint(),
            "repository_contract_revision": CORE_REPOSITORY_CONTRACT_REVISION,
            "repository_observation_revision": CORE_REPOSITORY_OBSERVATION_REVISION,
            "bounded_shell_subset_revision": CORE_BOUNDED_SHELL_SUBSET_REVISION,
            "repository_association_policy_revision":
                CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
            "repository_outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            "repository_local_root_authorization_fingerprint_revision":
                CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
            "repository_candidate_set": "strictly_sorted_unique_kind_and_path_pairs"
        },
        "core_materialization": {
            "contract_version": CORE_MATERIALIZATION_CONTRACT_VERSION,
            "sequence": [
                "begin_core_materialization", "apply_core_source_delta_page",
                "core_event_state_page", "apply_core_event_delta_page",
                "finish_core_materialization"
            ],
            "authority": "one_generation_pinned_core_snapshot_delta_feed",
            "initial": "the_helper_reconciles_every_present_source_because_it_has_no_prior_event_state",
            "incremental": "source_delta_ack_reconcile_sources_is_the_exact_ordered_changed_or_removed_subset_and_event_state_pages_drive_bounded_added_replaced_and_tombstoned_event_delta_pages",
            "removal": "source_removal_is_resumable_as_bounded_event_tombstone_pages_and_deletes_source_control_state only_on_its_terminal_page",
            "replacement": "prior_event_removal_and_replacement_record_insertion_are_atomic_in_one_bounded_event_delta_page",
            "records": "complete_core_records_are_read_only_from_the_pinned_core_source_event_page_api_and_only_added_or_replaced_events_cross_the_protocol",
            "repository_data": "repository_bindings_abstentions_file_and_vcs_observations_exist_only_inside_core_records",
            "publication": "explicit_terminal_counts_at_most_one_prior_receipt_cas_and_a_small_final_control_transaction",
            "replay": "exact_completed_generation_may_skip_all_delta_and_event pages",
            "integrity": "strict_page_sequence_content_generation_and_transactional_staging_without_hash_chains"
        },
        "status_contract": {
            "axes": [
                "core_projection_currentness", "materialized_target_coverage",
                "repository_coverage", "access", "supported_operations",
                "available_operations"
            ],
            "repository_coverage_axes": [
                "repository_candidate_events", "logical_binding_events",
                "certified_live_root_access_events", "file_evidence_events",
                "exact_commit_evidence_events", "exact_pull_request_evidence_events"
            ],
            "coverage_bound": "repository_candidate_events_is_at_most_receipt_event_count_and every_specialized_axis_is_a_subset_of_logical_binding_events_which_is_a_subset_of_repository_candidate_events",
            "terminal_coverage": {
                "empty": "receipt_event_count_is_zero_and_all_repository_coverage_axes_are_zero",
                "abstained": "receipt_event_count_is_positive_and_logical_binding_events_is_zero",
                "complete": "receipt_event_count_is_positive_and_logical_binding_events_is_positive"
            },
            "operation_prerequisites": {
                "global": ["current_complete_projection", "entitlement", "graph_key"],
                "file_blame": [
                    "local_repository", "certified_live_root_access_events", "file_evidence_events",
                    "exact_commit_evidence_events"
                ],
                "commit_blame": ["exact_commit_evidence_events"],
                "pull_request_blame": ["exact_pull_request_evidence_events"]
            },
            "availability": "available_is_a_supported_subset_that_satisfies_each_operation_prerequisite_and_ready_operations_may_be_conservatively_omitted",
            "terminal_quiet": "current_empty_or_abstained_is_terminal_and_advertises_no_available_blame_operation"
        },
        "representative_frames": {
            "host_status": frame_hex(&status),
            "helper_protocol_mismatch": frame_hex(&error)
        }
    })
}
