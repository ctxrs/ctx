use std::collections::{BTreeSet, VecDeque};

use anyhow::{anyhow, bail, Result};
use ctx_history_core::{CertifiedSource, MAX_ENCODED_CORE_RECORD_BYTES};
use ctx_history_index::{
    CoreEventPageBudget, GenerationManifest, SourceEventCursor, VerifiedIndex,
};
use ctx_pro_host_protocol::{
    core_record_sha256, ApplyCoreEventDeltaPageRequest, ApplyCoreSourceDeltaPageRequest,
    BeginCoreMaterializationRequest, Capability, CoreEventDelta, CoreEventDeltaPage,
    CoreEventDeltaPageApplied, CoreEventReplacement, CoreEventState, CoreEventStatePage,
    CoreEventStatePageRequest, CoreEventTombstone, CoreGenerationHead, CoreMaterializationBegan,
    CoreMaterializationFinished, CoreMaterializationReceipt, CoreMaterializationReceiptIdentity,
    CoreSourceDelta, CoreSourceDeltaPage, CoreSourceDeltaPageApplied, CoreSourceReconciliation,
    CoreSourceRemoval, CoreSourceState, FinishCoreMaterializationRequest, HelperMessage,
    HostMessage, StatusRequest, MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_ITEMS, MAX_CORE_EVENT_STATE_PAGE_ITEMS,
    MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
};
use sha2::{Digest, Sha256};

use super::*;

// Complete content remains capped at 16 MiB. JSON escaping can expand one
// otherwise-valid Core record, so encoded paging admits Core's validated
// singleton maximum while the protocol's larger wire bound covers the
// envelope.
const MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES: usize = MAX_ENCODED_CORE_RECORD_BYTES;
const CORE_RECORD_PAGE_BUDGET: CoreEventPageBudget = CoreEventPageBudget::new(
    MAX_CORE_RECORD_PAGE_ENCODED_PAYLOAD_BYTES,
    MAX_CORE_EVENT_DELTA_PAGE_CONTENT_BYTES,
);

#[derive(Debug, Clone)]
pub(super) struct CoreMaterializationSyncReport {
    pub(super) receipt: CoreMaterializationReceipt,
    #[cfg(test)]
    pub(super) changed_sources: u64,
    #[cfg(test)]
    pub(super) removed_sources: u64,
    #[cfg(test)]
    pub(super) event_delta_pages: u64,
    #[cfg(test)]
    pub(super) event_mutations: u64,
    pub(super) replayed: bool,
}

trait CoreMaterializationConsumer {
    fn begin(
        &mut self,
        request: BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan>;

    fn apply_source_delta(
        &mut self,
        request: ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied>;

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage>;

    fn apply_event_delta(
        &mut self,
        request: ApplyCoreEventDeltaPageRequest,
    ) -> Result<CoreEventDeltaPageApplied>;

    fn finish(
        &mut self,
        request: FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinished>;
}

struct ProtocolCoreMaterializationConsumer {
    client: ProClient,
}

impl ProtocolCoreMaterializationConsumer {
    fn exchange(&mut self, message: HostMessage) -> Result<HelperMessage> {
        self.client.exchange(message, BATCH_TIMEOUT)
    }
}

impl CoreMaterializationConsumer for ProtocolCoreMaterializationConsumer {
    fn begin(
        &mut self,
        request: BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan> {
        match self.exchange(HostMessage::BeginCoreMaterialization(request))? {
            HelperMessage::CoreMaterializationBegan(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-begin response"),
        }
    }

    fn apply_source_delta(
        &mut self,
        request: ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied> {
        match self.exchange(HostMessage::ApplyCoreSourceDeltaPage(request))? {
            HelperMessage::CoreSourceDeltaPageApplied(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-delta response"),
        }
    }

    fn event_states(&mut self, request: CoreEventStatePageRequest) -> Result<CoreEventStatePage> {
        match self.exchange(HostMessage::CoreEventStatePage(request))? {
            HelperMessage::CoreEventStatePage(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-event-state response"),
        }
    }

    fn apply_event_delta(
        &mut self,
        request: ApplyCoreEventDeltaPageRequest,
    ) -> Result<CoreEventDeltaPageApplied> {
        match self.exchange(HostMessage::ApplyCoreEventDeltaPage(request))? {
            HelperMessage::CoreEventDeltaPageApplied(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-event-delta response"),
        }
    }

    fn finish(
        &mut self,
        request: FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinished> {
        match self.exchange(HostMessage::FinishCoreMaterialization(request))? {
            HelperMessage::CoreMaterializationFinished(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-finish response"),
        }
    }
}

pub(super) fn sync_generation_pinned_core(
    data_root: &Path,
    index: &VerifiedIndex,
) -> Result<CoreMaterializationSyncReport> {
    let required = BTreeSet::from([Capability::Status, Capability::CoreMaterialization]);
    let mut consumer = ProtocolCoreMaterializationConsumer {
        client: ProClient::connect(data_root, &required)?,
    };
    let status = match consumer.exchange(HostMessage::Status(StatusRequest {
        requested_core_generation_id: Some(index.generation_id().to_owned()),
    }))? {
        HelperMessage::Status(status) => {
            status
                .validate()
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            status
        }
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-status response"),
    };
    sync_core_feed(index, status.core_receipt.as_ref(), &mut consumer)
}

fn sync_core_feed<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    prior_receipt: Option<&CoreMaterializationReceipt>,
    consumer: &mut C,
) -> Result<CoreMaterializationSyncReport> {
    let sources = core_source_states(index.manifest())?;
    let head = core_generation_head(index, &sources)?;
    if head.core_generation_id != index.generation_id() {
        bail!(
            "core_generation_mismatch: generation head {} does not match pinned Core {}",
            head.core_generation_id,
            index.generation_id()
        );
    }
    if let Some(receipt) = prior_receipt {
        receipt
            .validate()
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    }
    let expected_prior_receipt = prior_receipt
        .map(CoreMaterializationReceiptIdentity::from_receipt)
        .transpose()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let begin = BeginCoreMaterializationRequest {
        head: head.clone(),
        expected_prior_receipt: expected_prior_receipt.clone(),
    };
    let begin_identity = begin
        .acknowledgement_identity()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let began = consumer.begin(begin)?;
    began
        .validate_for_identity(&begin_identity)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;

    let mut source_delta_pages = 0_u32;
    let mut changed_sources = 0_u32;
    let mut removed_sources = 0_u32;
    let mut event_mutations = 0_u64;
    let mut event_delta_pages = 0_u64;

    if !began.replayed {
        let deltas = core_snapshot_deltas(index.manifest(), &sources)?;
        let delta_pages =
            build_delta_pages(&began.materialization_id, index.generation_id(), deltas)?;
        source_delta_pages = u32::try_from(delta_pages.len())
            .map_err(|_| anyhow!("invalid_request: Core delta page count overflowed"))?;
        let mut reconcile_sources = Vec::new();
        for page in delta_pages {
            let acknowledgement_identity = page.acknowledgement_identity();
            let request = ApplyCoreSourceDeltaPageRequest { page };
            let applied = consumer.apply_source_delta(request)?;
            applied
                .validate_for_identity(&acknowledgement_identity)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            changed_sources = changed_sources
                .checked_add(applied.changed_sources)
                .ok_or_else(|| anyhow!("invalid_response: changed-source count overflowed"))?;
            removed_sources = removed_sources
                .checked_add(applied.removed_sources)
                .ok_or_else(|| anyhow!("invalid_response: removal count overflowed"))?;
            reconcile_sources.extend(applied.reconcile_sources);
        }

        reconcile_sources.sort_by_key(|item| item.source_index);
        for pair in reconcile_sources.windows(2) {
            if pair[0].source_index >= pair[1].source_index {
                bail!("invalid_response: Core reconciliations are not strictly ordered");
            }
        }
        for reconciliation in reconcile_sources {
            let report = reconcile_source_events(
                index,
                &began.materialization_id,
                reconciliation,
                consumer,
            )?;
            event_delta_pages = event_delta_pages
                .checked_add(report.pages)
                .ok_or_else(|| anyhow!("invalid_response: Core event page count overflowed"))?;
            event_mutations = event_mutations
                .checked_add(report.mutations)
                .ok_or_else(|| anyhow!("invalid_response: Core event mutation count overflowed"))?;
        }
    }

    let finish = FinishCoreMaterializationRequest {
        materialization_id: began.materialization_id,
        head: head.clone(),
        expected_prior_receipt,
        source_delta_pages,
        changed_sources,
        removed_sources,
        event_delta_pages: u32::try_from(event_delta_pages)
            .map_err(|_| anyhow!("invalid_request: Core event delta page count overflowed"))?,
        event_mutations,
    };
    finish
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let finished = consumer.finish(finish)?;
    finished
        .receipt
        .validate_for_head(&head)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if finished.receipt.materializer_revision != began.materializer_revision {
        bail!("invalid_response: terminal Core receipt changed materializer revision");
    }

    Ok(CoreMaterializationSyncReport {
        receipt: finished.receipt,
        #[cfg(test)]
        changed_sources: u64::from(changed_sources),
        #[cfg(test)]
        removed_sources: u64::from(removed_sources),
        #[cfg(test)]
        event_delta_pages,
        #[cfg(test)]
        event_mutations,
        replayed: began.replayed,
    })
}

fn core_source_states(manifest: &GenerationManifest) -> Result<Vec<CoreSourceState>> {
    let mut states = manifest
        .sources
        .iter()
        .map(|source| {
            Ok(CoreSourceState {
                source: source.observation().source().clone(),
                source_revision_sha256: certified_source_revision_sha256(source)?,
                event_count: source.counts().indexed_documents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    states.sort_by_key(|state| state.source.identity().digest());
    for pair in states.windows(2) {
        if pair[0].source.identity().digest() >= pair[1].source.identity().digest() {
            bail!("invalid_request: Core sources are not unique by stable identity");
        }
    }
    Ok(states)
}

fn core_generation_head(
    index: &VerifiedIndex,
    sources: &[CoreSourceState],
) -> Result<CoreGenerationHead> {
    let manifest = index.manifest();
    CoreGenerationHead::new(
        index.generation_id(),
        manifest.manifest_version,
        manifest.identity_version,
        manifest.core_record_contract_fingerprint.clone(),
        manifest.lexical_schema_version,
        manifest.lexical_analyzer_version,
        manifest.policy_schema_hash.clone(),
        sources,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))
}

fn core_snapshot_deltas(
    manifest: &GenerationManifest,
    sources: &[CoreSourceState],
) -> Result<Vec<CoreSourceDelta>> {
    let mut deltas = sources
        .iter()
        .cloned()
        .map(CoreSourceDelta::Present)
        .collect::<Vec<_>>();
    for removal in &manifest.removals {
        deltas.push(CoreSourceDelta::Removed(CoreSourceRemoval {
            source: removal.source().clone(),
            removal_revision_sha256: canonical_sha256(removal)?,
        }));
    }
    deltas.sort_by_key(|delta| delta.source().identity().digest());
    for pair in deltas.windows(2) {
        if pair[0].source().identity().digest() >= pair[1].source().identity().digest() {
            bail!("invalid_request: Core snapshot retains and removes the same source");
        }
    }
    Ok(deltas)
}

fn build_delta_pages(
    materialization_id: &str,
    generation_id: &str,
    deltas: Vec<CoreSourceDelta>,
) -> Result<Vec<CoreSourceDeltaPage>> {
    if deltas.is_empty() {
        return Ok(Vec::new());
    }
    let mut chunks = Vec::<Vec<CoreSourceDelta>>::new();
    let mut current = Vec::new();
    for delta in deltas {
        current.push(delta);
        let test =
            CoreSourceDeltaPage::new(materialization_id, generation_id, 0, false, current.clone());
        if current.len() > MAX_CORE_SOURCE_DELTA_PAGE_ITEMS || test.is_err() {
            let overflow = current
                .pop()
                .ok_or_else(|| anyhow!("internal: empty Core delta page"))?;
            if current.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core source delta exceeds its wire bound"
                ));
            }
            chunks.push(std::mem::take(&mut current));
            current.push(overflow);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }

    let chunk_count = chunks.len();
    let mut pages = Vec::with_capacity(chunk_count);
    for (index, chunk) in chunks.into_iter().enumerate() {
        let page = CoreSourceDeltaPage::new(
            materialization_id,
            generation_id,
            u32::try_from(index)
                .map_err(|_| anyhow!("invalid_request: Core delta page index overflowed"))?,
            index + 1 == chunk_count,
            chunk,
        )
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        pages.push(page);
    }
    Ok(pages)
}

#[derive(Debug, Clone, Copy)]
struct EventReconciliationReport {
    pages: u64,
    mutations: u64,
}

fn reconcile_source_events<C: CoreMaterializationConsumer>(
    index: &VerifiedIndex,
    materialization_id: &str,
    reconciliation: CoreSourceReconciliation,
    consumer: &mut C,
) -> Result<EventReconciliationReport> {
    let current_source = match &reconciliation.delta {
        CoreSourceDelta::Present(source) => Some(source),
        CoreSourceDelta::Removed(_) => None,
    };
    let mut state_after = None;
    let mut state_page_index = 0_u32;
    let mut state_terminal = false;
    let mut states = VecDeque::<CoreEventState>::new();
    let mut current_cursor = None;
    let mut current_terminal = current_source.is_none();
    let mut current = VecDeque::<ctx_history_core::CoreRecord>::new();
    let mut pending = Vec::<CoreEventDelta>::new();
    let mut event_page_index = 0_u32;
    let mut pages = 0_u64;
    let mut mutations = 0_u64;

    loop {
        if states.is_empty() && !state_terminal {
            let request = CoreEventStatePageRequest {
                materialization_id: materialization_id.to_owned(),
                core_generation_id: index.generation_id().to_owned(),
                reconciliation: reconciliation.clone(),
                page_index: state_page_index,
                after_event_id: state_after,
                maximum_items: u32::try_from(MAX_CORE_EVENT_STATE_PAGE_ITEMS)
                    .map_err(|_| anyhow!("invalid_request: Core event state limit overflowed"))?,
            };
            request
                .validate()
                .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
            let response = consumer.event_states(request.clone())?;
            response
                .validate_for(&request)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            state_terminal = response.terminal;
            if let Some(last) = response.states.last() {
                state_after = Some(last.event_id);
            }
            states.extend(response.states);
            state_page_index = state_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event state page overflowed"))?;
        }
        if current.is_empty() && !current_terminal {
            let source = current_source.ok_or_else(|| {
                anyhow!("invalid_request: removed Core source unexpectedly requested records")
            })?;
            let (records, next_cursor, terminal) =
                next_current_records(index, source, current_cursor.as_ref())?;
            current = records.into();
            current_cursor = next_cursor;
            current_terminal = terminal;
        }

        let delta = match (states.front(), current.front()) {
            (None, None) if state_terminal && current_terminal => break,
            (None, None) => continue,
            (Some(state), None) => {
                let state = state.clone();
                states.pop_front();
                Some(CoreEventDelta::Tombstoned(CoreEventTombstone {
                    event_id: state.event_id,
                    prior_core_record_sha256: state.core_record_sha256,
                }))
            }
            (None, Some(_)) => {
                Some(CoreEventDelta::Added(current.pop_front().ok_or_else(
                    || anyhow!("internal: missing current Core event"),
                )?))
            }
            (Some(state), Some(record)) => {
                match state.event_id.digest().cmp(&record.event_id.digest()) {
                    std::cmp::Ordering::Less => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        Some(CoreEventDelta::Tombstoned(CoreEventTombstone {
                            event_id: state.event_id,
                            prior_core_record_sha256: state.core_record_sha256,
                        }))
                    }
                    std::cmp::Ordering::Greater => {
                        Some(CoreEventDelta::Added(current.pop_front().ok_or_else(
                            || anyhow!("internal: missing current Core event"),
                        )?))
                    }
                    std::cmp::Ordering::Equal => {
                        let state = states
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing prior Core event state"))?;
                        let record = current
                            .pop_front()
                            .ok_or_else(|| anyhow!("internal: missing current Core event"))?;
                        let current_sha256 = core_record_sha256(&record)
                            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
                        (state.requires_replacement || current_sha256 != state.core_record_sha256)
                            .then_some(CoreEventDelta::Replaced(CoreEventReplacement {
                                prior_core_record_sha256: state.core_record_sha256,
                                record,
                            }))
                    }
                }
            }
        };
        let Some(delta) = delta else {
            continue;
        };
        mutations = mutations
            .checked_add(1)
            .ok_or_else(|| anyhow!("invalid_request: Core event mutation count overflowed"))?;
        if pending.len() == MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
            send_event_delta_page(
                consumer,
                materialization_id,
                index.generation_id(),
                &reconciliation,
                event_page_index,
                false,
                std::mem::take(&mut pending),
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
        }
        pending.push(delta);
        let candidate = event_delta_page(
            materialization_id,
            index.generation_id(),
            &reconciliation,
            event_page_index,
            false,
            pending.clone(),
        );
        if candidate.is_err() {
            let overflow = pending
                .pop()
                .ok_or_else(|| anyhow!("internal: empty Core event delta page"))?;
            if pending.is_empty() {
                return Err(anyhow!(
                    "invalid_request: one Core event delta exceeds its page bound"
                ));
            }
            send_event_delta_page(
                consumer,
                materialization_id,
                index.generation_id(),
                &reconciliation,
                event_page_index,
                false,
                std::mem::take(&mut pending),
            )?;
            pages = pages.saturating_add(1);
            event_page_index = event_page_index
                .checked_add(1)
                .ok_or_else(|| anyhow!("invalid_request: Core event delta page overflowed"))?;
            pending.push(overflow);
        }
    }

    send_event_delta_page(
        consumer,
        materialization_id,
        index.generation_id(),
        &reconciliation,
        event_page_index,
        true,
        pending,
    )?;
    pages = pages.saturating_add(1);
    Ok(EventReconciliationReport { pages, mutations })
}

fn next_current_records(
    index: &VerifiedIndex,
    source: &CoreSourceState,
    cursor: Option<&SourceEventCursor>,
) -> Result<(
    Vec<ctx_history_core::CoreRecord>,
    Option<SourceEventCursor>,
    bool,
)> {
    let source_page = index.core_source_event_page_with_budget(
        &source.source,
        cursor,
        MAX_CORE_EVENT_DELTA_PAGE_ITEMS,
        CORE_RECORD_PAGE_BUDGET,
    )?;
    if source_page.generation_id != index.generation_id()
        || !source_page.source.exact_descriptor_eq(&source.source)
    {
        bail!("core_generation_mismatch: Core record page escaped its pinned generation");
    }
    if source_page.items.len() > MAX_CORE_EVENT_DELTA_PAGE_ITEMS {
        bail!("invalid_request: Core record page exceeded its item bound");
    }
    if source_page.encoded_core_bytes > CORE_RECORD_PAGE_BUDGET.maximum_encoded_core_bytes {
        bail!(
            "invalid_request: one Core record exceeds the {}-byte Pro page encoded-payload bound",
            CORE_RECORD_PAGE_BUDGET.maximum_encoded_core_bytes
        );
    }
    if source_page.content_bytes > CORE_RECORD_PAGE_BUDGET.maximum_content_bytes {
        bail!(
            "invalid_request: one Core record exceeds the {}-byte Pro page content bound",
            CORE_RECORD_PAGE_BUDGET.maximum_content_bytes
        );
    }
    let terminal = source_page.terminal;
    let next_cursor = source_page.next_cursor;
    let records = source_page
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    Ok((records, next_cursor, terminal))
}

fn event_delta_page(
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreEventDelta>,
) -> Result<CoreEventDeltaPage> {
    let page = CoreEventDeltaPage {
        materialization_id: materialization_id.to_owned(),
        core_generation_id: generation_id.to_owned(),
        reconciliation: reconciliation.clone(),
        page_index,
        terminal,
        deltas,
    };
    page.validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn send_event_delta_page<C: CoreMaterializationConsumer>(
    consumer: &mut C,
    materialization_id: &str,
    generation_id: &str,
    reconciliation: &CoreSourceReconciliation,
    page_index: u32,
    terminal: bool,
    deltas: Vec<CoreEventDelta>,
) -> Result<()> {
    let page = event_delta_page(
        materialization_id,
        generation_id,
        reconciliation,
        page_index,
        terminal,
        deltas,
    )?;
    let request = ApplyCoreEventDeltaPageRequest { page };
    let response = consumer.apply_event_delta(request.clone())?;
    response
        .validate_for(&request.page)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))
}

fn certified_source_revision_sha256(source: &CertifiedSource) -> Result<String> {
    source.validate_contract()?;
    canonical_sha256(source)
}

fn canonical_sha256(value: &impl serde::Serialize) -> Result<String> {
    let encoded = serde_json::to_vec(value)?;
    let digest = Sha256::digest(encoded);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
#[path = "core_materialization_feed/tests.rs"]
mod tests;
