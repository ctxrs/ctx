use std::collections::BTreeMap;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, LexicalDocument, WriterOptions};
use tempfile::tempdir;

use super::*;

fn source(name: &str) -> SourceKey {
    SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "fixture-v1",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn certificate(source: &SourceKey, revision: u8, documents: u64) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents,
            retained_records: documents,
            indexed_documents: documents,
            certified_bytes: documents.saturating_mul(100),
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn document(source: &SourceKey, sequence: u64, body: String) -> LexicalDocument {
    let native_session = TypedKey::utf8("session").unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", native_session.clone())
            .unwrap(),
    })
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &NativeItemKey::native_id("event", TypedKey::U64(sequence)).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator: SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: sequence.saturating_mul(100),
                byte_length: u64::try_from(body.len()).unwrap(),
                physical_ordinal: sequence,
                native_session_key: Some(native_session),
                native_event_key: Some(TypedKey::U64(sequence)),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            [sequence as u8; 32],
        )
        .unwrap(),
        provider_session_id: Some("session".to_owned()),
        branch: Some("main".to_owned()),
        source_path: Some("/provider-is-not-needed/session.jsonl".to_owned()),
        agent_type: "primary".to_owned(),
        is_primary: true,
        event_sequence: sequence,
        occurred_at_unix_ms: Some(1_700_000_000_000 + sequence as i64),
        event_type: "message".to_owned(),
        role: Some("assistant".to_owned()),
        body,
        workspace: Some("ctx".to_owned()),
        cwd: Some("/work/ctx".to_owned()),
        touched_files: vec!["src/lib.rs".to_owned()],
    }
}

fn add_source_with_count(
    writer: &mut GenerationWriter,
    source: &SourceKey,
    revision: u8,
    bodies: Vec<String>,
) {
    let count = u64::try_from(bodies.len()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, body) in bodies.into_iter().enumerate() {
        writer
            .add_document(document(source, u64::try_from(index + 1).unwrap(), body))
            .unwrap();
    }
    writer
        .certify_source(certificate(source, revision, count))
        .unwrap();
}

fn receipt_for(index: &VerifiedIndex, revision: &str) -> CoreMaterializationReceipt {
    let sources = core_source_states(index.manifest()).unwrap();
    let head = core_generation_head(index, &sources).unwrap();
    CoreMaterializationReceipt {
        core_generation_id: head.core_generation_id,
        core_record_contract_fingerprint: head.core_record_contract_fingerprint,
        source_snapshot_sha256: head.source_snapshot_sha256,
        materializer_revision: revision.to_owned(),
        source_count: head.source_count,
        event_count: head.event_count,
    }
}

#[derive(Default)]
struct Consumer {
    revision: String,
    known_revisions: BTreeMap<[u8; 32], String>,
    replay_begin: bool,
    replay_pages: bool,
    wrong_delta_generation: bool,
    delta_pages: Vec<CoreSourceDeltaPage>,
    record_pages: Vec<CoreRecordPage>,
    finish: Option<FinishCoreMaterializationRequest>,
}

impl Consumer {
    fn new() -> Self {
        Self {
            revision: "test-core-materializer-v1".to_owned(),
            ..Self::default()
        }
    }
}

impl CoreMaterializationConsumer for Consumer {
    fn begin(
        &mut self,
        request: &BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan> {
        Ok(CoreMaterializationBegan {
            materialization_id: ctx_pro_host_protocol::core_materialization_id(
                request,
                &self.revision,
            )
            .map_err(|error| anyhow!(error.message))?,
            core_generation_id: request.head.core_generation_id.clone(),
            materializer_revision: self.revision.clone(),
            expected_prior_receipt: request.expected_prior_receipt.clone(),
            replayed: self.replay_begin,
        })
    }

    fn apply_source_delta(
        &mut self,
        request: &ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied> {
        self.delta_pages.push(request.page.clone());
        let mut materialize_sources = Vec::new();
        let mut removed_sources = 0_u32;
        for delta in &request.page.deltas {
            match delta {
                CoreSourceDelta::Present(state) => {
                    let identity = state.source.identity().digest();
                    if self.known_revisions.get(&identity) != Some(&state.source_revision_sha256) {
                        materialize_sources.push(state.clone());
                    }
                    self.known_revisions
                        .insert(identity, state.source_revision_sha256.clone());
                }
                CoreSourceDelta::Removed(removal) => {
                    self.known_revisions
                        .remove(&removal.source.identity().digest());
                    removed_sources = removed_sources.saturating_add(1);
                }
            }
        }
        Ok(CoreSourceDeltaPageApplied {
            materialization_id: request.page.materialization_id.clone(),
            core_generation_id: if self.wrong_delta_generation {
                "f".repeat(64)
            } else {
                request.page.core_generation_id.clone()
            },
            page_index: request.page.page_index,
            changed_sources: u32::try_from(materialize_sources.len()).unwrap(),
            removed_sources,
            materialize_sources,
            replayed: self.replay_pages,
        })
    }

    fn materialize_records(
        &mut self,
        request: &MaterializeCoreRecordPageRequest,
    ) -> Result<CoreRecordPageMaterialized> {
        self.record_pages.push(request.page.clone());
        Ok(CoreRecordPageMaterialized {
            materialization_id: request.page.materialization_id.clone(),
            core_generation_id: request.page.core_generation_id.clone(),
            source: request.page.source.source.clone(),
            source_revision_sha256: request.page.source.source_revision_sha256.clone(),
            source_index: request.page.source_index,
            page_index: request.page.page_index,
            accepted_records: u32::try_from(request.page.records.len()).unwrap(),
            terminal: request.page.terminal,
            replayed: self.replay_pages,
        })
    }

    fn finish(
        &mut self,
        request: &FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinished> {
        self.finish = Some(request.clone());
        Ok(CoreMaterializationFinished {
            receipt: CoreMaterializationReceipt {
                core_generation_id: request.head.core_generation_id.clone(),
                core_record_contract_fingerprint: request
                    .head
                    .core_record_contract_fingerprint
                    .clone(),
                source_snapshot_sha256: request.head.source_snapshot_sha256.clone(),
                materializer_revision: self.revision.clone(),
                source_count: request.head.source_count,
                event_count: request.head.event_count,
            },
            replayed: self.replay_begin,
        })
    }
}

#[test]
fn unchanged_large_source_is_not_resent_when_another_source_changes() {
    let temp = tempdir().unwrap();
    let large = source("large.jsonl");
    let changed = source("changed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &large, 1, vec!["L".repeat(32 * 1024)]);
    add_source_with_count(&mut writer, &changed, 2, vec!["changed body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let states = core_source_states(index.manifest()).unwrap();

    let mut consumer = Consumer::new();
    let large_state = states
        .iter()
        .find(|state| state.source.exact_descriptor_eq(&large))
        .unwrap();
    consumer.known_revisions.insert(
        large.identity().digest(),
        large_state.source_revision_sha256.clone(),
    );
    consumer
        .known_revisions
        .insert(changed.identity().digest(), "0".repeat(64));

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.materialized_records, 1);
    assert!(consumer.record_pages.iter().all(|page| {
        page.source.source.exact_descriptor_eq(&changed)
            && page
                .records
                .iter()
                .all(|record| record.content.normalized_body.as_deref() == Some("changed body"))
    }));
    assert!(!consumer.record_pages.iter().any(|page| {
        page.records.iter().any(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.len() > 16 * 1024)
        })
    }));
}

#[test]
fn exact_replay_is_a_no_op_with_no_delta_or_record_pages() {
    let temp = tempdir().unwrap();
    let source = source("replay.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert!(report.replayed);
    assert!(consumer.delta_pages.is_empty());
    assert!(consumer.record_pages.is_empty());
    let finish = consumer.finish.unwrap();
    assert_eq!(finish.changed_sources, 0);
    assert_eq!(finish.removed_sources, 0);
    assert_eq!(finish.materialized_records, 0);
}

#[test]
fn restarted_staging_replays_pages_and_still_finishes_with_actual_counts() {
    let temp = tempdir().unwrap();
    let source = source("restart.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.replay_pages = true;

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert!(!report.replayed);
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.materialized_records, 1);
    assert_eq!(consumer.delta_pages.len(), 1);
    assert_eq!(consumer.record_pages.len(), 1);
    let finish = consumer.finish.unwrap();
    assert_eq!(finish.changed_sources, 1);
    assert_eq!(finish.materialized_records, 1);
    assert_eq!(
        finish.materialization_id,
        consumer.record_pages[0].materialization_id
    );
}

#[test]
fn large_records_make_progress_one_bounded_fetch_at_a_time() {
    let temp = tempdir().unwrap();
    let source = source("large-progress.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(
        &mut writer,
        &source,
        1,
        vec![
            "a".repeat(1024 * 1024),
            "b".repeat(1024 * 1024),
            "c".repeat(1024 * 1024),
        ],
    );
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.materialized_records, 3);
    assert_eq!(report.record_pages, 3);
    assert_eq!(consumer.record_pages.len(), 3);
    assert!(consumer.record_pages.iter().all(|page| {
        page.records.len() == CORE_RECORD_FETCH_ITEMS
            && page.content_bytes().unwrap() == 1024 * 1024
    }));
    assert!(!consumer.record_pages[0].terminal);
    assert!(!consumer.record_pages[1].terminal);
    assert!(consumer.record_pages[2].terminal);
}

#[test]
fn deletion_is_applied_from_delta_without_resending_unchanged_records() {
    let temp = tempdir().unwrap();
    let retained = source("retained.jsonl");
    let removed = source("removed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &retained, 1, vec!["retained".to_owned()]);
    add_source_with_count(&mut writer, &removed, 1, vec!["removed".to_owned()]);
    writer.commit(|_| true).unwrap();
    let prior_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&prior_index, "test-core-materializer-v1");
    let retained_revision = core_source_states(prior_index.manifest())
        .unwrap()
        .into_iter()
        .find(|state| state.source.exact_descriptor_eq(&retained))
        .unwrap()
        .source_revision_sha256;
    drop(prior_index);

    let observation = SourceInventoryObservation::new(
        removed.provider(),
        "fixture-root",
        TypedKey::utf8("fixture-authority").unwrap(),
        "inventory-v1",
        vec![2],
    )
    .unwrap();
    let inventory = CertifiedSourceInventory::certify(
        observation.clone(),
        observation,
        "discovery-v1",
        vec![retained.clone()],
    )
    .unwrap();
    let deletion = CertifiedSourceDeletion::from_inventory(removed.clone(), &inventory).unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.delete_source(deletion, inventory).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let mut consumer = Consumer::new();
    consumer
        .known_revisions
        .insert(retained.identity().digest(), retained_revision);
    consumer
        .known_revisions
        .insert(removed.identity().digest(), "1".repeat(64));
    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 0);
    assert_eq!(report.removed_sources, 1);
    assert!(consumer.record_pages.is_empty());
    assert!(consumer.delta_pages.iter().flat_map(|page| &page.deltas).any(|delta| {
        matches!(delta, CoreSourceDelta::Removed(value) if value.source.exact_descriptor_eq(&removed))
    }));
}

#[test]
fn generation_mismatched_delta_ack_fails_closed() {
    let temp = tempdir().unwrap();
    let source = source("mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.wrong_delta_generation = true;
    assert!(sync_core_feed(&index, None, &mut consumer)
        .unwrap_err()
        .to_string()
        .contains("acknowledgement"));
}

#[test]
fn producer_has_no_provider_resolver_or_hydration_dependency() {
    let source = include_str!("../core_materialization_feed.rs");
    for forbidden in [
        "ctx_history_capture",
        "SourceBackedResolver",
        "hydrate",
        "reread_source",
    ] {
        assert!(!source.contains(forbidden), "producer contains {forbidden}");
    }
    assert!(source.contains("core_source_event_page"));
}
