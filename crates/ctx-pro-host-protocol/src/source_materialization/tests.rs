use std::io::Cursor;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSourceDeletion, CertifiedSourceInventory,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceObservation, SourceRecordLocator, TypedKey,
};
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::{
    read_frame, write_frame, Capability, HelperEnvelope, HelperMessage, HostEnvelope, HostMessage,
    StatusResult, MAX_FRAME_PAYLOAD_BYTES,
};

fn certified_source() -> CertifiedSource {
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([3; 32]),
    )
    .unwrap();
    let observation = SourceObservation::new(source, "fixture-revision-v1", vec![7]).unwrap();
    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 10,
        ..ScannedSourceCounts::default()
    };
    let frontier =
        SourceFrontier::new("fixture-frontier-v1", TypedKey::U64(1), 10, [9; 32]).unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [9; 32],
        counts,
        Some(frontier),
    )
    .unwrap()
}

fn source_key_at(index: u32) -> SourceKey {
    let mut lineage = [0_u8; 32];
    lineage[..4].copy_from_slice(&index.to_be_bytes());
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage(lineage),
    )
    .unwrap()
}

fn certified_source_at(index: u32, revision_bytes: usize) -> CertifiedSource {
    let source = source_key_at(index);
    let mut revision = vec![7; revision_bytes];
    revision[..4].copy_from_slice(&index.to_be_bytes());
    let observation = SourceObservation::new(source, "fixture-revision-v1", revision).unwrap();
    let counts = ScannedSourceCounts {
        complete_records: 1,
        retained_records: 1,
        indexed_documents: 1,
        certified_bytes: 10,
        ..ScannedSourceCounts::default()
    };
    let mut digest = [9_u8; 32];
    digest[..4].copy_from_slice(&index.to_be_bytes());
    let frontier = SourceFrontier::new(
        "fixture-frontier-v1",
        TypedKey::U64(u64::from(index) + 1),
        10,
        digest,
    )
    .unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        digest,
        counts,
        Some(frontier),
    )
    .unwrap()
}

fn provider_native_source_at(index: u32, key_bytes: usize) -> CertifiedSource {
    let key_prefix = format!("{index:08x}:");
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::ProviderNative {
            namespace: "fixture-source".to_owned(),
            key: TypedKey::Utf8(format!(
                "{key_prefix}{}",
                "x".repeat(key_bytes.saturating_sub(key_prefix.len()))
            )),
        },
    )
    .unwrap();
    let observation =
        SourceObservation::new(source, "fixture-revision-v1", index.to_be_bytes().to_vec())
            .unwrap();
    let mut digest = [9_u8; 32];
    digest[..4].copy_from_slice(&index.to_be_bytes());
    let frontier = SourceFrontier::new(
        "fixture-frontier-v1",
        TypedKey::U64(u64::from(index) + 1),
        10,
        digest,
    )
    .unwrap();
    CertifiedSource::certify_with_frontier(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        digest,
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 10,
            ..ScannedSourceCounts::default()
        },
        Some(frontier),
    )
    .unwrap()
}

fn terminal_progress(source: &CertifiedSource) -> SourceProgress {
    SourceProgress {
        source: source.observation().source().clone(),
        source_epoch: 1,
        certified_revision_sha256: certified_source_revision_sha256(source).unwrap(),
        frontier: source.frontier().cloned(),
        materializer_revision: "fixture-materializer-v1".to_owned(),
        terminal: true,
    }
}

fn source_record(content: &[u8], event_sequence: u64) -> SourceRecord {
    let source = certified_source().observation().source().clone();
    let session_key =
        NativeSessionKey::native_id("fixture-session", TypedKey::utf8("session-1").unwrap())
            .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "fixture-session",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key =
        NativeItemKey::native_id("fixture-event", TypedKey::U64(event_sequence)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "fixture-event",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::ProviderNative {
            namespace: "fixture-record".to_owned(),
            coordinate: TypedKey::U64(event_sequence),
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
            worktree_root: Some(
                SourceWorktreeRootLocator::new("/workspace/repository".to_owned()).unwrap(),
            ),
        }),
        SourceRecordMetadata {
            event_sequence,
            occurred_at_unix_ms: Some(1_700_000_000_000),
            event_type: "message".to_owned(),
            role: Some("assistant".to_owned()),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace/repository".to_owned()),
            touched_files: vec!["src/lib.rs".to_owned()],
        },
        vec![
            TransientSourceFact::Message(SourceMessageFact {
                content: TransientSourceContent::from_bytes(content).unwrap(),
            }),
            TransientSourceFact::Command(SourceCommandFact {
                call_id: Some("call-1".to_owned()),
                tool_name: Some("exec_command".to_owned()),
                command: TransientSourceContent::from_bytes(b"cargo test").unwrap(),
                working_directory: Some("/workspace/repository".to_owned()),
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

#[test]
fn worktree_root_locator_requires_a_bounded_normalized_absolute_path() {
    for invalid in [
        "",
        "relative/repository",
        "/workspace/../repository",
        "/workspace/./repository",
        "/workspace/repository\nother",
        r"C:relative\repository",
    ] {
        assert!(
            SourceWorktreeRootLocator::new(invalid.to_owned()).is_err(),
            "unexpectedly accepted {invalid:?}"
        );
    }
    for valid in [
        "/workspace/repository",
        r"C:\workspace\repository",
        r"\\server\share\repository",
    ] {
        SourceWorktreeRootLocator::new(valid.to_owned()).unwrap();
    }
}

#[test]
fn worktree_root_locator_requires_a_worktree_identity() {
    let context = SourceRepositoryContext {
        repository_id: "repository-1".to_owned(),
        checkout_id: None,
        worktree_id: None,
        object_format: Some("sha1".to_owned()),
        worktree_root: Some(
            SourceWorktreeRootLocator::new("/workspace/repository".to_owned()).unwrap(),
        ),
    };

    assert_eq!(
        context.validate().unwrap_err().class,
        ErrorClass::InvalidRequest
    );
}

fn related_session_id() -> StableEntityId {
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::CatalogLineage([4; 32]),
    )
    .unwrap();
    let session_key =
        NativeSessionKey::native_id("fixture-session", TypedKey::utf8("session-2").unwrap())
            .unwrap();
    derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "fixture-session",
        native_session_key: &session_key,
    })
    .unwrap()
}

fn manifest() -> SourceManifest {
    SourceManifest::new("a".repeat(64), vec![certified_source()], Vec::new()).unwrap()
}

fn progress(terminal: bool) -> SourceProgress {
    let source = certified_source();
    SourceProgress {
        source: source.observation().source().clone(),
        source_epoch: 1,
        certified_revision_sha256: certified_source_revision_sha256(&source).unwrap(),
        frontier: terminal.then(|| source.frontier().unwrap().clone()),
        materializer_revision: "fixture-materializer-v1".to_owned(),
        terminal,
    }
}

fn removal() -> SourceRemoval {
    let source = certified_source().observation().source().clone();
    let observation = SourceInventoryObservation::new(
        source.provider(),
        "fixture-root",
        TypedKey::utf8("fixture-authority").unwrap(),
        "fixture-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "fixture-discovery-v1",
        Vec::new(),
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(source, &inventory).unwrap();
    SourceRemoval::new(deletion, inventory).unwrap()
}

#[test]
fn source_backed_pro_active_lifecycle_variants_have_exact_tags_and_round_trip() {
    let manifest = manifest();
    let prepared = progress(false);
    let requests = vec![
        HostMessage::PrepareSource(PrepareSourceRequest {
            core_generation_id: manifest.core_generation_id.clone(),
            source: prepared.source.clone(),
            certified_revision_sha256: prepared.certified_revision_sha256.clone(),
            materializer_revision: prepared.materializer_revision.clone(),
            disposition: SourceDisposition::NewSource,
            expected_prior: None,
        }),
        HostMessage::MaterializeSourcePage(MaterializeSourcePageRequest {
            core_generation_id: manifest.core_generation_id.clone(),
            expected_prior: prepared.clone(),
            next_frontier: certified_source().frontier().cloned(),
            terminal: true,
            records: vec![source_record(b"message", 1)],
        }),
        HostMessage::DeleteSource(DeleteSourceRequest {
            core_generation_id: manifest.core_generation_id.clone(),
            removal: removal(),
            expected_prior: progress(true),
        }),
    ];
    let tags = ["prepare_source", "materialize_source_page", "delete_source"];
    for (index, (message, tag)) in requests.into_iter().zip(tags).enumerate() {
        let envelope = HostEnvelope {
            sequence: index as u64,
            request_id: Uuid::from_u128(index as u128 + 1),
            message,
        };
        assert_eq!(
            serde_json::to_value(&envelope).unwrap()["message"]["kind"],
            tag
        );
        let mut frame = Vec::new();
        write_frame(&mut frame, &envelope).unwrap();
        assert_eq!(
            read_frame::<_, HostEnvelope>(&mut Cursor::new(frame)).unwrap(),
            envelope
        );
    }

    let responses = vec![
        HelperMessage::SourcePrepared(SourcePrepared {
            core_generation_id: manifest.core_generation_id.clone(),
            progress: prepared.clone(),
            replayed: false,
        }),
        HelperMessage::SourcePageMaterialized(SourcePageMaterialized {
            core_generation_id: manifest.core_generation_id.clone(),
            progress: progress(true),
            accepted_records: 1,
            materialized_facts: 3,
            replayed: false,
        }),
        HelperMessage::SourceDeleted(SourceDeleted {
            core_generation_id: manifest.core_generation_id.clone(),
            source: prepared.source,
            removed_source_epoch: 1,
            replayed: false,
        }),
        HelperMessage::SourceManifestFinished(SourceManifestFinished {
            receipt: SourceManifestReceipt {
                core_generation_id: manifest.core_generation_id,
                manifest_aggregate_sha256: "b".repeat(64),
                materializer_revision: "fixture-materializer-v1".to_owned(),
                progress: SourceProgressReceipt::from_progress(&[progress(true)]).unwrap(),
            },
            replayed: false,
        }),
    ];
    let tags = [
        "source_prepared",
        "source_page_materialized",
        "source_deleted",
        "source_manifest_finished",
    ];
    for (index, (message, tag)) in responses.into_iter().zip(tags).enumerate() {
        let envelope = HelperEnvelope {
            sequence: index as u64,
            request_id: Uuid::from_u128(index as u128 + 20),
            message,
        };
        assert_eq!(
            serde_json::to_value(&envelope).unwrap()["message"]["kind"],
            tag
        );
        let mut frame = Vec::new();
        write_frame(&mut frame, &envelope).unwrap();
        assert_eq!(
            read_frame::<_, HelperEnvelope>(&mut Cursor::new(frame)).unwrap(),
            envelope
        );
    }
}

#[test]
fn retired_whole_manifest_wire_kinds_are_rejected() {
    for kind in ["begin_source_manifest", "finish_source_manifest"] {
        assert!(
            serde_json::from_value::<HostMessage>(json!({"kind": kind, "body": {}})).is_err(),
            "retired host message kind {kind} must be rejected"
        );
    }
    assert!(serde_json::from_value::<HelperMessage>(
        json!({"kind": "source_manifest_began", "body": {}})
    )
    .is_err());
}

#[test]
fn source_backed_pro_maximum_content_stays_below_frame_limit_after_encoding() {
    let mut record = source_record(&vec![b'"'; MAX_SOURCE_CONTENT_BYTES], 1);
    record.facts.truncate(1);
    let request = MaterializeSourcePageRequest {
        core_generation_id: "a".repeat(64),
        expected_prior: progress(false),
        next_frontier: certified_source().frontier().cloned(),
        terminal: true,
        records: vec![record],
    };
    request.validate().unwrap();
    let envelope = HostEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(1),
        message: HostMessage::MaterializeSourcePage(request),
    };
    let encoded = serde_json::to_vec(&envelope).unwrap();
    assert!(encoded.len() <= MAX_SOURCE_PAGE_WIRE_BYTES + 512);
    assert!(encoded.len() < MAX_FRAME_PAYLOAD_BYTES);
}

#[test]
fn source_backed_pro_page_count_finish_cas_and_deletion_witness_are_bounded() {
    let mut request = MaterializeSourcePageRequest {
        core_generation_id: "a".repeat(64),
        expected_prior: progress(false),
        next_frontier: certified_source().frontier().cloned(),
        terminal: true,
        records: (1..=MAX_SOURCE_RECORDS_PER_PAGE)
            .map(|sequence| source_record(b"", sequence as u64))
            .collect(),
    };
    request.validate().unwrap();
    request
        .records
        .push(source_record(b"", MAX_SOURCE_RECORDS_PER_PAGE as u64 + 1));
    assert_eq!(request.validate().unwrap_err().class, ErrorClass::Bounds);
    request.records.clear();

    let manifest = manifest();
    let finish = FinishSourceManifestRequest {
        manifest: manifest.clone(),
        expected_progress: vec![progress(true)],
    };
    finish.validate().unwrap();

    let mut wrong = finish;
    wrong.expected_progress[0].frontier = None;
    assert_eq!(wrong.validate().unwrap_err().class, ErrorClass::Sequence);

    let deletion_manifest =
        SourceManifest::new("b".repeat(64), Vec::new(), vec![removal()]).unwrap();
    let encoded = serde_json::to_vec(&deletion_manifest).unwrap();
    let decoded: SourceManifest = serde_json::from_slice(&encoded).unwrap();
    decoded.validate().unwrap();
}

#[test]
fn source_manifest_admission_pages_a_3464_source_production_fixture_deterministically() {
    const SOURCE_COUNT: usize = 3_464;
    let mut sources = (0..u32::try_from(SOURCE_COUNT).unwrap())
        .map(|index| certified_source_at(index, 4 * 1024))
        .collect::<Vec<_>>();
    sources.sort_by_key(source_identity_digest);
    let page_count = u32::try_from(SOURCE_COUNT.div_ceil(MAX_SOURCE_MANIFEST_PAGE_ITEMS)).unwrap();
    let header = SourceManifestHeader::new(
        "a".repeat(64),
        1,
        1,
        1,
        1,
        "b".repeat(64),
        page_count,
        &sources,
        &[],
    )
    .unwrap();
    assert_eq!(header.source_count, 3_464);
    assert_eq!(
        BeginSourceManifestRequest {
            manifest: SourceManifest::new("a".repeat(64), sources.clone(), Vec::new()).unwrap(),
        }
        .validate()
        .unwrap_err()
        .class,
        ErrorClass::Bounds,
        "the rollback whole-manifest transfer must remain bounded"
    );

    let build_pages = || {
        let mut previous =
            SourceManifestAdmissionCursor::initial(&header).next_page_previous_sha256;
        sources
            .chunks(MAX_SOURCE_MANIFEST_PAGE_ITEMS)
            .enumerate()
            .map(|(page_index, entries)| {
                let page = SourceManifestPage::new(
                    &header,
                    &previous,
                    u32::try_from(page_index).unwrap(),
                    u32::try_from(page_index * MAX_SOURCE_MANIFEST_PAGE_ITEMS).unwrap(),
                    SourceManifestPageEntries::Sources(entries.to_vec()),
                )
                .unwrap();
                previous.clone_from(&page.page_sha256);
                page
            })
            .collect::<Vec<_>>()
    };
    let pages = build_pages();
    assert_eq!(
        pages.len(),
        SOURCE_COUNT.div_ceil(MAX_SOURCE_MANIFEST_PAGE_ITEMS)
    );
    assert!(pages.iter().all(|page| {
        serde_json::to_vec(&AdmitSourceManifestPageRequest { page: page.clone() })
            .unwrap()
            .len()
            <= MAX_SOURCE_MANIFEST_PAGE_WIRE_BYTES
    }));
    assert_eq!(pages, build_pages());
    header.validate_contents(&sources, &[]).unwrap();
}

#[test]
fn source_progress_pages_bound_the_full_5863_source_activation_and_status_lifecycle() {
    const SOURCE_COUNT: usize = 5_863;
    const SOURCE_KEY_BYTES: usize = 4 * 1024;
    let mut sources = (0..u32::try_from(SOURCE_COUNT).unwrap())
        .map(|index| provider_native_source_at(index, SOURCE_KEY_BYTES))
        .collect::<Vec<_>>();
    sources.sort_by_key(source_identity_digest);
    let progress = sources.iter().map(terminal_progress).collect::<Vec<_>>();
    let legacy_progress_bytes = serde_json::to_vec(&progress).unwrap().len();
    assert!(
        legacy_progress_bytes > MAX_SOURCE_CONTROL_WIRE_BYTES,
        "fixture must reproduce the retired source-count-scaled failure"
    );
    let page_count = u32::try_from(SOURCE_COUNT.div_ceil(MAX_SOURCE_MANIFEST_PAGE_ITEMS)).unwrap();
    let header = SourceManifestHeader::new(
        "a".repeat(64),
        1,
        1,
        1,
        1,
        "b".repeat(64),
        page_count,
        &sources,
        &[],
    )
    .unwrap();
    let admission = SourceManifestAdmissionReceipt {
        header: header.clone(),
        page_count: header.page_count,
        terminal_chain_sha256: "c".repeat(64),
    };
    let progress_receipt = SourceProgressReceipt::from_progress(&progress).unwrap();
    assert_eq!(
        usize::try_from(progress_receipt.page_count).unwrap(),
        SOURCE_COUNT.div_ceil(MAX_SOURCE_PROGRESS_PAGE_ITEMS)
    );

    for (page_index, entries) in progress.chunks(MAX_SOURCE_PROGRESS_PAGE_ITEMS).enumerate() {
        let page = SourceProgressPage::new(
            &progress_receipt,
            u32::try_from(page_index).unwrap(),
            entries.to_vec(),
            page_index % 2 == 0,
        )
        .unwrap();
        page.validate_for(&progress_receipt).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_SOURCE_PROGRESS_PAGE_WIRE_BYTES);
    }
    progress_receipt
        .validate_contents(&progress, Some("fixture-materializer-v1"), true)
        .unwrap();

    let admitted = SourceManifestAdmitted {
        receipt: admission.clone(),
        materializer_revision: "fixture-materializer-v1".to_owned(),
        progress: progress_receipt.clone(),
        replayed: true,
    };
    admitted.validate().unwrap();
    let activation = FinishAdmittedSourceManifestRequest {
        admission,
        expected_progress: progress_receipt.clone(),
    };
    activation.validate().unwrap();
    let final_response = SourceManifestFinished {
        receipt: SourceManifestReceipt {
            core_generation_id: header.core_generation_id,
            manifest_aggregate_sha256: header.aggregate_sha256,
            materializer_revision: "fixture-materializer-v1".to_owned(),
            progress: progress_receipt,
        },
        replayed: true,
    };
    final_response.validate().unwrap();
    let status = StatusResult {
        state: crate::GraphState::Ready,
        authority: crate::MaterializationAuthority::Source,
        source_receipt: Some(final_response.receipt.clone()),
    };
    status.validate().unwrap();

    let admitted_bytes = serde_json::to_vec(&admitted).unwrap().len();
    let activation_bytes = serde_json::to_vec(&activation).unwrap().len();
    let final_response_bytes = serde_json::to_vec(&final_response).unwrap().len();
    let status_bytes = serde_json::to_vec(&status).unwrap().len();
    let final_envelope = HelperEnvelope {
        sequence: u64::MAX,
        request_id: Uuid::from_u128(1),
        message: HelperMessage::SourceManifestFinished(final_response),
    };
    let final_payload_bytes = serde_json::to_vec(&final_envelope).unwrap().len();
    let mut final_frame = Vec::new();
    write_frame(&mut final_frame, &final_envelope).unwrap();
    eprintln!(
        "5863-source bytes: legacy_progress={legacy_progress_bytes} admitted={admitted_bytes} activation={activation_bytes} final_response={final_response_bytes} status={status_bytes} final_payload={final_payload_bytes} final_frame={} cap={MAX_SOURCE_CONTROL_WIRE_BYTES}",
        final_frame.len()
    );
    for encoded in [
        admitted_bytes,
        activation_bytes,
        final_response_bytes,
        status_bytes,
        final_payload_bytes,
    ] {
        assert!(encoded <= MAX_SOURCE_CONTROL_WIRE_BYTES);
    }
    assert!(final_payload_bytes < MAX_FRAME_PAYLOAD_BYTES);
}

#[test]
fn source_progress_pages_reject_oversize_order_digest_and_corrupt_replay() {
    let mut sources = (0..65)
        .map(|index| certified_source_at(index, 4))
        .collect::<Vec<_>>();
    sources.sort_by_key(source_identity_digest);
    let progress = sources.iter().map(terminal_progress).collect::<Vec<_>>();
    let receipt = SourceProgressReceipt::from_progress(&progress).unwrap();

    assert_eq!(
        SourceProgressPage::new(&receipt, 0, progress.clone(), false)
            .unwrap_err()
            .class,
        ErrorClass::Bounds
    );
    let mut out_of_order = progress[..MAX_SOURCE_PROGRESS_PAGE_ITEMS].to_vec();
    out_of_order.swap(0, 1);
    assert_eq!(
        SourceProgressPage::new(&receipt, 0, out_of_order, false)
            .unwrap_err()
            .class,
        ErrorClass::InvalidRequest
    );
    let mut corrupt = SourceProgressPage::new(
        &receipt,
        0,
        progress[..MAX_SOURCE_PROGRESS_PAGE_ITEMS].to_vec(),
        true,
    )
    .unwrap();
    corrupt.progress[0].source_epoch += 1;
    assert_eq!(
        corrupt.validate_for(&receipt).unwrap_err().class,
        ErrorClass::InvalidRequest
    );
    let mut wrong_receipt = receipt;
    wrong_receipt.aggregate_sha256 = "f".repeat(64);
    assert_eq!(
        corrupt.validate_for(&wrong_receipt).unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn source_manifest_admission_rejects_an_oversize_metadata_only_page() {
    let observed_sources = (10_000..14_000).map(source_key_at).collect::<Vec<_>>();
    let observation = SourceInventoryObservation::new(
        "fixture",
        "fixture-root",
        TypedKey::utf8("fixture-authority").unwrap(),
        "fixture-inventory-v1",
        vec![1],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "fixture-discovery-v1",
        observed_sources,
    )
    .unwrap();
    let mut removals = (20_000..20_064)
        .map(|index| {
            let deletion =
                CertifiedSourceDeletion::from_inventory(source_key_at(index), &inventory).unwrap();
            SourceRemoval::new(deletion, inventory.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    removals.sort_by_key(|removal| removal.deletion.source().identity().digest());
    let header = SourceManifestHeader::new(
        "a".repeat(64),
        1,
        1,
        1,
        1,
        "b".repeat(64),
        1,
        &[],
        &removals,
    )
    .unwrap();
    let previous = SourceManifestAdmissionCursor::initial(&header).next_page_previous_sha256;
    let result = SourceManifestPage::new(
        &header,
        previous,
        0,
        0,
        SourceManifestPageEntries::Removals(removals),
    );
    match result {
        Err(error) => assert_eq!(error.class, ErrorClass::Bounds),
        Ok(page) => panic!(
            "oversize page unexpectedly encoded in {} bytes",
            serde_json::to_vec(&page).unwrap().len()
        ),
    }
}

#[test]
fn source_manifest_admission_binds_exact_counts_and_aggregate_digest() {
    let mut sources = vec![certified_source_at(1, 4), certified_source_at(2, 4)];
    sources.sort_by_key(source_identity_digest);
    let header =
        SourceManifestHeader::new("a".repeat(64), 1, 1, 1, 1, "b".repeat(64), 1, &sources, &[])
            .unwrap();
    let mut wrong_count = header.clone();
    wrong_count.source_count -= 1;
    assert_eq!(
        wrong_count
            .validate_contents(&sources, &[])
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );
    let mut wrong_page_count = header.clone();
    wrong_page_count.page_count = 2;
    assert_eq!(
        wrong_page_count
            .validate_contents(&sources, &[])
            .unwrap_err()
            .class,
        ErrorClass::InvalidRequest
    );
    let mut wrong_digest = header;
    wrong_digest.aggregate_sha256 = "f".repeat(64);
    assert_eq!(
        wrong_digest
            .validate_contents(&sources, &[])
            .unwrap_err()
            .class,
        ErrorClass::InvalidRequest
    );
}

#[test]
fn source_manifest_admission_rejects_duplicate_and_out_of_order_entries() {
    let mut sources = vec![
        certified_source_at(1, 4),
        certified_source_at(2, 4),
        certified_source_at(3, 4),
    ];
    sources.sort_by_key(source_identity_digest);
    let header =
        SourceManifestHeader::new("a".repeat(64), 1, 1, 1, 1, "b".repeat(64), 1, &sources, &[])
            .unwrap();
    let previous = SourceManifestAdmissionCursor::initial(&header).next_page_previous_sha256;

    let duplicate = vec![sources[0].clone(), sources[0].clone()];
    assert_eq!(
        SourceManifestPage::new(
            &header,
            &previous,
            0,
            0,
            SourceManifestPageEntries::Sources(duplicate),
        )
        .unwrap_err()
        .class,
        ErrorClass::InvalidRequest
    );

    let mut out_of_order = sources;
    out_of_order.swap(0, 1);
    assert_eq!(
        SourceManifestPage::new(
            &header,
            previous,
            0,
            0,
            SourceManifestPageEntries::Sources(out_of_order),
        )
        .unwrap_err()
        .class,
        ErrorClass::InvalidRequest
    );
}

#[test]
fn source_manifest_admission_restart_preserves_exact_cursor_and_replay_state() {
    let mut sources = vec![
        certified_source_at(1, 4),
        certified_source_at(2, 4),
        certified_source_at(3, 4),
    ];
    sources.sort_by_key(source_identity_digest);
    let header =
        SourceManifestHeader::new("a".repeat(64), 1, 1, 1, 1, "b".repeat(64), 3, &sources, &[])
            .unwrap();
    let initial = SourceManifestAdmissionCursor::initial(&header);
    let first_page = SourceManifestPage::new(
        &header,
        &initial.next_page_previous_sha256,
        0,
        0,
        SourceManifestPageEntries::Sources(vec![sources[0].clone()]),
    )
    .unwrap();
    let second_page = SourceManifestPage::new(
        &header,
        &first_page.page_sha256,
        1,
        1,
        SourceManifestPageEntries::Sources(vec![sources[1].clone()]),
    )
    .unwrap();
    let cursor = SourceManifestAdmissionCursor {
        core_generation_id: header.core_generation_id.clone(),
        aggregate_sha256: header.aggregate_sha256.clone(),
        next_page_previous_sha256: second_page.page_sha256,
        next_page_index: 2,
        next_source_index: 2,
        next_removal_index: 0,
    };
    let restarted = SourceManifestAdmissionBegan {
        cursor: cursor.clone(),
        replayed: true,
    };
    restarted.validate_for(&header).unwrap();
    assert_eq!(
        serde_json::from_slice::<SourceManifestAdmissionBegan>(
            &serde_json::to_vec(&restarted).unwrap()
        )
        .unwrap(),
        restarted
    );

    let replayed_page = SourceManifestPageAdmitted {
        cursor: cursor.clone(),
        replayed: true,
    };
    replayed_page.validate_for(&header).unwrap();
    assert_eq!(replayed_page.cursor, cursor);

    let third_page = SourceManifestPage::new(
        &header,
        &cursor.next_page_previous_sha256,
        2,
        2,
        SourceManifestPageEntries::Sources(vec![sources[2].clone()]),
    )
    .unwrap();
    let complete = SourceManifestAdmissionCursor {
        next_page_previous_sha256: third_page.page_sha256,
        next_page_index: 3,
        next_source_index: header.source_count,
        ..cursor
    };
    assert!(complete.is_complete_for(&header));
    SourceManifestAdmissionReceipt {
        header: header.clone(),
        page_count: header.page_count,
        terminal_chain_sha256: complete.next_page_previous_sha256.clone(),
    }
    .validate()
    .unwrap();

    let mut restarted_for_another_manifest = restarted;
    restarted_for_another_manifest.cursor.aggregate_sha256 = "f".repeat(64);
    assert_eq!(
        restarted_for_another_manifest
            .validate_for(&header)
            .unwrap_err()
            .class,
        ErrorClass::Sequence
    );
}

#[test]
fn source_backed_pro_unknown_capability_fails_closed_before_negotiation() {
    let hello = json!({
        "protocol_version": crate::PROTOCOL_VERSION,
        "protocol_fingerprint": crate::PROTOCOL_FINGERPRINT,
        "host_version": "fixture",
        "capabilities": [
            Capability::SourceMaterialization,
            "future_source_materialization"
        ]
    });
    assert!(serde_json::from_value::<crate::HelloRequest>(hello).is_err());
}

#[test]
fn source_backed_pro_upgrade_progress_and_cross_source_relationships_are_valid() {
    let mut old_progress = progress(false);
    old_progress.materializer_revision = "fixture-materializer-v0".to_owned();
    SourceManifestBegan {
        core_generation_id: "a".repeat(64),
        materializer_revision: "fixture-materializer-v1".to_owned(),
        progress: vec![old_progress.clone()],
        replayed: false,
    }
    .validate()
    .unwrap();

    let related = related_session_id();
    let mut record = source_record(b"message", 1);
    record.relationships.root_session_id = related;
    record.relationships.parent_session_id = Some(related);
    record.validate_and_count_bytes().unwrap();

    old_progress.source_epoch = u64::MAX;
    let exhausted = PrepareSourceRequest {
        core_generation_id: "a".repeat(64),
        source: old_progress.source.clone(),
        certified_revision_sha256: "b".repeat(64),
        materializer_revision: "fixture-materializer-v1".to_owned(),
        disposition: SourceDisposition::Rewrite,
        expected_prior: Some(old_progress),
    };
    assert_eq!(
        exhausted.validate().unwrap_err().class,
        ErrorClass::Sequence
    );
}

#[test]
fn source_backed_pro_transient_debug_redacts_all_detector_content() {
    let record = source_record(b"TRANSIENT_SOURCE_DEBUG_CANARY", 1);
    let debug = format!("{record:?}");
    assert!(!debug.contains("TRANSIENT_SOURCE_DEBUG_CANARY"));
    assert!(debug.contains("<redacted>"));
}
