use std::{collections::BTreeSet, io::Cursor};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceObservation,
    SourceRecordLocator, TypedKey,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;

const INVENTORY: &str = include_str!("../testdata/v1/inventory.json");

fn inventory() -> Value {
    serde_json::from_str(INVENTORY).expect("Protocol V1 inventory must be valid JSON")
}

fn inventory_enum(value: &Value, name: &str) -> BTreeSet<String> {
    value["canonical_inventory"]["enums"][name]
        .as_array()
        .unwrap_or_else(|| panic!("{name} enum inventory"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("{name} enum item"))
                .to_owned()
        })
        .collect()
}

fn insert_wire<T: serde::Serialize>(values: &mut BTreeSet<String>, value: T) {
    let encoded = serde_json::to_value(value).expect("wire enum serialization");
    values.insert(
        encoded
            .as_str()
            .expect("wire enum must serialize as a string")
            .to_owned(),
    );
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => panic!("invalid golden hex"),
            };
            (nibble(pair[0]) << 4) | nibble(pair[1])
        })
        .collect()
}

fn host_kind(message: &HostMessage) -> &'static str {
    match message {
        HostMessage::Hello(_) => "hello",
        HostMessage::Authorize(_) => "authorize",
        HostMessage::PrepareGraphKeyDeletion(_) => "prepare_graph_key_deletion",
        HostMessage::ConfirmGraphKeyDeletion(_) => "confirm_graph_key_deletion",
        HostMessage::Status(_) => "status",
        HostMessage::SyncJournal(_) => "sync_journal",
        HostMessage::BeginOutputInventory(_) => "begin_output_inventory",
        HostMessage::ObserveOutputSource(_) => "observe_output_source",
        HostMessage::MaterializeOutputPage(_) => "materialize_output_page",
        HostMessage::FinishOutputInventory(_) => "finish_output_inventory",
        HostMessage::GetOutputProgress(_) => "get_output_progress",
        HostMessage::BeginSourceManifest(_) => "begin_source_manifest",
        HostMessage::BeginSourceManifestAdmission(_) => "begin_source_manifest_admission",
        HostMessage::AdmitSourceManifestPage(_) => "admit_source_manifest_page",
        HostMessage::FinishSourceManifestAdmission(_) => "finish_source_manifest_admission",
        HostMessage::PrepareSource(_) => "prepare_source",
        HostMessage::MaterializeSourcePage(_) => "materialize_source_page",
        HostMessage::DeleteSource(_) => "delete_source",
        HostMessage::FinishSourceManifest(_) => "finish_source_manifest",
        HostMessage::FinishAdmittedSourceManifest(_) => "finish_admitted_source_manifest",
        HostMessage::Blame(_) => "blame",
    }
}

fn helper_kind(message: &HelperMessage) -> &'static str {
    match message {
        HelperMessage::Hello(_) => "hello",
        HelperMessage::Authorized(_) => "authorized",
        HelperMessage::GraphKeyDeletionPrepared(_) => "graph_key_deletion_prepared",
        HelperMessage::GraphKeyDeleted(_) => "graph_key_deleted",
        HelperMessage::Status(_) => "status",
        HelperMessage::JournalSynced(_) => "journal_synced",
        HelperMessage::OutputInventoryBegan(_) => "output_inventory_began",
        HelperMessage::OutputSourceObserved(_) => "output_source_observed",
        HelperMessage::OutputPageMaterialized(_) => "output_page_materialized",
        HelperMessage::OutputInventoryFinished(_) => "output_inventory_finished",
        HelperMessage::OutputProgress(_) => "output_progress",
        HelperMessage::SourceManifestBegan(_) => "source_manifest_began",
        HelperMessage::SourceManifestAdmissionBegan(_) => "source_manifest_admission_began",
        HelperMessage::SourceManifestPageAdmitted(_) => "source_manifest_page_admitted",
        HelperMessage::SourceManifestAdmitted(_) => "source_manifest_admitted",
        HelperMessage::SourcePrepared(_) => "source_prepared",
        HelperMessage::SourcePageMaterialized(_) => "source_page_materialized",
        HelperMessage::SourceDeleted(_) => "source_deleted",
        HelperMessage::SourceManifestFinished(_) => "source_manifest_finished",
        HelperMessage::Blame(_) => "blame",
        HelperMessage::Error(_) => "error",
    }
}

fn host_operation_kind(message: &HostMessage) -> Option<&'static str> {
    match message {
        HostMessage::Authorize(request) => Some(match request.entitlement.grant.access_kind {
            EntitlementAccessKind::Trial => "authorize_trial",
            EntitlementAccessKind::Active => "authorize_active",
            EntitlementAccessKind::CancelingPaid => "authorize_canceling_paid",
        }),
        HostMessage::SyncJournal(request) => Some(match request.mode {
            JournalSyncMode::FullBaseline => {
                assert!(request
                    .records
                    .iter()
                    .all(|record| record.operation == JournalOperation::Upsert));
                assert_eq!(request.records.len(), 3);
                for entity_kind in [
                    JournalEntityKind::Event,
                    JournalEntityKind::FileTouch,
                    JournalEntityKind::VcsChange,
                ] {
                    assert!(request
                        .records
                        .iter()
                        .any(|record| record.entity_kind == entity_kind));
                }
                "sync_journal_full_baseline_upsert"
            }
            JournalSyncMode::Incremental => {
                assert!(request
                    .records
                    .iter()
                    .all(|record| record.operation == JournalOperation::Delete));
                "sync_journal_incremental_delete"
            }
        }),
        HostMessage::ObserveOutputSource(request) => Some(match request.availability {
            OutputSourceAvailability::Available => "observe_output_source_available",
            OutputSourceAvailability::Unavailable => "observe_output_source_unavailable",
            OutputSourceAvailability::Error => "observe_output_source_error",
        }),
        HostMessage::MaterializeOutputPage(page) => Some(match &page.disposition {
            OutputSourceDisposition::NewSource => {
                assert!(matches!(
                    page.observations.as_slice(),
                    [ProOutputObservation {
                        kind: OutputObservationKind::Command,
                        outcome: OutputOutcomeMetadata {
                            outcome: OutputOutcome::Success,
                            ..
                        },
                        ..
                    }]
                ));
                "materialize_output_page_new_source_command_success"
            }
            OutputSourceDisposition::AppendOrResume => {
                assert!(matches!(
                    page.observations.as_slice(),
                    [ProOutputObservation {
                        kind: OutputObservationKind::Tool,
                        outcome: OutputOutcomeMetadata {
                            outcome: OutputOutcome::Failure,
                            ..
                        },
                        ..
                    }]
                ));
                "materialize_output_page_append_or_resume_tool_failure"
            }
            OutputSourceDisposition::Rewrite => {
                assert!(matches!(
                    page.observations.as_slice(),
                    [
                        ProOutputObservation {
                            kind: OutputObservationKind::Command,
                            outcome: OutputOutcomeMetadata {
                                outcome: OutputOutcome::Timeout,
                                ..
                            },
                            ..
                        },
                        ProOutputObservation {
                            kind: OutputObservationKind::Tool,
                            outcome: OutputOutcomeMetadata {
                                outcome: OutputOutcome::Unknown,
                                ..
                            },
                            ..
                        }
                    ]
                ));
                "materialize_output_page_rewrite_command_timeout_and_tool_unknown"
            }
        }),
        HostMessage::Blame(request) => Some(match &request.target {
            BlameTarget::File { lines: None, .. } => "blame_file",
            BlameTarget::File {
                lines: Some(LineRange { start, end }),
                ..
            } if start == end => "blame_file_line",
            BlameTarget::File { lines: Some(_), .. } => "blame_file_range",
            BlameTarget::Commit { .. } => "blame_commit",
            BlameTarget::PullRequest { selector, .. } if selector.starts_with("https://") => {
                "blame_pull_request_url"
            }
            BlameTarget::PullRequest { .. } => "blame_pull_request_number",
        }),
        HostMessage::Hello(_)
        | HostMessage::PrepareGraphKeyDeletion(_)
        | HostMessage::ConfirmGraphKeyDeletion(_)
        | HostMessage::Status(_)
        | HostMessage::BeginOutputInventory(_)
        | HostMessage::FinishOutputInventory(_)
        | HostMessage::GetOutputProgress(_)
        | HostMessage::BeginSourceManifest(_)
        | HostMessage::BeginSourceManifestAdmission(_)
        | HostMessage::AdmitSourceManifestPage(_)
        | HostMessage::FinishSourceManifestAdmission(_)
        | HostMessage::PrepareSource(_)
        | HostMessage::MaterializeSourcePage(_)
        | HostMessage::DeleteSource(_)
        | HostMessage::FinishSourceManifest(_)
        | HostMessage::FinishAdmittedSourceManifest(_) => None,
    }
}

fn helper_operation_kind(message: &HelperMessage) -> Option<&'static str> {
    match message {
        HelperMessage::Authorized(result) => Some(match result.state {
            EntitlementAccessState::Trial => "authorized_trial",
            EntitlementAccessState::Active => "authorized_active",
            EntitlementAccessState::CancelingPaid => "authorized_canceling_paid",
            EntitlementAccessState::OfflineGrace => "authorized_offline_grace",
            EntitlementAccessState::Locked => "authorized_locked",
        }),
        HelperMessage::Status(result) => Some(match result.state {
            GraphState::NotMaterialized => "status_not_materialized",
            GraphState::NeedsRebuild => "status_needs_rebuild",
            GraphState::Partial => "status_partial",
            GraphState::NeedsResume => "status_needs_resume",
            GraphState::Ready => "status_ready",
        }),
        HelperMessage::OutputSourceObserved(result) => Some(match result.availability {
            OutputSourceAvailability::Available => "output_source_observed_available",
            OutputSourceAvailability::Unavailable => "output_source_observed_unavailable",
            OutputSourceAvailability::Error => "output_source_observed_error",
        }),
        HelperMessage::Blame(result) => Some(match &result.target {
            ResolvedBlameTarget::File {
                requested_lines: None,
                ..
            } => "blame_file",
            ResolvedBlameTarget::File {
                requested_lines: Some(LineRange { start, end }),
                ..
            } if start == end => "blame_file_line",
            ResolvedBlameTarget::File {
                requested_lines: Some(_),
                ..
            } => "blame_file_range",
            ResolvedBlameTarget::Commit { .. } => "blame_commit",
            ResolvedBlameTarget::PullRequest { .. }
                if result.matches.iter().all(|blame_match| {
                    matches!(
                        blame_match,
                        BlameMatch::PullRequest(PullRequestBlameMatch {
                            relationship: PullRequestBlameRelationship::Activity(_),
                            ..
                        })
                    )
                }) =>
            {
                "blame_pull_request_activity_without_commit_membership"
            }
            ResolvedBlameTarget::PullRequest { .. } => {
                assert!(result.matches.iter().all(|blame_match| {
                    matches!(
                        blame_match,
                        BlameMatch::PullRequest(PullRequestBlameMatch {
                            relationship: PullRequestBlameRelationship::Commit(_),
                            ..
                        })
                    )
                }));
                "blame_pull_request_commit_membership"
            }
        }),
        HelperMessage::Hello(_)
        | HelperMessage::GraphKeyDeletionPrepared(_)
        | HelperMessage::GraphKeyDeleted(_)
        | HelperMessage::JournalSynced(_)
        | HelperMessage::OutputInventoryBegan(_)
        | HelperMessage::OutputPageMaterialized(_)
        | HelperMessage::OutputInventoryFinished(_)
        | HelperMessage::OutputProgress(_)
        | HelperMessage::SourceManifestBegan(_)
        | HelperMessage::SourceManifestAdmissionBegan(_)
        | HelperMessage::SourceManifestPageAdmitted(_)
        | HelperMessage::SourceManifestAdmitted(_)
        | HelperMessage::SourcePrepared(_)
        | HelperMessage::SourcePageMaterialized(_)
        | HelperMessage::SourceDeleted(_)
        | HelperMessage::SourceManifestFinished(_)
        | HelperMessage::Error(_) => None,
    }
}

fn checkpoint() -> JournalCheckpoint {
    JournalCheckpoint {
        position: JournalPosition {
            generation: 1,
            sequence: 0,
        },
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        cumulative_digest: initial_journal_digest(1),
    }
}

fn authorization() -> AuthorizationRequest {
    let capabilities = BTreeSet::from([
        EntitlementCapability::GraphRead,
        EntitlementCapability::GraphWrite,
    ]);
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
                capabilities,
            },
            signature_base64url: base64url(&[2; ED25519_SIGNATURE_BYTES]),
        },
        installation_public_key_base64url: base64url(&[3; INSTALLATION_PUBLIC_KEY_BYTES]),
        challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        proof_signature_base64url: base64url(&[5; ED25519_SIGNATURE_BYTES]),
    }
}

fn blame() -> BlameRequest {
    BlameRequest {
        target: BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: Some("ctxrs/ctx".to_owned()),
        },
        limit: 10,
        cursor: None,
        expected_snapshot: QuerySnapshotExpectation::Journal {
            checkpoint: checkpoint(),
            projection_pending: false,
        },
    }
}

fn journal_request() -> JournalSyncRequest {
    let checkpoint = checkpoint();
    JournalSyncRequest {
        mode: JournalSyncMode::FullBaseline,
        canonical_schema_version: 47,
        canonical_schema_identity: "ctx-store-schema-47-final-v3".to_owned(),
        projection_contract_version: PROJECTION_CONTRACT_VERSION,
        contract_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
        prior_checkpoint: checkpoint.clone(),
        context: JournalContextWindow {
            base_checkpoint: checkpoint.clone(),
            records: Vec::new(),
        },
        frozen_through: checkpoint,
        authorized_repository_roots: Vec::new(),
        records: Vec::new(),
    }
}

fn output_source() -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: "codex".to_owned(),
        namespace_id: "codex-session-jsonl".to_owned(),
        source_id: "fixture/session.jsonl".to_owned(),
    }
}

fn output_cursor() -> OutputNativeCursor {
    OutputNativeCursor {
        version: 1,
        payload_base64: "Y3Vyc29yLTE=".to_owned(),
    }
}

fn output_page() -> ProOutputMaterializationPage {
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
        observations: Vec::new(),
    }
}

fn certified_source() -> CertifiedSource {
    let source = ctx_history_core::SourceKey::derive(
        "conformance",
        "conformance_jsonl",
        "conformance-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .unwrap();
    let observation = SourceObservation::new(source, "conformance-revision-v1", vec![7]).unwrap();
    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 10,
        ..ScannedSourceCounts::default()
    };
    let frontier =
        SourceFrontier::new("conformance-frontier-v1", TypedKey::U64(1), 10, [9; 32]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "conformance-parser-v1",
        [9; 32],
        counts,
        Some(frontier),
    )
    .unwrap()
}

fn source_manifest() -> SourceManifest {
    SourceManifest::new("a".repeat(64), vec![certified_source()], Vec::new()).unwrap()
}

fn source_manifest_header() -> SourceManifestHeader {
    let manifest = source_manifest();
    SourceManifestHeader::new(
        manifest.core_generation_id,
        1,
        1,
        1,
        1,
        "b".repeat(64),
        &manifest.sources,
        &manifest.removals,
    )
    .unwrap()
}

fn source_manifest_page() -> SourceManifestPage {
    SourceManifestPage::new(
        &source_manifest_header(),
        0,
        0,
        SourceManifestPageEntries::Sources(vec![certified_source()]),
    )
    .unwrap()
}

fn source_manifest_admission_receipt() -> SourceManifestAdmissionReceipt {
    SourceManifestAdmissionReceipt {
        header: source_manifest_header(),
        page_count: 1,
    }
}

fn source_progress(terminal: bool) -> SourceProgress {
    let source = certified_source();
    SourceProgress {
        source: source.observation().source().clone(),
        source_epoch: 1,
        certified_revision_sha256: certified_source_revision_sha256(&source).unwrap(),
        frontier: terminal.then(|| source.frontier().unwrap().clone()),
        materializer_revision: "conformance-source-materializer-v1".to_owned(),
        terminal,
    }
}

fn source_record() -> SourceRecord {
    let source = certified_source().observation().source().clone();
    let session_key =
        NativeSessionKey::native_id("conformance-session", TypedKey::utf8("session-1").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "conformance-session",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key = NativeItemKey::native_id("conformance-event", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "conformance-event",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::ProviderNative {
            namespace: "conformance-record".to_owned(),
            coordinate: TypedKey::U64(1),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some([8; 32]),
        [6; 32],
    )
    .unwrap();
    SourceRecord::new(
        event_id,
        session_id,
        locator,
        SourceSessionRelationships {
            direct_session_id: session_id,
            root_session_id: session_id,
            parent_session_id: None,
            provider_session_id: Some("provider-session-1".to_owned()),
            agent_id: Some("agent-1".to_owned()),
        },
        Some(SourceRepositoryContext {
            repository_id: "repository-1".to_owned(),
            checkout_id: Some("checkout-1".to_owned()),
            worktree_id: Some("worktree-1".to_owned()),
            object_format: Some("sha1".to_owned()),
        }),
        SourceRecordMetadata {
            event_sequence: 1,
            occurred_at_unix_ms: Some(1_753_232_400_000),
            event_type: "assistant_message".to_owned(),
            role: Some("assistant".to_owned()),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace/ctx".to_owned()),
            touched_files: vec!["src/lib.rs".to_owned()],
        },
        vec![
            TransientSourceFact::Message(SourceMessageFact {
                content: TransientSourceContent::from_bytes(b"conformance message").unwrap(),
            }),
            TransientSourceFact::Command(SourceCommandFact {
                call_id: Some("call-1".to_owned()),
                tool_name: Some("exec_command".to_owned()),
                command: TransientSourceContent::from_bytes(b"cargo test").unwrap(),
                working_directory: Some("/workspace/ctx".to_owned()),
            }),
            TransientSourceFact::Result(SourceResultFact {
                call_id: Some("call-1".to_owned()),
                outcome: SourceOutcome::Success,
                exit_code: Some(0),
                duration_ms: Some(42),
                content: TransientSourceContent::from_bytes(b"ok").unwrap(),
            }),
        ],
    )
    .unwrap()
}

fn source_removal() -> SourceRemoval {
    let source = certified_source().observation().source().clone();
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "conformance-root",
        TypedKey::utf8("conformance-authority").unwrap(),
        "conformance-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "conformance-discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source, &inventory).unwrap();
    SourceRemoval::new(deletion, inventory).unwrap()
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

fn provider_output_blame_result() -> BlameResult {
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

fn host_messages() -> Vec<HostMessage> {
    vec![
        HostMessage::Hello(HelloRequest::current(
            "conformance-host",
            BTreeSet::from([
                Capability::EntitlementAuthorization,
                Capability::GraphKeyDeletion,
                Capability::Status,
                Capability::JournalSync,
                Capability::OutputMaterialization,
                Capability::SourceMaterialization,
                Capability::Query,
                Capability::GitRead,
            ]),
        )),
        HostMessage::Authorize(authorization()),
        HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
            installation_key_thumbprint: base64url(&[1; 32]),
        }),
        HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest {
            authorization: authorization(),
        }),
        HostMessage::Status(StatusRequest {}),
        HostMessage::SyncJournal(journal_request()),
        HostMessage::BeginOutputInventory(BeginOutputInventoryRequest { generation: 1 }),
        HostMessage::ObserveOutputSource(ObserveOutputSourceRequest {
            generation: 1,
            source: output_source(),
            availability: OutputSourceAvailability::Available,
        }),
        HostMessage::MaterializeOutputPage(output_page()),
        HostMessage::FinishOutputInventory(FinishOutputInventoryRequest { generation: 1 }),
        HostMessage::GetOutputProgress(OutputProgressRequest {
            sources: vec![output_source()],
        }),
        HostMessage::BeginSourceManifest(BeginSourceManifestRequest {
            manifest: source_manifest(),
        }),
        HostMessage::BeginSourceManifestAdmission(BeginSourceManifestAdmissionRequest {
            header: source_manifest_header(),
        }),
        HostMessage::AdmitSourceManifestPage(AdmitSourceManifestPageRequest {
            page: source_manifest_page(),
        }),
        HostMessage::FinishSourceManifestAdmission(FinishSourceManifestAdmissionRequest {
            header: source_manifest_header(),
        }),
        HostMessage::PrepareSource(PrepareSourceRequest {
            core_generation_id: "a".repeat(64),
            source: source_progress(false).source,
            certified_revision_sha256: source_progress(false).certified_revision_sha256,
            materializer_revision: "conformance-source-materializer-v1".to_owned(),
            disposition: SourceDisposition::NewSource,
            expected_prior: None,
        }),
        HostMessage::MaterializeSourcePage(MaterializeSourcePageRequest {
            core_generation_id: "a".repeat(64),
            expected_prior: source_progress(false),
            next_frontier: source_progress(true).frontier,
            terminal: true,
            records: vec![source_record()],
        }),
        HostMessage::DeleteSource(DeleteSourceRequest {
            core_generation_id: "a".repeat(64),
            removal: source_removal(),
            expected_prior: source_progress(true),
        }),
        HostMessage::FinishSourceManifest(FinishSourceManifestRequest {
            manifest: source_manifest(),
            expected_progress: vec![source_progress(true)],
        }),
        HostMessage::FinishAdmittedSourceManifest(FinishAdmittedSourceManifestRequest {
            admission: source_manifest_admission_receipt(),
            expected_progress: vec![source_progress(true)],
        }),
        HostMessage::Blame(blame()),
    ]
}

fn helper_messages() -> Vec<HelperMessage> {
    let capabilities = BTreeSet::from([
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::SourceMaterialization,
        Capability::Query,
        Capability::GitRead,
    ]);
    vec![
        HelperMessage::Hello(HelloResult {
            protocol_version: PROTOCOL_VERSION,
            protocol_fingerprint: PROTOCOL_FINGERPRINT.to_owned(),
            helper_version: "conformance-helper".to_owned(),
            capabilities,
            authorization_challenge_base64url: base64url(&[4; AUTHORIZATION_CHALLENGE_BYTES]),
        }),
        HelperMessage::Authorized(AuthorizationResult {
            state: EntitlementAccessState::Trial,
            refresh_required: false,
            expires_at_unix: 175,
            access_deadline_unix: 200,
            grace_deadline_unix: 250,
            capabilities: BTreeSet::from([EntitlementCapability::GraphRead]),
        }),
        HelperMessage::GraphKeyDeletionPrepared(GraphKeyDeletionPrepared {
            challenge_base64url: base64url(&[6; GRAPH_KEY_DELETION_CHALLENGE_BYTES]),
            expires_at_unix: 200,
            key_present: true,
        }),
        HelperMessage::GraphKeyDeleted(GraphKeyDeleted { deleted: true }),
        HelperMessage::Status(StatusResult {
            state: GraphState::NotMaterialized,
            authority: MaterializationAuthority::Journal,
            checkpoint: None,
            source_receipt: None,
        }),
        HelperMessage::JournalSynced(JournalSyncResult {
            committed_through: checkpoint(),
            accepted_records: 0,
            replayed: false,
            frozen_complete: true,
        }),
        HelperMessage::OutputInventoryBegan(OutputInventoryBegan {
            generation: 1,
            materializer_revision: "fixture-materializer-1".to_owned(),
        }),
        HelperMessage::OutputSourceObserved(OutputSourceObserved {
            generation: 1,
            source: output_source(),
            availability: OutputSourceAvailability::Available,
        }),
        HelperMessage::OutputPageMaterialized(OutputPageMaterialized {
            inventory_generation: 1,
            source: output_source(),
            source_epoch: 0,
            committed_cursor: output_cursor(),
            accepted_outputs: 0,
            materialized_facts: 0,
            materialized_evidence: 0,
            replayed: false,
        }),
        HelperMessage::OutputInventoryFinished(OutputInventoryFinished {
            generation: 1,
            observed_sources: 1,
            unavailable_sources: 0,
        }),
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
        HelperMessage::SourceManifestBegan(SourceManifestBegan {
            core_generation_id: "a".repeat(64),
            materializer_revision: "conformance-source-materializer-v1".to_owned(),
            progress: Vec::new(),
            replayed: false,
        }),
        HelperMessage::SourceManifestAdmissionBegan(SourceManifestAdmissionBegan {
            cursor: SourceManifestAdmissionCursor::initial(&source_manifest_header()),
            replayed: false,
        }),
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
        HelperMessage::SourceManifestAdmitted(SourceManifestAdmitted {
            receipt: source_manifest_admission_receipt(),
            materializer_revision: "conformance-source-materializer-v1".to_owned(),
            progress: Vec::new(),
            replayed: false,
        }),
        HelperMessage::SourcePrepared(SourcePrepared {
            core_generation_id: "a".repeat(64),
            progress: source_progress(false),
            replayed: false,
        }),
        HelperMessage::SourcePageMaterialized(SourcePageMaterialized {
            core_generation_id: "a".repeat(64),
            progress: source_progress(true),
            accepted_records: 1,
            materialized_facts: 3,
            replayed: false,
        }),
        HelperMessage::SourceDeleted(SourceDeleted {
            core_generation_id: "a".repeat(64),
            source: source_progress(true).source,
            removed_source_epoch: 1,
            replayed: false,
        }),
        HelperMessage::SourceManifestFinished(SourceManifestFinished {
            receipt: SourceManifestReceipt {
                core_generation_id: "a".repeat(64),
                manifest_aggregate_sha256: "b".repeat(64),
                materializer_revision: "conformance-source-materializer-v1".to_owned(),
                progress: vec![source_progress(true)],
            },
            replayed: false,
        }),
        HelperMessage::Blame(provider_output_blame_result()),
        HelperMessage::Error(ProtocolError::new(
            ErrorClass::ProtocolMismatch,
            "exact Protocol V1 mismatch",
        )),
    ]
}

#[test]
fn canonical_inventory_and_exported_fingerprint_are_exact() {
    let value = inventory();
    let canonical = serde_json::to_vec(&value["canonical_inventory"])
        .expect("canonical inventory serialization");
    let digest = hex(&Sha256::digest(&canonical));
    assert_eq!(digest, value["canonical_sha256"]);
    assert_eq!(digest, PROTOCOL_FINGERPRINT);
    assert_eq!(value["canonical_inventory"]["protocol_version"], 1);
    assert_eq!(
        value["canonical_inventory"]["projection_contract_version"],
        1
    );
}

#[test]
fn inventory_freezes_every_message_kind_capability_and_error() {
    let value = inventory();
    let contract = &value["canonical_inventory"];
    let host: Vec<_> = host_messages().iter().map(host_kind).collect();
    let helper: Vec<_> = helper_messages().iter().map(helper_kind).collect();
    assert_eq!(
        serde_json::to_value(host).unwrap(),
        contract["host_message_kinds"]
    );
    assert_eq!(
        serde_json::to_value(helper).unwrap(),
        contract["helper_message_kinds"]
    );

    let capabilities = [
        Capability::EntitlementAuthorization,
        Capability::GraphKeyDeletion,
        Capability::Status,
        Capability::JournalSync,
        Capability::OutputMaterialization,
        Capability::SourceMaterialization,
        Capability::Query,
        Capability::GitRead,
    ];
    assert_eq!(
        serde_json::to_value(
            capabilities
                .iter()
                .map(|capability| capability.wire_name())
                .collect::<Vec<_>>()
        )
        .unwrap(),
        contract["capabilities"]
    );
    for error in contract["enums"]["error_class"]
        .as_array()
        .expect("error inventory")
    {
        serde_json::from_value::<ErrorClass>(error.clone()).expect("typed error class");
    }
}

#[test]
fn all_typed_messages_have_strict_canonical_v1_frames() {
    for (sequence, message) in host_messages().into_iter().enumerate() {
        let envelope = HostEnvelope {
            sequence: sequence as u64,
            request_id: Uuid::from_u128(sequence as u128 + 1),
            message,
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &envelope).unwrap();
        assert_eq!(&encoded[6..8], &PROTOCOL_VERSION.to_be_bytes());
        assert_eq!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(encoded)).unwrap(),
            envelope
        );
    }
    for (sequence, message) in helper_messages().into_iter().enumerate() {
        let envelope = HelperEnvelope {
            sequence: sequence as u64,
            request_id: Uuid::from_u128(sequence as u128 + 1),
            message,
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &envelope).unwrap();
        assert_eq!(&encoded[6..8], &PROTOCOL_VERSION.to_be_bytes());
        assert_eq!(
            read_frame::<_, HelperEnvelope>(&mut Cursor::new(encoded)).unwrap(),
            envelope
        );
    }
}

#[test]
fn representative_inventory_frames_decode_byte_for_byte() {
    let value = inventory();
    let frames = &value["canonical_inventory"]["representative_frames"];
    for (name, expected) in [
        ("host_status", {
            let mut bytes = Vec::new();
            write_frame(
                &mut bytes,
                &HostEnvelope {
                    sequence: 0,
                    request_id: Uuid::from_u128(1),
                    message: HostMessage::Status(StatusRequest {}),
                },
            )
            .unwrap();
            hex(&bytes)
        }),
        ("helper_protocol_mismatch", {
            let mut bytes = Vec::new();
            write_frame(
                &mut bytes,
                &HelperEnvelope {
                    sequence: 0,
                    request_id: Uuid::from_u128(1),
                    message: HelperMessage::Error(ProtocolError::new(
                        ErrorClass::ProtocolMismatch,
                        "exact Protocol V1 mismatch",
                    )),
                },
            )
            .unwrap();
            hex(&bytes)
        }),
    ] {
        assert_eq!(frames[name], expected);
    }
}

#[test]
fn every_generated_golden_frame_round_trips_byte_for_byte() {
    let value = inventory();
    for group in ["host_frames", "cursor_frames"] {
        for encoded in value["golden_vectors"][group]
            .as_object()
            .expect("host golden map")
            .values()
        {
            let bytes = unhex(encoded.as_str().expect("host golden hex"));
            let decoded = read_frame::<_, HostEnvelope>(&mut Cursor::new(&bytes)).unwrap();
            let mut round_trip = Vec::new();
            write_frame(&mut round_trip, &decoded).unwrap();
            assert_eq!(round_trip, bytes);
        }
    }
    for group in ["helper_frames", "error_frames"] {
        for encoded in value["golden_vectors"][group]
            .as_object()
            .expect("helper golden map")
            .values()
        {
            let bytes = unhex(encoded.as_str().expect("helper golden hex"));
            let decoded = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
            let mut round_trip = Vec::new();
            write_frame(&mut round_trip, &decoded).unwrap();
            assert_eq!(round_trip, bytes);
        }
    }
    for (group, helper) in [
        ("host_request_frames", false),
        ("helper_response_frames", true),
    ] {
        for encoded in value["golden_vectors"]["operation_frames"][group]
            .as_object()
            .expect("operation golden map")
            .values()
        {
            let bytes = unhex(encoded.as_str().expect("operation golden hex"));
            let mut round_trip = Vec::new();
            if helper {
                let decoded = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
                write_frame(&mut round_trip, &decoded).unwrap();
            } else {
                let decoded = read_frame::<_, HostEnvelope>(&mut Cursor::new(&bytes)).unwrap();
                write_frame(&mut round_trip, &decoded).unwrap();
            }
            assert_eq!(round_trip, bytes);
        }
    }
}

#[path = "conformance/operation_frames.rs"]
mod operation_frames;

#[test]
fn pr_membership_fixture_binds_structured_provider_output_without_activity_inference() {
    let value = inventory();
    let host_encoded = value["golden_vectors"]["operation_frames"]["host_request_frames"]
        ["materialize_output_page_new_source_command_success"]
        .as_str()
        .expect("structured provider-output host frame");
    let host = read_frame::<_, HostEnvelope>(&mut Cursor::new(unhex(host_encoded))).unwrap();
    let HostMessage::MaterializeOutputPage(page) = host.message else {
        panic!("structured provider-output fixture is not an output page");
    };
    page.validate().unwrap();
    let [observation] = page.observations.as_slice() else {
        panic!("structured provider-output fixture must contain one observation");
    };
    let command = observation
        .command
        .as_ref()
        .expect("structured provider-output command");
    assert_eq!(command.tool_name, "gh");
    assert_eq!(
        command.command,
        "gh pr view 42 --repo ctxrs/ctx --json url,commits,mergeCommit"
    );
    let structured: Value = serde_json::from_slice(&observation.content.decode().unwrap()).unwrap();
    assert_eq!(structured["url"], "https://github.com/ctxrs/ctx/pull/42");
    assert_eq!(
        structured["commits"][0]["oid"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        structured["mergeCommit"]["oid"],
        "89abcdef0123456789abcdef0123456789abcdef"
    );

    let membership_encoded = value["golden_vectors"]["operation_frames"]["helper_response_frames"]
        ["blame_pull_request_commit_membership"]
        .as_str()
        .expect("PR membership helper frame");
    let membership =
        read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(membership_encoded))).unwrap();
    let HelperMessage::Blame(membership) = membership.message else {
        panic!("PR membership fixture is not a blame result");
    };
    let provider_output = membership.evidence[0]
        .citation
        .provider_output
        .as_ref()
        .expect("PR membership provider-output evidence");
    assert_eq!(provider_output.source_id, page.source.source_id);
    assert_eq!(provider_output.source_epoch, page.source_epoch);
    assert_eq!(provider_output.coordinate, observation.coordinate);
    assert_eq!(provider_output.locator, observation.locator);
    assert_eq!(
        provider_output.availability,
        OutputSourceAvailability::Available
    );
    assert!(membership.matches.iter().all(|blame_match| {
        let BlameMatch::PullRequest(PullRequestBlameMatch {
            relationship: PullRequestBlameRelationship::Commit(commit),
            ..
        }) = blame_match
        else {
            return false;
        };
        commit.evidence_numbers == [1]
            && commit
                .production
                .iter()
                .all(|attribution| !attribution.evidence_numbers.contains(&1))
    }));

    let activity_encoded = value["golden_vectors"]["operation_frames"]["helper_response_frames"]
        ["blame_pull_request_activity_without_commit_membership"]
        .as_str()
        .expect("activity-only PR helper frame");
    let activity =
        read_frame::<_, HelperEnvelope>(&mut Cursor::new(unhex(activity_encoded))).unwrap();
    let HelperMessage::Blame(activity) = activity.message else {
        panic!("activity-only PR fixture is not a blame result");
    };
    assert_eq!(activity.matches.len(), 8);
    assert!(activity.matches.iter().all(|blame_match| {
        matches!(
            blame_match,
            BlameMatch::PullRequest(PullRequestBlameMatch {
                relationship: PullRequestBlameRelationship::Activity(_),
                ..
            })
        )
    }));
    assert!(activity
        .evidence
        .iter()
        .all(|evidence| evidence.citation.provider_output.is_none()));
}

#[test]
fn provider_output_citation_branch_is_typed_exclusive_and_strict() {
    let value = inventory();
    assert_eq!(
        value["canonical_inventory"]["dto_fields"]["ProviderOutputEvidence"],
        serde_json::json!({
            "required": ["source_id", "source_epoch", "locator", "coordinate", "availability"],
            "optional": []
        })
    );
    assert_eq!(
        value["canonical_inventory"]["evidence_citation"]["selection"],
        "exactly_one_usable_branch"
    );

    let encoded = value["golden_vectors"]["helper_frames"]["blame"]
        .as_str()
        .expect("provider-output blame golden");
    let bytes = unhex(encoded);
    let envelope = read_frame::<_, HelperEnvelope>(&mut Cursor::new(&bytes)).unwrap();
    let HelperMessage::Blame(result) = envelope.message else {
        panic!("blame golden decoded as another helper message");
    };
    assert_eq!(result, provider_output_blame_result());
    assert!(result.evidence[0].citation.is_usable());

    let mut mixed = serde_json::to_value(&result).unwrap();
    mixed["evidence"][0]["citation"]["source_path"] =
        Value::String("fixture/session.jsonl".to_owned());
    assert!(serde_json::from_value::<BlameResult>(mixed).is_err());

    let mut unknown = serde_json::to_value(&result).unwrap();
    unknown["evidence"][0]["citation"]["provider_output"]["fallback"] = Value::Bool(true);
    assert!(serde_json::from_value::<BlameResult>(unknown).is_err());

    let mut noncanonical_locator = serde_json::to_value(&result).unwrap();
    noncanonical_locator["evidence"][0]["citation"]["provider_output"]["locator"]
        ["payload_base64"] = Value::String("not-base64".to_owned());
    assert!(serde_json::from_value::<BlameResult>(noncanonical_locator).is_err());
}

#[test]
fn blame_match_unknown_fields_fail_closed() {
    let result = provider_output_blame_result();
    let mut blame_match = serde_json::to_value(&result.matches[0]).unwrap();
    blame_match["future_match_metadata"] = Value::Bool(true);
    assert!(serde_json::from_value::<BlameMatch>(blame_match).is_err());
}

#[test]
fn maximum_escaping_roots_boundary_is_exact_and_under_four_mib() {
    let roots = (0..MAX_AUTHORIZED_REPOSITORY_ROOTS)
        .map(|index| {
            let prefix = format!("/{index:03}/");
            format!(
                "{prefix}{}",
                "\\".repeat(2048_usize.saturating_sub(prefix.len()))
            )
        })
        .collect::<Vec<_>>();
    let request = JournalSyncRequest {
        authorized_repository_roots: roots,
        ..journal_request()
    };
    request.validate().unwrap();
    let envelope = HostEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(1),
        message: HostMessage::SyncJournal(request),
    };
    let bytes = serde_json::to_vec(&envelope).unwrap();
    let boundary = &inventory()["golden_vectors"]["boundary_frames"]["maximum_escaping_roots"];
    assert_eq!(boundary["payload_bytes"], bytes.len());
    assert_eq!(boundary["sha256"], hex(&Sha256::digest(&bytes)));
    assert!(bytes.len() <= MAX_JOURNAL_SYNC_ENVELOPE_BYTES);
}
