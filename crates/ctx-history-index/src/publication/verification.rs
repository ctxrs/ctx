use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use tantivy::{
    schema::{Field, IndexRecordOption},
    termdict::TermMerger,
    DocAddress, DocSet, Executor, InvertedIndexReader, Searcher, TERMINATED,
};
use uuid::Uuid;

use crate::{
    fields_from_schema, hex,
    query::{self, IdentityFieldRole},
    staging::accumulate_core_record,
    GenerationManifest, IndexError, Result, WriterOptions,
};

#[derive(Default)]
struct SourceAggregate {
    count: u64,
    accumulator: [u8; 32],
}

struct SegmentVerification {
    document_count: u64,
    document_decodes: usize,
    source_aggregates: BTreeMap<String, SourceAggregate>,
    parent_session_documents: u64,
}

#[derive(Default)]
struct VerificationCounters {
    active_workers: AtomicUsize,
    max_active_workers: AtomicUsize,
}

struct ActiveVerificationWorker<'a> {
    counters: Option<&'a VerificationCounters>,
}

impl<'a> ActiveVerificationWorker<'a> {
    fn enter(counters: Option<&'a VerificationCounters>) -> Self {
        if let Some(counters) = counters {
            let active = counters.active_workers.fetch_add(1, Ordering::SeqCst) + 1;
            counters
                .max_active_workers
                .fetch_max(active, Ordering::SeqCst);
        }
        Self { counters }
    }
}

impl Drop for ActiveVerificationWorker<'_> {
    fn drop(&mut self) {
        if let Some(counters) = self.counters {
            counters.active_workers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[derive(Default)]
struct VerificationRunMetrics {
    #[cfg(test)]
    worker_budget: usize,
    segment_tasks: usize,
    document_decodes: usize,
    source_terms: usize,
    max_active_workers: usize,
    max_buffered_segments: usize,
    max_buffered_event_identities: usize,
    max_buffered_session_identities: usize,
}

pub(crate) fn verify_searcher_structure(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<()> {
    let actual = searcher.num_docs();
    if actual != manifest.indexed_documents {
        return Err(IndexError::DocumentCountMismatch {
            manifest: manifest.indexed_documents,
            index: actual,
        });
    }
    Ok(())
}

pub(crate) fn verify_searcher(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    let worker_budget = verification_worker_budget(searcher.segment_readers().len());
    verify_searcher_with_options(searcher, manifest, worker_budget, false, false).map(|_| ())
}

fn verification_worker_budget(segment_count: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    segment_count
        .max(1)
        .min(available)
        .min(WriterOptions::default().indexer_threads.max(1))
}

fn verify_searcher_with_options(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    requested_worker_budget: usize,
    instrument: bool,
    synchronize_first_wave: bool,
) -> Result<VerificationRunMetrics> {
    let fields = fields_from_schema(searcher.schema())?;
    query::validate_verification_projection(fields)?;
    let segment_count = searcher.segment_readers().len();
    let worker_budget = requested_worker_budget.max(1).min(segment_count.max(1));
    let executor = if worker_budget == 1 {
        Executor::single_thread()
    } else {
        Executor::multi_thread(worker_budget, "ctx-generation-verify-")?
    };
    let counters = instrument.then(VerificationCounters::default);
    let first_wave_size = worker_budget.min(segment_count);
    let rendezvous =
        (synchronize_first_wave && first_wave_size > 1).then(|| Barrier::new(first_wave_size));
    let mut metrics = VerificationRunMetrics {
        #[cfg(test)]
        worker_budget,
        ..VerificationRunMetrics::default()
    };
    let mut total_documents = 0_u64;
    let mut parent_session_documents = 0_u64;
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();

    for wave_start in (0..segment_count).step_by(worker_budget) {
        let wave_end = (wave_start + worker_budget).min(segment_count);
        let wave_rendezvous = (wave_start == 0).then_some(rendezvous.as_ref()).flatten();
        let segments = executor.map(
            |segment_ord| {
                Ok(verify_segment(
                    searcher,
                    segment_ord,
                    wave_rendezvous,
                    counters.as_ref(),
                ))
            },
            wave_start..wave_end,
        )?;
        metrics.max_buffered_segments = metrics.max_buffered_segments.max(segments.len());
        for segment in segments {
            let segment = segment?;
            metrics.segment_tasks += 1;
            metrics.document_decodes = metrics
                .document_decodes
                .checked_add(segment.document_decodes)
                .ok_or(IndexError::CountOverflow)?;
            metrics.source_terms = metrics
                .source_terms
                .checked_add(segment.source_aggregates.len())
                .ok_or(IndexError::CountOverflow)?;
            total_documents = total_documents
                .checked_add(segment.document_count)
                .ok_or(IndexError::CountOverflow)?;
            parent_session_documents = parent_session_documents
                .checked_add(segment.parent_session_documents)
                .ok_or(IndexError::CountOverflow)?;
            merge_source_aggregates(&mut source_aggregates, segment.source_aggregates)?;
        }
    }

    metrics.max_active_workers = counters
        .as_ref()
        .map(|counters| counters.max_active_workers.load(Ordering::SeqCst))
        .unwrap_or(0);
    if total_documents != manifest.indexed_documents {
        return Err(IndexError::DocumentCountMismatch {
            manifest: manifest.indexed_documents,
            index: total_documents,
        });
    }
    verify_event_identities(searcher, fields.event_id, total_documents)?;
    verify_session_identities(
        searcher,
        [
            (fields.session_id, IdentityFieldRole::Session),
            (fields.parent_session_id, IdentityFieldRole::ParentSession),
            (fields.root_session_id, IdentityFieldRole::RootSession),
        ],
        [total_documents, parent_session_documents, total_documents],
    )?;
    verify_manifest_aggregates(manifest, source_aggregates)?;
    metrics.max_buffered_event_identities = usize::from(total_documents != 0);
    metrics.max_buffered_session_identities = usize::from(total_documents != 0);
    Ok(metrics)
}

fn verify_segment(
    searcher: &Searcher,
    segment_ord: usize,
    rendezvous: Option<&Barrier>,
    counters: Option<&VerificationCounters>,
) -> Result<SegmentVerification> {
    let _active_worker = ActiveVerificationWorker::enter(counters);
    if let Some(rendezvous) = rendezvous {
        rendezvous.wait();
    }
    let segment = searcher.segment_reader(segment_ord as u32);
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();
    let mut document_decodes = 0;
    let mut parent_session_documents = 0_u64;
    for doc_id in 0..segment.max_doc() {
        if segment.is_deleted(doc_id) {
            continue;
        }
        document_decodes += 1;
        let record = query::stored_verification_record(
            searcher,
            DocAddress::new(segment_ord as u32, doc_id),
        )?;
        let source = source_aggregates.entry(record.source_owner).or_default();
        source.count = source
            .count
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        accumulate_core_record(&mut source.accumulator, &record.core_record_leaf);
        parent_session_documents = parent_session_documents
            .checked_add(u64::from(record.has_parent_session))
            .ok_or(IndexError::CountOverflow)?;
    }
    Ok(SegmentVerification {
        document_count: u64::from(segment.num_docs()),
        document_decodes,
        source_aggregates,
        parent_session_documents,
    })
}

fn merge_source_aggregates(
    target: &mut BTreeMap<String, SourceAggregate>,
    source: BTreeMap<String, SourceAggregate>,
) -> Result<()> {
    for (source_id, aggregate) in source {
        let total = target.entry(source_id).or_default();
        total.count = total
            .count
            .checked_add(aggregate.count)
            .ok_or(IndexError::CountOverflow)?;
        accumulate_core_record(&mut total.accumulator, &aggregate.accumulator);
    }
    Ok(())
}

fn verify_manifest_aggregates(
    manifest: &GenerationManifest,
    mut actual: BTreeMap<String, SourceAggregate>,
) -> Result<()> {
    for expected in &manifest.core_record_aggregates {
        let source_id = expected.source_identity_digest().to_owned();
        let observed = actual.remove(&source_id).unwrap_or_default();
        if observed.count != expected.indexed_documents() {
            return Err(IndexError::CoreRecordAggregateCountMismatch {
                source_id,
                manifest: expected.indexed_documents(),
                index: observed.count,
            });
        }
        if observed.accumulator != expected.accumulator_bytes()? {
            return Err(IndexError::CoreRecordAggregateMismatch(source_id));
        }
    }
    if let Some((source_id, aggregate)) = actual.into_iter().next() {
        return Err(IndexError::SourceCountMismatch {
            source_id,
            manifest: 0,
            index: aggregate.count,
        });
    }
    Ok(())
}

fn verify_event_identities(searcher: &Searcher, field: Field, expected: u64) -> Result<()> {
    let segments = searcher.segment_readers();
    let inverted_indexes = segments
        .iter()
        .map(|segment| segment.inverted_index(field))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = 0_u64;
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let mut digest = None;
        for (segment_ord, term_info) in merged.current_segment_ords_and_term_infos() {
            for_each_live_posting(
                &inverted_indexes[segment_ord],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences = occurrences
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    let identity =
                        query::stored_identity_record(searcher, address, IdentityFieldRole::Event)?
                            .identity;
                    if identity.as_uuid() != uuid {
                        return Err(IndexError::InvalidStoredDocumentField("event_id"));
                    }
                    match digest {
                        None => digest = Some(identity.digest()),
                        Some(existing) if existing == identity.digest() => {
                            return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
                        }
                        Some(existing) => {
                            return Err(IndexError::CompactIdentityCollision {
                                kind: "event",
                                uuid,
                                existing_digest: hex(&existing),
                                new_digest: hex(&identity.digest()),
                            });
                        }
                    }
                    Ok(())
                },
            )?;
        }
    }
    if occurrences != expected {
        return Err(IndexError::InvalidStoredDocumentField("event_id"));
    }
    Ok(())
}

fn verify_session_identities(
    searcher: &Searcher,
    fields: [(Field, IdentityFieldRole); 3],
    expected_occurrences: [u64; 3],
) -> Result<()> {
    let segments = searcher.segment_readers();
    let mut mappings = Vec::with_capacity(fields.len() * segments.len());
    let mut inverted_indexes = Vec::with_capacity(fields.len() * segments.len());
    for (role_index, (field, role)) in fields.into_iter().enumerate() {
        for (segment_ord, segment) in segments.iter().enumerate() {
            inverted_indexes.push(segment.inverted_index(field)?);
            mappings.push((segment_ord, role_index, role));
        }
    }
    let streams = inverted_indexes
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut occurrences = [0_u64; 3];
    while merged.advance() {
        let uuid = canonical_uuid_term(merged.key(), "session_id")?;
        let mut digest = None;
        let mut owner = None::<String>;
        for (stream_index, term_info) in merged.current_segment_ords_and_term_infos() {
            let (segment_ord, role_index, role) = mappings[stream_index];
            for_each_live_posting(
                &inverted_indexes[stream_index],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences[role_index] = occurrences[role_index]
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    let record = query::stored_identity_record(searcher, address, role)?;
                    if record.identity.as_uuid() != uuid {
                        return Err(IndexError::InvalidStoredDocumentField("session_id"));
                    }
                    match digest {
                        None => digest = Some(record.identity.digest()),
                        Some(existing) if existing == record.identity.digest() => {}
                        Some(existing) => {
                            return Err(IndexError::CompactIdentityCollision {
                                kind: "session",
                                uuid,
                                existing_digest: hex(&existing),
                                new_digest: hex(&record.identity.digest()),
                            });
                        }
                    }
                    if let Some(candidate_owner) = record.source_owner {
                        match owner.as_deref() {
                            Some(existing) if existing != candidate_owner => {
                                return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
                            }
                            None => owner = Some(candidate_owner),
                            _ => {}
                        }
                    }
                    Ok(())
                },
            )?;
        }
    }
    if occurrences != expected_occurrences {
        return Err(IndexError::InvalidStoredDocumentField("session_id"));
    }
    Ok(())
}

fn for_each_live_posting(
    inverted: &InvertedIndexReader,
    term_info: &tantivy::postings::TermInfo,
    segment_ord: usize,
    segment: &tantivy::SegmentReader,
    mut visit: impl FnMut(DocAddress) -> Result<()>,
) -> Result<()> {
    let mut postings = inverted.read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?;
    let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
    let mut doc_id = postings.doc();
    while doc_id != TERMINATED {
        if !segment.is_deleted(doc_id) {
            visit(DocAddress::new(segment_ord, doc_id))?;
        }
        doc_id = postings.advance();
    }
    Ok(())
}

fn canonical_uuid_term(term: &[u8], field: &'static str) -> Result<Uuid> {
    let term =
        std::str::from_utf8(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    let uuid = Uuid::parse_str(term).map_err(|_| IndexError::InvalidStoredDocumentField(field))?;
    if uuid.to_string() != term {
        return Err(IndexError::InvalidStoredDocumentField(field));
    }
    Ok(uuid)
}

#[cfg(test)]
pub(crate) fn verify_searcher_reference(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<()> {
    verify_searcher(searcher, manifest)
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct VerificationMetrics {
    pub(crate) worker_budget: usize,
    pub(crate) segment_tasks: usize,
    pub(crate) document_decodes: usize,
    pub(crate) source_terms: usize,
    pub(crate) max_active_workers: usize,
    pub(crate) max_buffered_segments: usize,
    pub(crate) max_buffered_event_identities: usize,
    pub(crate) max_buffered_session_identities: usize,
}

#[cfg(test)]
pub(crate) fn verify_searcher_with_metrics(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    worker_budget: usize,
    synchronize_first_wave: bool,
) -> Result<VerificationMetrics> {
    let metrics = verify_searcher_with_options(
        searcher,
        manifest,
        worker_budget,
        true,
        synchronize_first_wave,
    )?;
    Ok(VerificationMetrics {
        worker_budget: metrics.worker_budget,
        segment_tasks: metrics.segment_tasks,
        document_decodes: metrics.document_decodes,
        source_terms: metrics.source_terms,
        max_active_workers: metrics.max_active_workers,
        max_buffered_segments: metrics.max_buffered_segments,
        max_buffered_event_identities: metrics.max_buffered_event_identities,
        max_buffered_session_identities: metrics.max_buffered_session_identities,
    })
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ReferenceVerificationMetrics {
    pub(crate) query_passes: usize,
    pub(crate) segment_query_visits: usize,
    pub(crate) document_decodes: usize,
}

#[cfg(test)]
pub(crate) fn verify_searcher_reference_with_metrics(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<ReferenceVerificationMetrics> {
    verify_searcher(searcher, manifest)?;
    let query_passes = manifest.sources.len() + 1;
    Ok(ReferenceVerificationMetrics {
        query_passes,
        segment_query_visits: query_passes
            .checked_mul(searcher.segment_readers().len())
            .ok_or(IndexError::CountOverflow)?,
        document_decodes: usize::try_from(searcher.num_docs())
            .map_err(|_| IndexError::CountOverflow)?,
    })
}
