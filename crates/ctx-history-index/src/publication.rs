use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    },
};

use ctx_history_core::{CertifiedSource, SourceKey, StableEntityId, IDENTITY_VERSION};
use tantivy::{
    collector::Count, directory::Directory, schema::IndexRecordOption, DocAddress, Executor, Index,
    IndexMeta, ReloadPolicy, Searcher, Term,
};
use uuid::Uuid;

use crate::{
    current_source_generation_policy_hash,
    durable_directory::DurableMmapDirectory,
    fields_from_schema,
    identity::{hex, is_generation_id, register_event_identity, sha256_hex, source_token},
    query, required_field, CommitPayload, GenerationManifest, IndexError, Result, WriterOptions,
    COMMIT_PAYLOAD_VERSION, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION,
    LEXICAL_SCHEMA_VERSION, MANIFEST_DIRECTORY,
};

pub(crate) fn load_manifest_for_metas(
    root: &Path,
    metas: &IndexMeta,
) -> Result<GenerationManifest> {
    let payload = metas
        .payload
        .as_ref()
        .ok_or(IndexError::MissingCommitPayload)?;
    let payload: CommitPayload = serde_json::from_str(payload)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(&payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let path = manifest_path(root, &payload.generation_id);
    let bytes = fs::read(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => IndexError::MissingManifest(payload.generation_id.clone()),
        _ => IndexError::Io(error),
    })?;
    let actual = sha256_hex(&bytes);
    if actual != payload.generation_id {
        return Err(IndexError::ManifestDigestMismatch {
            expected: payload.generation_id,
            actual,
        });
    }
    let manifest: GenerationManifest = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&manifest)? != bytes {
        return Err(IndexError::NonCanonicalManifest);
    }
    if manifest.manifest_version != GENERATION_MANIFEST_VERSION {
        return Err(IndexError::UnsupportedManifest(manifest.manifest_version));
    }
    if manifest.identity_version != IDENTITY_VERSION
        || manifest.lexical_schema_version != LEXICAL_SCHEMA_VERSION
        || manifest.lexical_analyzer_version != LEXICAL_ANALYZER_VERSION
    {
        return Err(IndexError::GenerationContractMismatch {
            identity: manifest.identity_version,
            schema: manifest.lexical_schema_version,
            analyzer: manifest.lexical_analyzer_version,
        });
    }
    let expected_policy_hash = current_source_generation_policy_hash()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash,
        });
    }
    manifest.validate_contract()?;
    Ok(manifest)
}

pub(crate) fn reconcile_commit_error(
    index: &Index,
    root: &Path,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    commit_error: tantivy::TantivyError,
) -> Result<u64> {
    let metas = index.load_metas().map_err(|reconcile_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; reopening meta.json failed: {reconcile_error}"),
        }
    })?;
    let visible_generation = payload_generation_id(&metas).map_err(|payload_error| {
        IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage: "commit reconciliation",
            detail: format!("{commit_error}; visible payload is invalid: {payload_error}"),
        }
    })?;
    if visible_generation.as_deref() == Some(expected_generation_id) {
        let verification = (|| -> Result<u64> {
            let manifest = load_manifest_for_metas(root, &metas)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            let searcher = reader.searcher();
            if searcher_generation(&searcher) != meta_generation(&metas) {
                return Err(IndexError::ConcurrentGenerationChange);
            }
            verify_searcher(&searcher, &manifest)?;
            Ok(metas.opstamp)
        })();
        return verification.map_err(|verification_error| {
            IndexError::CommittedGenerationNeedsRecovery {
                generation_id: expected_generation_id.to_owned(),
                stage: "commit reconciliation",
                detail: format!(
                    "{commit_error}; new payload is visible but verification failed: \
                     {verification_error}"
                ),
            }
        });
    }
    if visible_generation.as_deref() == previous_generation_id
        || (previous_generation_id.is_none()
            && visible_generation.is_none()
            && metas.segments.is_empty())
    {
        return Err(IndexError::Tantivy(commit_error));
    }
    Err(IndexError::CommittedGenerationNeedsRecovery {
        generation_id: expected_generation_id.to_owned(),
        stage: "commit reconciliation",
        detail: format!(
            "{commit_error}; expected old generation {:?} or new generation, found {:?}",
            previous_generation_id, visible_generation
        ),
    })
}

pub(crate) fn payload_generation_id(metas: &IndexMeta) -> Result<Option<String>> {
    let Some(payload) = metas.payload.as_deref() else {
        return Ok(None);
    };
    let payload: CommitPayload = serde_json::from_str(payload)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(&payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    Ok(Some(payload.generation_id))
}

pub(crate) fn classify_publication_failure(
    index: &Index,
    expected_generation_id: &str,
    previous_generation_id: Option<&str>,
    stage: &'static str,
    error: tantivy::TantivyError,
) -> IndexError {
    let visible_generation = index
        .load_metas()
        .map_err(IndexError::from)
        .and_then(|metas| payload_generation_id(&metas));
    match visible_generation {
        Ok(visible) if visible.as_deref() == previous_generation_id => IndexError::Tantivy(error),
        Ok(None) if previous_generation_id.is_none() => IndexError::Tantivy(error),
        Ok(visible) => IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage,
            detail: format!("{error}; visible generation is {visible:?}"),
        },
        Err(reconcile_error) => IndexError::CommittedGenerationNeedsRecovery {
            generation_id: expected_generation_id.to_owned(),
            stage,
            detail: format!("{error}; visibility reconciliation failed: {reconcile_error}"),
        },
    }
}

pub(crate) fn write_manifest(
    root: &Path,
    generation_id: &str,
    manifest: &GenerationManifest,
) -> Result<()> {
    let bytes = serde_json::to_vec(manifest)?;
    let actual = sha256_hex(&bytes);
    if actual != generation_id {
        return Err(IndexError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    let directory = root.join(MANIFEST_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let path = manifest_path(root, generation_id);
    if path.is_file() {
        let existing = fs::read(&path)?;
        if existing == bytes {
            // A prior process may have died after publishing this immutable
            // filename but before synchronizing either its contents or its
            // directory entry. Re-fence both before meta.json can name it.
            File::open(&path)?.sync_all()?;
            sync_directory(&directory)?;
            return Ok(());
        }
        let quarantine = directory.join(format!(
            ".{generation_id}.corrupt-{}",
            Uuid::now_v7().simple()
        ));
        fs::rename(&path, quarantine)?;
        sync_directory(&directory)?;
    }

    // The writer lock serializes manifest publication, so no-clobber hard-link
    // tricks are unnecessary and exclude filesystems without hard-link
    // support. Reuse the same durable atomic replacement primitive as
    // Tantivy's meta publication.
    let durable_directory =
        DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    durable_directory.atomic_write(&relative_path, &bytes)?;
    Ok(())
}

pub(crate) fn verify_searcher_structure(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<()> {
    verify_total_document_count(searcher, manifest.indexed_documents)
}

pub(crate) fn verify_searcher(searcher: &Searcher, manifest: &GenerationManifest) -> Result<()> {
    let worker_budget = verification_worker_budget(searcher.segment_readers().len());
    verify_searcher_with_options(searcher, manifest, worker_budget, false, false).map(|_| ())
}

struct SegmentVerification {
    document_count: u64,
    document_decodes: usize,
    source_terms: usize,
    source_counts: BTreeMap<String, u64>,
    event_identities: HashMap<Uuid, [u8; 32]>,
    session_identities: HashMap<Uuid, ([u8; 32], Option<String>)>,
    identity_error: Option<IndexError>,
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

fn verification_worker_budget(segment_count: usize) -> usize {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let indexer_budget = WriterOptions::default().indexer_threads;
    segment_count
        .max(1)
        .min(available)
        .min(indexer_budget.max(1))
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
    let source_field = required_field(searcher.schema(), "source_key")?;
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
    let mut source_counts = BTreeMap::<String, u64>::new();
    let mut event_identities = HashMap::new();
    let mut session_identities = HashMap::new();
    let mut deferred_identity_error = None;

    for wave_start in (0..segment_count).step_by(worker_budget) {
        let wave_end = (wave_start + worker_budget).min(segment_count);
        let wave_rendezvous = (wave_start == 0).then_some(rendezvous.as_ref()).flatten();
        let segments = executor.map(
            |segment_ord| {
                Ok(verify_segment(
                    searcher,
                    segment_ord,
                    source_field,
                    wave_rendezvous,
                    counters.as_ref(),
                ))
            },
            wave_start..wave_end,
        )?;
        metrics.max_buffered_segments = metrics.max_buffered_segments.max(segments.len());
        metrics.max_buffered_event_identities = metrics.max_buffered_event_identities.max(
            segments
                .iter()
                .filter_map(|segment| segment.as_ref().ok())
                .map(|segment| segment.event_identities.len())
                .sum(),
        );
        metrics.max_buffered_session_identities = metrics.max_buffered_session_identities.max(
            segments
                .iter()
                .filter_map(|segment| segment.as_ref().ok())
                .map(|segment| segment.session_identities.len())
                .sum(),
        );

        for segment in segments {
            let segment = segment?;
            metrics.segment_tasks += 1;
            metrics.document_decodes = metrics
                .document_decodes
                .checked_add(segment.document_decodes)
                .ok_or(IndexError::CountOverflow)?;
            metrics.source_terms = metrics
                .source_terms
                .checked_add(segment.source_terms)
                .ok_or(IndexError::CountOverflow)?;
            total_documents = total_documents
                .checked_add(segment.document_count)
                .ok_or(IndexError::CountOverflow)?;
            merge_source_counts(&mut source_counts, segment.source_counts)?;

            if deferred_identity_error.is_none() {
                if let Err(error) = merge_identity_maps(
                    &mut event_identities,
                    &mut session_identities,
                    segment.event_identities,
                    segment.session_identities,
                ) {
                    deferred_identity_error = Some(error);
                } else if segment.identity_error.is_some() {
                    deferred_identity_error = segment.identity_error;
                }
            }
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
    for source in &manifest.sources {
        let source_id = source_token(source.observation().source());
        let actual = source_counts.get(&source_id).copied().unwrap_or(0);
        let expected = source.counts().indexed_documents;
        if actual != expected {
            return Err(IndexError::SourceCountMismatch {
                source_id,
                manifest: expected,
                index: actual,
            });
        }
    }
    if let Some(error) = deferred_identity_error {
        return Err(error);
    }
    Ok(metrics)
}

fn verify_segment(
    searcher: &Searcher,
    segment_ord: usize,
    source_field: tantivy::schema::Field,
    rendezvous: Option<&Barrier>,
    counters: Option<&VerificationCounters>,
) -> Result<SegmentVerification> {
    let _active_worker = ActiveVerificationWorker::enter(counters);
    if let Some(rendezvous) = rendezvous {
        rendezvous.wait();
    }
    let segment = searcher.segment_reader(segment_ord as u32);
    let inverted_index = segment.inverted_index(source_field)?;
    let mut source_term_stream = inverted_index.terms().stream()?;
    let mut source_counts = BTreeMap::new();
    let mut source_terms = 0;
    while let Some((source_id, term_info)) = source_term_stream.next() {
        source_terms += 1;
        let source_id = std::str::from_utf8(source_id)
            .map_err(|_| IndexError::InvalidStoredDocumentField("source_key"))?
            .to_owned();
        let count = if let Some(alive_bitset) = segment.alive_bitset() {
            u64::from(
                inverted_index
                    .read_postings_from_terminfo(term_info, IndexRecordOption::Basic)?
                    .doc_freq_given_deletes(alive_bitset),
            )
        } else {
            u64::from(term_info.doc_freq)
        };
        if count != 0 {
            source_counts.insert(source_id, count);
        }
    }

    let mut event_identities = HashMap::new();
    let mut session_identities = HashMap::new();
    let mut document_decodes = 0;
    let mut identity_error = None;
    for doc_id in 0..segment.max_doc() {
        if segment.is_deleted(doc_id) {
            continue;
        }
        document_decodes += 1;
        let verification = (|| -> Result<()> {
            let event = query::stored_verification_record(
                searcher,
                DocAddress::new(segment_ord as u32, doc_id),
            )?;
            register_event_identity(&mut event_identities, event.event_id)?;
            register_generation_session_identity(
                &mut session_identities,
                event.session_id,
                Some(&event.source_owner),
            )?;
            if let Some(parent_session_id) = event.parent_session_id {
                register_generation_session_identity(
                    &mut session_identities,
                    parent_session_id,
                    None,
                )?;
            }
            register_generation_session_identity(
                &mut session_identities,
                event.root_session_id,
                None,
            )
        })();
        if let Err(error) = verification {
            identity_error = Some(error);
            break;
        }
    }

    Ok(SegmentVerification {
        document_count: u64::from(segment.num_docs()),
        document_decodes,
        source_terms,
        source_counts,
        event_identities,
        session_identities,
        identity_error,
    })
}

fn merge_source_counts(
    target: &mut BTreeMap<String, u64>,
    source: BTreeMap<String, u64>,
) -> Result<()> {
    for (source_id, count) in source {
        let total = target.entry(source_id).or_default();
        *total = total.checked_add(count).ok_or(IndexError::CountOverflow)?;
    }
    Ok(())
}

pub(crate) fn verify_source_document_count(
    searcher: &Searcher,
    source: &CertifiedSource,
) -> Result<()> {
    verify_source_count(
        searcher,
        source.observation().source(),
        source.counts().indexed_documents,
    )
}

pub(crate) fn verify_source_absent(searcher: &Searcher, source: &SourceKey) -> Result<()> {
    verify_source_count(searcher, source, 0)
}

fn verify_source_count(searcher: &Searcher, source: &SourceKey, expected: u64) -> Result<()> {
    use tantivy::query::TermQuery;

    let source_id = source_token(source);
    let source_field = required_field(searcher.schema(), "source_key")?;
    let query = TermQuery::new(
        Term::from_field_text(source_field, &source_id),
        IndexRecordOption::Basic,
    );
    let actual = searcher.search(&query, &Count)? as u64;
    if actual != expected {
        return Err(IndexError::SourceCountMismatch {
            source_id,
            manifest: expected,
            index: actual,
        });
    }
    Ok(())
}

fn merge_identity_maps(
    event_identities: &mut HashMap<Uuid, [u8; 32]>,
    session_identities: &mut HashMap<Uuid, ([u8; 32], Option<String>)>,
    segment_event_identities: HashMap<Uuid, [u8; 32]>,
    segment_session_identities: HashMap<Uuid, ([u8; 32], Option<String>)>,
) -> Result<()> {
    let mut segment_events = segment_event_identities.into_iter().collect::<Vec<_>>();
    segment_events.sort_unstable_by_key(|(uuid, _)| *uuid);
    for (uuid, digest) in segment_events {
        match event_identities.entry(uuid) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(digest);
            }
            std::collections::hash_map::Entry::Occupied(entry) if *entry.get() == digest => {
                return Err(IndexError::DuplicateEventIdentity(uuid.to_string()));
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                return Err(IndexError::CompactIdentityCollision {
                    kind: "event",
                    uuid,
                    existing_digest: hex(entry.get()),
                    new_digest: hex(&digest),
                });
            }
        }
    }

    let mut segment_sessions = segment_session_identities.into_iter().collect::<Vec<_>>();
    segment_sessions.sort_unstable_by_key(|(uuid, _)| *uuid);
    for (uuid, (digest, owner)) in segment_sessions {
        merge_generation_session_identity(session_identities, uuid, digest, owner.as_deref())?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn verify_generation_identities(searcher: &Searcher) -> Result<()> {
    let fields = fields_from_schema(searcher.schema())?;
    let mut event_identities = HashMap::new();
    let mut session_identities = HashMap::new();
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        for doc_id in 0..segment.max_doc() {
            if segment.is_deleted(doc_id) {
                continue;
            }
            let event = query::stored_event_record(
                searcher,
                DocAddress::new(segment_ord as u32, doc_id),
                fields,
            )?;
            register_event_identity(&mut event_identities, event.event_id)?;
            let owner = source_token(event.locator.source());
            register_generation_session_identity(
                &mut session_identities,
                event.session_id,
                Some(&owner),
            )?;
            if let Some(parent_session_id) = event.parent_session_id {
                register_generation_session_identity(
                    &mut session_identities,
                    parent_session_id,
                    None,
                )?;
            }
            register_generation_session_identity(
                &mut session_identities,
                event.root_session_id,
                None,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn register_generation_session_identity(
    identities: &mut HashMap<Uuid, ([u8; 32], Option<String>)>,
    identity: StableEntityId,
    owner: Option<&str>,
) -> Result<()> {
    merge_generation_session_identity(identities, identity.as_uuid(), identity.digest(), owner)
}

fn merge_generation_session_identity(
    identities: &mut HashMap<Uuid, ([u8; 32], Option<String>)>,
    uuid: Uuid,
    digest: [u8; 32],
    owner: Option<&str>,
) -> Result<()> {
    match identities.entry(uuid) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert((digest, owner.map(str::to_owned)));
            Ok(())
        }
        std::collections::hash_map::Entry::Occupied(mut entry) if entry.get().0 == digest => {
            let registered_owner = &mut entry.get_mut().1;
            match (registered_owner.as_deref(), owner) {
                (Some(existing), Some(candidate)) if existing != candidate => {
                    Err(IndexError::DuplicateSessionIdentity(uuid.to_string()))
                }
                (None, Some(candidate)) => {
                    *registered_owner = Some(candidate.to_owned());
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        std::collections::hash_map::Entry::Occupied(entry) => {
            Err(IndexError::CompactIdentityCollision {
                kind: "session",
                uuid,
                existing_digest: hex(&entry.get().0),
                new_digest: hex(&digest),
            })
        }
    }
}

pub(crate) fn verify_total_document_count(searcher: &Searcher, expected: u64) -> Result<()> {
    let actual = searcher.search(&tantivy::query::AllQuery, &Count)? as u64;
    if actual != expected {
        return Err(IndexError::DocumentCountMismatch {
            manifest: expected,
            index: actual,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn verify_searcher_reference(
    searcher: &Searcher,
    manifest: &GenerationManifest,
) -> Result<()> {
    use tantivy::query::TermQuery;

    verify_total_document_count(searcher, manifest.indexed_documents)?;
    let source_field = required_field(searcher.schema(), "source_key")?;
    for source in &manifest.sources {
        let source_id = source_token(source.observation().source());
        let query = TermQuery::new(
            Term::from_field_text(source_field, &source_id),
            IndexRecordOption::Basic,
        );
        let actual = searcher.search(&query, &Count)? as u64;
        let expected = source.counts().indexed_documents;
        if actual != expected {
            return Err(IndexError::SourceCountMismatch {
                source_id,
                manifest: expected,
                index: actual,
            });
        }
    }
    verify_generation_identities(searcher)
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
    verify_searcher_reference(searcher, manifest)?;
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

pub(crate) fn meta_generation(metas: &IndexMeta) -> BTreeMap<String, Option<u64>> {
    metas
        .segments
        .iter()
        .map(|segment| (segment.id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

pub(crate) fn searcher_generation(searcher: &Searcher) -> BTreeMap<String, Option<u64>> {
    searcher
        .segment_readers()
        .iter()
        .map(|segment| (segment.segment_id().uuid_string(), segment.delete_opstamp()))
        .collect()
}

pub(crate) fn manifest_path(root: &Path, generation_id: &str) -> PathBuf {
    root.join(MANIFEST_DIRECTORY)
        .join(format!("{generation_id}.json"))
}

#[cfg(not(windows))]
pub(crate) fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod verification_merge_tests {
    use super::*;

    #[test]
    fn deterministic_merge_rejects_event_and_session_duplicates_and_collisions() {
        let event_uuid = Uuid::from_u128(1);
        let mut events = HashMap::from([(event_uuid, [1; 32])]);
        let duplicate = merge_identity_maps(
            &mut events,
            &mut HashMap::new(),
            HashMap::from([(event_uuid, [1; 32])]),
            HashMap::new(),
        )
        .unwrap_err();
        assert!(matches!(duplicate, IndexError::DuplicateEventIdentity(_)));

        let mut events = HashMap::from([(event_uuid, [1; 32])]);
        let collision = merge_identity_maps(
            &mut events,
            &mut HashMap::new(),
            HashMap::from([(event_uuid, [2; 32])]),
            HashMap::new(),
        )
        .unwrap_err();
        assert!(matches!(
            collision,
            IndexError::CompactIdentityCollision { kind: "event", .. }
        ));

        let session_uuid = Uuid::from_u128(2);
        let mut sessions = HashMap::from([(session_uuid, ([3; 32], Some("first".to_owned())))]);
        let duplicate = merge_identity_maps(
            &mut HashMap::new(),
            &mut sessions,
            HashMap::new(),
            HashMap::from([(session_uuid, ([3; 32], Some("second".to_owned())))]),
        )
        .unwrap_err();
        assert!(matches!(duplicate, IndexError::DuplicateSessionIdentity(_)));

        let mut sessions = HashMap::from([(session_uuid, ([3; 32], Some("first".to_owned())))]);
        let collision = merge_identity_maps(
            &mut HashMap::new(),
            &mut sessions,
            HashMap::new(),
            HashMap::from([(session_uuid, ([4; 32], Some("first".to_owned())))]),
        )
        .unwrap_err();
        assert!(matches!(
            collision,
            IndexError::CompactIdentityCollision {
                kind: "session",
                ..
            }
        ));
    }
}
