use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{anyhow, bail, Result};
use ctx_history_core::{CertifiedSource, SourceFrontier, SourceKey};
use ctx_pro_host_protocol::{Capability, HelperMessage, HostMessage};

use super::{protocol_error, ProClient, BATCH_TIMEOUT};

#[path = "source_backed_pro_provider.rs"]
mod source_backed_pro_provider;

/// Synchronizes Pro from one already-published, generation-pinned Core source manifest.
///
/// Exact canonical content is hydrated through the supplied source resolver.
/// Any helper or hydration failure leaves Core intact and Pro independently retryable.
pub(crate) fn sync_source_manifest_materialization(
    data_root: &Path,
    manifest: ctx_pro_host_protocol::SourceManifest,
    index: &ctx_history_index::VerifiedIndex,
    resolver: &ctx_history_capture::SourceBackedResolverRegistry,
) -> Result<ctx_pro_host_protocol::SourceManifestReceipt> {
    source_backed_pro_provider::sync_generation_pinned_source_manifest(
        data_root, manifest, index, resolver,
    )
    .map(|report| report.receipt)
}

/// Deferred Pro catch-up over the authoritative source-backed wire contract.
///
/// Core is already published before this coordinator runs. Any helper or
/// hydration failure therefore leaves Core intact and Pro retryable from its
/// independently committed per-source progress.
mod source_backed_feed {
    use super::*;
    use anyhow::Context as _;
    use ctx_pro_host_protocol::{
        certified_source_revision_sha256, AdmitSourceManifestPageRequest,
        BeginSourceManifestAdmissionRequest, DeleteSourceRequest,
        FinishAdmittedSourceManifestRequest, FinishSourceManifestAdmissionRequest,
        FinishSourceManifestRequest, MaterializeSourcePageRequest, PrepareSourceRequest,
        SourceDeleted, SourceDisposition, SourceManifest, SourceManifestAdmissionBegan,
        SourceManifestAdmissionReceipt, SourceManifestAdmitted, SourceManifestBegan,
        SourceManifestFinished, SourceManifestHeader, SourceManifestPage,
        SourceManifestPageAdmitted, SourceManifestPageEntries, SourceManifestReceipt,
        SourcePageMaterialized, SourcePrepared, SourceProgress, SourceRecord, SourceRemoval,
        MAX_SOURCE_MANIFEST_PAGE_ITEMS,
    };

    const MAX_CANONICAL_SOURCE_PAGES: u64 = 1_000_000;

    pub(super) type SourceBackedProManifest = SourceManifest;
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
        pub(super) prepared_sources: u64,
        pub(super) rewritten_sources: u64,
        pub(super) deleted_sources: u64,
        pub(super) reread_pages: u64,
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
        if admitted.receipt.header != header
            || admitted.receipt.page_count != cursor.next_page_index
        {
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
                    if error.class == ctx_pro_host_protocol::ErrorClass::Bounds
                        && end > start + 1 =>
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
            deleted_sources = deleted_sources.saturating_add(1);
        }

        let mut final_sources = Vec::with_capacity(manifest.sources.len());
        let mut prepared_sources = 0_u64;
        let mut rewritten_sources = 0_u64;
        let mut reread_pages = 0_u64;
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
                        && progress.materializer_revision
                            == consumer_state.materializer_revision =>
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
            prepared_sources = prepared_sources.saturating_add(1);
            if disposition == SourceBackedProDisposition::Rewrite {
                rewritten_sources = rewritten_sources.saturating_add(1);
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
                reread_pages = reread_pages.saturating_add(1);
                reread_records = reread_records.saturating_add(u64::from(accepted_records));
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
            prepared_sources,
            rewritten_sources,
            deleted_sources,
            reread_pages,
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
        fn validate_request(&self)
            -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
    }

    impl SourceRequestValidation for PrepareSourceRequest {
        fn validate_request(
            &self,
        ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
            self.validate()
        }
    }

    impl SourceRequestValidation for MaterializeSourcePageRequest {
        fn validate_request(
            &self,
        ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
            self.validate()
        }
    }

    impl SourceRequestValidation for DeleteSourceRequest {
        fn validate_request(
            &self,
        ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
            self.validate()
        }
    }

    impl SourceRequestValidation for FinishSourceManifestRequest {
        fn validate_request(
            &self,
        ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError> {
            self.validate()
        }
    }

    trait SourceResponseValidation {
        fn validate_response(
            &self,
        ) -> std::result::Result<(), ctx_pro_host_protocol::ProtocolError>;
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
}

#[cfg(test)]
mod tests {
    use super::source_backed_feed::*;
    use super::*;
    use std::fs;

    use ctx_history_capture::{ingest_codex_source_backed_v0, CodexLocatorResolverV0};
    use ctx_history_core::{
        CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory, ScannedSourceCounts,
        SourceInventoryObservation, SourceObservation, TypedKey,
    };
    use ctx_history_index::VerifiedIndex;
    use ctx_pro_host_protocol::{
        certified_source_revision_sha256, DeleteSourceRequest, FinishAdmittedSourceManifestRequest,
        FinishSourceManifestRequest, MaterializeSourcePageRequest, PrepareSourceRequest,
        SourceDeleted, SourceManifestAdmissionBegan, SourceManifestAdmitted, SourceManifestBegan,
        SourceManifestFinished, SourceManifestHeader, SourceManifestPage,
        SourceManifestPageAdmitted, SourceMessageFact, SourcePageMaterialized, SourcePrepared,
        SourceRecordMetadata, SourceSessionRelationships, TransientSourceContent,
        TransientSourceFact,
    };
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    const MATERIALIZER_REVISION: &str = "pro-source-materializer-v1";

    #[derive(Debug, Clone)]
    struct FixtureSourceFeed {
        generation_id: String,
        generation_manifest: ctx_history_index::GenerationManifest,
        source: CertifiedSource,
        records: Vec<SourceBackedProRecord>,
        intermediate_frontier: SourceFrontier,
    }

    impl FixtureSourceFeed {
        fn provider(&self) -> FixtureProvider {
            let terminal_frontier = self
                .source
                .frontier()
                .expect("Codex fixture has a certified frontier")
                .clone();
            FixtureProvider {
                pages: vec![
                    SourceBackedProviderPage {
                        source: self.source.observation().source().clone(),
                        expected_prior_frontier: None,
                        next_frontier: Some(self.intermediate_frontier.clone()),
                        terminal: false,
                        records: vec![self.records[0].clone()],
                    },
                    SourceBackedProviderPage {
                        source: self.source.observation().source().clone(),
                        expected_prior_frontier: Some(self.intermediate_frontier.clone()),
                        next_frontier: Some(terminal_frontier),
                        terminal: true,
                        records: vec![self.records[1].clone()],
                    },
                ],
                requests: Vec::new(),
            }
        }

        fn manifest(&self) -> SourceBackedProManifest {
            SourceBackedProManifest::new(
                self.generation_id.clone(),
                vec![self.source.clone()],
                Vec::new(),
            )
            .expect("fixture manifest")
        }
    }

    #[derive(Default)]
    struct FixtureProvider {
        pages: Vec<SourceBackedProviderPage>,
        requests: Vec<Option<SourceFrontier>>,
    }

    impl SourceBackedProProvider for FixtureProvider {
        fn reread_source_page(
            &mut self,
            source: &CertifiedSource,
            expected_prior_frontier: Option<&SourceFrontier>,
        ) -> Result<SourceBackedProviderPage> {
            self.requests.push(expected_prior_frontier.cloned());
            self.pages
                .iter()
                .find(|page| {
                    page.source
                        .exact_descriptor_eq(source.observation().source())
                        && page.expected_prior_frontier.as_ref() == expected_prior_frontier
                })
                .cloned()
                .ok_or_else(|| anyhow!("fixture provider has no matching source page"))
        }
    }

    struct FixtureConsumer {
        materializer_revision: String,
        progress: BTreeMap<[u8; 32], SourceBackedProProgress>,
        durable_event_ids: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
        transient_record_digests: Vec<[u8; 32]>,
        dispositions: Vec<SourceBackedProDisposition>,
        deleted_epochs: Vec<u64>,
        finish_called: bool,
        corrupt_page_ack: bool,
        admission_header: Option<SourceManifestHeader>,
        admission_cursor: Option<ctx_pro_host_protocol::SourceManifestAdmissionCursor>,
        admission_replayed: Vec<bool>,
    }

    impl FixtureConsumer {
        fn new(progress: Vec<SourceBackedProProgress>) -> Self {
            Self {
                materializer_revision: MATERIALIZER_REVISION.to_owned(),
                progress: progress
                    .into_iter()
                    .map(|progress| (progress.source.identity().digest(), progress))
                    .collect(),
                durable_event_ids: BTreeMap::new(),
                transient_record_digests: Vec::new(),
                dispositions: Vec::new(),
                deleted_epochs: Vec::new(),
                finish_called: false,
                corrupt_page_ack: false,
                admission_header: None,
                admission_cursor: None,
                admission_replayed: Vec::new(),
            }
        }

        fn durable_ids_for(&self, source: &SourceKey) -> BTreeSet<[u8; 32]> {
            self.durable_event_ids
                .get(&source.identity().digest())
                .cloned()
                .unwrap_or_default()
        }
    }

    impl SourceBackedProPageConsumer for FixtureConsumer {
        fn prepare_source(&mut self, request: &PrepareSourceRequest) -> Result<SourcePrepared> {
            assert_eq!(request.materializer_revision, self.materializer_revision);
            self.dispositions.push(request.disposition);
            let (source_epoch, frontier, terminal) = match request.disposition {
                SourceBackedProDisposition::NewSource => (1, None, false),
                SourceBackedProDisposition::Resume => {
                    let prior = request.expected_prior.as_ref().expect("resume progress");
                    (prior.source_epoch, prior.frontier.clone(), prior.terminal)
                }
                SourceBackedProDisposition::Rewrite => {
                    let prior = request.expected_prior.as_ref().expect("rewrite progress");
                    self.durable_event_ids
                        .remove(&request.source.identity().digest());
                    (prior.source_epoch.saturating_add(1), None, false)
                }
            };
            Ok(SourcePrepared {
                core_generation_id: request.core_generation_id.clone(),
                progress: SourceBackedProProgress {
                    source: request.source.clone(),
                    source_epoch,
                    certified_revision_sha256: request.certified_revision_sha256.clone(),
                    frontier,
                    materializer_revision: request.materializer_revision.clone(),
                    terminal,
                },
                replayed: false,
            })
        }

        fn materialize_source_page(
            &mut self,
            request: &MaterializeSourcePageRequest,
        ) -> Result<SourcePageMaterialized> {
            let source_id = request.expected_prior.source.identity().digest();
            let durable = self.durable_event_ids.entry(source_id).or_default();
            for record in &request.records {
                self.transient_record_digests
                    .push(Sha256::digest(serde_json::to_vec(&record.facts).unwrap()).into());
                durable.insert(record.event_id.digest());
            }
            let progress = request.next_progress();
            self.progress.insert(source_id, progress.clone());
            let accepted_records = u32::try_from(request.records.len())
                .expect("fixture page record count fits u32")
                .saturating_add(u32::from(self.corrupt_page_ack));
            let materialized_facts = request.records.iter().fold(0_u32, |total, record| {
                total.saturating_add(
                    u32::try_from(record.facts.len()).expect("fixture fact count fits u32"),
                )
            });
            Ok(SourcePageMaterialized {
                core_generation_id: request.core_generation_id.clone(),
                progress,
                accepted_records,
                materialized_facts,
                replayed: false,
            })
        }

        fn delete_source(&mut self, request: &DeleteSourceRequest) -> Result<SourceDeleted> {
            assert!(request
                .removal
                .deletion
                .source()
                .exact_descriptor_eq(&request.expected_prior.source));
            let source_id = request.expected_prior.source.identity().digest();
            self.progress.remove(&source_id);
            self.durable_event_ids.remove(&source_id);
            self.deleted_epochs
                .push(request.expected_prior.source_epoch);
            Ok(SourceDeleted {
                core_generation_id: request.core_generation_id.clone(),
                source: request.expected_prior.source.clone(),
                removed_source_epoch: request.expected_prior.source_epoch,
                replayed: false,
            })
        }
    }

    impl SourceBackedProConsumer for FixtureConsumer {
        fn begin_source_manifest(
            &mut self,
            manifest: &SourceBackedProManifest,
        ) -> Result<SourceManifestBegan> {
            Ok(SourceManifestBegan {
                core_generation_id: manifest.core_generation_id.clone(),
                materializer_revision: self.materializer_revision.clone(),
                progress: self.progress.values().cloned().collect(),
                replayed: false,
            })
        }

        fn finish_source_manifest(
            &mut self,
            request: &FinishSourceManifestRequest,
        ) -> Result<SourceManifestFinished> {
            self.finish_called = true;
            self.progress = request
                .expected_progress
                .iter()
                .cloned()
                .map(|progress| (progress.source.identity().digest(), progress))
                .collect();
            Ok(SourceManifestFinished {
                receipt: SourceBackedProReceipt {
                    core_generation_id: request.manifest.core_generation_id.clone(),
                    manifest_aggregate_sha256: "b".repeat(64),
                    materializer_revision: self.materializer_revision.clone(),
                    progress: request.expected_progress.clone(),
                },
                replayed: false,
            })
        }
    }

    impl SourceBackedProAdmissionConsumer for FixtureConsumer {
        fn begin_source_manifest_admission(
            &mut self,
            header: &SourceManifestHeader,
        ) -> Result<SourceManifestAdmissionBegan> {
            let replayed = self.admission_header.as_ref() == Some(header);
            if !replayed {
                self.admission_header = Some(header.clone());
                self.admission_cursor =
                    Some(ctx_pro_host_protocol::SourceManifestAdmissionCursor::initial(header));
            }
            self.admission_replayed.push(replayed);
            Ok(SourceManifestAdmissionBegan {
                cursor: self
                    .admission_cursor
                    .clone()
                    .expect("fixture admission cursor"),
                replayed,
            })
        }

        fn admit_source_manifest_page(
            &mut self,
            page: &SourceManifestPage,
        ) -> Result<SourceManifestPageAdmitted> {
            let header = self
                .admission_header
                .as_ref()
                .expect("fixture admission header");
            let cursor = self
                .admission_cursor
                .as_ref()
                .expect("fixture admission cursor");
            let next = cursor_after_page(header, cursor, page)?;
            self.admission_cursor = Some(next.clone());
            Ok(SourceManifestPageAdmitted {
                cursor: next,
                replayed: false,
            })
        }

        fn finish_source_manifest_admission(
            &mut self,
            header: &SourceManifestHeader,
        ) -> Result<SourceManifestAdmitted> {
            assert_eq!(self.admission_header.as_ref(), Some(header));
            let cursor = self
                .admission_cursor
                .as_ref()
                .expect("fixture admission cursor");
            assert!(cursor.is_complete_for(header));
            Ok(SourceManifestAdmitted {
                receipt: ctx_pro_host_protocol::SourceManifestAdmissionReceipt {
                    header: header.clone(),
                    page_count: cursor.next_page_index,
                },
                materializer_revision: self.materializer_revision.clone(),
                progress: self.progress.values().cloned().collect(),
                replayed: false,
            })
        }

        fn finish_admitted_source_manifest(
            &mut self,
            request: &FinishAdmittedSourceManifestRequest,
        ) -> Result<SourceManifestFinished> {
            self.finish_called = true;
            self.progress = request
                .expected_progress
                .iter()
                .cloned()
                .map(|progress| (progress.source.identity().digest(), progress))
                .collect();
            Ok(SourceManifestFinished {
                receipt: SourceBackedProReceipt {
                    core_generation_id: request.admission.header.core_generation_id.clone(),
                    manifest_aggregate_sha256: request.admission.header.aggregate_sha256.clone(),
                    materializer_revision: self.materializer_revision.clone(),
                    progress: request.expected_progress.clone(),
                },
                replayed: false,
            })
        }
    }

    #[test]
    fn source_backed_helper_negotiates_source_materialization_capability() {
        assert_eq!(
            source_backed_feed::source_materialization_capabilities(),
            BTreeSet::from([Capability::SourceMaterialization])
        );
    }

    #[test]
    fn public_transport_exposes_only_the_paged_source_manifest_path() {
        let source = include_str!("client_output.rs");
        for forbidden in [
            ["Pro", "OutputImport"].concat(),
            ["ClientPro", "OutputSink"].concat(),
            ["Import", "Profile"].concat(),
            ["BeginOutput", "Inventory"].concat(),
            ["MaterializeOutput", "Page"].concat(),
            ["ProReplay", "Only"].concat(),
            ["sync_source_backed_pro_feed_", "deferred("].concat(),
            ["HostMessage::BeginSource", "Manifest("].concat(),
            ["HostMessage::FinishSource", "Manifest("].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "public Pro transport contains retired path {forbidden}"
            );
        }
        for required in [
            "sync_source_manifest_materialization",
            "BeginSourceManifestAdmission",
            "AdmitSourceManifestPage",
            "FinishSourceManifestAdmission",
            "FinishAdmittedSourceManifest",
        ] {
            assert!(
                source.contains(required),
                "public Pro transport is missing {required}"
            );
        }
    }

    #[test]
    fn source_backed_pro_v026_uses_generation_pinned_paged_manifest_admission() {
        let fixture = public_codex_fixture();
        let mut provider = fixture.provider();
        let mut consumer = FixtureConsumer::new(Vec::new());
        let report = sync_source_backed_pro_feed_paged(
            fixture.manifest(),
            &fixture.generation_manifest,
            &mut provider,
            &mut consumer,
        )
        .expect("paged source admission and catch-up");

        assert!(consumer.finish_called);
        assert_eq!(report.receipt.core_generation_id, fixture.generation_id);
        assert_eq!(
            report.receipt.manifest_aggregate_sha256,
            consumer
                .admission_header
                .as_ref()
                .expect("admission header")
                .aggregate_sha256
        );
        assert_eq!(
            consumer
                .admission_cursor
                .as_ref()
                .expect("admission cursor")
                .next_source_index,
            1
        );
    }

    #[test]
    fn paged_manifest_restart_replays_admission_and_reuses_terminal_source_progress() {
        let fixture = public_codex_fixture();
        let mut consumer = FixtureConsumer::new(Vec::new());
        let mut first_provider = fixture.provider();
        let first = sync_source_backed_pro_feed_paged(
            fixture.manifest(),
            &fixture.generation_manifest,
            &mut first_provider,
            &mut consumer,
        )
        .expect("initial paged source sync");
        let first_receipt = first.receipt;

        let mut restarted_provider = fixture.provider();
        let restarted = sync_source_backed_pro_feed_paged(
            fixture.manifest(),
            &fixture.generation_manifest,
            &mut restarted_provider,
            &mut consumer,
        )
        .expect("restarted paged source sync");

        assert_eq!(consumer.admission_replayed, [false, true]);
        assert_eq!(restarted.reread_pages, 0);
        assert_eq!(restarted.reread_records, 0);
        assert!(restarted_provider.requests.is_empty());
        assert_eq!(restarted.receipt, first_receipt);
    }

    #[test]
    fn source_backed_pro_new_and_lagging_reread_from_independent_frontiers() {
        let fixture = public_codex_fixture();

        let mut new_provider = fixture.provider();
        let mut new_consumer = FixtureConsumer::new(Vec::new());
        let new_report =
            sync_source_backed_pro_feed(fixture.manifest(), &mut new_provider, &mut new_consumer)
                .expect("new Pro source catch-up");

        assert_eq!(
            new_consumer.dispositions,
            [SourceBackedProDisposition::NewSource]
        );
        assert_eq!(new_report.reread_pages, 2);
        assert_eq!(new_report.reread_records, 2);
        assert_eq!(new_report.prepared_sources, 1);
        assert_eq!(new_report.receipt.core_generation_id, fixture.generation_id);
        assert_eq!(
            new_provider.requests,
            [None, Some(fixture.intermediate_frontier.clone())]
        );
        assert_eq!(new_consumer.transient_record_digests.len(), 2);
        assert_eq!(
            new_consumer.durable_ids_for(fixture.source.observation().source()),
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect()
        );

        let revision = certified_source_revision_sha256(&fixture.source).expect("source revision");
        let lagging_progress = SourceBackedProProgress {
            source: fixture.source.observation().source().clone(),
            source_epoch: 7,
            certified_revision_sha256: revision,
            frontier: Some(fixture.intermediate_frontier.clone()),
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            terminal: false,
        };
        let mut lagging_provider = fixture.provider();
        let mut lagging_consumer = FixtureConsumer::new(vec![lagging_progress]);
        lagging_consumer
            .durable_event_ids
            .entry(fixture.source.observation().source().identity().digest())
            .or_default()
            .insert(fixture.records[0].event_id.digest());
        let lagging_report = sync_source_backed_pro_feed(
            fixture.manifest(),
            &mut lagging_provider,
            &mut lagging_consumer,
        )
        .expect("lagging Pro source catch-up");

        assert_eq!(
            lagging_consumer.dispositions,
            [SourceBackedProDisposition::Resume]
        );
        assert_eq!(
            lagging_provider.requests,
            [Some(fixture.intermediate_frontier)]
        );
        assert_eq!(lagging_report.reread_pages, 1);
        assert_eq!(lagging_report.reread_records, 1);
        assert_eq!(lagging_report.rewritten_sources, 0);
        assert_eq!(
            lagging_consumer.durable_ids_for(fixture.source.observation().source()),
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect()
        );
    }

    #[test]
    fn source_backed_pro_rewrite_invalidates_old_epoch_before_reread() {
        let fixture = public_codex_fixture();
        let rewritten = rewritten_certificate(&fixture.source);
        let old_revision = certified_source_revision_sha256(&fixture.source).expect("old revision");
        let prior = SourceBackedProProgress {
            source: fixture.source.observation().source().clone(),
            source_epoch: 11,
            certified_revision_sha256: old_revision,
            frontier: fixture.source.frontier().cloned(),
            materializer_revision: "pro-source-materializer-v0".to_owned(),
            terminal: true,
        };
        let source_id = prior.source.identity().digest();
        let retained_record = fixture.records[1].clone();
        let mut consumer = FixtureConsumer::new(vec![prior]);
        consumer.durable_event_ids.insert(
            source_id,
            fixture
                .records
                .iter()
                .map(|record| record.event_id.digest())
                .collect(),
        );
        let mut provider = FixtureProvider {
            pages: vec![SourceBackedProviderPage {
                source: rewritten.observation().source().clone(),
                expected_prior_frontier: None,
                next_frontier: rewritten.frontier().cloned(),
                terminal: true,
                records: vec![retained_record.clone()],
            }],
            requests: Vec::new(),
        };
        let manifest =
            SourceBackedProManifest::new("c".repeat(64), vec![rewritten], Vec::new()).unwrap();

        let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
            .expect("rewrite catch-up");

        assert_eq!(consumer.dispositions, [SourceBackedProDisposition::Rewrite]);
        assert_eq!(report.rewritten_sources, 1);
        assert_eq!(provider.requests, [None]);
        assert_eq!(
            consumer.durable_ids_for(fixture.source.observation().source()),
            BTreeSet::from([retained_record.event_id.digest()])
        );
        assert!(!consumer
            .durable_ids_for(fixture.source.observation().source())
            .contains(&fixture.records[0].event_id.digest()));
        assert_eq!(report.receipt.progress[0].source_epoch, 12);
    }

    #[test]
    fn source_backed_pro_deletion_requires_certified_complete_inventory() {
        let fixture = public_codex_fixture();
        let source = fixture.source.observation().source().clone();
        let prior = SourceBackedProProgress {
            source: source.clone(),
            source_epoch: 17,
            certified_revision_sha256: certified_source_revision_sha256(&fixture.source).unwrap(),
            frontier: fixture.source.frontier().cloned(),
            materializer_revision: MATERIALIZER_REVISION.to_owned(),
            terminal: true,
        };
        let inventory_observation = SourceInventoryObservation::new(
            source.provider(),
            "fixture-codex-root",
            TypedKey::utf8("public-codex-fixture").unwrap(),
            "fixture-inventory-v1",
            vec![1],
        )
        .unwrap();
        let inventory = CertifiedSourceInventory::certify(
            inventory_observation.clone(),
            inventory_observation,
            "fixture-discovery-v1",
            Vec::new(),
        )
        .unwrap();
        let deletion = CertifiedSourceDeletion::from_inventory(source.clone(), &inventory).unwrap();
        let removal = SourceBackedProRemoval::new(deletion, inventory).unwrap();
        let manifest =
            SourceBackedProManifest::new("d".repeat(64), Vec::new(), vec![removal]).unwrap();
        let mut provider = FixtureProvider::default();
        let mut consumer = FixtureConsumer::new(vec![prior.clone()]);
        consumer.durable_event_ids.insert(
            source.identity().digest(),
            BTreeSet::from([fixture.records[0].event_id.digest()]),
        );

        let report = sync_source_backed_pro_feed(manifest, &mut provider, &mut consumer)
            .expect("certified deletion");

        assert_eq!(report.deleted_sources, 1);
        assert_eq!(consumer.deleted_epochs, [17]);
        assert!(consumer.durable_ids_for(&source).is_empty());
        assert!(consumer.finish_called);
        assert!(report.receipt.progress.is_empty());

        let manifest_without_proof =
            SourceBackedProManifest::new("e".repeat(64), Vec::new(), Vec::new()).unwrap();
        let mut provider = FixtureProvider::default();
        let mut consumer = FixtureConsumer::new(vec![prior]);
        let error =
            sync_source_backed_pro_feed(manifest_without_proof, &mut provider, &mut consumer)
                .expect_err("missing deletion proof must fail");

        assert!(error.to_string().contains("without a certified deletion"));
        assert!(!consumer.finish_called);
    }

    #[test]
    fn source_backed_pro_mismatched_page_ack_never_publishes_receipt() {
        let fixture = public_codex_fixture();
        let mut provider = fixture.provider();
        let mut consumer = FixtureConsumer::new(Vec::new());
        consumer.corrupt_page_ack = true;

        let error = sync_source_backed_pro_feed(fixture.manifest(), &mut provider, &mut consumer)
            .expect_err("wrong CAS acknowledgement");

        assert!(error.to_string().contains("wrong source page CAS"));
        assert!(!consumer.finish_called);
    }

    fn public_codex_fixture() -> FixtureSourceFeed {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let manifest_dir = if manifest_dir.is_absolute() {
            manifest_dir.to_path_buf()
        } else {
            std::env::current_dir()
                .expect("Bazel test current directory")
                .join(manifest_dir)
        };
        let public_fixture_root = fs::canonicalize(
            manifest_dir.join("../../tests/fixtures/provider-history/codex-sessions"),
        )
        .expect("canonical public Codex fixture root");
        let temp = tempdir().expect("temporary source-backed index");
        let fixture_root = temp.path().join("sessions");
        let fixture_day = fixture_root.join("2026/06/23");
        fs::create_dir_all(&fixture_day).expect("fixture destination");
        for filename in ["root.jsonl", "subagent.jsonl"] {
            fs::copy(
                public_fixture_root.join("2026/06/23").join(filename),
                fixture_day.join(filename),
            )
            .expect("copy public Codex fixture into ordinary test files");
        }
        let index_root = temp.path().join("index");
        ingest_codex_source_backed_v0(&fixture_root, &index_root)
            .expect("ingest public Codex fixture");
        let index = VerifiedIndex::open(&index_root).expect("open fixture source manifest");
        let candidate = index
            .search_event_candidates("Follow repo instructions", 1)
            .expect("search fixture")
            .into_iter()
            .next()
            .expect("fixture event");
        let events = index
            .events_for_session(candidate.event.session_id.as_uuid())
            .expect("fixture session events");
        assert!(events.len() >= 2);
        let source_key = events[0].locator.source().clone();
        let source = index
            .manifest()
            .sources
            .iter()
            .find(|source| {
                source
                    .observation()
                    .source()
                    .exact_descriptor_eq(&source_key)
            })
            .expect("fixture source certificate")
            .clone();
        let resolver =
            CodexLocatorResolverV0::discover([&fixture_root]).expect("fixture locator resolver");
        let records = events
            .into_iter()
            .take(2)
            .map(|event| {
                let hydrated = resolver
                    .hydrate(&event.locator)
                    .expect("hydrate fixture record");
                let provider_record: serde_json::Value =
                    serde_json::from_slice(&hydrated.provider_bytes)
                        .expect("parse hydrated fixture record");
                let detector_message = provider_record
                    .pointer("/payload/content")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    !detector_message.is_empty(),
                    "fixture provider adapter must normalize detector message content"
                );
                SourceBackedProRecord::new(
                    event.event_id,
                    event.session_id,
                    event.locator,
                    SourceSessionRelationships {
                        direct_session_id: event.session_id,
                        root_session_id: event.session_id,
                        parent_session_id: None,
                        provider_session_id: event.provider_session_id,
                        agent_id: None,
                    },
                    None,
                    SourceRecordMetadata {
                        event_sequence: event.event_sequence,
                        occurred_at_unix_ms: event.occurred_at_unix_ms,
                        event_type: event.event_type,
                        role: event.role,
                        workspace: event.workspace,
                        cwd: event.cwd,
                        touched_files: event.touched_files,
                    },
                    vec![TransientSourceFact::Message(SourceMessageFact {
                        content: TransientSourceContent::from_bytes(detector_message.as_bytes())
                            .expect("fixture detector content bound"),
                    })],
                )
                .expect("source-backed Pro record")
            })
            .collect::<Vec<_>>();
        let intermediate_frontier =
            SourceFrontier::new("fixture-event", TypedKey::U64(1), 1, [1; 32])
                .expect("intermediate source frontier");
        FixtureSourceFeed {
            generation_id: index.generation_id().to_owned(),
            generation_manifest: index.manifest().clone(),
            source,
            records,
            intermediate_frontier,
        }
    }

    fn rewritten_certificate(base: &CertifiedSource) -> CertifiedSource {
        let source = base.observation().source().clone();
        let observation = SourceObservation::new(source, "fixture-rewrite-v1", vec![9]).unwrap();
        let counts = ScannedSourceCounts {
            certified_bytes: base.counts().certified_bytes,
            ..base.counts()
        };
        let digest = [9; 32];
        let frontier = SourceFrontier::new(
            "fixture-rewrite-record",
            TypedKey::U64(counts.complete_records),
            counts.certified_bytes,
            digest,
        )
        .unwrap();

        CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            base.parser_revision(),
            digest,
            counts,
            Some(frontier),
        )
        .unwrap()
    }
}
