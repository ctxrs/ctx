use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use ctx_history_core::CertifiedSource;
use ctx_history_index::{GenerationManifest, SourceEventCursor, VerifiedIndex};
use ctx_pro_host_protocol::{
    ApplyCoreSourceDeltaPageRequest, BeginCoreMaterializationRequest, Capability,
    CoreGenerationHead, CoreMaterializationBegan, CoreMaterializationFinished,
    CoreMaterializationReceipt, CoreMaterializationReceiptIdentity, CoreRecordPage,
    CoreRecordPageMaterialized, CoreSourceDelta, CoreSourceDeltaPage, CoreSourceDeltaPageApplied,
    CoreSourceRemoval, CoreSourceState, FinishCoreMaterializationRequest, HelperMessage,
    HostMessage, MaterializeCoreRecordPageRequest, StatusRequest, MAX_CORE_SOURCE_DELTA_PAGE_ITEMS,
};
use sha2::{Digest, Sha256};

use super::*;

// Conservative bridge until Core exposes its source-local address-first page API with a
// caller-supplied byte bound. Keep the integration at `next_record_page`: request addresses
// under MAX_CORE_RECORD_PAGE_CONTENT_BYTES, then materialize only that bounded address set.
const CORE_RECORD_FETCH_ITEMS: usize = 1;

#[derive(Debug, Clone)]
pub(super) struct CoreMaterializationSyncReport {
    pub(super) receipt: CoreMaterializationReceipt,
    #[cfg(test)]
    pub(super) changed_sources: u64,
    #[cfg(test)]
    pub(super) removed_sources: u64,
    #[cfg(test)]
    pub(super) record_pages: u64,
    #[cfg(test)]
    pub(super) materialized_records: u64,
    #[cfg(test)]
    pub(super) replayed: bool,
}

trait CoreMaterializationConsumer {
    fn begin(
        &mut self,
        request: &BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan>;

    fn apply_source_delta(
        &mut self,
        request: &ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied>;

    fn materialize_records(
        &mut self,
        request: &MaterializeCoreRecordPageRequest,
    ) -> Result<CoreRecordPageMaterialized>;

    fn finish(
        &mut self,
        request: &FinishCoreMaterializationRequest,
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
        request: &BeginCoreMaterializationRequest,
    ) -> Result<CoreMaterializationBegan> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::BeginCoreMaterialization(request.clone()))? {
            HelperMessage::CoreMaterializationBegan(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-begin response"),
        }
    }

    fn apply_source_delta(
        &mut self,
        request: &ApplyCoreSourceDeltaPageRequest,
    ) -> Result<CoreSourceDeltaPageApplied> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::ApplyCoreSourceDeltaPage(request.clone()))? {
            HelperMessage::CoreSourceDeltaPageApplied(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-delta response"),
        }
    }

    fn materialize_records(
        &mut self,
        request: &MaterializeCoreRecordPageRequest,
    ) -> Result<CoreRecordPageMaterialized> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::MaterializeCoreRecordPage(request.clone()))? {
            HelperMessage::CoreRecordPageMaterialized(response) => Ok(response),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-Core-record response"),
        }
    }

    fn finish(
        &mut self,
        request: &FinishCoreMaterializationRequest,
    ) -> Result<CoreMaterializationFinished> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::FinishCoreMaterialization(request.clone()))? {
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
    let began = consumer.begin(&begin)?;
    began
        .validate_for(&begin)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;

    let mut source_delta_pages = 0_u32;
    let mut changed_sources = 0_u32;
    let mut removed_sources = 0_u32;
    let mut materialized_records = 0_u64;
    let mut record_pages = 0_u64;

    if !began.replayed {
        let deltas = core_snapshot_deltas(index.manifest(), &sources)?;
        let delta_pages =
            build_delta_pages(&began.materialization_id, index.generation_id(), deltas)?;
        source_delta_pages = u32::try_from(delta_pages.len())
            .map_err(|_| anyhow!("invalid_request: Core delta page count overflowed"))?;
        let mut materialize_sources = Vec::new();
        for page in delta_pages {
            let request = ApplyCoreSourceDeltaPageRequest { page };
            let applied = consumer.apply_source_delta(&request)?;
            applied
                .validate_for(&request.page)
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            changed_sources = changed_sources
                .checked_add(applied.changed_sources)
                .ok_or_else(|| anyhow!("invalid_response: changed-source count overflowed"))?;
            removed_sources = removed_sources
                .checked_add(applied.removed_sources)
                .ok_or_else(|| anyhow!("invalid_response: removal count overflowed"))?;
            materialize_sources.extend(applied.materialize_sources);
        }

        for source in &materialize_sources {
            let source_index = sources
                .binary_search_by_key(&source.source.identity().digest(), |state| {
                    state.source.identity().digest()
                })
                .map_err(|_| {
                    anyhow!("invalid_response: requested materialization source is not in the Core head")
                })?;
            let mut cursor = None;
            let mut page_index = 0_u32;
            loop {
                let page = next_record_page(
                    index,
                    source,
                    cursor.as_ref(),
                    &began.materialization_id,
                    u32::try_from(source_index)
                        .map_err(|_| anyhow!("invalid_request: Core source index overflowed"))?,
                    page_index,
                )?;
                let terminal = page.terminal;
                let last_event = page.records.last().map(|record| record.event_id);
                materialized_records = materialized_records
                    .checked_add(u64::try_from(page.records.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| {
                        anyhow!("invalid_request: Core materialized record count overflowed")
                    })?;
                let request = MaterializeCoreRecordPageRequest { page };
                let response = consumer.materialize_records(&request)?;
                response
                    .validate_for(&request.page)
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                record_pages = record_pages.saturating_add(1);
                if terminal {
                    break;
                }
                let after = last_event.ok_or_else(|| {
                    anyhow!("invalid_response: nonterminal Core page has no event cursor")
                })?;
                cursor = Some(SourceEventCursor::new(
                    index.generation_id(),
                    source.source.clone(),
                    after,
                ));
                page_index = page_index
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("invalid_request: Core page index overflowed"))?;
            }
        }
    }

    let finish = FinishCoreMaterializationRequest {
        materialization_id: began.materialization_id,
        head,
        expected_prior_receipt,
        source_delta_pages,
        changed_sources,
        removed_sources,
        record_pages: u32::try_from(record_pages)
            .map_err(|_| anyhow!("invalid_request: Core record page count overflowed"))?,
        materialized_records,
    };
    let finished = consumer.finish(&finish)?;
    finished
        .validate_for(&finish)
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
        record_pages,
        #[cfg(test)]
        materialized_records,
        #[cfg(test)]
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

fn next_record_page(
    index: &VerifiedIndex,
    source: &CoreSourceState,
    cursor: Option<&SourceEventCursor>,
    materialization_id: &str,
    source_index: u32,
    page_index: u32,
) -> Result<CoreRecordPage> {
    let source_page =
        index.core_source_event_page(&source.source, cursor, CORE_RECORD_FETCH_ITEMS)?;
    if source_page.generation_id != index.generation_id()
        || !source_page.source.exact_descriptor_eq(&source.source)
    {
        bail!("core_generation_mismatch: Core record page escaped its pinned generation");
    }
    let terminal = source_page.terminal;
    let records = source_page
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    CoreRecordPage::new(
        materialization_id,
        index.generation_id(),
        source.clone(),
        source_index,
        page_index,
        terminal,
        records,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))
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
