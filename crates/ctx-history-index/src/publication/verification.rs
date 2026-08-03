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

include!("verification/logical.rs");

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
