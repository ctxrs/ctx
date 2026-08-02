use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey,
    SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_pro_host_protocol::{
    write_frame, HelperEnvelope, FRAME_HEADER_BYTES, MAX_CORE_CONTROL_WIRE_BYTES,
    MAX_FRAME_PAYLOAD_BYTES,
};
use tempfile::tempdir;
use uuid::Uuid;

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

fn large_native_key_source(index: usize) -> SourceKey {
    const NATIVE_KEY_BYTES: usize = 48 * 1024;
    let prefix = format!("removal-{index:04}-");
    let mut native_key = prefix.clone();
    native_key.push_str(&"x".repeat(NATIVE_KEY_BYTES - prefix.len()));
    source(&native_key)
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

fn source_delta(index: usize, anchor_bytes: usize, schema_variant_bytes: usize) -> CoreSourceDelta {
    let suffix = format!("-{index:05}");
    let anchor = format!("{}{}", "x".repeat(anchor_bytes - suffix.len()), suffix);
    let source = SourceKey::derive(
        "fixture",
        "fixture_jsonl",
        "s".repeat(schema_variant_bytes),
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(anchor).unwrap()).unwrap(),
    )
    .unwrap();
    CoreSourceDelta::Present(CoreSourceState {
        source,
        core_record_accumulator: "a".repeat(64),
        event_count: 1,
    })
}

fn ordered_source_deltas(count: usize, anchor_bytes: usize) -> Vec<CoreSourceDelta> {
    let encoded_lengths = (0..count)
        .map(|index| {
            serde_json::to_vec(&source_delta(index, anchor_bytes, 1))
                .unwrap()
                .len()
        })
        .collect::<Vec<_>>();
    let maximum_encoded_length = encoded_lengths.iter().copied().max().unwrap();
    let mut deltas = encoded_lengths
        .into_iter()
        .enumerate()
        .map(|(index, encoded_length)| {
            source_delta(
                index,
                anchor_bytes,
                1 + maximum_encoded_length - encoded_length,
            )
        })
        .collect::<Vec<_>>();
    assert!(deltas
        .iter()
        .all(|delta| serde_json::to_vec(delta).unwrap().len() == maximum_encoded_length));
    deltas.sort_by_key(|delta| delta.source().identity().digest());
    deltas
}

type EventStates = BTreeMap<[u8; 32], BTreeMap<[u8; 32], CoreEventState>>;

#[derive(Clone, Copy)]
enum SourceResponseMutation {
    DuplicateSource,
    StalePresent,
    RemoveCurrent,
    SkipMaterializeIndex,
}

#[derive(Default)]
struct Consumer {
    revision: String,
    known_accumulators: BTreeMap<[u8; 32], String>,
    known_sources: BTreeMap<[u8; 32], SourceKey>,
    known_events: EventStates,
    seen_sources: BTreeSet<[u8; 32]>,
    next_materialize_index: u32,
    replay_begin: bool,
    replay_pages: bool,
    wrong_delta_generation: bool,
    wrong_acknowledgement_page_index: bool,
    source_response_mutation: Option<SourceResponseMutation>,
    wrong_event_page_index: bool,
    source_exchanges: u64,
    source_page_applications: u64,
    source_feed_terminal: bool,
    event_exchanges: u64,
    state_exchanges: u64,
    source_acknowledgement_requests: Vec<(u32, u32)>,
    source_acknowledgements: BTreeMap<u32, Vec<CoreSourceDeltaPageApplied>>,
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
        if !self.replay_begin {
            self.seen_sources.clear();
            self.next_materialize_index = 0;
            self.source_feed_terminal = false;
            self.source_acknowledgement_requests.clear();
            self.source_acknowledgements.clear();
            self.delta_pages.clear();
        }
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
        request.validate().map_err(|error| anyhow!(error.message))?;
        let page = request.page;
        assert!(serde_json::to_vec(&page).unwrap().len() <= MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES);
        let acknowledgement_page_index = request.acknowledgement_page_index;
        self.source_exchanges = self.source_exchanges.saturating_add(1);
        self.source_acknowledgement_requests
            .push((page.page_index, acknowledgement_page_index));
        if let Some(responses) = self.source_acknowledgements.get(&page.page_index) {
            let original = self
                .delta_pages
                .iter()
                .find(|original| original.page_index == page.page_index)
                .ok_or_else(|| anyhow!("cached source page has no original request"))?;
            if original != &page {
                bail!("source page replay changed the original request");
            }
            let response = responses
                .get(usize::try_from(acknowledgement_page_index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| anyhow!("acknowledgement page is beyond terminal"))?;
            if page.terminal && response.acknowledgement_terminal {
                self.source_feed_terminal = true;
            }
            return Ok(response);
        }
        if acknowledgement_page_index != 0 {
            bail!("first acknowledgement request for a source page must be zero");
        }
        self.source_page_applications = self.source_page_applications.saturating_add(1);
        let mut reconcile_sources = Vec::new();
        for delta in &page.deltas {
            let identity = delta.source().identity().digest();
            let reconcile = match delta {
                CoreSourceDelta::Present(state) => {
                    self.seen_sources.insert(identity);
                    self.known_sources.insert(identity, state.source.clone());
                    let changed = self.known_accumulators.get(&identity)
                        != Some(&state.core_record_accumulator);
                    self.known_accumulators
                        .insert(identity, state.core_record_accumulator.clone());
                    changed
                }
                CoreSourceDelta::Removed(_) => {
                    panic!("host source snapshots cannot carry removals")
                }
            };
            if reconcile {
                reconcile_sources.push(CoreSourceReconciliation {
                    materialize_index: self.next_materialize_index,
                    delta: delta.clone(),
                });
                self.next_materialize_index = self.next_materialize_index.saturating_add(1);
            }
        }
        if page.terminal {
            let unseen = self
                .known_sources
                .iter()
                .filter(|(identity, _)| !self.seen_sources.contains(*identity))
                .map(|(identity, source)| (*identity, source.clone()))
                .collect::<Vec<_>>();
            for (identity, source) in unseen {
                self.known_accumulators.remove(&identity);
                reconcile_sources.push(CoreSourceReconciliation {
                    materialize_index: self.next_materialize_index,
                    delta: CoreSourceDelta::Removed(CoreSourceRemoval { source }),
                });
                self.next_materialize_index = self.next_materialize_index.saturating_add(1);
            }
        }
        let acknowledgement_page_count = reconcile_sources
            .len()
            .div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
            .max(1);
        let mut responses = (0..acknowledgement_page_count)
            .map(|index| {
                let start = index.saturating_mul(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
                let end = start
                    .saturating_add(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS)
                    .min(reconcile_sources.len());
                let page_reconciliations = reconcile_sources[start..end].to_vec();
                let changed_sources = u32::try_from(
                    page_reconciliations
                        .iter()
                        .filter(|item| matches!(item.delta, CoreSourceDelta::Present(_)))
                        .count(),
                )
                .unwrap();
                let removed_sources = u32::try_from(
                    page_reconciliations
                        .iter()
                        .filter(|item| matches!(item.delta, CoreSourceDelta::Removed(_)))
                        .count(),
                )
                .unwrap();
                CoreSourceDeltaPageApplied {
                    materialization_id: page.materialization_id.clone(),
                    core_generation_id: if self.wrong_delta_generation {
                        "f".repeat(64)
                    } else {
                        page.core_generation_id.clone()
                    },
                    page_index: page.page_index,
                    acknowledgement_page_index: if self.wrong_acknowledgement_page_index {
                        u32::try_from(index).unwrap().saturating_add(1)
                    } else {
                        u32::try_from(index).unwrap()
                    },
                    acknowledgement_terminal: index + 1 == acknowledgement_page_count,
                    changed_sources,
                    removed_sources,
                    reconcile_sources: page_reconciliations,
                    replayed: self.replay_pages,
                }
            })
            .collect::<Vec<_>>();
        if let Some(mutation) = self.source_response_mutation {
            let first_response = &mut responses[0];
            match mutation {
                SourceResponseMutation::DuplicateSource => {
                    let mut duplicate = first_response.reconcile_sources[0].clone();
                    duplicate.materialize_index = duplicate.materialize_index.saturating_add(1);
                    first_response.reconcile_sources.push(duplicate);
                    first_response.changed_sources =
                        first_response.changed_sources.saturating_add(1);
                }
                SourceResponseMutation::StalePresent => {
                    let CoreSourceDelta::Present(state) =
                        &mut first_response.reconcile_sources[0].delta
                    else {
                        panic!("expected present source reconciliation");
                    };
                    state.core_record_accumulator = "f".repeat(64);
                }
                SourceResponseMutation::RemoveCurrent => {
                    let current = first_response.reconcile_sources[0].delta.source().clone();
                    first_response.reconcile_sources[0].delta =
                        CoreSourceDelta::Removed(CoreSourceRemoval { source: current });
                    first_response.changed_sources =
                        first_response.changed_sources.saturating_sub(1);
                    first_response.removed_sources =
                        first_response.removed_sources.saturating_add(1);
                }
                SourceResponseMutation::SkipMaterializeIndex => {
                    first_response.reconcile_sources[0].materialize_index = first_response
                        .reconcile_sources[0]
                        .materialize_index
                        .saturating_add(1);
                }
            }
        }
        let response = responses[0].clone();
        self.delta_pages.push(page.clone());
        self.source_acknowledgements
            .insert(page.page_index, responses);
        if page.terminal && response.acknowledgement_terminal {
            self.source_feed_terminal = true;
        }
        Ok(response)
    }

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage> {
        if !self.source_feed_terminal {
            bail!("event state requested before terminal source acknowledgement");
        }
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
        if !self.source_feed_terminal {
            bail!("event delta applied before terminal source acknowledgement");
        }
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
            self.known_sources.remove(&source.identity().digest());
        }
        let response = CoreEventDeltaPageApplied {
            materialization_id: page.materialization_id.clone(),
            core_generation_id: page.core_generation_id.clone(),
            source: page.reconciliation.delta.source().clone(),
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
fn source_page_admission_uses_exact_wire_index_at_decimal_transitions() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);

    for transition in [9_u32, 99] {
        let count = usize::try_from(transition).unwrap() * 2 + 5;
        let deltas = ordered_source_deltas(count, 32);
        let pair_start = usize::try_from(transition).unwrap() * 2;
        let exact_boundary_page = CoreSourceDeltaPage::new(
            &materialization_id,
            &generation_id,
            transition,
            false,
            deltas[pair_start..pair_start + 2].to_vec(),
        )
        .unwrap();
        let maximum_wire_bytes = serde_json::to_vec(&exact_boundary_page).unwrap().len();

        let pages = build_delta_pages_with_wire_bound(
            &materialization_id,
            &generation_id,
            deltas.clone(),
            maximum_wire_bytes,
        )
        .unwrap();
        let repeated = build_delta_pages_with_wire_bound(
            &materialization_id,
            &generation_id,
            deltas,
            maximum_wire_bytes,
        )
        .unwrap();

        assert_eq!(pages, repeated);
        assert_eq!(
            pages[usize::try_from(transition).unwrap()],
            exact_boundary_page
        );
        assert_eq!(
            pages[usize::try_from(transition + 1).unwrap()].deltas.len(),
            1
        );
        assert_eq!(
            pages[usize::try_from(transition + 2).unwrap()].deltas.len(),
            2
        );
        assert_eq!(
            serde_json::to_vec(&pages[usize::try_from(transition + 2).unwrap()])
                .unwrap()
                .len(),
            maximum_wire_bytes
        );
        assert_eq!(
            pages.last().map(|page| page.page_index),
            Some(transition + 2)
        );
        for (expected_index, page) in pages.iter().enumerate() {
            assert_eq!(page.page_index, u32::try_from(expected_index).unwrap());
            assert_eq!(page.terminal, expected_index + 1 == pages.len());
            assert!(serde_json::to_vec(page).unwrap().len() <= maximum_wire_bytes);
            page.validate().unwrap();
        }
    }
}

#[test]
fn source_page_admission_enforces_the_protocol_wire_bound() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let deltas = ordered_source_deltas(140, 60 * 1024);

    assert_eq!(MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES, 4_194_304);
    let pages = build_delta_pages(&materialization_id, &generation_id, deltas).unwrap();

    assert!(pages.len() > 1);
    for (index, page) in pages.iter().enumerate() {
        let wire_bytes = serde_json::to_vec(page).unwrap().len();
        assert!(wire_bytes <= MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES);
        assert_eq!(page.page_index, u32::try_from(index).unwrap());
        assert_eq!(page.terminal, index + 1 == pages.len());
        page.validate().unwrap();
        if let Some(next) = pages.get(index + 1) {
            let next_delta_bytes = serde_json::to_vec(&next.deltas[0]).unwrap().len();
            assert!(
                page.deltas.len() == MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
                    || wire_bytes + 1 + next_delta_bytes > MAX_CORE_SOURCE_DELTA_PAGE_WIRE_BYTES
            );
        }
    }
}

#[test]
fn source_page_admission_rejects_an_oversized_singleton_with_typed_error() {
    let materialization_id = "d".repeat(64);
    let generation_id = "a".repeat(64);
    let delta = source_delta(0, 32, 1);
    let exact_singleton_bytes = serde_json::to_vec(
        &CoreSourceDeltaPage::new(
            &materialization_id,
            &generation_id,
            0,
            true,
            vec![delta.clone()],
        )
        .unwrap(),
    )
    .unwrap()
    .len();

    let error = build_delta_pages_with_wire_bound(
        &materialization_id,
        &generation_id,
        vec![delta],
        exact_singleton_bytes - 1,
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        CoreSourceDeltaPageBuildError::OversizedSingleton
    ));
    assert_eq!(
        error.to_string(),
        "invalid_request: one Core source delta exceeds its wire bound"
    );
}

#[test]
fn same_certificate_and_count_with_changed_core_record_is_reconciled() {
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
    let first_certificate = first.manifest().sources[0].clone();
    let first_accumulator = first.manifest().core_record_aggregates[0]
        .core_record_accumulator()
        .to_owned();
    let prior = receipt_for(&first, "test-core-materializer-v1");
    let mut consumer = Consumer::new();
    sync_core_feed(&first, None, &mut consumer).unwrap();
    drop(first);
    consumer.event_pages.clear();

    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(
        &mut writer,
        &source,
        1,
        vec![
            "one".to_owned(),
            "two revised".to_owned(),
            "three".to_owned(),
        ],
    );
    writer.commit(|_| true).unwrap();
    let second = VerifiedIndex::open_pinned(temp.path()).unwrap();
    assert_eq!(second.manifest().sources[0], first_certificate);
    assert_ne!(
        second.manifest().core_record_aggregates[0].core_record_accumulator(),
        first_accumulator
    );
    let second_sources = core_source_states(second.manifest()).unwrap();
    let second_head = core_generation_head(&second, &second_sources).unwrap();
    assert_ne!(
        prior.source_snapshot_sha256,
        second_head.source_snapshot_sha256
    );
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
    consumer.known_accumulators.insert(
        large.identity().digest(),
        large_state.core_record_accumulator.clone(),
    );
    consumer
        .known_accumulators
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
fn large_native_key_removals_without_receipt_drive_all_acknowledgements_idempotently() {
    const REMOVAL_COUNT: usize = 2_048;
    let temp = tempdir().unwrap();
    let writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let mut consumer = Consumer::new();
    for source_index in 0..REMOVAL_COUNT {
        let removed = large_native_key_source(source_index);
        consumer
            .known_sources
            .insert(removed.identity().digest(), removed);
    }

    let report = sync_core_feed(&index, None, &mut consumer).unwrap();
    let expected_acknowledgement_pages = REMOVAL_COUNT.div_ceil(MAX_CORE_SOURCE_DELTA_PAGE_ITEMS);
    assert_eq!(report.changed_sources, 0);
    assert_eq!(
        report.removed_sources,
        u64::try_from(REMOVAL_COUNT).unwrap()
    );
    assert_eq!(consumer.source_page_applications, 1);
    assert_eq!(
        consumer.finish.as_ref().unwrap().removed_sources,
        u32::try_from(REMOVAL_COUNT).unwrap()
    );
    assert_eq!(
        consumer.source_exchanges,
        u64::try_from(expected_acknowledgement_pages).unwrap()
    );
    assert_eq!(
        consumer.source_acknowledgement_requests,
        (0..expected_acknowledgement_pages)
            .map(|index| (0, u32::try_from(index).unwrap()))
            .collect::<Vec<_>>()
    );
    let responses = consumer.source_acknowledgements.get(&0).unwrap();
    assert_eq!(responses.len(), expected_acknowledgement_pages);
    assert_eq!(
        responses
            .iter()
            .map(|response| usize::try_from(response.removed_sources).unwrap())
            .sum::<usize>(),
        REMOVAL_COUNT
    );
    for (index, response) in responses.iter().enumerate() {
        assert_eq!(
            response.acknowledgement_page_index,
            u32::try_from(index).unwrap()
        );
        assert_eq!(
            response.acknowledgement_terminal,
            index + 1 == expected_acknowledgement_pages
        );
        assert_eq!(
            response.reconcile_sources.len(),
            MAX_CORE_SOURCE_DELTA_PAGE_ITEMS
        );
        assert!(serde_json::to_vec(response).unwrap().len() <= MAX_CORE_CONTROL_WIRE_BYTES);
        let mut frame = Vec::new();
        write_frame(
            &mut frame,
            &HelperEnvelope {
                sequence: u64::try_from(index).unwrap(),
                request_id: Uuid::from_u128(1),
                message: HelperMessage::CoreSourceDeltaPageApplied(response.clone()),
            },
        )
        .unwrap();
        assert!(frame.len() - FRAME_HEADER_BYTES <= MAX_FRAME_PAYLOAD_BYTES);
    }

    let source_page = consumer.delta_pages[0].clone();
    let original_responses = responses.clone();
    for (index, expected) in original_responses.into_iter().enumerate() {
        let replayed = consumer
            .apply_source_delta(ApplyCoreSourceDeltaPageRequest {
                page: source_page.clone(),
                acknowledgement_page_index: u32::try_from(index).unwrap(),
            })
            .unwrap();
        assert_eq!(replayed, expected);
    }
    assert_eq!(consumer.source_page_applications, 1);
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
fn event_page_is_authoritatively_validated_before_exchange() {
    let source = source("invalid-page.jsonl");
    let page = CoreEventDeltaPage {
        materialization_id: "d".repeat(64),
        core_generation_id: "a".repeat(64),
        reconciliation: CoreSourceReconciliation {
            materialize_index: 0,
            delta: CoreSourceDelta::Present(CoreSourceState {
                source,
                core_record_accumulator: "0".repeat(64),
                event_count: 0,
            }),
        },
        page_index: 0,
        terminal: false,
        deltas: Vec::new(),
    };
    let mut consumer = Consumer::new();

    let error = send_event_delta_page(&mut consumer, page).unwrap_err();
    assert!(error.to_string().contains("invalid_request"));
    assert_eq!(consumer.event_exchanges, 0);
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
fn source_acknowledgement_sequence_and_global_identity_fail_closed() {
    let temp = tempdir().unwrap();
    let source = source("invalid-source-ack.jsonl");
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, vec!["body".to_owned()]);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    for (mutation, expected) in [
        (
            SourceResponseMutation::DuplicateSource,
            "repeat a stable source identity",
        ),
        (SourceResponseMutation::StalePresent, "stale current source"),
        (
            SourceResponseMutation::RemoveCurrent,
            "removes a current source",
        ),
        (
            SourceResponseMutation::SkipMaterializeIndex,
            "indices are not contiguous",
        ),
    ] {
        let mut consumer = Consumer::new();
        consumer.source_response_mutation = Some(mutation);
        let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
        assert_eq!(consumer.source_exchanges, 1);
        assert_eq!(consumer.state_exchanges, 0);
        assert_eq!(consumer.event_exchanges, 0);
    }

    let mut consumer = Consumer::new();
    consumer.wrong_acknowledgement_page_index = true;
    let error = sync_core_feed(&index, None, &mut consumer).unwrap_err();
    assert!(error.to_string().contains("page CAS"), "{error:#}");
    assert_eq!(consumer.source_exchanges, 1);
    assert_eq!(consumer.state_exchanges, 0);
    assert_eq!(consumer.event_exchanges, 0);
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
