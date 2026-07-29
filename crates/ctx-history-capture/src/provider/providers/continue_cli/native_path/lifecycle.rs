use super::{
    normalize::ContinuePreparedPage,
    parse::{
        parse_continue_source, ContinueIncompleteSource, ContinueOutputExclusionStats,
        ContinueParseOutcome, ContinueSourceFailure, ContinueSourcePageStream,
    },
    source::{ContinueDiscovery, ContinuePathIter},
    ContinueNativePathError,
};

#[derive(Debug)]
pub(crate) enum ContinueSourceOutcome {
    Page(Box<ContinuePreparedPage>),
    Incomplete(Box<ContinueIncompleteSource>),
    Failed(ContinueSourceFailure),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContinuePreparationStats {
    pub(crate) source_content_reads: usize,
    pub(crate) source_bytes_read: u64,
    pub(crate) complete_sources: usize,
    pub(crate) observation_only_sources: usize,
    pub(crate) incomplete_sources: usize,
    pub(crate) failed_sources: usize,
    pub(crate) emitted_pages: usize,
    pub(crate) retained_events: usize,
    pub(crate) rejected_items: usize,
    pub(crate) identity_entries_peak: usize,
    pub(crate) maximum_resident_source_documents: usize,
    pub(crate) maximum_prepared_page_sources: usize,
    pub(crate) peak_page_rows: usize,
    pub(crate) peak_page_bytes: usize,
    pub(crate) output_exclusion: ContinueOutputExclusionStats,
}

pub(crate) struct ContinuePreparationStream<'a> {
    discovery: &'a ContinueDiscovery,
    paths: ContinuePathIter,
    active_source: Option<Box<ContinueSourcePageStream>>,
    stats: ContinuePreparationStats,
    done: bool,
}

pub(crate) fn prepare_continue_discovery(
    discovery: &ContinueDiscovery,
) -> Result<ContinuePreparationStream<'_>, ContinueNativePathError> {
    Ok(ContinuePreparationStream {
        discovery,
        paths: discovery.paths()?,
        active_source: None,
        stats: ContinuePreparationStats::default(),
        done: false,
    })
}

impl Iterator for ContinuePreparationStream<'_> {
    type Item = Result<ContinueSourceOutcome, ContinueNativePathError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }

            if let Some(active) = self.active_source.as_mut() {
                match active.next_page() {
                    Ok(Some(page)) => {
                        self.stats.emitted_pages = self.stats.emitted_pages.saturating_add(1);
                        self.stats.retained_events =
                            self.stats.retained_events.saturating_add(page.events.len());
                        self.stats.peak_page_rows = self.stats.peak_page_rows.max(page.row_count);
                        self.stats.peak_page_bytes =
                            self.stats.peak_page_bytes.max(page.estimated_bytes);
                        self.stats.maximum_prepared_page_sources =
                            self.stats.maximum_prepared_page_sources.max(1);
                        if page.source.is_some() {
                            self.stats.complete_sources =
                                self.stats.complete_sources.saturating_add(1);
                        }
                        if page.terminal {
                            if let Some(authority) = page.authority.as_ref() {
                                self.stats.rejected_items = self
                                    .stats
                                    .rejected_items
                                    .saturating_add(authority.rejected_items);
                            }
                            if let Some(output) = page.output_exclusion {
                                add_output_stats(&mut self.stats.output_exclusion, output);
                            }
                            self.active_source = None;
                        }
                        return Some(Ok(ContinueSourceOutcome::Page(Box::new(page))));
                    }
                    Ok(None) => {
                        self.active_source = None;
                        continue;
                    }
                    Err(failure) => {
                        self.active_source = None;
                        self.stats.failed_sources = self.stats.failed_sources.saturating_add(1);
                        return Some(Ok(ContinueSourceOutcome::Failed(failure)));
                    }
                }
            }

            let path = match self.paths.next()? {
                Ok(path) => path,
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            };
            let snapshot = match self.discovery.open_source(&path) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.stats.failed_sources = self.stats.failed_sources.saturating_add(1);
                    return Some(Err(error));
                }
            };
            self.stats.source_content_reads = self.stats.source_content_reads.saturating_add(1);
            self.stats.source_bytes_read = self
                .stats
                .source_bytes_read
                .saturating_add(snapshot.observation().raw_bytes());
            self.stats.maximum_resident_source_documents =
                self.stats.maximum_resident_source_documents.max(1);

            match parse_continue_source(snapshot, self.discovery.index()) {
                Ok(ContinueParseOutcome::Complete(source)) => {
                    self.active_source = Some(source);
                }
                Ok(ContinueParseOutcome::Incomplete(source)) => {
                    self.stats.incomplete_sources = self.stats.incomplete_sources.saturating_add(1);
                    return Some(Ok(ContinueSourceOutcome::Incomplete(source)));
                }
                Err(failure) => {
                    self.stats.failed_sources = self.stats.failed_sources.saturating_add(1);
                    return Some(Ok(ContinueSourceOutcome::Failed(failure)));
                }
            }
            if self.active_source.is_none() {
                self.done = true;
                return None;
            }
        }
    }
}

fn add_output_stats(
    aggregate: &mut ContinueOutputExclusionStats,
    source: ContinueOutputExclusionStats,
) {
    aggregate.native_results_observed = aggregate
        .native_results_observed
        .saturating_add(source.native_results_observed);
    aggregate.unproven_payloads_skipped = aggregate
        .unproven_payloads_skipped
        .saturating_add(source.unproven_payloads_skipped);
    aggregate.result_payload_bytes_skipped = aggregate
        .result_payload_bytes_skipped
        .saturating_add(source.result_payload_bytes_skipped);
    aggregate.call_body_bytes_skipped = aggregate
        .call_body_bytes_skipped
        .saturating_add(source.call_body_bytes_skipped);
    aggregate.retained_decode_string_allocations = aggregate
        .retained_decode_string_allocations
        .saturating_add(source.retained_decode_string_allocations);
    aggregate.retained_decode_string_bytes = aggregate
        .retained_decode_string_bytes
        .saturating_add(source.retained_decode_string_bytes);
}
