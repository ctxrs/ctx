use super::*;
use anyhow::Context as _;
use ctx_pro_host_protocol::{
    SourceManifestAdmissionReceipt, SourceManifestPageEntries, MAX_SOURCE_MANIFEST_PAGE_ITEMS,
};

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

pub(in super::super) fn sync_source_backed_pro_feed_paged<P, C>(
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
    let maximum_page_count = u32::try_from(
        manifest
            .sources
            .len()
            .saturating_add(manifest.removals.len()),
    )
    .map_err(|_| anyhow!("invalid_request: source manifest page count overflowed"))?;
    let planning_header = SourceManifestHeader::new(
        manifest.core_generation_id.clone(),
        generation_manifest.manifest_version,
        generation_manifest.identity_version,
        generation_manifest.lexical_schema_version,
        generation_manifest.lexical_analyzer_version,
        generation_manifest.policy_schema_hash.clone(),
        maximum_page_count,
        &manifest.sources,
        &manifest.removals,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let planning_topology =
        plan_manifest_topology(&planning_header, &manifest.sources, &manifest.removals)?;
    let page_count = u32::try_from(planning_topology.len())
        .map_err(|_| anyhow!("invalid_request: source manifest page count overflowed"))?;
    let header = SourceManifestHeader::new(
        manifest.core_generation_id.clone(),
        generation_manifest.manifest_version,
        generation_manifest.identity_version,
        generation_manifest.lexical_schema_version,
        generation_manifest.lexical_analyzer_version,
        generation_manifest.policy_schema_hash.clone(),
        page_count,
        &manifest.sources,
        &manifest.removals,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let topology = plan_manifest_topology(&header, &manifest.sources, &manifest.removals)?;
    if topology != planning_topology {
        bail!("internal: source manifest page topology changed after identity pinning");
    }
    let terminal_chain_sha256 =
        validate_manifest_topology(&header, &topology, &manifest.sources, &manifest.removals)?;
    let began = consumer.begin_source_manifest_admission(&header)?;
    began
        .validate_for(&header)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let resume_page = usize::try_from(began.cursor.next_page_index)
        .map_err(|_| anyhow!("invalid_response: source admission cursor overflowed"))?;
    if resume_page > topology.len() {
        bail!("invalid_response: helper resumed outside the source admission topology");
    }
    let mut expected_resume =
        ctx_pro_host_protocol::SourceManifestAdmissionCursor::initial(&header);
    for (page_index, plan) in topology.iter().take(resume_page).enumerate() {
        let page = materialize_manifest_page(
            &header,
            &expected_resume.next_page_previous_sha256,
            u32::try_from(page_index)
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            plan,
            &manifest.sources,
            &manifest.removals,
        )?;
        expected_resume = cursor_after_page(&header, &expected_resume, &page)?;
    }
    if began.cursor != expected_resume {
        bail!("invalid_response: helper resumed from the wrong source admission topology");
    }
    let mut cursor = began.cursor;
    for (page_index, plan) in topology.iter().enumerate().skip(resume_page) {
        let page = materialize_manifest_page(
            &header,
            &cursor.next_page_previous_sha256,
            u32::try_from(page_index)
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            plan,
            &manifest.sources,
            &manifest.removals,
        )?;
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
    if !cursor.is_complete_for(&header) {
        bail!("invalid_response: helper did not admit the exact source manifest");
    }
    let admitted = consumer.finish_source_manifest_admission(&header)?;
    let expected_receipt = SourceManifestAdmissionReceipt {
        header: header.clone(),
        page_count: header.page_count,
        terminal_chain_sha256,
    };
    if admitted.receipt != expected_receipt {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManifestPagePlan {
    Sources(std::ops::Range<usize>),
    Removals(std::ops::Range<usize>),
}

fn plan_manifest_topology(
    header: &SourceManifestHeader,
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<Vec<ManifestPagePlan>> {
    let mut topology = Vec::new();
    let mut previous_page_sha256 =
        ctx_pro_host_protocol::SourceManifestAdmissionCursor::initial(header)
            .next_page_previous_sha256;
    let mut source_index = 0;
    while source_index < sources.len() {
        let page = fit_manifest_page(
            header,
            &previous_page_sha256,
            u32::try_from(topology.len())
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            source_index,
            sources,
            SourceManifestPageEntries::Sources,
        )?;
        let next_source_index = source_index.saturating_add(page.entries.len());
        topology.push(ManifestPagePlan::Sources(source_index..next_source_index));
        source_index = next_source_index;
        previous_page_sha256.clone_from(&page.page_sha256);
    }
    let mut removal_index = 0;
    while removal_index < removals.len() {
        let page = fit_manifest_page(
            header,
            &previous_page_sha256,
            u32::try_from(topology.len())
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            removal_index,
            removals,
            SourceManifestPageEntries::Removals,
        )?;
        let next_removal_index = removal_index.saturating_add(page.entries.len());
        topology.push(ManifestPagePlan::Removals(
            removal_index..next_removal_index,
        ));
        removal_index = next_removal_index;
        previous_page_sha256.clone_from(&page.page_sha256);
    }
    Ok(topology)
}

fn validate_manifest_topology(
    header: &SourceManifestHeader,
    topology: &[ManifestPagePlan],
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<String> {
    let mut previous_page_sha256 =
        ctx_pro_host_protocol::SourceManifestAdmissionCursor::initial(header)
            .next_page_previous_sha256;
    for (page_index, plan) in topology.iter().enumerate() {
        let page = materialize_manifest_page(
            header,
            &previous_page_sha256,
            u32::try_from(page_index)
                .map_err(|_| anyhow!("invalid_request: manifest page index overflowed"))?,
            plan,
            sources,
            removals,
        )?;
        previous_page_sha256 = page.page_sha256;
    }
    Ok(previous_page_sha256)
}

fn materialize_manifest_page(
    header: &SourceManifestHeader,
    previous_page_sha256: &str,
    page_index: u32,
    plan: &ManifestPagePlan,
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<SourceManifestPage> {
    let (item_index, entries) = match plan {
        ManifestPagePlan::Sources(range) => (
            range.start,
            SourceManifestPageEntries::Sources(sources[range.clone()].to_vec()),
        ),
        ManifestPagePlan::Removals(range) => (
            range.start,
            SourceManifestPageEntries::Removals(removals[range.clone()].to_vec()),
        ),
    };
    let page = SourceManifestPage::new(
        header,
        previous_page_sha256,
        page_index,
        u32::try_from(item_index)
            .map_err(|_| anyhow!("invalid_request: manifest item index overflowed"))?,
        entries,
    )
    .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    AdmitSourceManifestPageRequest { page: page.clone() }
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    Ok(page)
}

fn fit_manifest_page<T: Clone>(
    header: &SourceManifestHeader,
    previous_page_sha256: &str,
    page_index: u32,
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
            previous_page_sha256,
            page_index,
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

pub(in super::super) fn cursor_after_page(
    header: &SourceManifestHeader,
    cursor: &ctx_pro_host_protocol::SourceManifestAdmissionCursor,
    page: &SourceManifestPage,
) -> Result<ctx_pro_host_protocol::SourceManifestAdmissionCursor> {
    page.validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    if page.core_generation_id != header.core_generation_id
        || page.aggregate_sha256 != header.aggregate_sha256
        || page.page_index != cursor.next_page_index
        || page.previous_page_sha256 != cursor.next_page_previous_sha256
    {
        bail!("invalid_response: source page is outside its admission chain");
    }
    let count = u32::try_from(page.entries.len())
        .map_err(|_| anyhow!("invalid_request: manifest page item count overflowed"))?;
    let mut next = cursor.clone();
    next.next_page_previous_sha256.clone_from(&page.page_sha256);
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
