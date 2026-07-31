#[path = "source_backed_feed/admission.rs"]
mod admission;
#[path = "source_backed_feed/validation.rs"]
mod validation;

#[cfg(test)]
pub(super) use admission::cursor_after_page;
pub(super) use admission::sync_source_backed_pro_feed_paged;
use validation::{
    source_identity_digest, validate_consumer_state, validate_prepared_source,
    validate_provider_page, validate_request, validate_response, validate_source_backed_receipt,
};

use super::*;
use ctx_pro_host_protocol::{
    certified_source_revision_sha256, AdmitSourceManifestPageRequest,
    BeginSourceManifestAdmissionRequest, DeleteSourceRequest, FinishAdmittedSourceManifestRequest,
    FinishSourceManifestAdmissionRequest, FinishSourceManifestRequest,
    MaterializeSourcePageRequest, MaterializeSourcePagesRequest, PrepareSourceRequest,
    ReadSourceProgressPageRequest, SourceDeleted, SourceDisposition, SourceManifest,
    SourceManifestAdmissionBegan, SourceManifestAdmitted, SourceManifestBegan,
    SourceManifestFinished, SourceManifestHeader, SourceManifestPage, SourceManifestPageAdmitted,
    SourceManifestReceipt, SourcePagesMaterialized, SourcePrepared, SourceProgress,
    SourceProgressPage, SourceProgressReceipt, SourceRecord, SourceRemoval,
};

const MAX_CANONICAL_SOURCE_PAGES: u64 = 1_000_000;

pub(super) type SourceBackedProManifest = SourceManifest;
#[cfg(test)]
pub(super) type SourceBackedProRemoval = SourceRemoval;
pub(super) type SourceBackedProProgress = SourceProgress;
pub(super) type SourceBackedProRecord = SourceRecord;
pub(super) type SourceBackedProReceipt = SourceManifestReceipt;
pub(super) type SourceBackedProDisposition = SourceDisposition;

/// Provider-owned page produced by rereading an exact certified source.
#[derive(Debug, Clone)]
pub(super) struct SourceBackedProviderPage {
    pub(super) source: SourceKey,
    pub(super) expected_prior_frontier: Option<SourceFrontier>,
    pub(super) next_frontier: Option<SourceFrontier>,
    pub(super) terminal: bool,
    pub(super) records: Vec<SourceBackedProRecord>,
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedProSyncReport {
    pub(super) receipt: SourceBackedProReceipt,
    #[cfg(test)]
    pub(super) prepared_sources: u64,
    #[cfg(test)]
    pub(super) rewritten_sources: u64,
    #[cfg(test)]
    pub(super) deleted_sources: u64,
    #[cfg(test)]
    pub(super) reread_pages: u64,
    #[cfg(test)]
    pub(super) reread_records: u64,
}

/// Provider adapter boundary for canonical Pro catch-up.
///
/// Implementations enumerate the committed source manifest separately, then
/// reread native records from the supplied source frontier. They must hydrate
/// every returned record from its `SourceRecordLocator`; canonical Store bytes
/// are not an input to this interface.
pub(super) trait SourceBackedProProvider {
    fn reread_source_page(
        &mut self,
        source: &CertifiedSource,
        expected_prior_frontier: Option<&SourceFrontier>,
    ) -> Result<SourceBackedProviderPage>;
}

pub(super) trait SourceBackedProPageConsumer {
    fn prepare_source(&mut self, request: &PrepareSourceRequest) -> Result<SourcePrepared>;

    fn materialize_source_pages(
        &mut self,
        request: &MaterializeSourcePagesRequest,
    ) -> Result<SourcePagesMaterialized>;

    fn delete_source(&mut self, request: &DeleteSourceRequest) -> Result<SourceDeleted>;
}

/// State boundary used by the source reconciliation engine.
///
/// The production transport implements the paged admission trait instead;
/// `AdmittedSourceBackedConsumer` supplies this state boundary only after
/// the exact generation manifest has been admitted.
pub(super) trait SourceBackedProConsumer: SourceBackedProPageConsumer {
    fn begin_source_manifest(
        &mut self,
        manifest: &SourceBackedProManifest,
    ) -> Result<SourceManifestBegan>;

    fn finish_source_manifest(
        &mut self,
        request: &FinishSourceManifestRequest,
    ) -> Result<SourceManifestFinished>;
}

pub(super) trait SourceBackedProAdmissionConsumer: SourceBackedProPageConsumer {
    fn begin_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmissionBegan>;

    fn admit_source_manifest_page(
        &mut self,
        page: &SourceManifestPage,
    ) -> Result<SourceManifestPageAdmitted>;

    fn finish_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmitted>;

    fn read_source_progress_page(
        &mut self,
        request: &ReadSourceProgressPageRequest,
    ) -> Result<SourceProgressPage>;

    fn finish_admitted_source_manifest(
        &mut self,
        request: &FinishAdmittedSourceManifestRequest,
    ) -> Result<SourceManifestFinished>;
}

struct ProtocolSourceBackedProConsumer {
    client: ProClient,
}

impl ProtocolSourceBackedProConsumer {
    fn exchange(&mut self, message: HostMessage) -> Result<HelperMessage> {
        self.client.exchange(message, BATCH_TIMEOUT)
    }
}

impl SourceBackedProPageConsumer for ProtocolSourceBackedProConsumer {
    fn prepare_source(&mut self, request: &PrepareSourceRequest) -> Result<SourcePrepared> {
        validate_request(request)?;
        match self.exchange(HostMessage::PrepareSource(request.clone()))? {
            HelperMessage::SourcePrepared(result) => {
                validate_response(&result)?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-source-prepared response"),
        }
    }

    fn materialize_source_pages(
        &mut self,
        request: &MaterializeSourcePagesRequest,
    ) -> Result<SourcePagesMaterialized> {
        match self.exchange(HostMessage::MaterializeSourcePages(request.clone()))? {
            HelperMessage::SourcePagesMaterialized(result) => Ok(result),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-pages response")
            }
        }
    }

    fn delete_source(&mut self, request: &DeleteSourceRequest) -> Result<SourceDeleted> {
        validate_request(request)?;
        match self.exchange(HostMessage::DeleteSource(request.clone()))? {
            HelperMessage::SourceDeleted(result) => {
                validate_response(&result)?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => bail!("invalid_response: helper returned a non-source-deleted response"),
        }
    }
}

impl SourceBackedProAdmissionConsumer for ProtocolSourceBackedProConsumer {
    fn begin_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmissionBegan> {
        let request = BeginSourceManifestAdmissionRequest {
            header: header.clone(),
        };
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::BeginSourceManifestAdmission(request))? {
            HelperMessage::SourceManifestAdmissionBegan(result) => {
                result
                    .validate_for(header)
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-admission-begin response")
            }
        }
    }

    fn admit_source_manifest_page(
        &mut self,
        page: &SourceManifestPage,
    ) -> Result<SourceManifestPageAdmitted> {
        let request = AdmitSourceManifestPageRequest { page: page.clone() };
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::AdmitSourceManifestPage(request))? {
            HelperMessage::SourceManifestPageAdmitted(result) => Ok(result),
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-page-admission response")
            }
        }
    }

    fn finish_source_manifest_admission(
        &mut self,
        header: &SourceManifestHeader,
    ) -> Result<SourceManifestAdmitted> {
        let request = FinishSourceManifestAdmissionRequest {
            header: header.clone(),
        };
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::FinishSourceManifestAdmission(request))? {
            HelperMessage::SourceManifestAdmitted(result) => {
                result
                    .validate()
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-admitted response")
            }
        }
    }

    fn read_source_progress_page(
        &mut self,
        request: &ReadSourceProgressPageRequest,
    ) -> Result<SourceProgressPage> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::ReadSourceProgressPage(request.clone()))? {
            HelperMessage::SourceProgressPage(result) => {
                result
                    .validate_for(&request.progress)
                    .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-progress-page response")
            }
        }
    }

    fn finish_admitted_source_manifest(
        &mut self,
        request: &FinishAdmittedSourceManifestRequest,
    ) -> Result<SourceManifestFinished> {
        request
            .validate()
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        match self.exchange(HostMessage::FinishAdmittedSourceManifest(request.clone()))? {
            HelperMessage::SourceManifestFinished(result) => {
                validate_response(&result)?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-finished response")
            }
        }
    }
}

/// Reconciles Pro to one already-committed Core source manifest.
///
/// Core publication is neither prepared nor committed here. An unavailable
/// helper or provider reread leaves only Pro behind; a later or newly installed
/// Pro instance resumes from its own per-source epoch/frontier.
pub(super) fn sync_source_backed_pro_feed<P, C>(
    manifest: SourceBackedProManifest,
    provider: &mut P,
    consumer: &mut C,
) -> Result<SourceBackedProSyncReport>
where
    P: SourceBackedProProvider,
    C: SourceBackedProConsumer,
{
    manifest
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let consumer_state = consumer.begin_source_manifest(&manifest)?;
    validate_consumer_state(&manifest, &consumer_state)?;
    let mut progress_by_source = BTreeMap::new();
    for progress in consumer_state.progress {
        let source_id = progress.source.identity().digest();
        if progress_by_source.insert(source_id, progress).is_some() {
            bail!("invalid_response: Pro returned duplicate canonical source progress");
        }
    }

    let current_source_ids = manifest
        .sources
        .iter()
        .map(source_identity_digest)
        .collect::<BTreeSet<_>>();
    let stale_source_ids = progress_by_source
        .keys()
        .filter(|source_id| !current_source_ids.contains(*source_id))
        .copied()
        .collect::<Vec<_>>();
    #[cfg(test)]
    let mut deleted_sources = 0_u64;
    for source_id in stale_source_ids {
        let prior = progress_by_source
            .get(&source_id)
            .cloned()
            .ok_or_else(|| anyhow!("invalid_response: missing stale source progress"))?;
        let removal = manifest
            .removals
            .iter()
            .find(|removal| removal.deletion.source().identity().digest() == source_id)
            .ok_or_else(|| {
                anyhow!(
                    "source_changed: Pro source is absent from the manifest without a certified deletion"
                )
            })?;
        if !removal.deletion.source().exact_descriptor_eq(&prior.source) {
            bail!("source_changed: deletion witness does not describe Pro's source generation");
        }
        let request = DeleteSourceRequest {
            core_generation_id: manifest.core_generation_id.clone(),
            removal: removal.clone(),
            expected_prior: prior.clone(),
        };
        let deleted = consumer.delete_source(&request)?;
        if deleted.core_generation_id != manifest.core_generation_id
            || !deleted.source.exact_descriptor_eq(&prior.source)
            || deleted.removed_source_epoch != prior.source_epoch
        {
            bail!("invalid_response: Pro acknowledged the wrong source deletion CAS");
        }
        progress_by_source.remove(&source_id);
        #[cfg(test)]
        {
            deleted_sources = deleted_sources.saturating_add(1);
        }
    }

    let mut latest_progress = BTreeMap::new();
    let mut pending_materialization = MaterializationBatchBuilder::new();
    #[cfg(test)]
    let mut prepared_sources = 0_u64;
    #[cfg(test)]
    let mut rewritten_sources = 0_u64;
    #[cfg(test)]
    let mut reread_pages = 0_u64;
    #[cfg(test)]
    let mut reread_records = 0_u64;
    for source in &manifest.sources {
        let source_key = source.observation().source();
        let source_id = source_identity_digest(source);
        let certified_revision_sha256 = certified_source_revision_sha256(source)
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        let prior = progress_by_source.remove(&source_id);
        let disposition = match prior.as_ref() {
            None => SourceBackedProDisposition::NewSource,
            Some(progress)
                if progress.source.exact_descriptor_eq(source_key)
                    && progress.certified_revision_sha256 == certified_revision_sha256
                    && progress.materializer_revision == consumer_state.materializer_revision =>
            {
                SourceBackedProDisposition::Resume
            }
            Some(_) => SourceBackedProDisposition::Rewrite,
        };
        let prepare = PrepareSourceRequest {
            core_generation_id: manifest.core_generation_id.clone(),
            source: source_key.clone(),
            certified_revision_sha256: certified_revision_sha256.clone(),
            materializer_revision: consumer_state.materializer_revision.clone(),
            disposition,
            expected_prior: prior.clone(),
        };
        let prepared = consumer.prepare_source(&prepare)?;
        validate_prepared_source(&prepare, &prepared)?;
        let mut prepared = prepared.progress;
        latest_progress.insert(source_id, prepared.clone());
        #[cfg(test)]
        {
            prepared_sources = prepared_sources.saturating_add(1);
            if disposition == SourceBackedProDisposition::Rewrite {
                rewritten_sources = rewritten_sources.saturating_add(1);
            }
        }

        let mut source_pages = 0_u64;
        let mut lookahead = None;
        while !prepared.terminal {
            let mut provisional = prepared.clone();
            let mut coalesced: Option<SourcePageCoalescer> = None;
            loop {
                let item = if let Some(item) = lookahead.take() {
                    item
                } else {
                    source_pages = source_pages.saturating_add(1);
                    if source_pages > MAX_CANONICAL_SOURCE_PAGES {
                        bail!("invalid_response: provider source page count exceeds safety bound");
                    }
                    let page =
                        provider.reread_source_page(source, provisional.frontier.as_ref())?;
                    #[cfg(test)]
                    {
                        reread_pages = reread_pages.saturating_add(1);
                        reread_records = reread_records
                            .saturating_add(u64::try_from(page.records.len()).unwrap_or(u64::MAX));
                    }
                    validate_provider_page(source, &provisional, &page)?;
                    ProviderPageMaterializationItem::new(page, source_key)?
                };
                if let Some(accumulated) = coalesced.as_mut() {
                    if let Some(item) = accumulated.try_append(item)? {
                        lookahead = Some(item);
                        break;
                    }
                    provisional = accumulated.next_progress();
                    if accumulated.terminal() {
                        break;
                    }
                } else {
                    let accumulated = SourcePageCoalescer::new(
                        manifest.core_generation_id.clone(),
                        provisional.clone(),
                        item,
                    )?;
                    provisional = accumulated.next_progress();
                    let terminal = accumulated.terminal();
                    coalesced = Some(accumulated);
                    if terminal {
                        break;
                    }
                }
            }
            let item = coalesced
                .ok_or_else(|| anyhow!("internal: source page coalescing produced no request"))?
                .finish();
            let terminal = item.request.terminal;
            enqueue_materialization_request(
                consumer,
                &mut pending_materialization,
                &mut latest_progress,
                item,
            )?;
            if terminal {
                break;
            }
            flush_materialization_batch(
                consumer,
                &mut pending_materialization,
                &mut latest_progress,
            )?;
            prepared = latest_progress
                .get(&source_id)
                .cloned()
                .ok_or_else(|| anyhow!("invalid_response: missing materialized source progress"))?;
        }
    }
    if !progress_by_source.is_empty() {
        bail!("invalid_response: unreconciled Pro source progress remains");
    }
    flush_materialization_batch(consumer, &mut pending_materialization, &mut latest_progress)?;
    let mut final_sources = Vec::with_capacity(manifest.sources.len());
    for source in &manifest.sources {
        let source_id = source_identity_digest(source);
        let progress = latest_progress
            .remove(&source_id)
            .ok_or_else(|| anyhow!("invalid_response: missing final Pro source progress"))?;
        if !progress
            .source
            .exact_descriptor_eq(source.observation().source())
            || !progress.terminal
            || progress.frontier.as_ref() != source.frontier()
        {
            bail!("invalid_response: Pro terminated at the wrong certified source frontier");
        }
        final_sources.push(progress);
    }
    if !latest_progress.is_empty() {
        bail!("invalid_response: unexpected final Pro source progress remains");
    }
    let finish = FinishSourceManifestRequest {
        manifest: manifest.clone(),
        expected_progress: final_sources.clone(),
    };
    finish
        .validate_contents()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let finished = consumer.finish_source_manifest(&finish)?;
    let receipt = finished.receipt;
    validate_source_backed_receipt(
        &manifest,
        &consumer_state.materializer_revision,
        &final_sources,
        &receipt,
    )?;
    Ok(SourceBackedProSyncReport {
        receipt,
        #[cfg(test)]
        prepared_sources,
        #[cfg(test)]
        rewritten_sources,
        #[cfg(test)]
        deleted_sources,
        #[cfg(test)]
        reread_pages,
        #[cfg(test)]
        reread_records,
    })
}

struct ProviderPageMaterializationItem {
    next_frontier: Option<SourceFrontier>,
    terminal: bool,
    records: Vec<SourceRecord>,
    record_count: usize,
    content_bytes: usize,
    record_payload_bytes: usize,
    first_order: Option<(u64, [u8; 32])>,
    last_order: Option<(u64, [u8; 32])>,
    event_ids: BTreeSet<[u8; 32]>,
}

impl ProviderPageMaterializationItem {
    fn new(page: SourceBackedProviderPage, expected_source: &SourceKey) -> Result<Self> {
        let SourceBackedProviderPage {
            next_frontier,
            terminal,
            records,
            ..
        } = page;
        let mut content_bytes = 0_usize;
        let mut record_payload_bytes = 0_usize;
        let mut first_order = None;
        let mut last_order = None;
        let mut event_ids = BTreeSet::new();
        for record in &records {
            if !record.locator.source().exact_descriptor_eq(expected_source) {
                bail!("invalid_request: source page record belongs to another source descriptor");
            }
            let current = (record.metadata.event_sequence, record.event_id.digest());
            if last_order.is_some_and(|prior| prior >= current) {
                bail!("invalid_request: source page records must be in strict stable event order");
            }
            if !event_ids.insert(record.event_id.digest()) {
                bail!("invalid_request: source page contains a duplicate stable event ID");
            }
            content_bytes = content_bytes
                .checked_add(
                    record
                        .validate_and_count_bytes()
                        .map_err(|error| anyhow!("invalid_request: {}", error.message))?,
                )
                .ok_or_else(|| {
                    anyhow!("invalid_request: source page transient-content bytes overflowed")
                })?;
            let encoded_bytes = serde_json::to_vec(record)
                .map_err(|error| anyhow!("internal: encode source record: {error}"))?
                .len();
            record_payload_bytes = record_payload_bytes
                .checked_add(usize::from(last_order.is_some()))
                .and_then(|bytes| bytes.checked_add(encoded_bytes))
                .ok_or_else(|| {
                    anyhow!("invalid_request: source page encoded record bytes overflowed")
                })?;
            first_order.get_or_insert(current);
            last_order = Some(current);
        }
        Ok(Self {
            next_frontier,
            terminal,
            record_count: records.len(),
            records,
            content_bytes,
            record_payload_bytes,
            first_order,
            last_order,
            event_ids,
        })
    }
}

#[derive(serde::Serialize)]
struct MaterializeSourcePageShell<'a> {
    core_generation_id: &'a str,
    expected_prior: &'a SourceProgress,
    next_frontier: &'a Option<SourceFrontier>,
    terminal: bool,
    records: &'a [SourceRecord],
}

fn source_page_shell_encoded_bytes(
    core_generation_id: &str,
    expected_prior: &SourceProgress,
    next_frontier: &Option<SourceFrontier>,
    terminal: bool,
) -> Result<usize> {
    serde_json::to_vec(&MaterializeSourcePageShell {
        core_generation_id,
        expected_prior,
        next_frontier,
        terminal,
        records: &[],
    })
    .map(|encoded| encoded.len())
    .map_err(|error| anyhow!("internal: encode source materialization shell: {error}"))
}

struct SourcePageCoalescer {
    request: MaterializeSourcePageRequest,
    record_count: usize,
    content_bytes: usize,
    record_payload_bytes: usize,
    encoded_bytes: usize,
    last_order: Option<(u64, [u8; 32])>,
    event_ids: BTreeSet<[u8; 32]>,
}

impl SourcePageCoalescer {
    fn new(
        core_generation_id: String,
        expected_prior: SourceProgress,
        item: ProviderPageMaterializationItem,
    ) -> Result<Self> {
        let mut coalescer = Self {
            request: MaterializeSourcePageRequest {
                core_generation_id,
                expected_prior,
                next_frontier: None,
                terminal: false,
                records: Vec::new(),
            },
            record_count: 0,
            content_bytes: 0,
            record_payload_bytes: 0,
            encoded_bytes: 0,
            last_order: None,
            event_ids: BTreeSet::new(),
        };
        if coalescer.try_append(item)?.is_some() {
            bail!("invalid_request: source materialization page exceeds its bounded request");
        }
        Ok(coalescer)
    }

    fn try_append(
        &mut self,
        item: ProviderPageMaterializationItem,
    ) -> Result<Option<ProviderPageMaterializationItem>> {
        if self
            .last_order
            .zip(item.first_order)
            .is_some_and(|(prior, current)| prior >= current)
        {
            bail!("invalid_request: source page records must be in strict stable event order");
        }
        if item
            .event_ids
            .iter()
            .any(|event_id| self.event_ids.contains(event_id))
        {
            bail!("invalid_request: source page contains a duplicate stable event ID");
        }
        let record_count = self
            .record_count
            .checked_add(item.record_count)
            .ok_or_else(|| anyhow!("invalid_request: source page record count overflowed"))?;
        let content_bytes = self
            .content_bytes
            .checked_add(item.content_bytes)
            .ok_or_else(|| {
                anyhow!("invalid_request: source page transient-content bytes overflowed")
            })?;
        let record_payload_bytes = self
            .record_payload_bytes
            .checked_add(usize::from(self.record_count > 0 && item.record_count > 0))
            .and_then(|bytes| bytes.checked_add(item.record_payload_bytes))
            .ok_or_else(|| {
                anyhow!("invalid_request: source page encoded record bytes overflowed")
            })?;
        let encoded_bytes = source_page_shell_encoded_bytes(
            &self.request.core_generation_id,
            &self.request.expected_prior,
            &item.next_frontier,
            item.terminal,
        )?
        .checked_add(record_payload_bytes)
        .ok_or_else(|| anyhow!("invalid_request: source page encoded bytes overflowed"))?;
        if record_count > ctx_pro_host_protocol::MAX_SOURCE_RECORDS_PER_PAGE
            || content_bytes > ctx_pro_host_protocol::MAX_SOURCE_CONTENT_BYTES_PER_PAGE
            || encoded_bytes > ctx_pro_host_protocol::MAX_SOURCE_PAGE_WIRE_BYTES
        {
            return Ok(Some(item));
        }

        let ProviderPageMaterializationItem {
            next_frontier,
            terminal,
            records,
            last_order,
            event_ids,
            ..
        } = item;
        self.request.next_frontier = next_frontier;
        self.request.terminal = terminal;
        self.request.records.extend(records);
        self.record_count = record_count;
        self.content_bytes = content_bytes;
        self.record_payload_bytes = record_payload_bytes;
        self.encoded_bytes = encoded_bytes;
        if last_order.is_some() {
            self.last_order = last_order;
        }
        self.event_ids.extend(event_ids);
        Ok(None)
    }

    fn next_progress(&self) -> SourceProgress {
        self.request.next_progress()
    }

    fn terminal(&self) -> bool {
        self.request.terminal
    }

    fn finish(self) -> MaterializationBatchItem {
        #[cfg(test)]
        assert_eq!(
            self.encoded_bytes,
            serde_json::to_vec(&self.request)
                .expect("encode coalesced source materialization request")
                .len(),
            "incremental source-page wire accounting diverged from authoritative JSON"
        );
        MaterializationBatchItem {
            request: self.request,
            record_count: self.record_count,
            content_bytes: self.content_bytes,
            encoded_bytes: self.encoded_bytes,
        }
    }
}

const EMPTY_MATERIALIZATION_BATCH_JSON_BYTES: usize = b"{\"pages\":[]}".len();

struct MaterializationBatchItem {
    request: MaterializeSourcePageRequest,
    record_count: usize,
    content_bytes: usize,
    encoded_bytes: usize,
}

impl MaterializationBatchItem {
    #[cfg(test)]
    fn from_request(request: MaterializeSourcePageRequest) -> Result<Self> {
        let mut content_bytes = 0_usize;
        for record in &request.records {
            content_bytes = content_bytes
                .checked_add(
                    record
                        .validate_and_count_bytes()
                        .map_err(|error| anyhow!("invalid_request: {}", error.message))?,
                )
                .ok_or_else(|| {
                    anyhow!("invalid_request: source batch transient-content bytes overflowed")
                })?;
        }
        let encoded_bytes = serde_json::to_vec(&request)
            .map_err(|error| anyhow!("internal: encode source materialization request: {error}"))?
            .len();
        Ok(Self {
            record_count: request.records.len(),
            request,
            content_bytes,
            encoded_bytes,
        })
    }
}

struct MaterializationBatchBuilder {
    pages: Vec<MaterializeSourcePageRequest>,
    record_count: usize,
    content_bytes: usize,
    encoded_bytes: usize,
}

impl MaterializationBatchBuilder {
    fn new() -> Self {
        Self {
            pages: Vec::new(),
            record_count: 0,
            content_bytes: 0,
            encoded_bytes: EMPTY_MATERIALIZATION_BATCH_JSON_BYTES,
        }
    }

    fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    fn accepts(&self, item: &MaterializationBatchItem) -> Result<bool> {
        if let Some(first) = self.pages.first() {
            if first.core_generation_id != item.request.core_generation_id {
                bail!("invalid_request: source materialization batch mixes Core generations");
            }
        }
        if let Some(last) = self.pages.last() {
            if last.expected_prior.source.identity().digest()
                >= item.request.expected_prior.source.identity().digest()
            {
                bail!(
                    "invalid_request: source materialization batch pages must be sorted and unique by source identity"
                );
            }
        }
        let item_count = self.pages.len().checked_add(1).ok_or_else(|| {
            anyhow!("invalid_request: source materialization batch page count overflowed")
        })?;
        let record_count = self
            .record_count
            .checked_add(item.record_count)
            .ok_or_else(|| {
                anyhow!("invalid_request: source materialization batch record count overflowed")
            })?;
        let content_bytes = self
            .content_bytes
            .checked_add(item.content_bytes)
            .ok_or_else(|| {
                anyhow!(
                    "invalid_request: source materialization batch transient-content bytes overflowed"
                )
            })?;
        let separator_bytes = usize::from(!self.pages.is_empty());
        let encoded_bytes = self
            .encoded_bytes
            .checked_add(separator_bytes)
            .and_then(|bytes| bytes.checked_add(item.encoded_bytes))
            .ok_or_else(|| {
                anyhow!("invalid_request: source materialization batch encoded bytes overflowed")
            })?;
        Ok(
            item_count <= ctx_pro_host_protocol::MAX_SOURCE_MATERIALIZATION_BATCH_ITEMS
                && record_count <= ctx_pro_host_protocol::MAX_SOURCE_MATERIALIZATION_BATCH_RECORDS
                && content_bytes
                    <= ctx_pro_host_protocol::MAX_SOURCE_MATERIALIZATION_BATCH_CONTENT_BYTES
                && encoded_bytes
                    <= ctx_pro_host_protocol::MAX_SOURCE_MATERIALIZATION_BATCH_WIRE_BYTES,
        )
    }

    fn push(&mut self, item: MaterializationBatchItem) -> Result<()> {
        self.record_count = self
            .record_count
            .checked_add(item.record_count)
            .ok_or_else(|| {
                anyhow!("invalid_request: source materialization batch record count overflowed")
            })?;
        self.content_bytes = self
            .content_bytes
            .checked_add(item.content_bytes)
            .ok_or_else(|| {
                anyhow!(
                    "invalid_request: source materialization batch transient-content bytes overflowed"
                )
            })?;
        self.encoded_bytes = self
            .encoded_bytes
            .checked_add(usize::from(!self.pages.is_empty()))
            .and_then(|bytes| bytes.checked_add(item.encoded_bytes))
            .ok_or_else(|| {
                anyhow!("invalid_request: source materialization batch encoded bytes overflowed")
            })?;
        self.pages.push(item.request);
        Ok(())
    }

    fn take_request(&mut self) -> MaterializeSourcePagesRequest {
        let pages = std::mem::take(&mut self.pages);
        self.record_count = 0;
        self.content_bytes = 0;
        self.encoded_bytes = EMPTY_MATERIALIZATION_BATCH_JSON_BYTES;
        MaterializeSourcePagesRequest { pages }
    }
}

#[cfg(test)]
pub(super) fn materialization_batch_accounting(
    request: &MaterializeSourcePagesRequest,
) -> Result<(usize, usize, usize, usize)> {
    let mut builder = MaterializationBatchBuilder::new();
    for page in &request.pages {
        let item = MaterializationBatchItem::from_request(page.clone())?;
        if !builder.accepts(&item)? {
            bail!("fixture source materialization batch exceeds its running bounds");
        }
        builder.push(item)?;
    }
    Ok((
        builder.pages.len(),
        builder.record_count,
        builder.content_bytes,
        builder.encoded_bytes,
    ))
}

fn enqueue_materialization_request<C>(
    consumer: &mut C,
    pending: &mut MaterializationBatchBuilder,
    latest_progress: &mut BTreeMap<[u8; 32], SourceBackedProProgress>,
    item: MaterializationBatchItem,
) -> Result<()>
where
    C: SourceBackedProPageConsumer,
{
    if pending.accepts(&item)? {
        return pending.push(item);
    }
    if !pending.is_empty() {
        flush_materialization_batch(consumer, pending, latest_progress)?;
    }
    if !pending.accepts(&item)? {
        bail!("invalid_request: source materialization request exceeds aggregate batch bounds");
    }
    pending.push(item)
}

fn flush_materialization_batch<C>(
    consumer: &mut C,
    pending: &mut MaterializationBatchBuilder,
    latest_progress: &mut BTreeMap<[u8; 32], SourceBackedProProgress>,
) -> Result<()>
where
    C: SourceBackedProPageConsumer,
{
    if pending.is_empty() {
        return Ok(());
    }
    let request = pending.take_request();
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let materialized = consumer.materialize_source_pages(&request)?;
    materialized
        .validate_for_validated_request(&request)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let updates = materialized
        .pages
        .into_iter()
        .map(|page| (page.progress.source.identity().digest(), page.progress))
        .collect::<Vec<_>>();
    for (source_id, progress) in updates {
        latest_progress.insert(source_id, progress);
    }
    Ok(())
}

pub(super) fn sync_source_backed_pro_feed_deferred_paged<P>(
    data_root: &Path,
    manifest: SourceBackedProManifest,
    generation_manifest: &ctx_history_index::GenerationManifest,
    provider: &mut P,
) -> Result<SourceBackedProSyncReport>
where
    P: SourceBackedProProvider,
{
    let required = source_materialization_capabilities();
    let client = ProClient::connect(data_root, &required)?;
    let mut consumer = ProtocolSourceBackedProConsumer { client };
    sync_source_backed_pro_feed_paged(manifest, generation_manifest, provider, &mut consumer)
}

pub(super) fn source_materialization_capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([Capability::SourceMaterialization])
}
