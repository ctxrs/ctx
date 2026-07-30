use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    path::{Path, PathBuf},
};

use ctx_history_core::{CertifiedSource, SourceKey, StableEntityId, IDENTITY_VERSION};
use tantivy::{
    collector::Count, directory::Directory, schema::IndexRecordOption, DocAddress, Index,
    IndexMeta, ReloadPolicy, Searcher, Term,
};
use uuid::Uuid;

use crate::{
    current_source_generation_policy_hash,
    durable_directory::DurableMmapDirectory,
    fields_from_schema,
    identity::{hex, is_generation_id, register_event_identity, sha256_hex, source_token},
    query, required_field, CommitPayload, GenerationManifest, IndexError, Result,
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
    verify_searcher_structure(searcher, manifest)?;
    for source in &manifest.sources {
        verify_source_document_count(searcher, source)?;
    }
    verify_generation_identities(searcher)?;
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
    let uuid = identity.as_uuid();
    let digest = identity.digest();
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
