use super::*;

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
            "core_record_page_items": MAX_CORE_RECORD_PAGE_ITEMS,
            "core_record_page_content_bytes": MAX_CORE_RECORD_PAGE_CONTENT_BYTES,
            "core_source_delta_page_wire_bytes": MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
            "core_record_page_wire_bytes": MAX_CORE_RECORD_PAGE_WIRE_BYTES,
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
            "materialize_core_record_page", "finish_core_materialization", "blame"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "core_materialization_began", "core_source_delta_page_applied",
            "core_record_page_materialized", "core_materialization_finished", "blame", "error"
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
                &["source", "source_revision_sha256", "event_count"], &[]),
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
                "removed_sources", "materialize_sources", "replayed"
            ], &[]),
            "CoreRecordPage": fields(&[
                "materialization_id", "core_generation_id", "source", "source_index",
                "page_index", "terminal", "records"
            ], &[]),
            "MaterializeCoreRecordPageRequest": fields(&["page"], &[]),
            "CoreRecordPageMaterialized": fields(&[
                "materialization_id", "core_generation_id", "source",
                "source_revision_sha256", "source_index", "page_index", "accepted_records",
                "terminal", "replayed"
            ], &[]),
            "FinishCoreMaterializationRequest": fields(&[
                "materialization_id", "head", "expected_prior_receipt", "source_delta_pages",
                "changed_sources", "removed_sources", "record_pages", "materialized_records"
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
            ], &[])
        },
        "core_materialization": {
            "contract_version": CORE_MATERIALIZATION_CONTRACT_VERSION,
            "sequence": [
                "begin_core_materialization", "apply_core_source_delta_page",
                "materialize_core_record_page", "finish_core_materialization"
            ],
            "authority": "one_generation_pinned_core_snapshot_delta_feed",
            "initial": "the_helper_requests_every_present_source_because_it_has_no_prior_revision",
            "incremental": "producer_sends_every_current_source_as_present_and_delta_ack_materialize_sources_is_the_exact_ordered_actually_changed_subset_that_receives_record_pages",
            "removal": "removed_sources_are_applied_only_from_the_same_ordered_delta_pages",
            "records": "complete_core_records_are_read_only_from_the_pinned_core_source_event_page_api",
            "repository_data": "repository_bindings_abstentions_file_and_vcs_observations_exist_only_inside_core_records",
            "publication": "explicit_terminal_counts_and_at_most_one_prior_receipt_cas",
            "replay": "exact_completed_generation_may_skip_all_delta_and_record_pages",
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
            "coverage_bound": "each_repository_coverage_event_count_is_at_most_the_receipt_event_count_and_is_zero_without_a_receipt",
            "operation_prerequisites": {
                "global": ["current_complete_projection", "entitlement", "graph_key"],
                "file_blame": [
                    "local_repository", "certified_live_root_access_events", "file_evidence_events"
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
