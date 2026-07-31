use super::*;

fn wire_names<T: Copy>(values: &[T], name: impl Fn(T) -> &'static str) -> Vec<&'static str> {
    values.iter().copied().map(name).collect()
}

pub(super) fn inventory() -> Value {
    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::SourceMaterialization,
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
            "source_manifest_sources": MAX_SOURCE_MANIFEST_SOURCES,
            "source_manifest_removals": MAX_SOURCE_MANIFEST_REMOVALS,
            "source_inventory_sources": MAX_SOURCE_INVENTORY_SOURCES,
            "source_progress_sources": MAX_SOURCE_PROGRESS_SOURCES,
            "source_records_per_page": MAX_SOURCE_RECORDS_PER_PAGE,
            "source_facts_per_record": MAX_SOURCE_FACTS_PER_RECORD,
            "source_touched_files_per_record": MAX_SOURCE_TOUCHED_FILES_PER_RECORD,
            "source_content_bytes": MAX_SOURCE_CONTENT_BYTES,
            "source_content_bytes_per_page": MAX_SOURCE_CONTENT_BYTES_PER_PAGE,
            "source_manifest_wire_bytes": MAX_SOURCE_MANIFEST_WIRE_BYTES,
            "source_manifest_page_items": MAX_SOURCE_MANIFEST_PAGE_ITEMS,
            "source_manifest_page_wire_bytes": MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES,
            "source_control_wire_bytes": MAX_SOURCE_CONTROL_WIRE_BYTES,
            "source_page_wire_bytes": MAX_SOURCE_PAGE_WIRE_BYTES,
            "source_identity_bytes": MAX_SOURCE_IDENTITY_BYTES,
            "source_path_bytes": MAX_SOURCE_PATH_BYTES,
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
            "begin_source_manifest_admission",
            "admit_source_manifest_page",
            "finish_source_manifest_admission", "prepare_source", "materialize_source_page",
            "delete_source", "finish_admitted_source_manifest", "blame"
        ],
        "helper_message_kinds": [
            "hello", "authorized", "graph_key_deletion_prepared", "graph_key_deleted",
            "status",
            "source_manifest_admission_began",
            "source_manifest_page_admitted", "source_manifest_admitted",
            "source_prepared", "source_page_materialized", "source_deleted",
            "source_manifest_finished",
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
            "materialization_authority": ["source"],
            "observation_kind": ["event", "file_touch", "vcs_change"],
            "source_disposition": ["new_source", "resume", "rewrite"],
            "source_outcome": ["success", "failure", "timeout", "unknown"],
            "transient_source_fact_kind": ["message", "command", "result"],
            "stable_entity_kind": ["Source", "Session", "Event"],
            "source_anchor": ["ProviderNative", "CatalogLineage"],
            "typed_key": ["Null", "Bytes", "Utf8", "I64", "U64", "F64Bits", "Bool", "Composite"],
            "locator_revision_policy": ["ExactSourceRevision", "StableRecordEvidence"],
            "native_record_coordinate": [
                "Jsonl", "ProviderSqlite", "Document", "TreeRecord", "ProviderNative"
            ],
            "production_relationship": ["produced_by", "possibly_produced_by"],
            "pull_request_action": [
                "referenced", "created", "reviewed", "commented",
                "merged", "edited", "closed", "reopened"
            ],
            "pull_request_commit_relationship": ["contains_commit", "merged_as"],
            "pull_request_relationship_kind": ["activity", "commit"],
            "query_snapshot_expectation_kind": ["source"],
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
            "BeginSourceManifestRequest": fields(&["manifest"], &[]),
            "BeginSourceManifestAdmissionRequest": fields(&["header"], &[]),
            "AdmitSourceManifestPageRequest": fields(&["page"], &[]),
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
            "CertifiedSource": fields(
                &["observation", "parser_revision", "content_digest", "counts"],
                &["frontier"]),
            "CertifiedSourceDeletion": fields(&[
                "source", "inventory", "discovery_revision", "inventory_digest", "observed_sources"
            ], &[]),
            "CertifiedSourceInventory": fields(&[
                "observation", "discovery_revision", "source_digests", "inventory_digest"
            ], &[]),
            "EntitlementGrant": fields(&[
                "schema_version", "issuer", "key_id", "grant_id", "subject", "account_id",
                "product", "access_kind", "installation_key_thumbprint", "issued_at_unix",
                "not_before_unix", "refresh_after_unix", "access_deadline_unix",
                "grace_deadline_unix", "expires_at_unix", "minimum_helper_protocol",
                "revocation_epoch", "capabilities"
            ], &[]),
            "EvidenceCitation": fields(&[], &[
                "observation_id", "observation_seq", "observation_kind", "session_id", "event_id",
                "event_seq", "source_locator", "source_path", "fixture_line", "source_record_ordinal",
                "source_record_subrecord_index", "byte_range", "source_sha256"
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
            "LineRange": fields(&["start", "end"], &[]),
            "NumberedEvidence": fields(&["number", "citation"], &[]),
            "FinishSourceManifestRequest": fields(&["manifest", "expected_progress"], &[]),
            "FinishSourceManifestAdmissionRequest": fields(&["header"], &[]),
            "FinishAdmittedSourceManifestRequest": fields(
                &["admission", "expected_progress"], &[]),
            "PrepareGraphKeyDeletionRequest": fields(&["installation_key_thumbprint"], &[]),
            "ProtocolError": fields(&["class", "message", "retryable"], &[]),
            "PullRequestActivity": fields(&[
                "fact_id", "action", "session", "confidence", "state", "evidence_numbers"
            ], &["direct_actor", "owning_root", "fact_occurred_at_ms"]),
            "PullRequestBlameMatch": fields(&["pull_request", "relationship"], &[]),
            "PullRequestCommit": fields(
                &["fact_id", "relationship", "commit", "production", "evidence_numbers"], &[]),
            "QuerySnapshotExpectation.source": fields(&["kind", "receipt"], &[]),
            "ResourceRef": fields(&["id", "kind", "display"], &[]),
            "ResolvedBlameTarget.commit": fields(&["kind", "commit", "repository"], &[]),
            "ResolvedBlameTarget.file": fields(
                &["kind", "path", "repository"], &["requested_lines"]),
            "ResolvedBlameTarget.pull_request": fields(
                &["kind", "selector", "pull_request", "repository"], &[]),
            "SignedEntitlement": fields(&["grant", "signature_base64url"], &[]),
            "ScannedSourceCounts": fields(&[
                "complete_records", "retained_records", "rejected_records", "ignored_records",
                "indexed_documents", "certified_bytes"
            ], &[]),
            "SourceCommandFact": fields(
                &["command"], &["call_id", "tool_name", "working_directory"]),
            "SourceDeleted": fields(&[
                "core_generation_id", "source", "removed_source_epoch", "replayed"
            ], &[]),
            "SourceFrontier": fields(&[
                "checkpoint_kind", "checkpoint", "certified_prefix_bytes",
                "certified_prefix_digest"
            ], &[]),
            "SourceInventoryObservation": fields(&[
                "provider", "authority_namespace", "authority_key", "revision_kind", "revision"
            ], &[]),
            "SourceKey": fields(&[
                "provider", "source_format", "schema_variant", "provider_identity_version",
                "anchor", "identity"
            ], &[]),
            "SourceManifest": fields(&[
                "contract_version", "core_generation_id", "sources", "removals"
            ], &[]),
            "SourceManifestHeader": fields(&[
                "contract_version", "core_generation_id", "generation_manifest_version",
                "identity_version", "lexical_schema_version", "lexical_analyzer_version",
                "policy_schema_hash", "source_count", "removal_count", "page_count",
                "aggregate_sha256"
            ], &[]),
            "SourceManifestPage.sources": fields(&[
                "contract_version", "core_generation_id", "aggregate_sha256",
                "previous_page_sha256", "page_index", "item_index", "kind", "entries",
                "page_sha256"
            ], &[]),
            "SourceManifestPage.removals": fields(&[
                "contract_version", "core_generation_id", "aggregate_sha256",
                "previous_page_sha256", "page_index", "item_index", "kind", "entries",
                "page_sha256"
            ], &[]),
            "SourceManifestAdmissionCursor": fields(&[
                "core_generation_id", "aggregate_sha256", "next_page_previous_sha256",
                "next_page_index", "next_source_index", "next_removal_index"
            ], &[]),
            "SourceManifestAdmissionReceipt": fields(
                &["header", "page_count", "terminal_chain_sha256"],
                &[],
            ),
            "SourceManifestBegan": fields(&[
                "core_generation_id", "materializer_revision", "progress", "replayed"
            ], &[]),
            "SourceManifestAdmissionBegan": fields(&["cursor", "replayed"], &[]),
            "SourceManifestPageAdmitted": fields(&["cursor", "replayed"], &[]),
            "SourceManifestAdmitted": fields(&[
                "receipt", "materializer_revision", "progress", "replayed"
            ], &[]),
            "SourceManifestFinished": fields(&["receipt", "replayed"], &[]),
            "SourceManifestReceipt": fields(&[
                "core_generation_id", "manifest_aggregate_sha256",
                "materializer_revision", "progress"
            ], &[]),
            "SourceManifestReceiptIdentity": fields(&[
                "core_generation_id", "materializer_revision", "receipt_sha256"
            ], &[]),
            "SourceMessageFact": fields(&["content"], &[]),
            "SourceObservation": fields(&["source", "revision_kind", "revision"], &[]),
            "SourcePageMaterialized": fields(&[
                "core_generation_id", "progress", "accepted_records", "materialized_facts",
                "replayed"
            ], &[]),
            "SourcePrepared": fields(&["core_generation_id", "progress", "replayed"], &[]),
            "SourceProgress": fields(
                &[
                    "source", "source_epoch", "certified_revision_sha256",
                    "materializer_revision", "terminal"
                ],
                &["frontier"]),
            "SourceRecord": fields(
                &["event_id", "session_id", "locator", "relationships", "metadata", "facts"],
                &["repository"]),
            "SourceRecordLocator": fields(
                &["locator_version", "source", "coordinate", "revision_policy", "record_digest"],
                &["certified_source_revision_digest"]),
            "SourceRecordMetadata": fields(
                &["event_sequence", "event_type", "touched_files"],
                &["occurred_at_unix_ms", "role", "workspace", "cwd"]),
            "SourceRemoval": fields(&["deletion", "inventory"], &[]),
            "SourceRepositoryContext": fields(
                &["repository_id"],
                &["checkout_id", "worktree_id", "object_format", "worktree_root"]),
            "SourceResultFact": fields(
                &["outcome", "content"], &["call_id", "exit_code", "duration_ms"]),
            "SourceSessionRelationships": fields(
                &["direct_session_id", "root_session_id"],
                &["parent_session_id", "provider_session_id", "agent_id"]),
            "StableEntityId": fields(&[
                "contract_version", "entity_kind", "digest", "source_digest",
                "source_descriptor_digest", "uuid"
            ], &[]),
            "StatusRequest": fields(&[], &[]),
            "StatusResult": fields(
                &["state", "authority"], &["source_receipt"]),
            "SourceWorktreeRootLocator": fields(&["absolute_path"], &[]),
            "PrepareSourceRequest": fields(
                &[
                    "core_generation_id", "source", "certified_revision_sha256",
                    "materializer_revision", "disposition"
                ],
                &["expected_prior"]),
            "MaterializeSourcePageRequest": fields(
                &["core_generation_id", "expected_prior", "terminal", "records"],
                &["next_frontier"]),
            "DeleteSourceRequest": fields(&[
                "core_generation_id", "removal", "expected_prior"
            ], &[]),
            "TransientSourceContent": {
                "wire_type": "canonical_base64_string",
                "debug": "redacted"
            },
            "TransientSourceFact": {
                "wire_type": "internally_tagged_kind_with_body",
                "variants": ["message", "command", "result"]
            }
        },
        "source_materialization": {
            "contract_version": SOURCE_MATERIALIZATION_CONTRACT_VERSION,
            "authority": "certified_public_source_manifest_and_provider_reread_are_the_sole_body_authority",
            "lifecycle": [
                "begin_source_manifest_admission", "admit_source_manifest_page",
                "finish_source_manifest_admission", "prepare_source", "materialize_source_page",
                "delete_source", "finish_admitted_source_manifest"
            ],
            "progress": "independent_per_source_epoch_certified_revision_and_frontier_compare_and_swap",
            "materializer_upgrade": "begin_may_return_prior_revision_progress_which_prepare_rewrite_invalidates",
            "finish": "requires_the_admitted_manifest_receipt_and_expected_terminal_progress",
            "deletion": "requires_certified_source_deletion_paired_with_its_complete_inventory_witness",
            "detector_input": "normalized_transient_message_command_and_result_facts_with_call_outcome_session_and_repository_context",
            "relationships": "root_and_parent_session_ids_may_reference_other_source_lineages",
            "durability": "transient_detector_content_is_request_only_and_not_retained_as_full_body_after_page_handling",
            "failure": "core_is_already_published_and_pro_remains_retryable_from_committed_progress"
        },
        "evidence_citation": {
            "branches": {
                "canonical_or_source_path": [
                    "observation_id", "observation_seq", "observation_kind", "session_id",
                    "event_id", "event_seq", "source_path", "fixture_line",
                    "source_record_ordinal", "source_record_subrecord_index", "byte_range",
                    "source_sha256"
                ],
                "exact_source_locator": [
                    "source_locator", "source_record_ordinal", "byte_range",
                    "source_sha256"
                ]
            },
            "selection": "canonical_or_source_path_coordinates_must_be_usable_or_source_locator_must_be_contract_valid_with_exact_record_digest_and_derived_jsonl_coordinates"
        },
        "representative_frames": {
            "host_status": frame_hex(&status),
            "helper_protocol_mismatch": frame_hex(&error)
        }
    })
}
