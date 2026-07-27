use super::*;

fn wire_names<T: Copy>(values: &[T], name: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    values.iter().copied().map(name).collect()
}

pub(super) fn inventory() -> Value {
    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::Query,
        Capability::GitRead,
    ];
    let request_id = Uuid::from_u128(1);
    let status = HostEnvelope {
        sequence: 0,
        request_id,
        message: HostMessage::Status(StatusRequest {}),
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
        "projection_contract_version": PROJECTION_CONTRACT_VERSION,
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
            "journal_records_per_batch": MAX_JOURNAL_RECORDS_PER_BATCH,
            "journal_context_records": MAX_JOURNAL_CONTEXT_RECORDS,
            "journal_context_bytes": MAX_JOURNAL_CONTEXT_BYTES,
            "journal_payload_bytes": MAX_JOURNAL_PAYLOAD_BYTES,
            "journal_evidence_per_record": MAX_JOURNAL_EVIDENCE_PER_RECORD,
            "journal_identity_bytes": MAX_JOURNAL_IDENTITY_BYTES,
            "authorized_repository_roots": MAX_AUTHORIZED_REPOSITORY_ROOTS,
            "authorized_repository_root_bytes": MAX_AUTHORIZED_REPOSITORY_ROOT_BYTES,
            "authorized_repository_roots_total_bytes": MAX_AUTHORIZED_REPOSITORY_ROOTS_TOTAL_BYTES,
            "journal_sync_envelope_bytes": MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
            "output_observations_per_page": MAX_OUTPUT_OBSERVATIONS_PER_PAGE,
            "output_content_bytes": MAX_OUTPUT_CONTENT_BYTES,
            "output_content_bytes_per_page": MAX_OUTPUT_CONTENT_BYTES_PER_PAGE,
            "output_cursor_bytes": MAX_OUTPUT_CURSOR_BYTES,
            "output_locator_bytes": MAX_OUTPUT_LOCATOR_BYTES,
            "output_identity_bytes": MAX_OUTPUT_IDENTITY_BYTES,
            "output_command_bytes": MAX_OUTPUT_COMMAND_BYTES,
            "output_progress_sources": MAX_OUTPUT_PROGRESS_SOURCES,
            "blame_results": MAX_BLAME_RESULTS,
            "blame_cursor_bytes": MAX_BLAME_CURSOR_BYTES,
            "blame_evidence": MAX_BLAME_EVIDENCE,
            "blame_attributions_per_match": MAX_BLAME_ATTRIBUTIONS_PER_MATCH,
            "citations_per_fact": MAX_CITATIONS_PER_FACT,
            "blame_target_bytes": MAX_BLAME_TARGET_BYTES
        },
        "host_message_kinds": [
            "hello", "authorize", "prepare_graph_key_deletion",
            "confirm_graph_key_deletion", "status", "sync_journal",
            "begin_output_inventory", "observe_output_source", "materialize_output_page",
            "finish_output_inventory", "get_output_progress", "blame"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status", "journal_synced", "output_inventory_began", "output_source_observed",
            "output_page_materialized", "output_inventory_finished", "output_progress",
            "blame", "error"
        ],
        "capabilities": wire_names(&capabilities, Capability::wire_name),
        "enums": {
            "entitlement_access_kind": ["trial", "active", "canceling_paid"],
            "entitlement_access_state": ["trial", "active", "canceling_paid", "offline_grace", "locked"],
            "entitlement_capability": ["graph_read", "graph_write", "export", "migrate", "update"],
            "error_class": [
                "entitlement_expired", "key_store_unavailable", "key_store_locked",
                "not_materialized", "protocol_mismatch", "missing_source",
                "missing_repository", "stale_fact", "line_out_of_range",
                "stale_snapshot", "ambiguous", "corrupt",
                "invalid_request", "bounds", "sequence", "internal"
            ],
            "blame_match_kind": ["file", "commit", "pull_request"],
            "blame_target_kind": ["file", "commit", "pull_request"],
            "commit_fact_type": [
                "git.commit.produced", "git.commit.amended", "git.commit.cherry_picked",
                "git.commit.reverted", "git.commit.pushed", "git.commit.inspected",
                "git.commit.referenced", "git.commit.ambiguous"
            ],
            "commit_predicate": [
                "produced_by", "possibly_produced_by", "amended_by", "cherry_picked_from",
                "reverts", "pushed_by", "inspected_by", "referenced_by"
            ],
            "continuation_reason": ["more_matches", "more_committed_lines"],
            "fact_confidence": ["explicit", "high", "medium", "low", "ambiguous", "unknown"],
            "fact_state": ["asserted", "ambiguous", "contradicted", "superseded"],
            "graph_state": ["not_materialized", "needs_rebuild", "partial", "needs_resume", "ready"],
            "journal_entity_kind": ["event", "file_touch", "vcs_change"],
            "journal_operation": ["upsert", "delete"],
            "journal_sync_mode": ["full_baseline", "incremental"],
            "observation_kind": ["event", "file_touch", "vcs_change"],
            "output_observation_kind": ["command", "tool"],
            "output_outcome": ["success", "failure", "timeout", "unknown"],
            "output_source_availability": ["available", "unavailable", "error"],
            "output_source_disposition": ["append_or_resume", "new_source", "rewrite"],
            "production_relationship": ["produced_by", "possibly_produced_by"],
            "pull_request_action": [
                "referenced", "created", "reviewed", "commented",
                "merged", "edited", "closed", "reopened"
            ],
            "pull_request_commit_relationship": ["contains_commit", "merged_as"],
            "pull_request_relationship_kind": ["activity", "commit"],
            "resource_kind": ResourceKind::ALL.map(ResourceKind::wire_name),
            "worktree_status": ["clean", "differs"]
        },
        "dto_fields": {
            "AgentAttribution": fields(
                &["id", "relationship", "producing_session", "confidence", "state", "evidence_numbers"],
                &["direct_actor", "owning_root"]),
            "AuthorizationRequest": fields(
                &["entitlement", "installation_public_key_base64url", "challenge_base64url", "proof_signature_base64url"], &[]),
            "AuthorizationResult": fields(&[
                "state", "refresh_required", "expires_at_unix", "access_deadline_unix",
                "grace_deadline_unix", "capabilities"
            ], &[]),
            "ByteRange": fields(&["start", "end_exclusive"], &[]),
            "BeginOutputInventoryRequest": fields(&["generation"], &[]),
            "BlameContinuation": fields(&["cursor", "reason"], &[]),
            "BlameRequest": fields(&["target", "limit", "expected_snapshot"], &["cursor"]),
            "BlameResult": fields(&["target", "matches", "evidence"], &["git_snapshot", "next"]),
            "BlameTarget.commit": fields(&["kind", "oid"], &["repository"]),
            "BlameTarget.file": fields(&["kind", "path"], &["repository", "lines"]),
            "BlameTarget.pull_request": fields(&["kind", "selector"], &["repository"]),
            "ConfirmGraphKeyDeletionRequest": fields(&["authorization"], &[]),
            "CommitBlameMatch": fields(&[
                "fact_id", "fact_type", "predicate", "subject", "confidence", "state",
                "evidence_numbers"
            ], &["object", "fact_occurred_at_ms", "direct_actor", "owning_root"]),
            "ContentRef": fields(&["sha256", "byte_len"], &[]),
            "EntitlementGrant": fields(&[
                "schema_version", "issuer", "key_id", "grant_id", "subject", "account_id",
                "product", "access_kind", "installation_key_thumbprint", "issued_at_unix",
                "not_before_unix", "refresh_after_unix", "access_deadline_unix",
                "grace_deadline_unix", "expires_at_unix", "minimum_helper_protocol",
                "revocation_epoch", "capabilities"
            ], &[]),
            "EvidenceCitation": fields(&[], &[
                "observation_id", "observation_seq", "observation_kind", "session_id", "event_id",
                "event_seq", "source_path", "fixture_line", "source_record_ordinal",
                "source_record_subrecord_index", "byte_range", "source_sha256", "provider_output"
            ]),
            "FileBlameMatch": fields(
                &["id", "lines", "commit", "line_evidence_numbers", "production"], &[]),
            "GitSnapshot": fields(&["head_oid", "worktree_status"], &[]),
            "GraphKeyDeleted": fields(&["deleted"], &[]),
            "GraphKeyDeletionPrepared": fields(&["challenge_base64url", "expires_at_unix", "key_present"], &[]),
            "HelloRequest": fields(&["protocol_version", "protocol_fingerprint", "host_version", "capabilities"], &[]),
            "HelloResult": fields(&[
                "protocol_version", "protocol_fingerprint", "helper_version", "capabilities",
                "authorization_challenge_base64url"
            ], &[]),
            "HelperEnvelope": fields(&["sequence", "request_id", "message"], &[]),
            "HostEnvelope": fields(&["sequence", "request_id", "message"], &[]),
            "JournalCheckpoint": fields(&["position", "contract_fingerprint", "cumulative_digest"], &[]),
            "JournalContextWindow": fields(&["base_checkpoint", "records"], &[]),
            "JournalEvidenceIdentity": fields(&["event_id"], &[
                "source_id", "source_path", "source_record_ordinal", "source_record_subrecord_index",
                "byte_start", "byte_end_exclusive"
            ]),
            "JournalPosition": fields(&["generation", "sequence"], &[]),
            "JournalProvenanceIdentity": fields(&["entity_kind", "stable_entity_id"], &[
                "capture_source_id", "provider", "provider_external_id"
            ]),
            "JournalRecord": fields(&[
                "generation", "sequence", "projection_contract_version", "entity_kind",
                "stable_entity_id", "entity_revision", "operation", "payload_sha256",
                "evidence", "provenance", "cumulative_digest"
            ], &["canonical_payload"]),
            "JournalSyncRequest": fields(&[
                "mode", "canonical_schema_version", "canonical_schema_identity",
                "projection_contract_version", "contract_fingerprint", "prior_checkpoint",
                "context", "frozen_through", "authorized_repository_roots", "records"
            ], &[]),
            "JournalSyncResult": fields(&[
                "committed_through", "accepted_records", "replayed", "frozen_complete"
            ], &[]),
            "LineRange": fields(&["start", "end"], &[]),
            "NumberedEvidence": fields(&["number", "citation"], &[]),
            "FinishOutputInventoryRequest": fields(&["generation"], &[]),
            "ObserveOutputSourceRequest": fields(
                &["generation", "source", "availability"], &[]),
            "OutputAssociations": fields(
                &["direct_session_id", "root_session_id"],
                &["parent_session_id", "provider_session_id", "agent_id", "repository"]),
            "OutputCommandContext": fields(
                &["tool_name", "command"], &["working_directory"]),
            "OutputInventoryBegan": fields(
                &["generation", "materializer_revision"], &[]),
            "OutputInventoryFinished": fields(
                &["generation", "observed_sources", "unavailable_sources"], &[]),
            "OutputNativeCoordinate": fields(
                &["unit_key", "native_sequence"],
                &["native_record_id", "source_record_ordinal",
                  "source_record_subrecord_index", "byte_start", "byte_end_exclusive"]),
            "OutputNativeCursor": fields(&["version", "payload_base64"], &[]),
            "OutputOutcomeMetadata": fields(
                &["outcome"], &["exit_code", "duration_ms"]),
            "OutputPageMaterialized": fields(&[
                "inventory_generation", "source", "source_epoch", "committed_cursor",
                "accepted_outputs", "materialized_facts", "materialized_evidence", "replayed"
            ], &[]),
            "OutputProgressRequest": fields(&["sources"], &[]),
            "OutputProgressResult": fields(
                &["inventory_generation", "inventory_complete", "sources"], &[]),
            "OutputRepositoryContext": fields(
                &["repository_id"], &["checkout_id", "worktree_id", "object_format"]),
            "OutputSourceIdentity": fields(
                &["provider", "namespace_id", "source_id"], &[]),
            "OutputSourceLocator": fields(
                &["version", "kind", "payload_base64"], &[]),
            "OutputSourceObserved": fields(
                &["generation", "source", "availability"], &[]),
            "OutputSourceProgress": fields(&[
                "source", "source_epoch", "observed_revision", "parser_revision",
                "materializer_revision", "terminal", "availability"
            ], &["cursor", "last_seen_inventory"]),
            "PrepareGraphKeyDeletionRequest": fields(&["installation_key_thumbprint"], &[]),
            "ProOutputMaterializationPage": fields(&[
                "contract_version", "inventory_generation", "source", "source_epoch",
                "observed_revision", "parser_revision", "materializer_revision", "disposition",
                "next_safe_cursor", "terminal", "observations"
            ], &["expected_prior_source_epoch", "expected_prior_cursor"]),
            "ProOutputObservation": fields(&[
                "kind", "coordinate", "associations", "outcome", "locator", "content"
            ], &["occurred_at_unix_ms", "call_id", "command"]),
            "ProviderOutputEvidence": fields(&[
                "source_id", "source_epoch", "locator", "coordinate", "availability"
            ], &[]),
            "ProtocolError": fields(&["class", "message", "retryable"], &[]),
            "PullRequestActivity": fields(&[
                "fact_id", "action", "session", "confidence", "state", "evidence_numbers"
            ], &["direct_actor", "owning_root", "fact_occurred_at_ms"]),
            "PullRequestBlameMatch": fields(&["pull_request", "relationship"], &[]),
            "PullRequestCommit": fields(
                &["fact_id", "relationship", "commit", "production", "evidence_numbers"], &[]),
            "QuerySnapshotExpectation": fields(&["checkpoint", "projection_pending"], &[]),
            "ResourceRef": fields(&["id", "kind", "display"], &[]),
            "ResolvedBlameTarget.commit": fields(&["kind", "commit", "repository"], &[]),
            "ResolvedBlameTarget.file": fields(
                &["kind", "path", "repository"], &["requested_lines"]),
            "ResolvedBlameTarget.pull_request": fields(
                &["kind", "selector", "pull_request", "repository"], &[]),
            "SignedEntitlement": fields(&["grant", "signature_base64url"], &[]),
            "StatusRequest": fields(&[], &[]),
            "StatusResult": fields(&["state"], &["checkpoint"]),
            "TransientOutputContent": {
                "wire_type": "canonical_base64_string",
                "debug": "redacted"
            }
        },
        "canonical_payload": {
            "wire_type": "optional_json_value",
            "encoding": "compact_json_with_recursively_sorted_object_keys_and_integer_only_numbers",
            "digest": "sha256_of_canonical_compact_utf8_bytes_or_empty_bytes_for_delete",
            "tombstone": "delete_requires_absent_payload_and_positive_entity_revision"
        },
        "canonical_payload_schema": {
            "root": "CanonicalObservation",
            "dto_fields": {
                "CanonicalObservation": fields(&[
                    "observation_id", "observation_seq", "observation_kind", "event_id",
                    "event_seq", "occurred_at_ms", "history_record_id", "event_type", "role",
                    "payload", "metadata", "result", "actor", "run", "source", "typed_event",
                    "file_touch", "vcs_change", "citation", "semantic_digest"
                ], &[]),
                "CanonicalActor": fields(&[
                    "direct_session_id", "root_session_id", "parent_session_id",
                    "external_session_id", "external_agent_id", "agent_type", "role_hint",
                    "is_primary"
                ], &[]),
                "CanonicalRun": fields(&[
                    "id", "run_type", "status", "started_at_ms", "ended_at_ms", "exit_code",
                    "cwd", "command_preview"
                ], &[]),
                "CanonicalSource": fields(&[
                    "id", "provider", "path", "format", "root", "identity", "cwd",
                    "imported_observation", "permitted_bytes"
                ], &[]),
                "CanonicalSourceObservation": fields(
                    &["byte_size", "modified_at_ms", "sha256"], &[]),
                "CanonicalFileTouch": fields(&[
                    "id", "history_record_id", "run_id", "event_id", "vcs_workspace_id", "path",
                    "change_kind", "old_path", "line_count_delta", "confidence", "source_id"
                ], &[]),
                "CanonicalVcsChange": fields(&[
                    "id", "vcs_workspace_id", "kind", "change_id", "parent_change_ids",
                    "branch_or_bookmark", "tree_hash", "author_time_ms", "confidence", "source_id"
                ], &[]),
                "CanonicalCitation": fields(&[
                    "observation_id", "observation_seq", "observation_kind", "event_id", "event_seq",
                    "source_path", "fixture_line", "source_record_ordinal",
                    "source_record_subrecord_index", "byte_range", "source_sha256"
                ], &[]),
                "CanonicalResultEvidence": fields(&["outcome", "identifiers"], &[]),
                "CanonicalResultIdentifier": fields(&["kind", "value"], &[])
            },
            "enums": {
                "typed_event_kind": ["file_touched", "vcs_change"],
                "file_change_kind": ["read", "created", "modified", "deleted", "renamed", "unknown"],
                "confidence": ["explicit", "high", "medium", "low", "unknown"],
                "vcs_change_kind": ["git_commit", "git_branch", "git_worktree", "jj_change", "jj_bookmark", "patch", "working_copy"],
                "result_outcome": ["success", "failure", "unknown"],
                "result_evidence_kind": ["call_id", "git_commit_summary_id", "git_oid", "git_abbrev_oid", "forge_url"]
            },
            "identity_rules": {
                "uuid": "non_nil_uuid",
                "optional_identity_bytes": MAX_JOURNAL_IDENTITY_BYTES,
                "source_reads": "forbidden; source root/cwd/imported observation/permitted bytes are absent or null",
                "subrecord": "source_record_subrecord_index_requires_source_record_ordinal"
            }
        },
        "journal_chain": {
            "initial_domain_hex": hex(b"ctx-pro-journal-initial-v1\0"),
            "record_domain_hex": hex(b"ctx-pro-journal-record-v1\0"),
            "new_generation": "full_baseline_starts_at_sequence_1_after_sequence_0_checkpoint",
            "ordering": "strictly_contiguous",
            "frozen_terminal": "position_and_cumulative_digest_captured_with_records",
            "checkpoint_commit": "only_after_private_graph_transaction_commits"
        },
        "output_materialization": {
            "contract_version": OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
            "durability": "complete_output_content_is_request_only_and_never_part_of_journal_sync",
            "ordering": "strict_native_sequence_then_unit_key",
            "commit": "page_sent_only_after_core_safe_group_commit_and_source_revalidation",
            "progress": "independent_per_source_epoch_and_native_cursor_compare_and_swap"
        },
        "evidence_citation": {
            "branches": {
                "canonical_or_source": [
                    "observation_id", "observation_seq", "observation_kind", "session_id",
                    "event_id", "event_seq", "source_path", "fixture_line",
                    "source_record_ordinal", "source_record_subrecord_index", "byte_range",
                    "source_sha256"
                ],
                "provider_output": ["provider_output"]
            },
            "selection": "exactly_one_usable_branch",
            "provider_output_coordinates": "typed_source_epoch_locator_and_native_coordinate",
            "provider_output_availability": "available_unavailable_or_error_is_preserved"
        },
        "representative_frames": {
            "host_status": frame_hex(&status),
            "helper_protocol_mismatch": frame_hex(&error)
        }
    })
}
