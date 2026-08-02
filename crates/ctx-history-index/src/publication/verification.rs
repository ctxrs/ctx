use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use crc32fast::Hasher as Crc32;
use sha2::{Digest, Sha256};
use tantivy::{
    directory::{footer::Footer, Directory as _},
    index::SegmentComponent,
    postings::Postings,
    schema::{Field, IndexRecordOption},
    termdict::TermMerger,
    tokenizer::TokenStream,
    DocAddress, DocSet, Executor, HasLen, InvertedIndexReader, Searcher, Term, TERMINATED,
};
use uuid::Uuid;

#[cfg(test)]
use std::cell::Cell;

use crate::{
    durable_directory::DurableMmapDirectory,
    fields_from_schema, hex,
    query::{self, CompactIdentity, IdentityFieldRole},
    staging::accumulate_core_record,
    GenerationManifest, IndexError, Result, WriterOptions,
};

mod spill;

use spill::{
    ProjectionAccumulator, ProjectionDeltas, SpillVerificationIdentities, VerificationSpill,
    VERIFICATION_SPILL_BUFFER_BYTES, VERIFICATION_SPILL_RECORD_BYTES,
};

#[derive(Default)]
struct SourceAggregate {
    count: u64,
    accumulator: [u8; 32],
}

struct SegmentVerification {
    document_count: u64,
    document_decodes: usize,
    stored_core_bytes: u64,
    body_tokens: u64,
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
    stored_core_bytes: u64,
    body_tokens: u64,
    verification_spill_bytes: u64,
    verification_heap_payload_bound_bytes: usize,
}

#[cfg(test)]
thread_local! {
    static CHECKSUM_WALKS: Cell<usize> = const { Cell::new(0) };
    static LOGICAL_PASSES: Cell<usize> = const { Cell::new(0) };
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

const PHYSICAL_INTEGRITY_DOMAIN: &[u8] = b"ctx-tantivy-physical-integrity-v1\0";
const PHYSICAL_HASH_BUFFER_BYTES: usize = 64 * 1024;
const TANTIVY_META_FILE: &str = "meta.json";

#[derive(Debug)]
struct PhysicalFileDigest {
    path: String,
    length: u64,
    sha256: [u8; 32],
}

/// Computes one physical generation's canonical integrity digest.
///
/// The domain-separated stream contains the exact active file count followed
/// by each sorted UTF-8 relative path, file length, and SHA-256 of the complete
/// file bytes. The sorted path set always includes `meta.json` and every segment
/// file referenced by its active segment metadata. Managed bookkeeping, locks,
/// and temporary files are deliberately excluded because queries do not read them.
/// Segment bytes are streamed once and checked against their Tantivy CRC footer
/// while their stronger SHA-256 is computed.
pub(crate) fn physical_integrity_digest(
    index: &tantivy::Index,
    generation_path: &Path,
) -> Result<String> {
    #[cfg(test)]
    CHECKSUM_WALKS.with(|count| count.set(count.get() + 1));
    physical_integrity_digest_inner(index, generation_path)
}

fn physical_integrity_digest_inner(
    index: &tantivy::Index,
    generation_path: &Path,
) -> Result<String> {
    let directory =
        DurableMmapDirectory::open(generation_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    let entries = paths
        .into_iter()
        .map(|path| {
            hash_physical_file(
                &directory,
                generation_path,
                &path,
                path != Path::new(TANTIVY_META_FILE),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    canonical_physical_integrity_digest(&entries)
}

/// Verifies a generation against the physical authority in its pointer slot.
pub(crate) fn verify_physical_integrity(
    index: &tantivy::Index,
    generation_path: &Path,
    expected_digest: &str,
) -> Result<()> {
    #[cfg(test)]
    CHECKSUM_WALKS.with(|count| count.set(count.get() + 1));
    let actual = physical_integrity_digest_inner(index, generation_path)?;
    if actual != expected_digest {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn hash_physical_file(
    directory: &DurableMmapDirectory,
    generation_path: &Path,
    relative_path: &Path,
    validate_tantivy_footer: bool,
) -> Result<PhysicalFileDigest> {
    if relative_path.components().count() != 1 {
        return Err(IndexError::ChecksumMismatch);
    }
    let path = relative_path
        .to_str()
        .ok_or(IndexError::ChecksumMismatch)?
        .to_owned();
    let full_path = generation_path.join(relative_path);
    let metadata = full_path
        .metadata()
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if !metadata.is_file() {
        return Err(IndexError::ChecksumMismatch);
    }
    let length = metadata.len();
    let footer_contract = if validate_tantivy_footer {
        let slice = directory
            .open_read(relative_path)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        let (footer, body) =
            Footer::extract_footer(slice).map_err(|_| IndexError::ChecksumMismatch)?;
        Some((
            u64::try_from(body.len()).map_err(|_| IndexError::CountOverflow)?,
            footer.crc,
        ))
    } else {
        None
    };

    let mut file = File::open(&full_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let mut sha256 = Sha256::new();
    let mut crc32 = footer_contract.map(|_| Crc32::new());
    let mut body_remaining = footer_contract.map_or(0, |(body_length, _)| body_length);
    let mut bytes_read = 0_u64;
    let mut buffer = [0_u8; PHYSICAL_HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        if count == 0 {
            break;
        }
        let count_u64 = u64::try_from(count).map_err(|_| IndexError::CountOverflow)?;
        bytes_read = bytes_read
            .checked_add(count_u64)
            .ok_or(IndexError::CountOverflow)?;
        sha256.update(&buffer[..count]);
        if let Some(crc32) = crc32.as_mut() {
            let body_count = usize::try_from(body_remaining.min(count_u64))
                .map_err(|_| IndexError::CountOverflow)?;
            crc32.update(&buffer[..body_count]);
            body_remaining -= u64::try_from(body_count).map_err(|_| IndexError::CountOverflow)?;
        }
    }
    if bytes_read != length || body_remaining != 0 {
        return Err(IndexError::ChecksumMismatch);
    }
    if let (Some(crc32), Some((_, expected_crc32))) = (crc32, footer_contract) {
        if crc32.finalize() != expected_crc32 {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    Ok(PhysicalFileDigest {
        path,
        length,
        sha256: sha256.finalize().into(),
    })
}

fn canonical_physical_integrity_digest(entries: &[PhysicalFileDigest]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(PHYSICAL_INTEGRITY_DOMAIN);
    digest.update(
        u64::try_from(entries.len())
            .map_err(|_| IndexError::CountOverflow)?
            .to_be_bytes(),
    );
    for entry in entries {
        let path = entry.path.as_bytes();
        digest.update(
            u64::try_from(path.len())
                .map_err(|_| IndexError::CountOverflow)?
                .to_be_bytes(),
        );
        digest.update(path);
        digest.update(entry.length.to_be_bytes());
        digest.update(entry.sha256);
    }
    Ok(hex(&digest.finalize()))
}

#[cfg(test)]
#[test]
fn sha256_physical_authority_distinguishes_stubbed_crc_collision() {
    struct StubbedCrcFile<'a> {
        path: &'a str,
        bytes: &'a [u8],
        crc32: u32,
    }

    let first = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-A",
        crc32: 0xfeed_beef,
    };
    let second = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-B",
        crc32: 0xfeed_beef,
    };
    assert_eq!(first.path, second.path);
    assert_eq!(first.bytes.len(), second.bytes.len());
    assert_eq!(first.crc32, second.crc32);

    let entry = |file: &StubbedCrcFile<'_>| PhysicalFileDigest {
        path: file.path.to_owned(),
        length: u64::try_from(file.bytes.len()).unwrap(),
        sha256: Sha256::digest(file.bytes).into(),
    };
    assert_ne!(
        canonical_physical_integrity_digest(&[entry(&first)]).unwrap(),
        canonical_physical_integrity_digest(&[entry(&second)]).unwrap()
    );
}

fn active_index_files(index: &tantivy::Index) -> Result<BTreeSet<PathBuf>> {
    let directory = index.directory();
    let metas = index.load_metas()?;
    let mut expected_files = BTreeSet::new();
    for segment in &metas.segments {
        for component in [
            SegmentComponent::Postings,
            SegmentComponent::FastFields,
            SegmentComponent::FieldNorms,
            SegmentComponent::Terms,
            SegmentComponent::Store,
        ] {
            expected_files.insert(segment.relative_path(component));
        }
        let positions = segment.relative_path(SegmentComponent::Positions);
        if directory
            .exists(&positions)
            .map_err(|_| IndexError::ChecksumMismatch)?
        {
            expected_files.insert(positions);
        }
        if segment.has_deletes() {
            expected_files.insert(segment.relative_path(SegmentComponent::Delete));
        }
    }
    Ok(expected_files)
}

/// Verifies the complete publication authority carried by one immutable searcher.
///
/// This is the shared publication/startup path: physical Tantivy checksums are
/// checked before the exhaustive stored-Core, source aggregate, and identity
/// audit.
pub(crate) fn verify_complete_searcher(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    generation_path: &Path,
    expected_physical_integrity_digest: &str,
) -> Result<()> {
    verify_physical_integrity(
        searcher.index(),
        generation_path,
        expected_physical_integrity_digest,
    )?;
    verify_searcher(searcher, manifest)
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
    #[cfg(test)]
    LOGICAL_PASSES.with(|count| count.set(count.get() + 1));
    verify_searcher_structure(searcher, manifest)?;
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
    let mut expected_body_tokens = 0_u64;
    let mut parent_session_documents = 0_u64;
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();
    let source_ordinals = manifest
        .core_record_aggregates
        .iter()
        .enumerate()
        .map(|(ordinal, aggregate)| {
            Ok((
                aggregate.source_identity_digest().to_owned(),
                u32::try_from(ordinal).map_err(|_| IndexError::CountOverflow)?,
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let verification_spill = VerificationSpill::create(
        searcher
            .segment_readers()
            .iter()
            .map(tantivy::SegmentReader::max_doc),
    )?;
    metrics.verification_spill_bytes = verification_spill.logical_bytes();
    metrics.verification_heap_payload_bound_bytes = verification_spill
        .segment_offsets_heap_bytes()?
        .checked_add(
            worker_budget
                .checked_mul(VERIFICATION_SPILL_BUFFER_BYTES + VERIFICATION_SPILL_RECORD_BYTES)
                .ok_or(IndexError::CountOverflow)?,
        )
        .ok_or(IndexError::CountOverflow)?;

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
                    &verification_spill,
                    &source_ordinals,
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
            metrics.stored_core_bytes = metrics
                .stored_core_bytes
                .checked_add(segment.stored_core_bytes)
                .ok_or(IndexError::CountOverflow)?;
            metrics.body_tokens = metrics
                .body_tokens
                .checked_add(segment.body_tokens)
                .ok_or(IndexError::CountOverflow)?;
            metrics.source_terms = metrics
                .source_terms
                .checked_add(segment.source_aggregates.len())
                .ok_or(IndexError::CountOverflow)?;
            total_documents = total_documents
                .checked_add(segment.document_count)
                .ok_or(IndexError::CountOverflow)?;
            expected_body_tokens = expected_body_tokens
                .checked_add(segment.body_tokens)
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
    if live_body_token_count(searcher, fields.body_search)? != expected_body_tokens {
        return Err(IndexError::InvalidStoredDocumentField("body_search"));
    }
    let mut projection_deltas = verification_spill.load_projection_deltas()?;
    metrics.verification_heap_payload_bound_bytes = metrics
        .verification_heap_payload_bound_bytes
        .checked_add(projection_deltas.heap_bytes())
        .ok_or(IndexError::CountOverflow)?;
    verify_event_identities(
        searcher,
        fields.event_id,
        total_documents,
        &mut projection_deltas,
    )?;
    verify_session_identities(
        searcher,
        [
            (fields.session_id, IdentityFieldRole::Session),
            (fields.parent_session_id, IdentityFieldRole::ParentSession),
            (fields.root_session_id, IdentityFieldRole::RootSession),
        ],
        [total_documents, parent_session_documents, total_documents],
        &verification_spill,
        &mut projection_deltas,
    )?;
    verify_remaining_query_projections(searcher, fields, &mut projection_deltas)?;
    verify_query_projection_completion(searcher, &projection_deltas)?;
    verify_manifest_aggregates(manifest, source_aggregates)?;
    metrics.max_buffered_event_identities = 0;
    metrics.max_buffered_session_identities = 0;
    Ok(metrics)
}

fn verify_segment(
    searcher: &Searcher,
    segment_ord: usize,
    rendezvous: Option<&Barrier>,
    counters: Option<&VerificationCounters>,
    verification_spill: &VerificationSpill,
    source_ordinals: &HashMap<String, u32>,
) -> Result<SegmentVerification> {
    let _active_worker = ActiveVerificationWorker::enter(counters);
    if let Some(rendezvous) = rendezvous {
        rendezvous.wait();
    }
    let segment = searcher.segment_reader(segment_ord as u32);
    let fields = fields_from_schema(searcher.schema())?;
    let body_search = segment.inverted_index(fields.body_search)?;
    let mut body_analyzer = crate::analyzer::body_analyzer();
    let mut source_aggregates = BTreeMap::<String, SourceAggregate>::new();
    let mut document_decodes = 0;
    let mut stored_core_bytes = 0_u64;
    let mut body_tokens = 0_u64;
    let mut parent_session_documents = 0_u64;
    let mut identity_writer = verification_spill.segment_writer(segment_ord, segment.max_doc())?;
    for doc_id in 0..segment.max_doc() {
        if segment.is_deleted(doc_id) {
            identity_writer.write_deleted(doc_id)?;
            continue;
        }
        document_decodes += 1;
        let record = query::stored_verification_record(
            searcher,
            DocAddress::new(segment_ord as u32, doc_id),
            fields,
        )?;
        stored_core_bytes = stored_core_bytes
            .checked_add(
                u64::try_from(record.stored_core_bytes).map_err(|_| IndexError::CountOverflow)?,
            )
            .ok_or(IndexError::CountOverflow)?;
        verify_query_fast_fields(segment, doc_id, &record)?;
        body_tokens = body_tokens
            .checked_add(verify_body_projection(
                &body_search,
                &mut body_analyzer,
                fields.body_search,
                record.body.as_deref(),
                doc_id,
            )?)
            .ok_or(IndexError::CountOverflow)?;
        let projection_delta = expected_query_projection_delta(fields, &record)?;
        let source_ordinal = *source_ordinals
            .get(&record.source_owner)
            .ok_or(IndexError::InvalidStoredDocumentField("source_key"))?;
        let source = source_aggregates.entry(record.source_owner).or_default();
        source.count = source
            .count
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        accumulate_core_record(&mut source.accumulator, &record.core_record_leaf);
        parent_session_documents = parent_session_documents
            .checked_add(u64::from(record.identities.parent_session.is_some()))
            .ok_or(IndexError::CountOverflow)?;
        identity_writer.write_record(
            doc_id,
            SpillVerificationIdentities {
                session: record.identities.session,
                parent_session: record.identities.parent_session,
                root_session: record.identities.root_session,
                session_source_ordinal: source_ordinal,
            },
            projection_delta,
        )?;
    }
    identity_writer.finish()?;
    Ok(SegmentVerification {
        document_count: u64::from(segment.num_docs()),
        document_decodes,
        stored_core_bytes,
        body_tokens,
        source_aggregates,
        parent_session_documents,
    })
}

fn expected_query_projection_delta(
    fields: crate::Fields,
    record: &query::VerificationRecord,
) -> Result<ProjectionAccumulator> {
    let core = &record.core_record;
    let mut expected = Vec::<[u8; 32]>::new();
    let mut add = |term: Term| {
        expected.push(query_projection_digest(
            term.field(),
            term.serialized_value_bytes(),
        ));
    };

    add(Term::from_field_text(
        fields.event_id,
        &core.event_id.to_string(),
    ));
    add(Term::from_field_text(
        fields.event_identity_digest,
        &hex(&core.event_id.digest()),
    ));
    add(Term::from_field_text(
        fields.session_id,
        &core.session_id.to_string(),
    ));
    if let Some(parent_session_id) = core.parent_session_id {
        add(Term::from_field_text(
            fields.parent_session_id,
            &parent_session_id.to_string(),
        ));
    }
    add(Term::from_field_text(
        fields.root_session_id,
        &core.root_session_id.to_string(),
    ));
    add(Term::from_field_text(
        fields.source_key,
        &record.source_owner,
    ));
    add(Term::from_field_text(
        fields.provider,
        core.source.provider(),
    ));
    add(Term::from_field_text(
        fields.source_format,
        core.source.source_format(),
    ));
    if core.source.provider() == "custom" {
        if let Some(ctx_history_core::TypedKey::Composite(values)) = core.native_event_id.as_ref() {
            if let [ctx_history_core::TypedKey::Utf8(provider_key), ctx_history_core::TypedKey::Utf8(source_id), ctx_history_core::TypedKey::Utf8(_)] =
                values.as_slice()
            {
                add(Term::from_field_text(
                    fields.custom_provider_key,
                    provider_key,
                ));
                add(Term::from_field_text(fields.custom_source_id, source_id));
            }
        }
    }
    if let Some(provider_session_id) = &core.provider_session_id {
        add(Term::from_field_text(
            fields.provider_session_id,
            provider_session_id,
        ));
    }
    if let Some(branch) = &core.branch {
        add(Term::from_field_text(fields.branch, branch));
    }
    add(Term::from_field_text(fields.agent_type, &core.agent_type));
    add(Term::from_field_u64(
        fields.is_primary,
        u64::from(core.is_primary),
    ));
    add(Term::from_field_u64(
        fields.event_sequence,
        core.event_sequence,
    ));
    if let Some(occurred_at_unix_ms) = core.occurred_at_unix_ms {
        add(Term::from_field_i64(
            fields.occurred_at_unix_ms,
            occurred_at_unix_ms,
        ));
    }
    add(Term::from_field_text(fields.event_type, &core.event_type));
    if let Some(role) = &core.role {
        add(Term::from_field_text(fields.role, role));
    }
    for observation in &core.repository_vcs_observations {
        if let ctx_history_core::RepositoryVcsObservationKind::Outcome(outcome) = &observation.kind
        {
            for object_id in &outcome.produced_object_ids {
                add(Term::from_field_text(
                    fields.repository_produced_object_id,
                    &object_id.hex,
                ));
            }
        }
    }
    if let Some(workspace) = &core.workspace {
        add(Term::from_field_text(
            fields.workspace_filter,
            &workspace.to_lowercase(),
        ));
    }
    if let Some(cwd) = &core.cwd {
        add(Term::from_field_text(
            fields.workspace_filter,
            &cwd.to_lowercase(),
        ));
    }
    for observation in &core.repository_file_observations {
        add(Term::from_field_text(
            fields.touched_file_filter,
            &observation.relative_path.to_lowercase(),
        ));
        if let Some(prior_relative_path) = &observation.prior_relative_path {
            add(Term::from_field_text(
                fields.touched_file_filter,
                &prior_relative_path.to_lowercase(),
            ));
        }
    }
    add(Term::from_field_bytes(
        fields.source_event_order,
        &record.source_event_order,
    ));
    add(Term::from_field_bytes(
        fields.session_event_order,
        &record.session_event_order,
    ));
    add(Term::from_field_bytes(
        fields.semantic_event_order,
        &record.semantic_event_order,
    ));

    // Basic postings expose one membership per distinct term and document even
    // when Core contributes the same exact value through multiple properties.
    expected.sort_unstable();
    expected.dedup();
    let mut delta = ProjectionAccumulator::default();
    for digest in expected {
        delta.subtract(&digest);
    }
    Ok(delta)
}

fn query_projection_digest(field: Field, serialized_value: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.core-query-projection-v1\0");
    digest.update(field.field_id().to_be_bytes());
    digest.update((serialized_value.len() as u64).to_be_bytes());
    digest.update(serialized_value);
    digest.finalize().into()
}

fn verify_query_fast_fields(
    segment: &tantivy::SegmentReader,
    doc_id: u32,
    record: &query::VerificationRecord,
) -> Result<()> {
    macro_rules! verify_u64 {
        ($field:literal, $expected:expr) => {{
            let column = segment.fast_fields().u64($field)?;
            let mut values = column.values_for_doc(doc_id);
            if values.next() != Some($expected) || values.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField($field));
            }
        }};
    }
    macro_rules! verify_optional_i64 {
        ($field:literal, $expected:expr) => {{
            let column = segment.fast_fields().i64($field)?;
            let mut values = column.values_for_doc(doc_id);
            if values.next() != $expected || values.next().is_some() {
                return Err(IndexError::InvalidStoredDocumentField($field));
            }
        }};
    }

    verify_u64!("event_sequence", record.core_record.event_sequence);
    verify_optional_i64!(
        "occurred_at_unix_ms",
        record.core_record.occurred_at_unix_ms
    );
    verify_u64!(
        "core_content_bytes",
        u64::try_from(crate::index_document::core_content_bytes(
            &record.core_record.content
        )?)
        .map_err(|_| IndexError::CountOverflow)?
    );
    verify_u64!(
        "core_record_encoded_bytes",
        u64::try_from(record.stored_core_bytes).map_err(|_| IndexError::CountOverflow)?
    );
    Ok(())
}

fn verify_remaining_query_projections(
    searcher: &Searcher,
    fields: crate::Fields,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    for field in remaining_query_projection_fields(fields) {
        for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
            let inverted = segment.inverted_index(field)?;
            let mut terms = inverted.terms().stream()?;
            while terms.advance() {
                let digest = query_projection_digest(field, terms.key());
                for_each_live_posting(&inverted, terms.value(), segment_ord, segment, |address| {
                    projection_deltas.accumulate(address, &digest)
                })?;
            }
        }
    }
    Ok(())
}

fn remaining_query_projection_fields(fields: crate::Fields) -> [Field; 20] {
    [
        fields.event_identity_digest,
        fields.source_key,
        fields.provider,
        fields.source_format,
        fields.custom_provider_key,
        fields.custom_source_id,
        fields.provider_session_id,
        fields.branch,
        fields.agent_type,
        fields.is_primary,
        fields.event_sequence,
        fields.occurred_at_unix_ms,
        fields.event_type,
        fields.role,
        fields.repository_produced_object_id,
        fields.workspace_filter,
        fields.touched_file_filter,
        fields.source_event_order,
        fields.session_event_order,
        fields.semantic_event_order,
    ]
}

fn verify_query_projection_completion(
    searcher: &Searcher,
    projection_deltas: &ProjectionDeltas,
) -> Result<()> {
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        let segment_ord = u32::try_from(segment_ord).map_err(|_| IndexError::CountOverflow)?;
        for doc_id in 0..segment.max_doc() {
            if !segment.is_deleted(doc_id)
                && !projection_deltas.is_complete(DocAddress::new(segment_ord, doc_id))?
            {
                return Err(IndexError::InvalidStoredDocumentField("query_projection"));
            }
        }
    }
    Ok(())
}

fn verify_body_projection(
    inverted: &InvertedIndexReader,
    analyzer: &mut tantivy::tokenizer::TextAnalyzer,
    field: Field,
    body: Option<&str>,
    doc_id: u32,
) -> Result<u64> {
    let mut expected = BTreeMap::<String, Vec<u32>>::new();
    if let Some(body) = body {
        let mut stream = analyzer.token_stream(body);
        while stream.advance() {
            let token = stream.token();
            let position = u32::try_from(token.position).map_err(|_| IndexError::CountOverflow)?;
            expected
                .entry(token.text.clone())
                .or_default()
                .push(position);
        }
    }
    let mut token_count = 0_u64;
    for (text, expected_positions) in expected {
        token_count = token_count
            .checked_add(
                u64::try_from(expected_positions.len()).map_err(|_| IndexError::CountOverflow)?,
            )
            .ok_or(IndexError::CountOverflow)?;
        let term = Term::from_field_text(field, &text);
        let term_info = inverted
            .get_term_info(&term)?
            .ok_or(IndexError::InvalidStoredDocumentField("body_search"))?;
        let mut postings = inverted
            .read_postings_from_terminfo(&term_info, IndexRecordOption::WithFreqsAndPositions)?;
        if postings.doc() > doc_id
            || postings.seek(doc_id) != doc_id
            || postings.term_freq()
                != u32::try_from(expected_positions.len()).map_err(|_| IndexError::CountOverflow)?
        {
            return Err(IndexError::InvalidStoredDocumentField("body_search"));
        }
        let mut actual_positions = Vec::with_capacity(expected_positions.len());
        postings.positions(&mut actual_positions);
        if actual_positions != expected_positions {
            return Err(IndexError::InvalidStoredDocumentField("body_search"));
        }
    }
    Ok(token_count)
}

fn live_body_token_count(searcher: &Searcher, field: Field) -> Result<u64> {
    let mut total = 0_u64;
    for segment in searcher.segment_readers() {
        let inverted = segment.inverted_index(field)?;
        let mut terms = inverted.terms().stream()?;
        while terms.advance() {
            let mut postings = inverted
                .read_postings_from_terminfo(terms.value(), IndexRecordOption::WithFreqs)?;
            let mut doc_id = postings.doc();
            while doc_id != TERMINATED {
                if !segment.is_deleted(doc_id) {
                    total = total
                        .checked_add(u64::from(postings.term_freq()))
                        .ok_or(IndexError::CountOverflow)?;
                }
                doc_id = postings.advance();
            }
        }
    }
    Ok(total)
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

fn verify_event_identities(
    searcher: &Searcher,
    field: Field,
    expected: u64,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
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
        let projection_digest = query_projection_digest(field, merged.key());
        let mut seen = false;
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
                    projection_deltas.accumulate(address, &projection_digest)?;
                    if std::mem::replace(&mut seen, true) {
                        return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
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
    verification_spill: &VerificationSpill,
    projection_deltas: &mut ProjectionDeltas,
) -> Result<()> {
    let segments = searcher.segment_readers();
    let mut mappings = Vec::with_capacity(fields.len() * segments.len());
    let mut inverted_indexes = Vec::with_capacity(fields.len() * segments.len());
    for (role_index, (field, role)) in fields.into_iter().enumerate() {
        for (segment_ord, segment) in segments.iter().enumerate() {
            inverted_indexes.push(segment.inverted_index(field)?);
            mappings.push((segment_ord, role_index, role, field));
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
        let mut owner = None::<u32>;
        for (stream_index, term_info) in merged.current_segment_ords_and_term_infos() {
            let (segment_ord, role_index, role, field) = mappings[stream_index];
            let projection_digest = query_projection_digest(field, merged.key());
            for_each_live_posting(
                &inverted_indexes[stream_index],
                &term_info,
                segment_ord,
                &segments[segment_ord],
                |address| {
                    occurrences[role_index] = occurrences[role_index]
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    projection_deltas.accumulate(address, &projection_digest)?;
                    let (identity, source_owner) =
                        identity_for_role(verification_spill, address, role)?;
                    if identity.as_uuid() != uuid {
                        return Err(IndexError::InvalidStoredDocumentField("session_id"));
                    }
                    match digest {
                        None => digest = Some(identity.digest),
                        Some(existing) if existing == identity.digest => {}
                        Some(existing) => {
                            return Err(IndexError::CompactIdentityCollision {
                                kind: "session",
                                uuid,
                                existing_digest: hex(&existing),
                                new_digest: hex(&identity.digest),
                            });
                        }
                    }
                    if let Some(candidate_owner) = source_owner {
                        match owner {
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

fn identity_for_role(
    verification_spill: &VerificationSpill,
    address: DocAddress,
    role: IdentityFieldRole,
) -> Result<(CompactIdentity, Option<u32>)> {
    let identities = verification_spill.record(address, "session_id")?;
    match role {
        IdentityFieldRole::Session => {
            Ok((identities.session, Some(identities.session_source_ordinal)))
        }
        IdentityFieldRole::ParentSession => Ok((
            identities
                .parent_session
                .ok_or(IndexError::InvalidStoredDocumentField("parent_session_id"))?,
            None,
        )),
        IdentityFieldRole::RootSession => Ok((identities.root_session, None)),
    }
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
    pub(crate) stored_core_bytes: u64,
    pub(crate) body_tokens: u64,
    pub(crate) verification_spill_bytes: u64,
    pub(crate) verification_heap_payload_bound_bytes: usize,
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
        stored_core_bytes: metrics.stored_core_bytes,
        body_tokens: metrics.body_tokens,
        verification_spill_bytes: metrics.verification_spill_bytes,
        verification_heap_payload_bound_bytes: metrics.verification_heap_payload_bound_bytes,
    })
}

#[cfg(test)]
pub(crate) fn reset_verification_activity() {
    CHECKSUM_WALKS.with(|count| count.set(0));
    LOGICAL_PASSES.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn verification_activity() -> (usize, usize) {
    (
        CHECKSUM_WALKS.with(Cell::get),
        LOGICAL_PASSES.with(Cell::get),
    )
}
