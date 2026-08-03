use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::hint::black_box;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CertifiedSourceDeletion,
    CertifiedSourceInventory, CoreRecord, EventIdentityInput, NativeItemKey, NativeSessionKey,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey,
    SourceObservation, TypedKey,
};
use ctx_history_index::{
    GenerationWriter, SourceRouteIdentity, SourceRouteSnapshot, WriterOptions,
};
use ctx_pro_host_protocol::{
    write_frame, ApplyCoreEventDeltaPageRequest, CoreEventDeltaPageApplied,
    CoreEventDeltaPagesApplied, HelperEnvelope, FRAME_HEADER_BYTES, MAX_CORE_CONTROL_WIRE_BYTES,
    MAX_FRAME_PAYLOAD_BYTES,
};
use tempfile::tempdir;
use uuid::Uuid;

use super::*;

#[path = "tests/batching_replay.rs"]
mod batching_replay;
#[path = "tests/materialization.rs"]
mod materialization;
#[path = "tests/prefetch.rs"]
mod prefetch;
#[path = "tests/status.rs"]
mod status;

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

fn single_source_index(name: &str, bodies: Vec<String>) -> (tempfile::TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let source = source(name);
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default()).unwrap();
    add_source(&mut writer, &source, 1, bodies);
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open_pinned(temp.path()).unwrap();
    (temp, index)
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
    replay_status_currentness: Option<CoreProjectionCurrentness>,
    replay_pages: bool,
    wrong_begin_materialization: bool,
    wrong_delta_generation: bool,
    wrong_acknowledgement_page_index: bool,
    source_response_mutation: Option<SourceResponseMutation>,
    wrong_event_page_index: bool,
    source_exchanges: u64,
    source_page_applications: u64,
    source_feed_terminal: bool,
    status_generation_override: Option<String>,
    pre_finish_status_error: Option<String>,
    event_exchanges: u64,
    event_exchange_page_ids: Vec<Vec<([u8; 32], u32)>>,
    state_exchanges: u64,
    source_acknowledgement_requests: Vec<(u32, u32)>,
    source_acknowledgements: BTreeMap<u32, Vec<CoreSourceDeltaPageApplied>>,
    event_state_error_after: Option<u64>,
    source_response_loss_after: Option<u64>,
    event_state_response_loss_after: Option<u64>,
    event_response_loss_after: Option<u64>,
    lose_finish_response: bool,
    delta_pages: Vec<CoreSourceDeltaPage>,
    state_requests: Vec<CoreEventStatePageRequest>,
    event_pages: Vec<CoreEventDeltaPage>,
    finish: Option<FinishCoreMaterializationRequest>,
    finish_requests: Vec<FinishCoreMaterializationRequest>,
    last_receipt: Option<CoreMaterializationReceipt>,
    status_exchanges: u64,
    core_preparation_peak_workers: u16,
    journal_finish_activity: JournalFinishActivity,
    state_journal:
        BTreeMap<(String, [u8; 32], u32), (CoreEventStatePageRequest, CoreEventStatePage)>,
    event_journal:
        BTreeMap<(String, [u8; 32], u32), (CoreEventDeltaPage, CoreEventDeltaPageApplied)>,
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

    fn apply_event_page(&mut self, page: CoreEventDeltaPage) -> CoreEventDeltaPageApplied {
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
        response
    }
}

impl CoreMaterializationConsumer for Consumer {
    fn status(&mut self, request: StatusRequest) -> Result<ctx_pro_host_protocol::StatusResult> {
        self.status_exchanges = self.status_exchanges.saturating_add(1);
        if self.finish.is_none() {
            if let Some(error) = &self.pre_finish_status_error {
                bail!(error.clone());
            }
        }
        let currentness = if self.finish.is_some() {
            CoreProjectionCurrentness::Current
        } else {
            self.replay_status_currentness
                .unwrap_or(CoreProjectionCurrentness::Current)
        };
        let request = StatusRequest {
            requested_core_generation_id: self
                .status_generation_override
                .clone()
                .or(request.requested_core_generation_id),
        };
        Ok(status::result(
            request,
            currentness,
            self.last_receipt.clone(),
            self.core_preparation_peak_workers,
            self.journal_finish_activity.clone(),
        ))
    }

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
        let materialization_id = if self.wrong_begin_materialization {
            "f".repeat(64)
        } else {
            ctx_pro_host_protocol::core_materialization_id(&request, &self.revision)
                .map_err(|error| anyhow!(error.message))?
        };
        Ok(CoreMaterializationBegan {
            materialization_id,
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
                bail!("invalid_request: divergent duplicate Core source delta page");
            }
            let mut response = responses
                .get(usize::try_from(acknowledgement_page_index).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| anyhow!("acknowledgement page is beyond terminal"))?;
            response.replayed = true;
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
        if self.source_response_loss_after == Some(self.source_exchanges) {
            bail!("synthetic_source_response_lost: committed Core source page");
        }
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
        self.state_requests.push(request.clone());
        if self.event_state_error_after == Some(self.state_exchanges) {
            bail!("synthetic_event_state_error: ordered coordinator failure");
        }
        let key = (
            request.materialization_id.clone(),
            request.reconciliation.delta.source().identity().digest(),
            request.page_index,
        );
        if let Some((prior_request, prior_response)) = self.state_journal.get(&key) {
            if prior_request != &request {
                bail!("invalid_request: divergent duplicate Core event state page");
            }
            let mut response = prior_response.clone();
            response.replayed = true;
            return Ok(response);
        }
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
        let response = CoreEventStatePage {
            materialization_id: request.materialization_id.clone(),
            core_generation_id: request.core_generation_id.clone(),
            reconciliation: request.reconciliation.clone(),
            page_index: request.page_index,
            after_event_id: request.after_event_id,
            states: all.into_iter().take(maximum).collect(),
            terminal,
            replayed: self.replay_pages,
        };
        self.state_journal.insert(key, (request, response.clone()));
        if self.event_state_response_loss_after == Some(self.state_exchanges) {
            bail!("synthetic_state_response_lost: committed Core event state page");
        }
        Ok(response)
    }

    fn apply_event_delta_pages(&mut self, pages: Vec<CoreEventDeltaPage>) -> Result<()> {
        if !self.source_feed_terminal {
            bail!("event delta applied before terminal source acknowledgement");
        }
        self.event_exchanges = self.event_exchanges.saturating_add(1);
        self.event_exchange_page_ids.push(
            pages
                .iter()
                .map(|page| {
                    (
                        page.reconciliation.delta.source().identity().digest(),
                        page.page_index,
                    )
                })
                .collect(),
        );
        self.event_pages.extend(pages.iter().cloned());
        let mut has_replayed_page = false;
        let mut has_new_page = false;
        for page in &pages {
            let key = (
                page.materialization_id.clone(),
                page.reconciliation.delta.source().identity().digest(),
                page.page_index,
            );
            match self.event_journal.get(&key) {
                Some((prior_page, _)) if prior_page == page => has_replayed_page = true,
                Some(_) => bail!("invalid_request: divergent duplicate Core event delta page"),
                None => has_new_page = true,
            }
        }
        if pages.len() > 1 && has_replayed_page && has_new_page {
            bail!("invalid_request: mixed replayed and new Core event delta pages");
        }

        let request = ApplyCoreEventDeltaPagesRequest { pages };
        let identity = request.acknowledgement_identity().unwrap();
        let mut responses = Vec::with_capacity(request.pages.len());
        for page in &request.pages {
            let key = (
                page.materialization_id.clone(),
                page.reconciliation.delta.source().identity().digest(),
                page.page_index,
            );
            if let Some((_, prior_response)) = self.event_journal.get(&key) {
                let mut response = prior_response.clone();
                response.replayed = true;
                responses.push(response);
            } else {
                let response = self.apply_event_page(page.clone());
                self.event_journal
                    .insert(key, (page.clone(), response.clone()));
                responses.push(response);
            }
        }
        if self.event_response_loss_after == Some(self.event_exchanges) {
            bail!("synthetic_event_response_lost: committed Core event delta exchange");
        }
        CoreEventDeltaPagesApplied { pages: responses }
            .validate_for_identity(&identity)
            .map_err(|error| anyhow!("invalid_response: {}", error.message))
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
        self.last_receipt = Some(response.receipt.clone());
        self.finish_requests.push(request.clone());
        self.finish = Some(request);
        if self.lose_finish_response {
            self.lose_finish_response = false;
            bail!("synthetic_finish_response_lost: committed Core finish");
        }
        Ok(response)
    }
}

const TEST_MATERIALIZATION_ID: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";
const TEST_GENERATION_ID: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn reconciliation(source: &SourceKey) -> CoreSourceReconciliation {
    CoreSourceReconciliation {
        materialize_index: 0,
        delta: CoreSourceDelta::Present(CoreSourceState {
            source: source.clone(),
            core_record_accumulator: "3".repeat(64),
            event_count: 0,
        }),
    }
}

fn sorted_additions(source: &SourceKey, bodies: Vec<String>) -> Vec<CoreEventDelta> {
    let mut deltas = bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| {
            CoreEventDelta::Added(record(source, u64::try_from(index + 1).unwrap(), body))
        })
        .collect::<Vec<_>>();
    deltas.sort_by_key(|delta| delta.event_id().digest());
    deltas
}

fn legacy_event_delta_pages(
    reconciliation: &CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
) -> Result<Vec<CoreEventDeltaPage>> {
    let mut pending = Vec::new();
    let mut pages = Vec::new();
    let mut page_index = 0_u32;
    for delta in deltas {
        if pending.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            pages.push(event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
                false,
                std::mem::take(&mut pending),
            )?);
            page_index = page_index.checked_add(1).unwrap();
        }
        pending.push(delta);
        if event_delta_page(
            TEST_MATERIALIZATION_ID,
            TEST_GENERATION_ID,
            reconciliation,
            page_index,
            false,
            pending.clone(),
        )
        .is_err()
        {
            let overflow = pending.pop().unwrap();
            if pending.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core event delta exceeds its page bound"
                ));
            }
            pages.push(event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
                false,
                std::mem::take(&mut pending),
            )?);
            page_index = page_index.checked_add(1).unwrap();
            pending.push(overflow);
        }
    }
    pages.push(event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        reconciliation,
        page_index,
        true,
        pending,
    )?);
    Ok(pages)
}

fn incremental_event_delta_pages(
    reconciliation: &CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
) -> Result<Vec<CoreEventDeltaPage>> {
    let mut page_index = 0_u32;
    let mut pending = EventDeltaPageBuilder::new(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        reconciliation,
        page_index,
    )?;
    let mut pages = Vec::new();
    for delta in deltas {
        if pending.is_full() {
            let (deltas, _) = pending.into_deltas_with_wire_bytes(false)?;
            let deltas = deltas
                .into_iter()
                .map(PreparedEventDelta::into_typed)
                .collect();
            pages.push(event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
                false,
                deltas,
            )?);
            page_index = page_index.checked_add(1).unwrap();
            pending = EventDeltaPageBuilder::new(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
            )?;
        }
        if let Some(overflow) = pending.try_push(PreparedEventDelta::from_typed(delta)?)? {
            if pending.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core event delta exceeds its page bound"
                ));
            }
            let (deltas, _) = pending.into_deltas_with_wire_bytes(false)?;
            let deltas = deltas
                .into_iter()
                .map(PreparedEventDelta::into_typed)
                .collect();
            pages.push(event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
                false,
                deltas,
            )?);
            page_index = page_index.checked_add(1).unwrap();
            pending = EventDeltaPageBuilder::new(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                reconciliation,
                page_index,
            )?;
            pending.push_split_overflow(overflow)?;
        }
    }
    let (deltas, _) = pending.into_deltas_with_wire_bytes(true)?;
    let deltas = deltas
        .into_iter()
        .map(PreparedEventDelta::into_typed)
        .collect();
    pages.push(event_delta_page(
        TEST_MATERIALIZATION_ID,
        TEST_GENERATION_ID,
        reconciliation,
        page_index,
        true,
        deltas,
    )?);
    Ok(pages)
}

fn assert_exact_page_boundary_equivalence(
    reconciliation: &CoreSourceReconciliation,
    deltas: Vec<CoreEventDelta>,
) {
    let legacy = legacy_event_delta_pages(reconciliation, deltas.clone()).unwrap();
    let incremental = incremental_event_delta_pages(reconciliation, deltas).unwrap();
    assert_eq!(incremental, legacy);

    let legacy_requests = legacy
        .into_iter()
        .map(|page| serde_json::to_vec(&ApplyCoreEventDeltaPageRequest { page }).unwrap())
        .collect::<Vec<_>>();
    let incremental_requests = incremental
        .into_iter()
        .map(|page| {
            let request = ApplyCoreEventDeltaPageRequest { page };
            request.validate().unwrap();
            serde_json::to_vec(&request).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(incremental_requests, legacy_requests);
}

fn single_delta_event_pages(
    source: &SourceKey,
    page_count: usize,
    body_bytes: usize,
) -> Vec<CoreEventDeltaPage> {
    let reconciliation = reconciliation(source);
    let deltas = sorted_additions(source, vec!["x".repeat(body_bytes); page_count]);
    deltas
        .into_iter()
        .enumerate()
        .map(|(index, delta)| {
            event_delta_page(
                TEST_MATERIALIZATION_ID,
                TEST_GENERATION_ID,
                &reconciliation,
                u32::try_from(index).unwrap(),
                index + 1 == page_count,
                vec![delta],
            )
            .unwrap()
        })
        .collect()
}

fn applied_page(page: &CoreEventDeltaPage) -> CoreEventDeltaPageApplied {
    CoreEventDeltaPageApplied {
        materialization_id: page.materialization_id.clone(),
        core_generation_id: page.core_generation_id.clone(),
        source: page.reconciliation.delta.source().clone(),
        page_index: page.page_index,
        additions: u32::try_from(
            page.deltas
                .iter()
                .filter(|delta| matches!(delta, CoreEventDelta::Added(_)))
                .count(),
        )
        .unwrap(),
        replacements: u32::try_from(
            page.deltas
                .iter()
                .filter(|delta| matches!(delta, CoreEventDelta::Replaced(_)))
                .count(),
        )
        .unwrap(),
        tombstones: u32::try_from(
            page.deltas
                .iter()
                .filter(|delta| matches!(delta, CoreEventDelta::Tombstoned(_)))
                .count(),
        )
        .unwrap(),
        terminal: page.terminal,
        replayed: false,
    }
}

fn successful_plural_response(message: &HostMessage) -> HelperMessage {
    let HostMessage::ApplyCoreEventDeltaPages(request) = message else {
        panic!("expected plural Core event delta request")
    };
    HelperMessage::CoreEventDeltaPagesApplied(CoreEventDeltaPagesApplied {
        pages: request.pages.iter().map(applied_page).collect(),
    })
}

fn event_page_batches(pages: Vec<CoreEventDeltaPage>) -> Vec<Vec<CoreEventDeltaPage>> {
    let mut builder = EventDeltaPageBatchBuilder::new().unwrap();
    let mut batches = Vec::new();
    for page in pages {
        let terminal = page.terminal;
        let page = prepared_event_delta_page_from_typed(page).unwrap();
        if let Some(overflow) = builder.try_push(page).unwrap() {
            batches.push(builder.take_request().unwrap().into_typed_pages());
            builder.push_empty_overflow(overflow).unwrap();
        }
        if terminal {
            batches.push(builder.take_request().unwrap().into_typed_pages());
        }
    }
    assert!(builder.is_empty());
    batches
}
