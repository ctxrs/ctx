use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
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
    staging::{accumulate_core_record, core_record_accumulator_leaf},
    GenerationManifest, IndexError, Result,
};

use super::certification::{open_artifact, recapture_artifact, ArtifactIdentity};

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

#[derive(Clone, Copy)]
struct SegmentVerificationTask {
    segment_ord: usize,
    start_doc_id: u32,
    end_doc_id: u32,
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
    verification_tracked_heap_bytes: usize,
}

#[cfg(test)]
thread_local! {
    static CHECKSUM_WALKS: Cell<usize> = const { Cell::new(0) };
    static HASHED_ARTIFACT_BYTES: Cell<u64> = const { Cell::new(0) };
    static LOGICAL_PASSES: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_TERMS: Cell<usize> = const { Cell::new(0) };
    static CANDIDATE_IDENTITY_DOCUMENTS: Cell<usize> = const { Cell::new(0) };
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
    let worker_budget = verification_worker_budget(searcher.num_docs());
    verify_searcher_with_options(searcher, manifest, worker_budget, false, false).map(|_| ())
}

/// Verifies the complete publication authority carried by one immutable searcher.
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

const PHYSICAL_INTEGRITY_DOMAIN: &[u8] = b"ctx-tantivy-physical-integrity-v1\0";
const PHYSICAL_HASH_BUFFER_BYTES: usize = 64 * 1024;
const TANTIVY_META_FILE: &str = "meta.json";
const MAX_VERIFICATION_WORKERS: usize = 24;

#[derive(Debug)]
struct PhysicalFileDigest {
    artifact: ArtifactIdentity,
    sha256: [u8; 32],
}

#[derive(Debug)]
struct PhysicalDigestPart {
    path: String,
    length: u64,
    sha256: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct PhysicalIntegrityAudit {
    digest: String,
    artifacts: Vec<ArtifactIdentity>,
}

impl PhysicalIntegrityAudit {
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn artifacts(&self) -> &[ArtifactIdentity] {
        &self.artifacts
    }

    pub(super) fn artifact_paths(&self) -> Vec<String> {
        self.artifacts
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect()
    }
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
    Ok(physical_integrity_audit(index, generation_path)?.digest)
}

pub(crate) fn physical_integrity_audit(
    index: &tantivy::Index,
    generation_path: &Path,
) -> Result<PhysicalIntegrityAudit> {
    #[cfg(test)]
    CHECKSUM_WALKS.with(|count| count.set(count.get() + 1));
    let directory =
        DurableMmapDirectory::open(generation_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let root = generation_path
        .parent()
        .filter(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name == "index-generations")
        })
        .and_then(Path::parent)
        .ok_or(IndexError::ChecksumMismatch)?;
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    let entries = paths
        .into_iter()
        .map(|path| {
            hash_physical_file(
                root,
                &directory,
                generation_path,
                &path,
                path != Path::new(TANTIVY_META_FILE),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let parts = entries
        .iter()
        .map(|entry| PhysicalDigestPart {
            path: entry.artifact.path.clone(),
            length: entry.artifact.identity.length(),
            sha256: entry.sha256,
        })
        .collect::<Vec<_>>();
    let digest = canonical_physical_integrity_digest(&parts)?;
    let artifacts = entries.into_iter().map(|entry| entry.artifact).collect();
    Ok(PhysicalIntegrityAudit { digest, artifacts })
}

/// Verifies a generation against the physical authority in its pointer slot.
pub(crate) fn verify_physical_integrity(
    index: &tantivy::Index,
    generation_path: &Path,
    expected_digest: &str,
) -> Result<()> {
    let audit = physical_integrity_audit(index, generation_path)?;
    if audit.digest != expected_digest {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn hash_physical_file(
    root: &Path,
    directory: &DurableMmapDirectory,
    generation_path: &Path,
    relative_path: &Path,
    validate_tantivy_footer: bool,
) -> Result<PhysicalFileDigest> {
    let (mut file, artifact) = open_artifact(root, generation_path, relative_path)?;
    let length = artifact.identity.length();
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
        #[cfg(test)]
        HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(count_u64)));
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
    if recapture_artifact(root, generation_path, relative_path)? != artifact {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(PhysicalFileDigest {
        artifact,
        sha256: sha256.finalize().into(),
    })
}

fn canonical_physical_integrity_digest(entries: &[PhysicalDigestPart]) -> Result<String> {
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

    let entry = |file: &StubbedCrcFile<'_>| PhysicalDigestPart {
        path: file.path.to_owned(),
        length: u64::try_from(file.bytes.len()).unwrap(),
        sha256: Sha256::digest(file.bytes).into(),
    };
    assert_ne!(
        canonical_physical_integrity_digest(&[entry(&first)]).unwrap(),
        canonical_physical_integrity_digest(&[entry(&second)]).unwrap()
    );
}

pub(crate) fn active_index_files(index: &tantivy::Index) -> Result<BTreeSet<PathBuf>> {
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

/// Verifies a writer-produced candidate without replaying an already-audited base.
///
/// A cold or recovery candidate has no reusable base and therefore keeps the
/// complete stored-Core and posting audit. For an incremental candidate, every
/// segment not present in the immutable base contributes its event and session
/// identity terms to an identity-delta audit. Every changed Core record is fully
/// decoded once. Each changed identity is then resolved against all live
/// candidate segments, while an already-audited retained identity is decoded at
/// most once per role and term. This preserves duplicate/collision and
/// cross-source session ownership checks without replaying unrelated terms or
/// retained records that share one session.
pub(crate) fn verify_publication_candidate(
    searcher: &Searcher,
    manifest: &GenerationManifest,
    base_searcher: Option<&Searcher>,
) -> Result<()> {
    let Some(base_searcher) = base_searcher else {
        return verify_searcher(searcher, manifest);
    };

    verify_searcher_structure(searcher, manifest)?;
    let fields = fields_from_schema(searcher.schema())?;
    query::validate_verification_projection(fields)?;
    let base_segment_ids = base_searcher
        .segment_readers()
        .iter()
        .map(|segment| segment.segment_id().uuid_string())
        .collect::<HashSet<_>>();
    let changed_segments = searcher
        .segment_readers()
        .iter()
        .enumerate()
        .filter_map(|(segment_ord, segment)| {
            (!base_segment_ids.contains(&segment.segment_id().uuid_string())).then_some(segment_ord)
        })
        .collect::<Vec<_>>();

    let expected_parent_sessions =
        verify_candidate_event_identities(searcher, fields, &changed_segments)?;
    verify_candidate_session_identities(
        searcher,
        fields,
        &changed_segments,
        expected_parent_sessions,
    )
}

fn verify_candidate_event_identities(
    searcher: &Searcher,
    fields: crate::Fields,
    changed_segments: &[usize],
) -> Result<u64> {
    if changed_segments.is_empty() {
        return Ok(0);
    }
    let segments = searcher.segment_readers();
    let changed_segment_set = changed_segments.iter().copied().collect::<HashSet<_>>();
    let expected_changed_documents =
        changed_segments.iter().try_fold(0_u64, |total, &ordinal| {
            total
                .checked_add(u64::from(segments[ordinal].num_docs()))
                .ok_or(IndexError::CountOverflow)
        })?;
    let changed_inverted = changed_segments
        .iter()
        .map(|segment_ord| segments[*segment_ord].inverted_index(fields.event_id))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = changed_inverted
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut changed_documents = 0_u64;
    let mut parent_sessions = 0_u64;
    while merged.advance() {
        note_candidate_identity_term();
        let uuid = canonical_uuid_term(merged.key(), "event_id")?;
        let mut digest = None;
        for (segment_ord, segment) in segments.iter().enumerate() {
            let inverted = segment.inverted_index(fields.event_id)?;
            let Some(term_info) = inverted.terms().get(merged.key())? else {
                continue;
            };
            for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
                note_candidate_identity_document();
                let identities = if changed_segment_set.contains(&segment_ord) {
                    let record = query::stored_verification_record(searcher, address, fields)?;
                    changed_documents = changed_documents
                        .checked_add(1)
                        .ok_or(IndexError::CountOverflow)?;
                    parent_sessions = parent_sessions
                        .checked_add(u64::from(record.identities.parent_session.is_some()))
                        .ok_or(IndexError::CountOverflow)?;
                    record.identities
                } else {
                    query::stored_verification_identities(searcher, address, fields)?
                };
                let identity = identities.event;
                if identity.as_uuid() != uuid {
                    return Err(IndexError::InvalidStoredDocumentField("event_id"));
                }
                match digest {
                    None => digest = Some(identity.digest),
                    Some(existing) if existing == identity.digest => {
                        return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
                    }
                    Some(existing) => {
                        return Err(IndexError::CompactIdentityCollision {
                            kind: "event",
                            uuid,
                            existing_digest: hex(&existing),
                            new_digest: hex(&identity.digest),
                        });
                    }
                }
                Ok(())
            })?;
        }
    }
    if changed_documents != expected_changed_documents {
        return Err(IndexError::InvalidStoredDocumentField("event_id"));
    }
    Ok(parent_sessions)
}

fn verify_candidate_session_identities(
    searcher: &Searcher,
    fields: crate::Fields,
    changed_segments: &[usize],
    expected_parent_sessions: u64,
) -> Result<()> {
    if changed_segments.is_empty() {
        return Ok(());
    }
    let segments = searcher.segment_readers();
    let changed_segment_set = changed_segments.iter().copied().collect::<HashSet<_>>();
    let expected_changed_documents =
        changed_segments.iter().try_fold(0_u64, |total, &ordinal| {
            total
                .checked_add(u64::from(segments[ordinal].num_docs()))
                .ok_or(IndexError::CountOverflow)
        })?;
    let roles = [
        (fields.session_id, IdentityFieldRole::Session),
        (fields.parent_session_id, IdentityFieldRole::ParentSession),
        (fields.root_session_id, IdentityFieldRole::RootSession),
    ];
    let changed_inverted = roles
        .iter()
        .flat_map(|(field, _)| {
            changed_segments
                .iter()
                .map(move |segment_ord| segments[*segment_ord].inverted_index(*field))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let streams = changed_inverted
        .iter()
        .map(|inverted| inverted.terms().stream())
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut merged = TermMerger::new(streams);
    let mut changed_occurrences = [0_u64; 3];
    while merged.advance() {
        note_candidate_identity_term();
        let uuid = canonical_uuid_term(merged.key(), "session_id")?;
        let mut digest = None;
        let mut owner = None::<[u8; 32]>;
        for (field, role) in roles {
            let mut decoded_retained_identity = false;
            for (segment_ord, segment) in segments.iter().enumerate() {
                let inverted = segment.inverted_index(field)?;
                let Some(term_info) = inverted.terms().get(merged.key())? else {
                    continue;
                };
                for_each_live_posting(&inverted, &term_info, segment_ord, segment, |address| {
                    let changed = changed_segment_set.contains(&segment_ord);
                    if changed {
                        let role_index = match role {
                            IdentityFieldRole::Session => 0,
                            IdentityFieldRole::ParentSession => 1,
                            IdentityFieldRole::RootSession => 2,
                        };
                        changed_occurrences[role_index] = changed_occurrences[role_index]
                            .checked_add(1)
                            .ok_or(IndexError::CountOverflow)?;
                    } else if std::mem::replace(&mut decoded_retained_identity, true) {
                        return Ok(());
                    }
                    note_candidate_identity_document();
                    let identities =
                        query::stored_verification_identities(searcher, address, fields)?;
                    let (identity, candidate_owner) = match role {
                        IdentityFieldRole::Session => {
                            (identities.session, Some(identities.session_source_owner))
                        }
                        IdentityFieldRole::ParentSession => (
                            identities.parent_session.ok_or(
                                IndexError::InvalidStoredDocumentField("parent_session_id"),
                            )?,
                            None,
                        ),
                        IdentityFieldRole::RootSession => (identities.root_session, None),
                    };
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
                    if let Some(candidate_owner) = candidate_owner {
                        match owner {
                            Some(existing) if existing != candidate_owner => {
                                return Err(IndexError::DuplicateSessionIdentity(uuid.to_string()));
                            }
                            None => owner = Some(candidate_owner),
                            _ => {}
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
    if changed_occurrences
        != [
            expected_changed_documents,
            expected_parent_sessions,
            expected_changed_documents,
        ]
    {
        return Err(IndexError::InvalidStoredDocumentField("session_id"));
    }
    Ok(())
}

#[cfg(test)]
fn note_candidate_identity_term() {
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_candidate_identity_term() {}

#[cfg(test)]
fn note_candidate_identity_document() {
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn note_candidate_identity_document() {}

fn verification_worker_budget(document_count: u64) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    usize::try_from(document_count)
        .unwrap_or(usize::MAX)
        .max(1)
        .min(available)
        .min(MAX_VERIFICATION_WORKERS)
}

#[cfg(test)]
#[test]
fn verification_tasks_split_large_segments_into_contiguous_bounded_ranges() {
    let max_docs = [1_052_077, 976_361, 131_836, 3_341];
    let tasks = segment_verification_tasks_for_max_docs(&max_docs, 24).unwrap();
    assert!(tasks.len() > 24);

    for (segment_ord, max_doc) in max_docs.into_iter().enumerate() {
        let segment_tasks = tasks
            .iter()
            .filter(|task| task.segment_ord == segment_ord)
            .collect::<Vec<_>>();
        assert_eq!(segment_tasks.first().unwrap().start_doc_id, 0);
        assert_eq!(segment_tasks.last().unwrap().end_doc_id, max_doc);
        assert!(segment_tasks
            .windows(2)
            .all(|pair| pair[0].end_doc_id == pair[1].start_doc_id));
    }
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
    pub(crate) verification_tracked_heap_bytes: usize,
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
        verification_tracked_heap_bytes: metrics.verification_tracked_heap_bytes,
    })
}

#[cfg(test)]
pub(crate) fn reset_verification_activity() {
    CHECKSUM_WALKS.with(|count| count.set(0));
    HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(0));
    LOGICAL_PASSES.with(|count| count.set(0));
    CANDIDATE_IDENTITY_TERMS.with(|count| count.set(0));
    CANDIDATE_IDENTITY_DOCUMENTS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn verification_activity() -> (usize, usize) {
    (
        CHECKSUM_WALKS.with(Cell::get),
        LOGICAL_PASSES.with(Cell::get),
    )
}

#[cfg(test)]
pub(crate) fn hashed_artifact_bytes() -> u64 {
    HASHED_ARTIFACT_BYTES.with(Cell::get)
}

#[cfg(test)]
pub(crate) fn candidate_identity_verification_activity() -> (usize, usize) {
    (
        CANDIDATE_IDENTITY_TERMS.with(Cell::get),
        CANDIDATE_IDENTITY_DOCUMENTS.with(Cell::get),
    )
}
