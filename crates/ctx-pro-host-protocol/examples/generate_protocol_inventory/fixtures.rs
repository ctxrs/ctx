use super::*;

fn inventory_initial_journal_digest(generation: u64, fingerprint: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctx-pro-journal-initial-v1\0");
    hash.update(generation.to_be_bytes());
    hash.update(fingerprint.as_bytes());
    hex(&hash.finalize())
}

pub(super) fn checkpoint(fingerprint: &str) -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 0,
        },
        contract_fingerprint: fingerprint.to_owned(),
        cumulative_digest: inventory_initial_journal_digest(1, fingerprint),
    }
}

pub(super) fn authorization() -> AuthorizationRequest {
    AuthorizationRequest {
        entitlement: SignedEntitlement {
            grant: EntitlementGrant {
                schema_version: ENTITLEMENT_SCHEMA_VERSION,
                issuer: "https://commercial.ctx.rs".to_owned(),
                key_id: "fixture-v1".to_owned(),
                grant_id: "grant-1".to_owned(),
                subject: "user-1".to_owned(),
                account_id: "account-1".to_owned(),
                product: "ctx-local-pro".to_owned(),
                access_kind: EntitlementAccessKind::Trial,
                installation_key_thumbprint: base64url(&[1; 32]),
                issued_at_unix: 100,
                not_before_unix: 90,
                refresh_after_unix: 150,
                access_deadline_unix: 200,
                grace_deadline_unix: 250,
                expires_at_unix: 175,
                minimum_helper_protocol: PROTOCOL_VERSION,
                revocation_epoch: 0,
                capabilities: BTreeSet::from([
                    EntitlementCapability::GraphRead,
                    EntitlementCapability::GraphWrite,
                ]),
            },
            signature_base64url: base64url(&[2; ED25519_SIGNATURE_BYTES]),
        },
        installation_public_key_base64url: base64url(&[3; INSTALLATION_PUBLIC_KEY_BYTES]),
        challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        proof_signature_base64url: base64url(&[5; ED25519_SIGNATURE_BYTES]),
    }
}

pub(super) fn journal_request(roots: Vec<String>, fingerprint: &str) -> JournalSyncRequest {
    let checkpoint = checkpoint(fingerprint);
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: checkpoint.clone(),
        context: JournalContextWindow {
            base_checkpoint: checkpoint.clone(),
            records: Vec::new(),
        },
        frozen_through: checkpoint,
        authorized_repository_roots: roots,
        records: Vec::new(),
    }
}

fn canonical_observation_payload(
    entity_kind: JournalEntityKind,
    stable_entity_id: Uuid,
    sequence: u64,
) -> Value {
    let (observation_kind, event_type, typed_event, file_touch, vcs_change) = match entity_kind {
        JournalEntityKind::Event => (
            "event",
            "fixture_event",
            Value::Null,
            Value::Null,
            Value::Null,
        ),
        JournalEntityKind::FileTouch => (
            "file_touch",
            "file_touched",
            json!("file_touched"),
            json!({
                "id": stable_entity_id,
                "history_record_id": null,
                "run_id": null,
                "event_id": null,
                "vcs_workspace_id": null,
                "path": "src/lib.rs",
                "change_kind": "modified",
                "old_path": null,
                "line_count_delta": 1,
                "confidence": "explicit",
                "source_id": null
            }),
            Value::Null,
        ),
        JournalEntityKind::VcsChange => (
            "vcs_change",
            "vcs_change",
            json!("vcs_change"),
            Value::Null,
            json!({
                "id": stable_entity_id,
                "vcs_workspace_id": Uuid::from_u128(900 + u128::from(sequence)),
                "kind": "git_commit",
                "change_id": "0123456789abcdef0123456789abcdef01234567",
                "parent_change_ids": [],
                "branch_or_bookmark": "main",
                "tree_hash": "89abcdef0123456789abcdef0123456789abcdef",
                "author_time_ms": 1_753_232_400_000_i64,
                "confidence": "explicit",
                "source_id": null
            }),
        ),
    };
    let event_id = matches!(entity_kind, JournalEntityKind::Event).then_some(stable_entity_id);
    json!({
        "observation_id": stable_entity_id,
        "observation_seq": sequence,
        "observation_kind": observation_kind,
        "event_id": event_id,
        "event_seq": event_id.map(|_| sequence),
        "occurred_at_ms": 1_753_232_400_000_i64 + sequence as i64,
        "history_record_id": null,
        "event_type": event_type,
        "role": null,
        "payload": {},
        "metadata": {},
        "result": {
            "outcome": "unknown",
            "identifiers": []
        },
        "actor": null,
        "run": null,
        "source": null,
        "typed_event": typed_event,
        "file_touch": file_touch,
        "vcs_change": vcs_change,
        "citation": {
            "observation_id": stable_entity_id,
            "observation_seq": sequence,
            "observation_kind": observation_kind,
            "event_id": event_id,
            "event_seq": event_id.map(|_| sequence),
            "source_path": "fixture/session.jsonl",
            "fixture_line": sequence,
            "source_record_ordinal": sequence - 1,
            "source_record_subrecord_index": 0,
            "byte_range": {
                "start": sequence * 100,
                "end_exclusive": sequence * 100 + 80
            },
            "source_sha256": null
        },
        "semantic_digest": format!("{:064x}", 10_000 + sequence)
    })
}

fn journal_record(
    generation: u64,
    sequence: u64,
    entity_kind: JournalEntityKind,
    operation: JournalOperation,
    prior_digest: &str,
) -> JournalRecord {
    let stable_entity_id = Uuid::from_u128(100 + u128::from(sequence));
    let canonical_payload = match operation {
        JournalOperation::Upsert => Some(canonical_observation_payload(
            entity_kind,
            stable_entity_id,
            sequence,
        )),
        JournalOperation::Delete => None,
    };
    let payload_bytes = canonical_payload
        .as_ref()
        .map(canonical_payload_bytes)
        .transpose()
        .unwrap_or_else(|error| panic!("canonical fixture payload: {error:?}"))
        .unwrap_or_default();
    let mut record = JournalRecord {
        generation,
        sequence,
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        entity_kind,
        stable_entity_id,
        entity_revision: if matches!(operation, JournalOperation::Delete) {
            2
        } else {
            1
        },
        operation,
        canonical_payload,
        payload_sha256: sha256_hex(&payload_bytes),
        evidence: vec![JournalEvidenceIdentity {
            event_id: Uuid::from_u128(200 + u128::from(sequence)),
            source_id: Some(Uuid::from_u128(300 + u128::from(sequence))),
            source_path: Some("fixture/session.jsonl".to_owned()),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(sequence * 10),
            byte_end_exclusive: Some(sequence * 10 + 9),
        }],
        provenance: JournalProvenanceIdentity {
            entity_kind,
            stable_entity_id,
            capture_source_id: Some(Uuid::from_u128(300 + u128::from(sequence))),
            provider: Some("codex".to_owned()),
            provider_external_id: Some(format!("fixture-{sequence}")),
        },
        cumulative_digest: "0".repeat(64),
    };
    record.cumulative_digest = journal_record_digest(prior_digest, &record)
        .unwrap_or_else(|error| panic!("journal fixture digest: {error:?}"));
    record
}

pub(super) fn journal_operation_requests(fingerprint: &str) -> [JournalSyncRequest; 2] {
    let generation = 2;
    let initial = inventory_initial_journal_digest(generation, fingerprint);
    let event = journal_record(
        generation,
        1,
        JournalEntityKind::Event,
        JournalOperation::Upsert,
        &initial,
    );
    let file_touch = journal_record(
        generation,
        2,
        JournalEntityKind::FileTouch,
        JournalOperation::Upsert,
        &event.cumulative_digest,
    );
    let vcs_change = journal_record(
        generation,
        3,
        JournalEntityKind::VcsChange,
        JournalOperation::Upsert,
        &file_touch.cumulative_digest,
    );
    let full_checkpoint = JournalCheckpoint {
        position: JournalPosition {
            generation,
            sequence: 3,
        },
        contract_fingerprint: fingerprint.to_owned(),
        cumulative_digest: vcs_change.cumulative_digest.clone(),
    };
    let context_base = JournalCheckpoint {
        position: JournalPosition {
            generation,
            sequence: 0,
        },
        contract_fingerprint: fingerprint.to_owned(),
        cumulative_digest: initial.clone(),
    };
    let context_records = vec![event.clone(), file_touch.clone(), vcs_change.clone()];
    let full = JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 0,
            },
            contract_fingerprint: fingerprint.to_owned(),
            cumulative_digest: initial,
        },
        context: JournalContextWindow {
            base_checkpoint: context_base.clone(),
            records: Vec::new(),
        },
        frozen_through: full_checkpoint.clone(),
        authorized_repository_roots: vec!["/workspace/ctx".to_owned()],
        records: vec![event, file_touch, vcs_change],
    };
    let delete = journal_record(
        generation,
        4,
        JournalEntityKind::Event,
        JournalOperation::Delete,
        &full_checkpoint.cumulative_digest,
    );
    let incremental = JournalSyncRequest {
        mode: JournalSyncMode::Incremental,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: fingerprint.to_owned(),
        prior_checkpoint: full_checkpoint,
        context: JournalContextWindow {
            base_checkpoint: context_base,
            records: context_records,
        },
        frozen_through: JournalCheckpoint {
            position: JournalPosition {
                generation,
                sequence: 4,
            },
            contract_fingerprint: fingerprint.to_owned(),
            cumulative_digest: delete.cumulative_digest.clone(),
        },
        authorized_repository_roots: vec!["/workspace/ctx".to_owned()],
        records: vec![delete],
    };
    [full, incremental]
}

pub(super) fn blame_request(
    target: BlameTarget,
    cursor: Option<String>,
    fingerprint: &str,
) -> BlameRequest {
    BlameRequest {
        target,
        limit: 10,
        cursor,
        expected_snapshot: QuerySnapshotExpectation {
            checkpoint: checkpoint(fingerprint),
            projection_pending: false,
        },
    }
}

pub(super) fn blame(cursor: Option<String>, fingerprint: &str) -> BlameRequest {
    blame_request(
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        cursor,
        fingerprint,
    )
}

pub(super) fn output_source() -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: "codex".to_owned(),
        namespace_id: "codex-session-jsonl".to_owned(),
        source_id: "fixture/session.jsonl".to_owned(),
    }
}

pub(super) fn output_cursor() -> OutputNativeCursor {
    OutputNativeCursor {
        version: 1,
        payload_base64: "Y3Vyc29yLTE=".to_owned(),
    }
}

pub(super) fn output_page() -> ProOutputMaterializationPage {
    ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 1,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-1".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: output_cursor(),
        terminal: true,
        observations: vec![ProOutputObservation {
            kind: OutputObservationKind::Command,
            coordinate: OutputNativeCoordinate {
                unit_key: "output-1".to_owned(),
                native_sequence: 1,
                native_record_id: Some("record-1".to_owned()),
                source_record_ordinal: Some(0),
                source_record_subrecord_index: Some(0),
                byte_start: Some(0),
                byte_end_exclusive: Some(15),
            },
            occurred_at_unix_ms: Some(1_753_232_400_000),
            associations: OutputAssociations {
                direct_session_id: "session-1".to_owned(),
                root_session_id: "session-1".to_owned(),
                parent_session_id: None,
                provider_session_id: Some("provider-session-1".to_owned()),
                agent_id: Some("agent-1".to_owned()),
                repository: None,
            },
            call_id: Some("call-1".to_owned()),
            command: Some(OutputCommandContext {
                tool_name: "exec_command".to_owned(),
                command: "cargo check".to_owned(),
                working_directory: Some("/workspace/ctx".to_owned()),
            }),
            outcome: OutputOutcomeMetadata {
                outcome: OutputOutcome::Success,
                exit_code: Some(0),
                duration_ms: Some(42),
            },
            locator: OutputSourceLocator {
                version: 1,
                kind: "jsonl-byte-range".to_owned(),
                payload_base64: "bG9jYXRvci0x".to_owned(),
            },
            content: TransientOutputContent::from_bytes(b"complete output")
                .unwrap_or_else(|| panic!("small output fixture")),
        }],
    }
}

fn structured_pr_output_coordinate() -> OutputNativeCoordinate {
    OutputNativeCoordinate {
        unit_key: "structured-pr-membership-42".to_owned(),
        native_sequence: 42,
        native_record_id: Some("command-result-42".to_owned()),
        source_record_ordinal: Some(41),
        source_record_subrecord_index: Some(0),
        byte_start: Some(1_024),
        byte_end_exclusive: Some(1_280),
    }
}

fn structured_pr_output_locator() -> OutputSourceLocator {
    OutputSourceLocator {
        version: 1,
        kind: "jsonl-byte-range".to_owned(),
        payload_base64: "c3RydWN0dXJlZC1wci00Mg==".to_owned(),
    }
}

fn output_observation(
    kind: OutputObservationKind,
    sequence: u64,
    outcome: OutputOutcome,
    content: &[u8],
) -> ProOutputObservation {
    ProOutputObservation {
        kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("output-operation-{sequence}"),
            native_sequence: sequence,
            native_record_id: Some(format!("record-{sequence}")),
            source_record_ordinal: Some(sequence),
            source_record_subrecord_index: Some(0),
            byte_start: Some(sequence * 100),
            byte_end_exclusive: Some(sequence * 100 + content.len() as u64),
        },
        occurred_at_unix_ms: Some(1_753_232_400_000 + sequence as i64),
        associations: OutputAssociations {
            direct_session_id: "session-1".to_owned(),
            root_session_id: "session-1".to_owned(),
            parent_session_id: None,
            provider_session_id: Some("provider-session-1".to_owned()),
            agent_id: Some("agent-1".to_owned()),
            repository: Some(OutputRepositoryContext {
                repository_id: "repository:ctxrs/ctx".to_owned(),
                checkout_id: Some("checkout:ctxrs/ctx".to_owned()),
                worktree_id: Some("worktree:ctxrs/ctx".to_owned()),
                object_format: Some("sha1".to_owned()),
            }),
        },
        call_id: Some(format!("call-{sequence}")),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: match outcome {
                OutputOutcome::Success => Some(0),
                OutputOutcome::Failure => Some(1),
                OutputOutcome::Timeout | OutputOutcome::Unknown => None,
            },
            duration_ms: Some(40 + sequence),
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "jsonl-byte-range".to_owned(),
            payload_base64: base64::engine::general_purpose::STANDARD
                .encode(format!("locator-{sequence}")),
        },
        content: TransientOutputContent::from_bytes(content)
            .unwrap_or_else(|| panic!("small operation output fixture")),
    }
}

fn structured_pr_output_observation() -> ProOutputObservation {
    let mut observation = output_observation(
        OutputObservationKind::Command,
        42,
        OutputOutcome::Success,
        br#"{"url":"https://github.com/ctxrs/ctx/pull/42","commits":[{"oid":"0123456789abcdef0123456789abcdef01234567"}],"mergeCommit":{"oid":"89abcdef0123456789abcdef0123456789abcdef"}}"#,
    );
    observation.coordinate = structured_pr_output_coordinate();
    observation.locator = structured_pr_output_locator();
    observation.command = Some(OutputCommandContext {
        tool_name: "gh".to_owned(),
        command: "gh pr view 42 --repo ctxrs/ctx --json url,commits,mergeCommit".to_owned(),
        working_directory: Some("/workspace/ctx".to_owned()),
    });
    observation
}

pub(super) fn output_operation_pages() -> [ProOutputMaterializationPage; 3] {
    let new_source = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-structured-pr".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::NewSource,
        expected_prior_source_epoch: None,
        expected_prior_cursor: None,
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "bmV3LXNvdXJjZS1jdXJzb3I=".to_owned(),
        },
        terminal: false,
        observations: vec![structured_pr_output_observation()],
    };
    let append_or_resume = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 0,
        observed_revision: "revision-structured-pr".to_owned(),
        parser_revision: "parser-1".to_owned(),
        materializer_revision: "materializer-1".to_owned(),
        disposition: OutputSourceDisposition::AppendOrResume,
        expected_prior_source_epoch: Some(0),
        expected_prior_cursor: Some(OutputNativeCursor {
            version: 1,
            payload_base64: "bmV3LXNvdXJjZS1jdXJzb3I=".to_owned(),
        }),
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "cmVzdW1lZC1jdXJzb3I=".to_owned(),
        },
        terminal: false,
        observations: vec![output_observation(
            OutputObservationKind::Tool,
            43,
            OutputOutcome::Failure,
            b"structured forge query failed",
        )],
    };
    let rewrite = ProOutputMaterializationPage {
        contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
        inventory_generation: 2,
        source: output_source(),
        source_epoch: 1,
        observed_revision: "revision-rewritten".to_owned(),
        parser_revision: "parser-2".to_owned(),
        materializer_revision: "materializer-2".to_owned(),
        disposition: OutputSourceDisposition::Rewrite,
        expected_prior_source_epoch: Some(0),
        expected_prior_cursor: Some(OutputNativeCursor {
            version: 1,
            payload_base64: "cmVzdW1lZC1jdXJzb3I=".to_owned(),
        }),
        next_safe_cursor: OutputNativeCursor {
            version: 1,
            payload_base64: "cmV3cml0dGVuLWN1cnNvcg==".to_owned(),
        },
        terminal: true,
        observations: vec![
            output_observation(
                OutputObservationKind::Command,
                44,
                OutputOutcome::Timeout,
                b"structured forge query timed out",
            ),
            output_observation(
                OutputObservationKind::Tool,
                45,
                OutputOutcome::Unknown,
                b"structured forge query outcome unavailable",
            ),
        ],
    };
    [new_source, append_or_resume, rewrite]
}

fn provider_output_citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: None,
        observation_seq: None,
        observation_kind: None,
        session_id: None,
        event_id: None,
        event_seq: None,
        source_path: None,
        fixture_line: None,
        source_record_ordinal: None,
        source_record_subrecord_index: None,
        byte_range: None,
        source_sha256: None,
        provider_output: Some(ProviderOutputEvidence {
            source_id: "fixture/session.jsonl".to_owned(),
            source_epoch: 0,
            locator: OutputSourceLocator {
                version: 1,
                kind: "jsonl-byte-range".to_owned(),
                payload_base64: "bG9jYXRvci0x".to_owned(),
            },
            coordinate: OutputNativeCoordinate {
                unit_key: "output-1".to_owned(),
                native_sequence: 1,
                native_record_id: Some("record-1".to_owned()),
                source_record_ordinal: Some(0),
                source_record_subrecord_index: Some(0),
                byte_start: Some(0),
                byte_end_exclusive: Some(15),
            },
            availability: OutputSourceAvailability::Available,
        }),
    }
}

pub(super) fn structured_pr_provider_output_citation() -> EvidenceCitation {
    EvidenceCitation {
        observation_id: None,
        observation_seq: None,
        observation_kind: None,
        session_id: None,
        event_id: None,
        event_seq: None,
        source_path: None,
        fixture_line: None,
        source_record_ordinal: None,
        source_record_subrecord_index: None,
        byte_range: None,
        source_sha256: None,
        provider_output: Some(ProviderOutputEvidence {
            source_id: output_source().source_id,
            source_epoch: 0,
            locator: structured_pr_output_locator(),
            coordinate: structured_pr_output_coordinate(),
            availability: OutputSourceAvailability::Available,
        }),
    }
}

pub(super) fn provider_output_blame_result() -> BlameResult {
    BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: ResourceRef {
                id: "commit:fixture-1".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            repository: ResourceRef {
                id: "repository:fixture-1".to_owned(),
                kind: ResourceKind::Repository,
                display: "ctxrs/ctx".to_owned(),
            },
        },
        git_snapshot: None,
        matches: vec![BlameMatch::Commit(CommitBlameMatch {
            fact_id: "fact:fixture-1".to_owned(),
            fact_type: CommitFactType::Referenced,
            predicate: CommitPredicate::ReferencedBy,
            subject: ResourceRef {
                id: "commit:fixture-1".to_owned(),
                kind: ResourceKind::Commit,
                display: "0123456789abcdef".to_owned(),
            },
            object: Some(ResourceRef {
                id: "session:fixture-1".to_owned(),
                kind: ResourceKind::Session,
                display: "session-1".to_owned(),
            }),
            fact_occurred_at_ms: Some(1_753_232_400_000),
            confidence: FactConfidence::Explicit,
            state: FactState::Asserted,
            direct_actor: None,
            owning_root: None,
            evidence_numbers: vec![1],
        })],
        evidence: vec![NumberedEvidence {
            number: 1,
            citation: provider_output_citation(),
        }],
        next: None,
    }
}
