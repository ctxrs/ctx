use super::*;
use ctx_history_core::{
    core_record_contract_fingerprint, CORE_BOUNDED_SHELL_SUBSET_REVISION,
    CORE_MCP_TOOL_CALL_ATTRIBUTION_REVISION, CORE_RECORD_ACCUMULATOR_IDENTITY,
    CORE_RECORD_LEAF_DOMAIN, CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
    CORE_REPOSITORY_CONTRACT_REVISION,
    CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
    CORE_REPOSITORY_OBSERVATION_REVISION, CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
    CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION, CORE_SESSION_LINEAGE_REVISION,
    MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
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
            "exact Protocol V2 mismatch",
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
            "core_source_acknowledgement_page_items": MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
            "core_event_state_page_items": MAX_CORE_EVENT_STATE_PAGE_ITEMS,
            "core_event_delta_page_items": MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
            "core_event_delta_page_content_bytes": MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
            "core_event_delta_pages": MAX_CORE_EVENT_DELTA_PAGES,
            "core_event_delta_pages_request_wire_bytes":
                MAX_CORE_EVENT_DELTA_PAGES_REQUEST_WIRE_BYTES,
            "core_event_delta_pages_prepared_output_bytes":
                MAX_CORE_EVENT_DELTA_PAGES_PREPARED_OUTPUT_BYTES,
            "core_source_delta_page_wire_bytes": MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES,
            "core_event_state_page_wire_bytes": MAX_CORE_EVENT_STATE_PAGE_WIRE_BYTES,
            "core_event_delta_page_wire_bytes": MAX_CORE_EVENT_DELTA_PAGE_WIRE_BYTES,
            "core_control_wire_bytes": MAX_CORE_CONTROL_WIRE_BYTES,
            "core_materializer_revision_bytes": MAX_CORE_MATERIALIZER_REVISION_BYTES,
            "journal_finish_workers": MAX_JOURNAL_FINISH_WORKERS,
            "blame_results": MAX_BLAME_RESULTS,
            "blame_cursor_bytes": MAX_BLAME_CURSOR_BYTES,
            "blame_evidence": MAX_BLAME_EVIDENCE,
            "blame_attributions_per_match": MAX_BLAME_ATTRIBUTIONS_PER_MATCH,
            "blame_diagnostic_candidates": MAX_BLAME_DIAGNOSTIC_CANDIDATES,
            "citations_per_fact": MAX_CITATIONS_PER_FACT,
            "blame_target_bytes": MAX_BLAME_TARGET_BYTES,
            "commit_lineage_returned_events": MAX_COMMIT_LINEAGE_RETURNED_EVENTS,
            "commit_lineage_examined_events": MAX_COMMIT_LINEAGE_EXAMINED_EVENTS,
            "mcp_tool_call_attribution_component_bytes":
                MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
        },
        "commit_lineage_contract": {
            "operation_kinds": ["amend", "rebase", "cherry_pick"],
            "relation_classes": ["replacement", "derivation"],
            "proof_classes": ["record_exact", "repository_verified", "forge_verified"],
            "states": ["asserted", "ambiguous", "contradicted"],
            "omission_kinds": ["exact", "at_least", "unknown"],
            "endpoint_kinds": ["current_at_ref", "current_for_pr"],
            "stable_edge_order": [
                "operation_id", "operation_kind", "logical_repository_id",
                "source_object_format", "source_oid", "result_object_format", "result_oid"
            ],
            "stable_yield_order": ["operation_id", "yield_id", "actor_id"],
            "commit_identity": "canonical_logical_repository_id_plus_object_format_plus_full_oid",
            "operation_id": "canonical_lowercase_sha256_digest",
            "operation_grouping": "edges_and_yields_share_operation_id_and_consistent_metadata",
            "returned_event_count": "distinct_operation_ids_across_edges_and_yields",
            "asserted_yield_proof": "repository_verified",
            "connectivity": "all_operations_connect_to_requested_and_claimed_origins_and_endpoints_follow_directed_reachability",
            "match_pagination": "independent"
        },
        "host_message_kinds": [
            "hello", "authorize", "prepare_graph_key_deletion",
            "confirm_graph_key_deletion", "status",
            "begin_core_materialization", "apply_core_source_delta_page",
            "core_event_state_page", "apply_core_event_delta_page",
            "finish_core_materialization", "blame", "apply_core_event_delta_pages"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "core_materialization_began", "core_source_delta_page_applied",
            "core_event_state_page", "core_event_delta_page_applied",
            "core_materialization_finished", "blame", "error",
            "core_event_delta_pages_applied"
        ],
        "capabilities": wire_names(&capabilities, Capability::wire_name),
        "enums": {
            "error_class": [
                "entitlement_expired", "key_store_unavailable", "key_store_locked",
                "not_materialized", "protocol_mismatch", "missing_source",
                "missing_repository", "resource_not_found", "stale_fact",
                "line_out_of_range", "stale_snapshot", "ambiguous",
                "operation_unavailable", "corrupt", "invalid_request", "bounds", "rebuild_required",
                "sequence", "internal"
            ],
            "blame_attribution": ["proven", "possible", "conflicting", "none"],
            "blame_coverage_unit": [
                "committed_line", "commit_fact", "pull_request_relationship"
            ],
            "blame_diagnostic_reason": [
                "target_not_indexed", "repository_selector_not_indexed",
                "repository_not_bound", "checkout_unavailable", "git_unavailable",
                "repository_ambiguous", "target_ambiguous", "commit_rewrite_ambiguous",
                "file_blame_not_covered",
                "commit_blame_not_covered", "pull_request_blame_not_covered"
            ],
            "blame_diagnostic_candidate_kind": ["repository", "commit"],
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
            "session_relationship_kind": [
                "root", "delegated", "forked", "resumed_from", "workflow_child",
                "related_unknown"
            ],
            "event_origin_kind": ["unknown", "unique_to_session", "copied_from_ancestor"],
            "event_copy_proof_kind": [
                "native_event_identity", "native_copied_from_field",
                "native_call_result_identity", "certified_ordered_prefix"
            ],
            "repository_candidate_kind": [
                "session_cwd", "declared_tool_workdir", "derived_effective_cwd",
                "command_specific_repository_path", "file_activity_path", "vcs_activity_path",
                "outcome_operation_repository_path", "outcome_output_repository_path"
            ],
            "repository_file_invocation_kind": [
                "read", "create", "modify", "delete", "rename", "write"
            ],
            "query_snapshot_expectation_kind": ["core"],
            "resource_kind": ResourceKind::ALL.map(ResourceKind::wire_name)
        },
        "dto_fields": {
            "StatusRequest": fields(&["requested_core_generation_id"], &[]),
            "StatusResult": fields(&[
                "currentness", "requested_core_generation_id", "core_receipt", "coverage",
                "repository_coverage", "core_preparation_peak_workers", "access", "supported_operations",
                "available_operations", "storage_evidence"
            ], &[]),
            "RepositoryCoverage": fields(&[
                "repository_candidate_events", "logical_binding_events",
                "certified_live_root_access_events", "file_evidence_events",
                "exact_commit_evidence_events", "exact_pull_request_evidence_events"
            ], &[]),
            "ProStorageEvidence": fields(&[
                "graph_manifest_schema", "flat_format_version",
                "materializer_checkpoint_version", "journal_pack_format_version",
                "legacy_journals_written", "journal_pages_written", "journal_packs_written",
                "journal_finish_activity"
            ], &[]),
            "JournalFinishActivity": fields(&[
                "worker_limit", "peak_workers", "started_after_preparation"
            ], &[]),
            "ProAccessStatus": fields(&["entitlement", "graph_key", "local_repository"], &[]),
            "CoreSourceState": fields(
                &["source", "core_record_accumulator", "event_count"], &[]),
            "CoreSourceRemoval": fields(&["source"], &[]),
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
            "ApplyCoreSourceDeltaPageRequest": fields(
                &["page", "acknowledgement_page_index"], &[]),
            "CoreSourceDeltaPageApplied": fields(&[
                "materialization_id", "core_generation_id", "page_index",
                "acknowledgement_page_index", "acknowledgement_terminal",
                "changed_sources", "removed_sources", "reconcile_sources", "replayed"
            ], &[]),
            "CoreSourceReconciliation": fields(&["materialize_index", "delta"], &[]),
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
            "ApplyCoreEventDeltaPagesRequest": fields(&["pages"], &[]),
            "CoreEventDeltaPageApplied": fields(&[
                "materialization_id", "core_generation_id", "source", "page_index",
                "additions", "replacements", "tombstones", "terminal", "replayed"
            ], &[]),
            "CoreEventDeltaPagesApplied": fields(&["pages"], &[]),
            "FinishCoreMaterializationRequest": fields(&[
                "materialization_id", "head", "expected_prior_receipt", "source_delta_pages",
                "changed_sources", "removed_sources", "event_delta_pages", "event_mutations"
            ], &[]),
            "CoreMaterializationFinished": fields(&["receipt", "replayed"], &[]),
            "QuerySnapshotExpectation.core": fields(&["kind", "receipt"], &[]),
            "ProtocolError": fields(
                &["class", "message", "retryable", "details"], &[]),
            "BlameDiagnosticDetails": fields(
                &["reason", "candidates", "candidates_truncated"], &[]),
            "BlameDiagnosticCandidate.repository": fields(
                &["kind", "selector"], &[]),
            "BlameDiagnosticCandidate.commit": fields(
                &["kind", "repository", "oid"], &[]),
            "BlameOutcome": fields(&["attribution", "coverage"], &[]),
            "BlameCoverage": fields(&[
                "unit", "evaluated", "proven", "possible", "conflicting", "none"
            ], &[]),
            "BlameResult": fields(&[
                "snapshot", "target", "git_snapshot", "outcome", "matches", "evidence", "next",
                "lineage"
            ], &[]),
            "ExactCommitRef": fields(
                &["resource", "logical_repository_id", "object_format", "oid"], &[]),
            "CommitLineage": fields(&[
                "requested", "edges", "yielded_by", "origin", "endpoint", "complete",
                "ambiguous", "bounds"
            ], &[]),
            "CommitLineageEdge": fields(&[
                "operation_id", "kind", "relation_class", "source", "result", "actor",
                "proof_class", "state", "observed_at_ms", "evidence_numbers"
            ], &[]),
            "CommitLineageYield": fields(&[
                "yield_id", "operation_id", "logical_repository_id", "actor", "proof_class",
                "state", "observed_at_ms", "evidence_numbers"
            ], &[]),
            "CommitLineageBounds": fields(&[
                "returned_events", "returned_event_limit", "examined_events",
                "examined_event_limit", "omission", "truncation_reason"
            ], &[]),
            "ScopedCommitEndpoint.current_at_ref": fields(&[
                "kind", "commit", "scope", "observation_id", "observed_at_ms",
                "evidence_numbers"
            ], &[]),
            "ScopedCommitEndpoint.current_for_pr": fields(&[
                "kind", "commit", "scope", "observation_id", "observed_at_ms",
                "evidence_numbers"
             ], &[]),
            "EvidenceCitation": fields(&[
                "core_generation_id", "source", "session_id", "event_id", "event_sequence",
                "byte_range", "evidence_sha256"
            ], &[]),
            "AgentAttribution": fields(&[
                "id", "relationship", "producing_session", "parent_session",
                "direct_actor", "owning_root", "fact_occurred_at_ms", "confidence", "state",
                "evidence_numbers"
            ], &[]),
            "CommitBlameMatch": fields(&[
                "fact_id", "fact_type", "predicate", "subject", "object",
                "parent_session", "fact_occurred_at_ms", "confidence", "state",
                "direct_actor", "owning_root", "evidence_numbers"
            ], &[]),
            "PullRequestActivity": fields(&[
                "fact_id", "action", "session", "direct_actor", "owning_root",
                "fact_occurred_at_ms", "confidence", "state", "evidence_numbers"
            ], &[]),
            "PullRequestCommit": fields(&[
                "fact_id", "relationship", "commit", "fact_occurred_at_ms", "production",
                "evidence_numbers"
            ], &[]),
            "CoreRecord": fields(&[
                "record_version", "event_id", "session_id", "parent_session_id",
                "root_session_id", "session_relationship", "event_origin", "source",
                "provider_session_id", "native_event_id",
                "event_sequence", "occurred_at_unix_ms", "event_type", "role", "agent_type",
                "is_primary", "workspace", "branch", "cwd", "parser_revision",
                "normalization_revision", "content", "metadata", "repository_candidate_evidence",
                "repository_bindings", "repository_abstentions",
                "repository_file_invocation_evidence", "repository_file_observations",
                "repository_vcs_observations"
            ], &["mcp_tool_call"]),
            "EventOrigin.unknown": fields(&["kind"], &[]),
            "EventOrigin.unique_to_session": fields(&["kind"], &[]),
            "EventOrigin.copied_from_ancestor": fields(
                &["kind", "ancestor_session_id", "ancestor_event_id", "proof"], &[]),
            "McpToolCallAttribution": fields(&["server", "tool"], &[]),
            "RepositoryCandidateEvidence": fields(&[
                "repository_observation_revision", "bounded_shell_subset_revision",
                "association_policy_revision", "outcome_capture_revision", "candidates"
            ], &[]),
            "RepositoryCandidate": fields(&["kind", "path"], &[]),
            "RepositoryFileInvocationEvidence": fields(&[
                "operation_ordinal", "repository_binding_id", "relative_path",
                "prior_relative_path", "kind", "tool_name", "normalized_text_range"
            ], &[]),
            "RepositoryFileInvocationTextRange": fields(&["start", "end"], &[]),
            "RepositoryVcsObservation": fields(&[
                "repository_binding_id", "kind", "object_id", "parent_object_ids",
                "reference", "relative_path"
            ], &[]),
            "RepositoryVcsObservationKind.pull_request_association": fields(
                &["pull_request_association"], &[]),
            "RepositoryPullRequestAssociationObservation": fields(&[
                "pull_request", "merged_as", "contains_commits", "linkage",
                "association_capture_revision"
            ], &[]),
            "RepositoryPullRequestIdentity": fields(
                &["forge_repository", "number", "provider_id"], &[]),
            "GitObjectId": fields(&["format", "hex"], &[]),
            "RepositoryOutcomeLinkage": fields(&[
                "provider", "origin_call_id", "result_call_id", "origin_event_sequence",
                "continuation_call_id_sha256", "result_record_sha256"
            ], &[])
        },
        "core_record_contract": {
            "fingerprint": core_record_contract_fingerprint(),
            "leaf": {
                "helper": "ctx_pro_host_protocol::core_record_leaf_sha256",
                "paired_helper": "ctx_pro_host_protocol::core_record_digests",
                "domain": std::str::from_utf8(CORE_RECORD_LEAF_DOMAIN)
                    .unwrap_or("<invalid-Core-record-leaf-domain>"),
                "algorithm": "sha256(domain_then_canonical_event_id_then_u64_be_exact_canonical_core_record_json_length_then_exact_canonical_core_record_json)",
                "added_path": "CoreEventDelta.added.value",
                "replaced_path": "CoreEventDelta.replaced.value.record"
            },
            "accumulator": {
                "identity": std::str::from_utf8(CORE_RECORD_ACCUMULATOR_IDENTITY)
                    .unwrap_or("<invalid-Core-record-accumulator-identity>"),
                "algorithm": "sum_mod_2^256(sha256(identity_then_u64_be_canonical_event_id_length_then_canonical_event_id_then_core_record_leaf))"
            },
            "repository_contract_revision": CORE_REPOSITORY_CONTRACT_REVISION,
            "repository_observation_revision": CORE_REPOSITORY_OBSERVATION_REVISION,
            "bounded_shell_subset_revision": CORE_BOUNDED_SHELL_SUBSET_REVISION,
            "repository_association_policy_revision":
                CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
            "repository_pull_request_association_capture_revision":
                CORE_REPOSITORY_PULL_REQUEST_ASSOCIATION_CAPTURE_REVISION,
            "repository_outcome_capture_revision": CORE_REPOSITORY_OUTCOME_CAPTURE_REVISION,
            "repository_local_root_authorization_fingerprint_revision":
                CORE_REPOSITORY_LOCAL_ROOT_AUTHORIZATION_FINGERPRINT_REVISION,
            "mcp_tool_call_attribution_revision": CORE_MCP_TOOL_CALL_ATTRIBUTION_REVISION,
            "session_lineage_revision": CORE_SESSION_LINEAGE_REVISION,
            "mcp_tool_call_attribution": {
                "wire_path": "CoreRecord.mcp_tool_call",
                "presence": "optional_omitted_when_absent_explicit_null_rejected",
                "shape": "exact_server_and_tool_string_pair",
                "component_bound": "decoded_utf8_bytes",
                "maximum_component_bytes": MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
            },
            "repository_candidate_set": "strictly_sorted_unique_kind_and_path_pairs",
            "repository_file_invocation_evidence_set":
                "strictly_sorted_unique_typed_request_intent_bound_to_repository_and_normalized_body"
        },
        "core_materialization": {
            "contract_version": CORE_MATERIALIZATION_CONTRACT_VERSION,
            "sequence": [
                "begin_core_materialization", "apply_core_source_delta_page",
                "core_event_state_page", "apply_core_event_delta_pages",
                "finish_core_materialization"
            ],
            "authority": "one_generation_pinned_core_snapshot_delta_feed",
            "initial": "the_helper_reconciles_every_present_source_because_it_has_no_prior_event_state",
            "incremental": "each_source_delta_page_drives_zero_through_terminal_bounded_acknowledgement_pages_whose_exact_ordered_changed_or_removed_subset_is_collected_within_current_plus_retained_store_source_bounds_then_event_state_pages_drive_bounded_added_replaced_and_tombstoned_event_delta_pages",
            "source_acknowledgements": "request_and_response_share_an_explicit_acknowledgement_page_index_present_reconciliations_exist_only_on_page_zero_nonterminal_input_source_pages_complete_in_one_acknowledgement_page_each_response_marks_terminal_changed_removed_counts_cover_only_that_page_and_all_source_acknowledgements_finish_before_any_event_page",
            "removal": "source_removal_is_resumable_as_bounded_event_tombstone_pages_and_deletes_source_control_state only_on_its_terminal_page",
            "replacement": "prior_event_removal_and_replacement_record_insertion_are_atomic_in_one_bounded_event_delta_page",
            "event_delta_batching": {
                "request": "one_to_sixteen_pages_partitioned_into_one_contiguous_sub_batch_per_source_each_sharing_one_materialize_index_with_sub_batches_following_strictly_increasing_materialize_index_order_and_terminal_source_boundaries_and_each_source_retaining_strict_event_order",
                "aggregate_input": "exact_compact_request_json_is_at_most_sixty_eight_mib_while_every_page_retains_its_existing_bounds",
                "prepared_output": "helper_prepared_output_accounting_is_at_most_one_hundred_twenty_eight_mib",
                "acknowledgement": "one_ordered_count_exact_page_acknowledgement_per_requested_page"
            },
            "records": "complete_core_records_are_read_only_from_the_pinned_core_source_event_page_api_and_only_added_or_replaced_events_cross_the_protocol",
            "repository_data": "repository_bindings_abstentions_file_invocation_evidence_and_file_and_vcs_observations_exist_only_inside_core_records",
            "publication": "explicit_terminal_counts_at_most_one_prior_receipt_cas_and_a_small_final_control_transaction",
            "replay": "source_delta_requests_and_acknowledgement_pages_are_independently_idempotent_and_an_exact_completed_generation_may_skip_all_delta_and_event_pages",
            "integrity": "strict_source_acknowledgement_and_event_page_sequence_content_generation_and_transactional_staging_without_hash_chains"
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
