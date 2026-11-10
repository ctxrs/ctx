//! Portable Core checkpoints and explicit session exclusions, independent of
//! Tantivy's schema and segment format. Publication owns checkpoint authority.
use super::*;
use ctx_history_platform::platform_security::ensure_private_directory;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};

const DIRECTORY: &str = "retained";
const STAGING_PREFIX: &str = ".ctx-retained-";
const MAX_RECORD: u64 = ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES as u64 + 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    core_contract: String,
    generation: String,
    // Read the earlier self-contained checkpoint; new writes reference manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    manifest: Option<GenerationManifest>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    records: BTreeMap<String, String>,
    erased_sessions: BTreeSet<Uuid>,
}

fn invalid(message: impl Into<String>) -> IndexError {
    IndexError::RetainedHistory(message.into())
}

fn metadata_path(root: &Path, generation: &str) -> PathBuf {
    root.join(DIRECTORY).join(format!("{generation}.json"))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(invalid("invalid checkpoint metadata file"));
    }
    Ok(serde_json::from_reader(fs::File::open(path)?)?)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("checkpoint has no parent"))?;
    ensure_private_directory(parent)?;
    let mut stage = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempfile_in(parent)?;
    serde_json::to_writer(stage.as_file_mut(), value)?;
    stage.as_file().sync_all()?;
    durable_atomic_replace_file(stage.path(), path)?;
    sync_directory(parent)?;
    Ok(())
}

pub(super) fn load_erasures(root: &Path) -> Result<BTreeSet<Uuid>> {
    let path = root.join(DIRECTORY).join("erased-sessions.json");
    match read_json(&path) {
        Err(IndexError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(BTreeSet::new())
        }
        other => other,
    }
}

fn load(root: &Path, generation: &str) -> Result<Option<Checkpoint>> {
    let checkpoint: Checkpoint = match read_json(&metadata_path(root, generation)) {
        Err(IndexError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        result => result?,
    };
    if !matches!(checkpoint.version, 1 | 2)
        || checkpoint.generation != generation
        || checkpoint.core_contract != ctx_history_core::core_record_contract_fingerprint()
    {
        return Err(invalid(
            "checkpoint requires a supported Core migration; retained history was preserved",
        ));
    }
    if (checkpoint.version == 1 && checkpoint.manifest.is_none())
        || (checkpoint.version == 2
            && (checkpoint.manifest.is_some() || !checkpoint.records.is_empty()))
        || checkpoint
            .records
            .values()
            .any(|digest| !is_generation_id(digest))
    {
        return Err(invalid("invalid checkpoint metadata"));
    }
    Ok(Some(checkpoint))
}

fn record_blob(aggregate: &SourceCoreRecordAggregate) -> Result<String> {
    Ok(ctx_history_index_format::sha256_hex(&serde_json::to_vec(
        aggregate,
    )?))
}

/// Reuse the generation's authenticated manifest and its existing delta anchors.
/// Record blobs are addressed by the manifest's exact stored-record commitment.
pub(super) fn checkpoint(
    root: &Path,
    index: &VerifiedIndex,
    erased: &BTreeSet<Uuid>,
) -> Result<()> {
    if let Some(existing) = load(root, index.generation_id())? {
        if existing.version == 2 && existing.erased_sessions == *erased {
            return Ok(());
        }
    }
    let directory = root.join(DIRECTORY);
    ensure_private_directory(&directory)?;
    let blobs = directory.join("records");
    ensure_private_directory(&blobs)?;
    for (certificate, aggregate) in index
        .manifest()
        .sources
        .iter()
        .zip(&index.manifest().core_record_aggregates)
    {
        let source = certificate.observation().source();
        if source_token(source) != aggregate.source_identity_digest() {
            return Err(invalid("checkpoint source mismatch"));
        }
        let destination = blobs.join(format!("{}.jsonl", record_blob(aggregate)?));
        if destination.try_exists()? {
            continue;
        }
        let mut stage = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(&blobs)?;
        let mut output = BufWriter::new(stage.as_file_mut());
        let mut cursor = None;
        loop {
            let page = index.stored_core_source_event_page(source, cursor.as_ref(), 64)?;
            for stored in page.items {
                output.write_all(stored.stored_json.encoded_core_record()?)?;
                output.write_all(b"\n")?;
            }
            if page.terminal {
                break;
            }
            cursor = Some(
                page.next_cursor
                    .ok_or_else(|| invalid("checkpoint page made no progress"))?,
            );
        }
        output.flush()?;
        drop(output);
        stage.as_file().sync_all()?;
        fs::rename(stage.path(), &destination)?;
    }
    sync_directory(&blobs)?;
    write_json(
        &metadata_path(root, index.generation_id()),
        &Checkpoint {
            version: 2,
            core_contract: ctx_history_core::core_record_contract_fingerprint(),
            generation: index.generation_id().to_owned(),
            manifest: None,
            records: BTreeMap::new(),
            erased_sessions: erased.clone(),
        },
    )
}

impl GenerationWriter {
    /// Adds one complete generation-owned Core record.
    ///
    /// This is the canonical write API. No provider read locator is accepted,
    /// synthesized, or persisted by this path.
    pub fn add_core_record(&mut self, record: CoreRecord) -> Result<()> {
        let prepared = self.core_record_preparer().prepare(record)?;
        self.add_prepared_core_record(prepared)
    }

    pub(super) fn exclude_erased_record(&mut self, token: &str, session: Uuid) -> Result<bool> {
        if !self.erased_sessions.contains(&session) {
            return Ok(false);
        }
        let pending = self
            .pending
            .get_mut(token)
            .ok_or(IndexError::DocumentSourceNotActive)?;
        pending.staged_documents = pending
            .staged_documents
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        pending.erased_documents = pending
            .erased_documents
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(true)
    }

    pub(super) fn preserve_base_checkpoint(&self) -> Result<()> {
        if let Some(base) = self.base_generation_id() {
            if load(&self.root, base)?.is_none() {
                let index = VerifiedIndex::open_pinned(&self.root)?;
                checkpoint(&self.root, &index, &BTreeSet::new())?;
            }
        }
        Ok(())
    }

    pub(super) fn needs_retained_rebuild(&self) -> Result<bool> {
        let Some(pointer) = &self.active_pointer else {
            if self.root.join(DIRECTORY).exists() {
                return Err(invalid(
                    "retained history has no supported active pointer; history was preserved",
                ));
            }
            return Ok(false);
        };
        let Some(snapshot) = load(&self.root, pointer.active().generation_id())? else {
            if self.root.join(DIRECTORY).exists() {
                return Err(invalid(
                    "portable checkpoint missing; history was preserved",
                ));
            }
            return Ok(false);
        };
        if !snapshot.erased_sessions.is_subset(&self.erased_sessions) {
            return Err(invalid(
                "session exclusion journal is missing retained erasures",
            ));
        }
        Ok(self.base_manifest().is_none() || snapshot.erased_sessions != self.erased_sessions)
    }

    fn restore_retained(mut self) -> Result<CommitReceipt> {
        let generation = self
            .active_pointer
            .as_ref()
            .ok_or_else(|| invalid("no generation to rebuild"))?
            .active()
            .generation_id();
        let snapshot =
            load(&self.root, generation)?.ok_or_else(|| invalid("portable checkpoint missing"))?;
        let manifest = match snapshot.manifest {
            Some(manifest) => std::sync::Arc::new(manifest),
            None => ctx_history_index_format::load_retained_manifest(&self.root, generation)?,
        };
        // Reconstruct in a fresh physical index so erased stored bytes and old
        // lexical terms cannot survive in shared segments or delete postings.
        if self.base_publication.is_some() {
            self.base_publication = None;
            let candidate =
                create_candidate_generation(&self.root, None, self.writer_options.memory_bytes)?;
            self.index = candidate.index;
            self.fields = fields_from_schema(&self.index.schema())?;
            self.candidate_directory_name = Some(candidate.directory_name);
            self.candidate_activation_fence = Some(candidate.activation_fence);
            self.candidate_physical_proof = None;
            self.base_opstamp = self.index.load_metas()?.opstamp;
            self.replacement_memory_bytes = 0;
        }
        for (certificate, aggregate) in manifest
            .sources
            .iter()
            .zip(&manifest.core_record_aggregates)
        {
            let source = certificate.observation().source();
            let token = source_token(source);
            let blob = if snapshot.version == 1 {
                snapshot
                    .records
                    .get(&token)
                    .cloned()
                    .ok_or_else(|| invalid("source missing from portable checkpoint"))?
            } else {
                record_blob(aggregate)?
            };
            let path = self
                .root
                .join(DIRECTORY)
                .join("records")
                .join(format!("{blob}.jsonl"));
            if !fs::symlink_metadata(&path)?.file_type().is_file() {
                return Err(invalid("invalid retained record file"));
            }
            let mut input = BufReader::new(fs::File::open(&path)?);
            let mut accumulator = [0; 32];
            let mut count = 0_u64;
            self.begin_source(source.clone())?;
            loop {
                let mut line = Vec::new();
                let n = input
                    .by_ref()
                    .take(MAX_RECORD + 1)
                    .read_until(b'\n', &mut line)?;
                if n == 0 {
                    break;
                }
                if n as u64 > MAX_RECORD || line.last() != Some(&b'\n') {
                    return Err(invalid("invalid retained record length"));
                }
                let record: CoreRecord = serde_json::from_slice(&line)?;
                if !record.source.exact_descriptor_eq(source) {
                    return Err(invalid("retained record source mismatch"));
                }
                let leaf = core_record_leaf(record.event_id, &line[..line.len() - 1])?;
                accumulate_core_record(
                    &mut accumulator,
                    &core_record_accumulator_leaf(record.event_id, &leaf)?,
                );
                count = count.checked_add(1).ok_or(IndexError::CountOverflow)?;
                self.add_core_record(record)?;
            }
            if accumulator != aggregate.accumulator_bytes()?
                || count != aggregate.indexed_documents()
            {
                return Err(invalid("retained record checksum mismatch"));
            }
            let erased = aggregate.erased_documents();
            let pending = self
                .pending
                .get_mut(&token)
                .ok_or(IndexError::DocumentSourceNotActive)?;
            pending.staged_documents = pending
                .staged_documents
                .checked_add(erased)
                .ok_or(IndexError::CountOverflow)?;
            pending.erased_documents = pending
                .erased_documents
                .checked_add(erased)
                .ok_or(IndexError::CountOverflow)?;
            self.certify_source(certificate.clone())?;
        }
        self.retained_manifest = Some((*manifest).clone());
        self.commit(|_| true)
    }

    pub(super) fn rebuild_retained(self) -> Result<()> {
        self.restore_retained().map(|_| ())
    }

    /// Erases a session and permanently excludes its identity from local capture.
    /// Provider files and independently exported archives are untouched.
    pub fn erase_session(mut self, session: Uuid) -> Result<CommitReceipt> {
        self.preserve_base_checkpoint()?;
        if !self.erased_sessions.contains(&session) {
            let index = VerifiedIndex::open_pinned(&self.root)?;
            if index.session_by_id(session)?.is_none() {
                return Err(invalid("session not found"));
            }
            self.erased_sessions.insert(session);
            write_json(
                &self.root.join(DIRECTORY).join("erased-sessions.json"),
                &self.erased_sessions,
            )?;
        }
        self.restore_retained()
    }
}

pub(super) fn reclaim(root: &Path, pointer: Option<&ActiveGenerationPointer>) -> Result<()> {
    let directory = root.join(DIRECTORY);
    if !directory.try_exists()? {
        return Ok(());
    }
    let generations: BTreeSet<_> = pointer
        .into_iter()
        .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
        .map(|slot| slot.generation_id().to_owned())
        .collect();
    let mut blobs = BTreeSet::new();
    for generation in &generations {
        if let Some(snapshot) = load(root, generation)? {
            if snapshot.version == 1 {
                blobs.extend(snapshot.records.into_values());
                continue;
            }
            let manifest = ctx_history_index_format::load_retained_manifest(root, generation)?;
            for aggregate in &manifest.core_record_aggregates {
                blobs.insert(record_blob(aggregate)?);
            }
        }
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(STAGING_PREFIX) {
            fs::remove_file(entry.path())?;
            continue;
        }
        let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            continue;
        };
        if is_generation_id(stem) && !generations.contains(stem) {
            fs::remove_file(entry.path())?;
        }
    }
    let records = directory.join("records");
    if records.try_exists()? {
        for entry in fs::read_dir(&records)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(STAGING_PREFIX) {
                fs::remove_file(entry.path())?;
                continue;
            }
            let Some(stem) = name.to_str().and_then(|name| name.strip_suffix(".jsonl")) else {
                continue;
            };
            if is_generation_id(stem) && !blobs.contains(stem) {
                fs::remove_file(entry.path())?;
            }
        }
        sync_directory(&records)?;
    }
    sync_directory(&directory)?;
    Ok(())
}

/// Retry physical cleanup after readers release older generations. The writer
/// lock serializes reclamation with refresh and other erasure commands.
pub fn finish_session_erasure(root: impl AsRef<Path>) -> Result<()> {
    let writer = GenerationWriter::open(root, WriterOptions::default())?
        .into_writer()
        .map_err(|_| invalid("generation migration recovery required"))?;
    reclaim(&writer.root, writer.active_pointer.as_ref())?;
    let pointer = writer
        .active_pointer
        .as_ref()
        .ok_or_else(|| invalid("no active generation"))?;
    let allowed: BTreeSet<_> = std::iter::once(pointer.active())
        .chain(pointer.previous())
        .map(|slot| slot.directory().to_owned())
        .collect();
    for entry in fs::read_dir(writer.root.join(INDEX_GENERATIONS_DIRECTORY))? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && !allowed.contains(&entry.file_name().to_string_lossy().into_owned())
        {
            return Err(invalid("session excluded; older history is still held by readers or a retention lease; close them and retry erasure"));
        }
    }
    Ok(())
}
