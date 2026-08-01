use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

use ctx_history_core::{core_record_contract_fingerprint, CORE_RECORD_VERSION, IDENTITY_VERSION};
use tantivy::{directory::Directory, Index, IndexMeta, ReloadPolicy, Searcher};
use uuid::Uuid;

use crate::{
    current_source_generation_policy_hash,
    durable_directory::DurableMmapDirectory,
    identity::{is_generation_id, sha256_hex},
    CommitPayload, GenerationManifest, IndexError, Result, COMMIT_PAYLOAD_VERSION,
    GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION, LEXICAL_SCHEMA_VERSION,
    MANIFEST_DIRECTORY,
};

use super::verification::verify_searcher;

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
        || manifest.core_record_version != CORE_RECORD_VERSION
    {
        return Err(IndexError::GenerationContractMismatch {
            identity: manifest.identity_version,
            schema: manifest.lexical_schema_version,
            analyzer: manifest.lexical_analyzer_version,
            core_record: manifest.core_record_version,
        });
    }
    let expected_core_fingerprint = core_record_contract_fingerprint();
    if manifest.core_record_contract_fingerprint != expected_core_fingerprint {
        return Err(IndexError::CoreRecordContractMismatch {
            expected: expected_core_fingerprint,
            actual: manifest.core_record_contract_fingerprint,
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
            detail: format!("{commit_error}; candidate payload is invalid: {payload_error}"),
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
                stage: "candidate commit reconciliation",
                detail: format!(
                    "{commit_error}; candidate commit completed but verification failed: \
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
        stage: "candidate commit reconciliation",
        detail: format!(
            "{commit_error}; expected old generation {:?} or candidate generation, found {:?}",
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

    let durable_directory =
        DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    durable_directory.atomic_write(&relative_path, &bytes)?;
    Ok(())
}

pub(crate) fn reclaim_unreferenced_manifests(
    root: &Path,
    retained_generation_ids: &[String],
) -> Result<()> {
    let directory = root.join(MANIFEST_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let mut removed = false;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let immutable_generation = file_name
            .strip_suffix(".json")
            .filter(|generation_id| is_generation_id(generation_id));
        let corrupt_quarantine = file_name
            .strip_prefix('.')
            .and_then(|name| name.split_once(".corrupt-"))
            .is_some_and(|(generation_id, suffix)| {
                is_generation_id(generation_id) && !suffix.is_empty()
            });
        let should_remove = immutable_generation.is_some_and(|generation_id| {
            !retained_generation_ids
                .iter()
                .any(|retained| retained == generation_id)
        }) || corrupt_quarantine;
        if should_remove {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&directory)?;
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
