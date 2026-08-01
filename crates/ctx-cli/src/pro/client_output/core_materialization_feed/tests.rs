use std::collections::BTreeMap;

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey,
    SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_pro_host_protocol::MAX_CORE_RECORD_PAGE_WIRE_BYTES;
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

fn record(source: &SourceKey, sequence: u64, body: String) -> CoreRecord {
    let native_session = TypedKey::utf8("session").unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &NativeSessionKey::native_id("session", native_session).unwrap(),
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
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "primary",
        true,
        "core-materialization-feed-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some("session".to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.branch = Some("main".to_owned());
    record.occurred_at_unix_ms = Some(1_700_000_000_000 + sequence as i64);
    record.role = Some("assistant".to_owned());
    record.workspace = Some("ctx".to_owned());
    record.cwd = Some("/work/ctx".to_owned());
    record.validate_contract().unwrap();
    record
}

fn add_source(
    writer: &mut GenerationWriter,
    source: &SourceKey,
    revision: u8,
    bodies: Vec<String>,
) {
    let count = u64::try_from(bodies.len()).unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, body) in bodies.into_iter().enumerate() {
        writer
            .add_core_record(record(source, u64::try_from(index + 1).unwrap(), body))
            .unwrap();
    }
    writer
        .certify_source(certificate(source, revision, count))
        .unwrap();
}

fn encoded_record_bytes(page: &CoreRecordPage) -> usize {
    page.records
        .iter()
        .map(|record| serde_json::to_vec(record).unwrap().len())
        .sum()
}

fn encoded_page_bytes(page: &CoreRecordPage) -> usize {
    serde_json::to_vec(page).unwrap().len()
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

type EventStates = BTreeMap<[u8; 32], BTreeMap<[u8; 32], CoreEventState>>;

#[derive(Default)]
struct Consumer {
    revision: String,
    known_revisions: BTreeMap<[u8; 32], String>,
    known_events: EventStates,
    replay_begin: bool,
    replay_pages: bool,
    wrong_delta_generation: bool,
    wrong_event_page_index: bool,
    event_exchanges: u64,
    state_exchanges: u64,
    delta_pages: Vec<CoreSourceDeltaPage>,
    event_pages: Vec<CoreEventDeltaPage>,
    finish: Option<FinishCoreMaterializationRequest>,
}

impl Consumer {
    fn new() -> Self {
        Self {
            revision: "test-core-materializer-v1".to_owned(),
            ..Self::default()
        }
    }

    fn source_events_mut(&mut self, source: &SourceKey) -> &mut BTreeMap<[u8; 32], CoreEventState> {
        self.known_events
            .entry(source.identity().digest())
            .or_default()
    }
}

impl CoreMaterializationConsumer for Consumer {
    fn begin(
        &mut self,
        request: BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan> {
        Ok(CoreMaterializationBegan {
            materialization_id: ctx_pro_host_protocol::core_materialization_id(
                &request,
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
        request: ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied> {
        let page = request.page;
        let mut reconcile_sources = Vec::new();
        let mut changed_sources = 0_u32;
        let mut removed_sources = 0_u32;
        for (source_index, delta) in page.deltas.iter().enumerate() {
            let identity = delta.source().identity().digest();
            let reconcile = match delta {
                CoreSourceDelta::Present(state) => {
                    let changed =
                        self.known_revisions.get(&identity) != Some(&state.source_revision_sha256);
                    self.known_revisions
                        .insert(identity, state.source_revision_sha256.clone());
                    if changed {
                        changed_sources = changed_sources.saturating_add(1);
                    }
                    changed
                }
                CoreSourceDelta::Removed(_) => {
                    let existed = self.known_revisions.remove(&identity).is_some()
                        || self.known_events.contains_key(&identity);
                    if existed {
                        removed_sources = removed_sources.saturating_add(1);
                    }
                    existed
                }
            };
            if reconcile {
                reconcile_sources.push(CoreSourceReconciliation {
                    source_index: u32::try_from(source_index).unwrap(),
                    delta: delta.clone(),
                });
            }
        }
        let response = CoreSourceDeltaPageApplied {
            materialization_id: page.materialization_id.clone(),
            core_generation_id: if self.wrong_delta_generation {
                "f".repeat(64)
            } else {
                page.core_generation_id.clone()
            },
            page_index: page.page_index,
            changed_sources,
            removed_sources,
            reconcile_sources,
            replayed: self.replay_pages,
        };
        self.delta_pages.push(page);
        Ok(response)
    }

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage> {
        self.state_exchanges = self.state_exchanges.saturating_add(1);
        let source = request.reconciliation.delta.source();
        let after = request.after_event_id.map(|event| event.digest());
        let maximum = usize::try_from(request.maximum_items).unwrap();
        let all = self
            .known_events
            .get(&source.identity().digest())
            .into_iter()
            .flat_map(|events| events.values())
            .filter(|state| after.is_none_or(|after| state.event_id.digest() > after))
            .cloned()
            .collect::<Vec<_>>();
        let terminal = all.len() <= maximum;
        Ok(CoreEventStatePage {
            materialization_id: request.materialization_id,
            core_generation_id: request.core_generation_id,
            reconciliation: request.reconciliation,
            page_index: request.page_index,
            after_event_id: request.after_event_id,
            states: all.into_iter().take(maximum).collect(),
            terminal,
            replayed: self.replay_pages,
        })
    }

    fn apply_event_delta(
        &mut self,
        request: ApplyCoreEventDeltaPageRequest,
    ) -> Result<CoreEventDeltaPageApplied> {
        self.event_exchanges = self.event_exchanges.saturating_add(1);
        let page = request.page;
        let source = page.reconciliation.delta.source().clone();
        let mut additions = 0_u32;
        let mut replacements = 0_u32;
        let mut tombstones = 0_u32;
        for delta in &page.deltas {
            match delta {
                CoreEventDelta::Added(record) => {
                    let state = CoreEventState {
                        event_id: record.event_id,
                        core_record_sha256: core_record_sha256(record).unwrap(),
                        requires_replacement: false,
                    };
                    self.source_events_mut(&source)
                        .insert(state.event_id.digest(), state);
                    additions = additions.saturating_add(1);
                }
                CoreEventDelta::Replaced(replacement) => {
                    let state = CoreEventState {
                        event_id: replacement.record.event_id,
                        core_record_sha256: core_record_sha256(&replacement.record).unwrap(),
                        requires_replacement: false,
                    };
                    let prior = self
                        .source_events_mut(&source)
                        .insert(state.event_id.digest(), state);
                    assert_eq!(
                        prior.map(|state| state.core_record_sha256),
                        Some(replacement.prior_core_record_sha256.clone())
                    );
                    replacements = replacements.saturating_add(1);
                }
                CoreEventDelta::Tombstoned(tombstone) => {
                    let prior = self
                        .source_events_mut(&source)
                        .remove(&tombstone.event_id.digest());
                    assert_eq!(
                        prior.map(|state| state.core_record_sha256),
                        Some(tombstone.prior_core_record_sha256.clone())
                    );
                    tombstones = tombstones.saturating_add(1);
                }
            }
        }
        if page.terminal && matches!(page.reconciliation.delta, CoreSourceDelta::Removed(_)) {
            self.known_events.remove(&source.identity().digest());
        }
        let response = CoreEventDeltaPageApplied {
            materialization_id: page.materialization_id.clone(),
            core_generation_id: page.core_generation_id.clone(),
            source_index: page.reconciliation.source_index,
            page_index: if self.wrong_event_page_index {
                page.page_index.saturating_add(1)
            } else {
                page.page_index
            },
            additions,
            replacements,
            tombstones,
            terminal: page.terminal,
            replayed: self.replay_pages,
        };
        self.event_pages.push(page);
        Ok(response)
    }

    fn finish(
        &mut self,
        request: FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinished> {
        let response = CoreMaterializationFinished {
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
        };
        self.finish = Some(request);
        Ok(response)
    }
}

#[test]
fn one_event_change_emits_one_atomic_replacement_without_source_wide_sweep() {
    let temp = tempdir().unwrap();
    let source = source("changed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
    );
    writer.commit(|_| true).unwrap();
    let first = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&first, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&first, None, &mut consumer).unwrap();
    drop(first);
    consumer.event_pages.clear();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        2,
        vec![
            "one".to_owned(),
            "two revised".to_owned(),
            "three".to_owned(),
        ],
    );
    writer.commit(|_| true).unwrap();
    let second = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let report = sync_core_feed(&second, Some(&prior), &mut consumer).unwrap();

    assert_eq!(report.changed_sources, 1);
    assert_eq!(report.event_mutations, 1);
    assert_eq!(consumer.event_pages.len(), 1);
    assert!(consumer.event_pages[0].terminal);
    assert!(matches!(
        consumer.event_pages[0].deltas.as_slice(),
        [CoreEventDelta::Replaced(replacement)]
            if replacement.record.content.normalized_body.as_deref() == Some("two revised")
    ));
}

#[test]
fn unchanged_large_source_is_not_resent_when_another_source_changes() {
    let temp = tempdir().unwrap();
    let large = source("large.jsonl");
    let changed = source("changed.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &large, 1, vec!["L".repeat(32 * 1024)]);
    add_source(&mut writer, &changed, 2, vec!["changed body".to_owned()]);
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
    assert_eq!(report.event_mutations, 1);
    assert!(consumer.event_pages.iter().all(|page| {
        page.reconciliation
            .delta
            .source()
            .exact_descriptor_eq(&changed)
            && page.deltas.iter().all(|delta| match delta {
                CoreEventDelta::Added(record) => {
                    record.content.normalized_body.as_deref() == Some("changed body")
                }
                _ => false,
            })
    }));
}

#[test]
fn exact_replay_is_a_no_op_with_no_delta_or_event_pages() {
    let temp = tempdir().unwrap();
    let source = source("replay.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    consumer.replay_begin = true;

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert!(report.replayed);
    assert!(consumer.delta_pages.is_empty());
    assert!(consumer.event_pages.is_empty());
    let finish = consumer.finish.unwrap();
    assert_eq!(finish.changed_sources, 0);
    assert_eq!(finish.removed_sources, 0);
    assert_eq!(finish.event_mutations, 0);
}

#[test]
fn event_item_boundary_uses_one_exchange_per_bounded_page() {
    let temp = tempdir().unwrap();
    let source = source("item-boundary.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        (0..=MAX_CORE_EVENT_DELTA_PAGE_ITEMS)
            .map(|index| format!("body {index}"))
            .collect(),
    );
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(
        report.event_mutations,
        u64::try_from(MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1).unwrap()
    );
    assert_eq!(report.event_delta_pages, 2);
    assert_eq!(consumer.event_exchanges, 2);
    assert_eq!(
        consumer.event_pages[0].deltas.len(),
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS
    );
    assert_eq!(consumer.event_pages[1].deltas.len(), 1);
    assert!(!consumer.event_pages[0].terminal);
    assert!(consumer.event_pages[1].terminal);
}

#[test]
fn escaped_records_share_one_page_within_content_and_wire_bounds() {
    let temp = tempdir().unwrap();
    let source = source("encoded-boundary.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(
        &mut writer,
        &source,
        1,
        vec!["\"".repeat(5 * 1024 * 1024), "\"".repeat(5 * 1024 * 1024)],
    );
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.materialized_records, 2);
    assert_eq!(report.record_pages, 1);
    assert_eq!(consumer.record_exchanges, 1);
    assert_eq!(consumer.record_pages.len(), 1);
    let page = &consumer.record_pages[0];
    assert_eq!(page.records.len(), 2);
    assert_eq!(page.content_bytes().unwrap(), 10 * 1024 * 1024);
    assert!(encoded_record_bytes(page) > MAX_CORE_RECORD_PAGE_CONTENT_BYTES);
    assert!(encoded_record_bytes(page) <= MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES);
    assert!(encoded_page_bytes(page) <= MAX_CORE_RECORD_PAGE_WIRE_BYTES);
}

#[test]
fn escaped_single_record_above_content_byte_count_still_transports() {
    let temp = tempdir().unwrap();
    let source = source("overlarge-encoded.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source_with_count(&mut writer, &source, 1, vec!["\"".repeat(9 * 1024 * 1024)]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    assert_eq!(report.materialized_records, 1);
    assert_eq!(consumer.record_exchanges, 1);
    assert_eq!(consumer.record_pages.len(), 1);
    assert!(encoded_record_bytes(&consumer.record_pages[0]) > MAX_CORE_RECORD_PAGE_CONTENT_BYTES);
    assert!(
        encoded_record_bytes(&consumer.record_pages[0])
            <= MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES
    );
    assert!(encoded_page_bytes(&consumer.record_pages[0]) <= MAX_CORE_RECORD_PAGE_WIRE_BYTES);
}

#[test]
fn source_deletion_is_resumable_tombstone_pages() {
    let temp = tempdir().unwrap();
    let retained = source("retained.jsonl");
    let removed = source("removed.jsonl");
    let removed_count = MAX_CORE_EVENT_DELTA_PAGE_ITEMS + 1;
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &retained, 1, vec!["retained".to_owned()]);
    add_source(
        &mut writer,
        &removed,
        1,
        (0..removed_count)
            .map(|index| format!("removed {index}"))
            .collect(),
    );
    writer.commit(|_| true).unwrap();
    let prior_index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let prior = receipt_for(&prior_index, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&prior_index, None, &mut consumer).unwrap();
    drop(prior_index);
    consumer.event_pages.clear();

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

    let report = sync_core_feed(&index, Some(&prior), &mut consumer).unwrap();
    assert_eq!(report.changed_sources, 0);
    assert_eq!(report.removed_sources, 1);
    assert_eq!(
        report.event_mutations,
        u64::try_from(removed_count).unwrap()
    );
    assert_eq!(report.event_delta_pages, 2);
    assert!(consumer.event_pages.iter().all(|page| {
        page.reconciliation
            .delta
            .source()
            .exact_descriptor_eq(&removed)
            && page
                .deltas
                .iter()
                .all(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))
    }));
    assert!(!consumer
        .known_events
        .contains_key(&removed.identity().digest()));
}

#[test]
fn event_page_cas_mismatch_fails_closed_after_one_exchange() {
    let temp = tempdir().unwrap();
    let source = source("event-cas-mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    consumer.wrong_event_page_index = true;

    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert!(error.to_string().contains("acknowledgement"));
    assert_eq!(consumer.event_exchanges, 1);
}

#[test]
fn generation_mismatched_delta_ack_fails_closed() {
    let temp = tempdir().unwrap();
    let source = source("mismatch.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
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
fn producer_reads_only_pinned_core_records() {
    let source = include_str!("../core_materialization_feed.rs");
    for forbidden in ["ctx_history_capture", "reread_source"] {
        assert!(!source.contains(forbidden), "producer contains {forbidden}");
    }
    assert!(source.contains("core_source_event_page_with_budget"));
    assert!(!source.contains("MaterializeCoreRecordPage"));
}
