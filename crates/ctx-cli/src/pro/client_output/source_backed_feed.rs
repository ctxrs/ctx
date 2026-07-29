use super::*;
use anyhow::Context as _;
use ctx_pro_host_protocol::{
    certified_source_revision_sha256, AdmitSourceManifestPageRequest,
    BeginSourceManifestAdmissionRequest, DeleteSourceRequest, FinishAdmittedSourceManifestRequest,
    FinishSourceManifestAdmissionRequest, FinishSourceManifestRequest,
    MaterializeSourcePageRequest, PrepareSourceRequest, SourceDeleted, SourceDisposition,
    SourceManifest, SourceManifestAdmissionBegan, SourceManifestAdmissionReceipt,
    SourceManifestAdmitted, SourceManifestBegan, SourceManifestFinished, SourceManifestHeader,
    SourceManifestPage, SourceManifestPageAdmitted, SourceManifestPageEntries,
    SourceManifestReceipt, SourcePageMaterialized, SourcePrepared, SourceProgress, SourceRecord,
    SourceRemoval, MAX_SOURCE_MANIFEST_PAGE_ITEMS,
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

    fn materialize_source_page(
        &mut self,
        request: &MaterializeSourcePageRequest,
    ) -> Result<SourcePageMaterialized>;

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

    fn materialize_source_page(
        &mut self,
        request: &MaterializeSourcePageRequest,
    ) -> Result<SourcePageMaterialized> {
        validate_request(request)?;
        match self.exchange(HostMessage::MaterializeSourcePage(request.clone()))? {
            HelperMessage::SourcePageMaterialized(result) => {
                validate_response(&result)?;
                Ok(result)
            }
            HelperMessage::Error(error) => Err(protocol_error(error)),
            _ => {
                bail!("invalid_response: helper returned a non-source-page response")
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

struct AdmittedSourceBackedConsumer<'a, C> {
    inner: &'a mut C,
    header: SourceManifestHeader,
    admission: SourceManifestAdmissionReceipt,
    materializer_revision: String,
    progress: Vec<SourceProgress>,
}

impl<C> SourceBackedProPageConsumer for AdmittedSourceBackedConsumer<'_, C>
where
    C: SourceBackedProAdmissionConsumer,
{
    fn prepare_source(&mut self, request: &PrepareSourceRequest) -> Result<SourcePrepared> {
        SourceBackedProPageConsumer::prepare_source(self.inner, request)
    }

    fn materialize_source_page(
        &mut self,
        request: &MaterializeSourcePageRequest,
    ) -> Result<SourcePageMaterialized> {
        SourceBackedProPageConsumer::materialize_source_page(self.inner, request)
    }

    fn delete_source(&mut self, request: &DeleteSourceRequest) -> Result<SourceDeleted> {
        SourceBackedProPageConsumer::delete_source(self.inner, request)
    }
}

impl<C> SourceBackedProConsumer for AdmittedSourceBackedConsumer<'_, C>
where
    C: SourceBackedProAdmissionConsumer,
{
    fn begin_source_manifest(
        &mut self,
        manifest: &SourceBackedProManifest,
    ) -> Result<SourceManifestBegan> {
        self.header
            .validate_contents(&manifest.sources, &manifest.removals)
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        Ok(SourceManifestBegan {
            core_generation_id: self.header.core_generation_id.clone(),
            materializer_revision: self.materializer_revision.clone(),
            progress: self.progress.clone(),
            replayed: true,
        })
    }

    fn finish_source_manifest(
        &mut self,
        request: &FinishSourceManifestRequest,
    ) -> Result<SourceManifestFinished> {
        self.header
            .validate_contents(&request.manifest.sources, &request.manifest.removals)
            .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
        SourceBackedProAdmissionConsumer::finish_admitted_source_manifest(
            self.inner,
            &FinishAdmittedSourceManifestRequest {
                admission: self.admission.clone(),
                expected_progress: request.expected_progress.clone(),
            },
        )
    }
}

pub(super) fn sync_source_backed_pro_feed_paged<P, C>(
    manifest: SourceBackedProManifest,
    generation_manifest: &ctx_history_index::GenerationManifest,
    provider: &mut P,
    consumer: &mut C,
) -> Result<SourceBackedProSyncReport>
where
    P: SourceBackedProProvider,
    C: SourceBackedProAdmissionConsumer,
{
    manifest
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let generation_id = generation_manifest
        .generation_id()
        .context("corrupt: hash pinned Core generation manifest")?;
    if generation_id != manifest.core_generation_id {
        bail!("source_changed: Pro source manifest is not the pinned Core generation");
    }
    let header = SourceManifestHeader::new(
        manifest.core_generation_id.clone(),
        generation_manifest.manifest_version,
        generation_manifest.identity_version,
        generation_manifest.lexical_schema_version,
        generation_manifest.lexical_analyzer_version,
        generation_manifest.policy_schema_hash.clone(),
        &manifest.sources,
        &manifest.removals,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let began = consumer.begin_source_manifest_admission(&header)?;
    began
        .validate_for(&header)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let mut cursor = began.cursor;

    while cursor.next_source_index < header.source_count {
        let page = next_source_manifest_page(&header, &cursor, &manifest.sources)?;
        let expected = cursor_after_page(&header, &cursor, &page)?;
        let admitted = consumer.admit_source_manifest_page(&page)?;
        admitted
            .validate_for(&header)
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
        if admitted.cursor != expected {
            bail!("invalid_response: helper advanced the wrong source admission cursor");
        }
        cursor = admitted.cursor;
    }
    while cursor.next_removal_index < header.removal_count {
        let page = next_removal_manifest_page(&header, &cursor, &manifest.removals)?;
        let expected = cursor_after_page(&header, &cursor, &page)?;
        let admitted = consumer.admit_source_manifest_page(&page)?;
        admitted
            .validate_for(&header)
            .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
        if admitted.cursor != expected {
            bail!("invalid_response: helper advanced the wrong removal admission cursor");
        }
        cursor = admitted.cursor;
    }
    if !cursor.is_complete_for(&header) {
        bail!("invalid_response: helper did not admit the exact source manifest");
    }
    let admitted = consumer.finish_source_manifest_admission(&header)?;
    if admitted.receipt.header != header || admitted.receipt.page_count != cursor.next_page_index {
        bail!("invalid_response: helper admitted the wrong source manifest identity");
    }
    let expected_aggregate = header.aggregate_sha256.clone();
    let mut admitted_consumer = AdmittedSourceBackedConsumer {
        inner: consumer,
        header,
        admission: admitted.receipt,
        materializer_revision: admitted.materializer_revision,
        progress: admitted.progress,
    };
    let report = sync_source_backed_pro_feed(manifest, provider, &mut admitted_consumer)?;
    if report.receipt.manifest_aggregate_sha256 != expected_aggregate {
        bail!("invalid_response: Pro activated the wrong source manifest aggregate");
    }
    Ok(report)
}

fn next_source_manifest_page(
    header: &SourceManifestHeader,
    cursor: &ctx_pro_host_protocol::SourceManifestAdmissionCursor,
    sources: &[CertifiedSource],
) -> Result<SourceManifestPage> {
    let start = usize::try_from(cursor.next_source_index)
        .map_err(|_| anyhow!("invalid_response: source admission cursor overflowed"))?;
    fit_manifest_page(header, cursor, start, sources, |entries| {
        SourceManifestPageEntries::Sources(entries)
    })
}

fn next_removal_manifest_page(
    header: &SourceManifestHeader,
    cursor: &ctx_pro_host_protocol::SourceManifestAdmissionCursor,
    removals: &[SourceRemoval],
) -> Result<SourceManifestPage> {
    let start = usize::try_from(cursor.next_removal_index)
        .map_err(|_| anyhow!("invalid_response: removal admission cursor overflowed"))?;
    fit_manifest_page(header, cursor, start, removals, |entries| {
        SourceManifestPageEntries::Removals(entries)
    })
}

fn fit_manifest_page<T: Clone>(
    header: &SourceManifestHeader,
    cursor: &ctx_pro_host_protocol::SourceManifestAdmissionCursor,
    start: usize,
    entries: &[T],
    payload: impl Fn(Vec<T>) -> SourceManifestPageEntries,
) -> Result<SourceManifestPage> {
    let mut end = start
        .saturating_add(MAX_SOURCE_MANIFEST_PAGE_ITEMS)
        .min(entries.len());
    if start >= end {
        bail!("invalid_response: source admission cursor is outside its manifest entries");
    }
    loop {
        match SourceManifestPage::new(
            header,
            cursor.next_page_index,
            u32::try_from(start)
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            payload(entries[start..end].to_vec()),
        ) {
            Ok(page) => {
                let request = AdmitSourceManifestPageRequest { page: page.clone() };
                match request.validate() {
                    Ok(()) => return Ok(page),
                    Err(error)
                        if error.class == ctx_pro_host_protocol::ErrorClass::Bounds
                            && end > start + 1 =>
                    {
                        end -= 1;
                    }
                    Err(error) => {
                        return Err(anyhow!("invalid_request: {}", error.message));
                    }
                }
            }
            Err(error)
                if error.class == ctx_pro_host_protocol::ErrorClass::Bounds && end > start + 1 =>
            {
                end -= 1;
            }
            Err(error) => return Err(anyhow!("invalid_request: {}", error.message)),
        }
    }
}

pub(super) fn cursor_after_page(
    header: &SourceManifestHeader,
    cursor: &ctx_pro_host_protocol::SourceManifestAdmissionCursor,
    page: &SourceManifestPage,
) -> Result<ctx_pro_host_protocol::SourceManifestAdmissionCursor> {
    let count = u32::try_from(page.entries.len())
        .map_err(|_| anyhow!("invalid_request: manifest page item count overflowed"))?;
    let mut next = cursor.clone();
    next.next_page_index = next
        .next_page_index
        .checked_add(1)
        .ok_or_else(|| anyhow!("invalid_request: manifest page count overflowed"))?;
    match &page.entries {
        SourceManifestPageEntries::Sources(_) => {
            if page.item_index != cursor.next_source_index || cursor.next_removal_index != 0 {
                bail!("invalid_response: source page is outside its admission cursor");
            }
            next.next_source_index = next
                .next_source_index
                .checked_add(count)
                .ok_or_else(|| anyhow!("invalid_request: source count overflowed"))?;
        }
        SourceManifestPageEntries::Removals(_) => {
            if cursor.next_source_index != header.source_count
                || page.item_index != cursor.next_removal_index
            {
                bail!("invalid_response: removal page is outside its admission cursor");
            }
            next.next_removal_index = next
                .next_removal_index
                .checked_add(count)
                .ok_or_else(|| anyhow!("invalid_request: removal count overflowed"))?;
        }
    }
    next.validate_for(header)
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(next)
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

    let mut final_sources = Vec::with_capacity(manifest.sources.len());
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
        #[cfg(test)]
        {
            prepared_sources = prepared_sources.saturating_add(1);
            if disposition == SourceBackedProDisposition::Rewrite {
                rewritten_sources = rewritten_sources.saturating_add(1);
            }
        }

        let mut source_pages = 0_u64;
        while !prepared.terminal {
            source_pages = source_pages.saturating_add(1);
            if source_pages > MAX_CANONICAL_SOURCE_PAGES {
                bail!("invalid_response: provider source page count exceeds safety bound");
            }
            let page = provider.reread_source_page(source, prepared.frontier.as_ref())?;
            validate_provider_page(source, &prepared, &page)?;
            let accepted_records = u32::try_from(page.records.len())
                .map_err(|_| anyhow!("invalid_request: source page record count overflow"))?;
            let request = MaterializeSourcePageRequest {
                core_generation_id: manifest.core_generation_id.clone(),
                expected_prior: prepared.clone(),
                next_frontier: page.next_frontier,
                terminal: page.terminal,
                records: page.records,
            };
            request
                .validate()
                .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
            let materialized = consumer.materialize_source_page(&request)?;
            validate_materialized_page(&request, &materialized, accepted_records)?;
            prepared = materialized.progress;
            #[cfg(test)]
            {
                reread_pages = reread_pages.saturating_add(1);
                reread_records = reread_records.saturating_add(u64::from(accepted_records));
            }
        }
        if prepared.frontier.as_ref() != source.frontier() {
            bail!("invalid_response: Pro terminated at the wrong certified source frontier");
        }
        final_sources.push(SourceBackedProProgress {
            source: source_key.clone(),
            source_epoch: prepared.source_epoch,
            certified_revision_sha256,
            frontier: prepared.frontier,
            materializer_revision: consumer_state.materializer_revision.clone(),
            terminal: true,
        });
    }
    if !progress_by_source.is_empty() {
        bail!("invalid_response: unreconciled Pro source progress remains");
    }
    final_sources.sort_by_key(|progress| progress.source.identity().digest());
    let finish = FinishSourceManifestRequest {
        manifest: manifest.clone(),
        expected_progress: final_sources.clone(),
    };
    finish
        .validate()
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

fn validate_consumer_state(
    manifest: &SourceBackedProManifest,
    state: &SourceManifestBegan,
) -> Result<()> {
    state
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if state.core_generation_id != manifest.core_generation_id {
        bail!("invalid_response: Pro began the wrong source manifest");
    }
    Ok(())
}

fn validate_prepared_source(
    request: &PrepareSourceRequest,
    prepared: &SourcePrepared,
) -> Result<()> {
    prepared
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let progress = &prepared.progress;
    if prepared.core_generation_id != request.core_generation_id
        || !progress.source.exact_descriptor_eq(&request.source)
        || progress.certified_revision_sha256 != request.certified_revision_sha256
        || progress.materializer_revision != request.materializer_revision
    {
        bail!("invalid_response: Pro prepared the wrong canonical source");
    }
    match (request.disposition, request.expected_prior.as_ref()) {
        (SourceBackedProDisposition::NewSource, None) => {
            if progress.source_epoch != 1 || progress.frontier.is_some() || progress.terminal {
                bail!("invalid_response: new Pro source did not start from genesis");
            }
        }
        (SourceBackedProDisposition::Resume, Some(prior)) => {
            if !progress.exact_eq(prior) {
                bail!("invalid_response: Pro did not resume the exact source CAS");
            }
        }
        (SourceBackedProDisposition::Rewrite, Some(prior)) => {
            if progress.source_epoch
                != prior
                    .source_epoch
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("invalid_request: source epoch is exhausted"))?
                || progress.frontier.is_some()
                || progress.terminal
            {
                bail!("invalid_response: Pro did not invalidate the rewritten source epoch");
            }
        }
        _ => bail!("invalid_request: source disposition and prior progress disagree"),
    }
    Ok(())
}

fn validate_provider_page(
    source: &CertifiedSource,
    prepared: &SourceBackedProProgress,
    page: &SourceBackedProviderPage,
) -> Result<()> {
    let source_key = source.observation().source();
    if !page.source.exact_descriptor_eq(source_key)
        || page.expected_prior_frontier != prepared.frontier
    {
        bail!("source_changed: provider returned a page for the wrong source frontier");
    }
    if page.terminal {
        if page.next_frontier.as_ref() != source.frontier() {
            bail!("source_changed: terminal provider page missed the certified frontier");
        }
    } else if page.records.is_empty()
        || page.next_frontier.is_none()
        || page.next_frontier == page.expected_prior_frontier
        || page.next_frontier.as_ref() == source.frontier()
    {
        bail!("source_changed: provider returned a non-progressing source page");
    }
    Ok(())
}

fn validate_materialized_page(
    request: &MaterializeSourcePageRequest,
    materialized: &SourcePageMaterialized,
    accepted_records: u32,
) -> Result<()> {
    materialized
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    if materialized.core_generation_id != request.core_generation_id
        || !materialized.progress.exact_eq(&request.next_progress())
        || materialized.accepted_records != accepted_records
    {
        bail!("invalid_response: Pro acknowledged the wrong source page CAS");
    }
    Ok(())
}

fn validate_source_backed_receipt(
    manifest: &SourceBackedProManifest,
    materializer_revision: &str,
    expected_sources: &[SourceBackedProProgress],
    receipt: &SourceBackedProReceipt,
) -> Result<()> {
    if receipt.core_generation_id != manifest.core_generation_id
        || receipt.materializer_revision != materializer_revision
        || receipt.progress.len() != expected_sources.len()
        || !receipt
            .progress
            .iter()
            .zip(expected_sources)
            .all(|(actual, expected)| source_progress_exact_eq(actual, expected))
    {
        bail!("invalid_response: Pro published the wrong source-backed receipt");
    }
    Ok(())
}

fn source_progress_exact_eq(
    left: &SourceBackedProProgress,
    right: &SourceBackedProProgress,
) -> bool {
    left.exact_eq(right)
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

fn source_identity_digest(source: &CertifiedSource) -> [u8; 32] {
    source.observation().source().identity().digest()
}

fn validate_request<T>(request: &T) -> Result<()>
where
    T: SourceRequestValidation,
{
    request
        .validate_request()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))
}

fn validate_response<T>(response: &T) -> Result<()>
where
    T: SourceResponseValidation,
{
    response
        .validate_response()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))
}

trait SourceRequestValidation {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
}

impl SourceRequestValidation for PrepareSourceRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

impl SourceRequestValidation for MaterializeSourcePageRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

impl SourceRequestValidation for DeleteSourceRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

impl SourceRequestValidation for FinishSourceManifestRequest {
    fn validate_request(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
        self.validate()
    }
}

trait SourceResponseValidation {
    fn validate_response(&self) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
}

macro_rules! source_response_validation {
    ($($type:ty),+ $(,)?) => {
        $(
            impl SourceResponseValidation for $type {
                fn validate_response(
                    &self,
                ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
                    self.validate()
                }
            }
        )+
    };
}

source_response_validation!(
    SourceManifestBegan,
    SourcePrepared,
    SourcePageMaterialized,
    SourceDeleted,
    SourceManifestFinished,
);
