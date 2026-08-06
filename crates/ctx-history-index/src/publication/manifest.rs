use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use ctx_history_core::{CORE_RECORD_VERSION, IDENTITY_VERSION};
use serde::{Deserialize, Serialize};
use tantivy::{directory::Directory, IndexMeta, Searcher};
use uuid::Uuid;

use crate::{
    durable_directory::DurableMmapDirectory,
    expected_source_generation_policy_hash,
    identity::{is_generation_id, sha256_hex},
    validate_core_contract_fingerprint, CommitPayload, GenerationManifest, IndexError, Result,
    COMMIT_PAYLOAD_VERSION, GENERATION_MANIFEST_VERSION, LEXICAL_ANALYZER_VERSION,
    LEXICAL_SCHEMA_VERSION, MANIFEST_DIRECTORY, MAX_PUBLICATION_METADATA_BYTES,
};

const MAX_PUBLICATION_METADATA_ENCODED_BYTES: usize =
    MAX_PUBLICATION_METADATA_BYTES.div_ceil(3) * 4;
const MAX_COMMIT_PAYLOAD_BYTES: usize = MAX_PUBLICATION_METADATA_ENCODED_BYTES + 256;

#[derive(Debug)]
pub(crate) struct LoadedPublication {
    pub(crate) generation_id: String,
    pub(crate) manifest: GenerationManifest,
    pub(crate) metadata: Option<Arc<[u8]>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BorrowedCommitPayload<'a> {
    version: u32,
    #[serde(borrow)]
    generation_id: &'a str,
    #[serde(borrow)]
    publication_metadata: Option<&'a str>,
}

#[derive(Debug)]
struct DecodedCommitPayload {
    generation_id: String,
    publication_metadata: Option<Vec<u8>>,
}

pub(crate) fn load_publication_for_metas(
    root: &Path,
    metas: &IndexMeta,
) -> Result<LoadedPublication> {
    let payload = decode_commit_payload(
        metas
            .payload
            .as_deref()
            .ok_or(IndexError::MissingCommitPayload)?,
    )?;
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
    validate_core_contract_fingerprint(&manifest.core_record_contract_fingerprint)?;
    let expected_policy_hash = expected_source_generation_policy_hash()?;
    if manifest.policy_schema_hash != expected_policy_hash {
        return Err(IndexError::GenerationPolicyMismatch {
            expected: expected_policy_hash,
            actual: manifest.policy_schema_hash,
        });
    }
    manifest.validate_contract()?;
    Ok(LoadedPublication {
        generation_id: payload.generation_id,
        manifest,
        metadata: payload
            .publication_metadata
            .map(|metadata| Arc::from(metadata.into_boxed_slice())),
    })
}

pub(crate) fn canonical_commit_payload(
    generation_id: &str,
    publication_metadata: Option<&[u8]>,
) -> Result<String> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let publication_metadata = publication_metadata
        .map(|metadata| {
            if metadata.len() > MAX_PUBLICATION_METADATA_BYTES {
                return Err(IndexError::PublicationMetadataTooLarge {
                    actual: metadata.len(),
                    maximum: MAX_PUBLICATION_METADATA_BYTES,
                });
            }
            Ok(STANDARD_NO_PAD.encode(metadata))
        })
        .transpose()?;
    Ok(serde_json::to_string(&CommitPayload {
        version: COMMIT_PAYLOAD_VERSION,
        generation_id: generation_id.to_owned(),
        publication_metadata,
    })?)
}

fn decode_commit_payload(encoded: &str) -> Result<DecodedCommitPayload> {
    if encoded.len() > MAX_COMMIT_PAYLOAD_BYTES {
        return Err(IndexError::CommitPayloadTooLarge {
            actual: encoded.len(),
            maximum: MAX_COMMIT_PAYLOAD_BYTES,
        });
    }
    let payload: BorrowedCommitPayload<'_> = serde_json::from_str(encoded)?;
    if payload.version != COMMIT_PAYLOAD_VERSION {
        return Err(IndexError::UnsupportedCommitPayload(payload.version));
    }
    if !is_generation_id(payload.generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let publication_metadata_decoded_len = payload
        .publication_metadata
        .map(|metadata| {
            let decoded_len = unpadded_base64_decoded_len(metadata.len())?;
            if decoded_len > MAX_PUBLICATION_METADATA_BYTES {
                return Err(IndexError::PublicationMetadataTooLarge {
                    actual: decoded_len,
                    maximum: MAX_PUBLICATION_METADATA_BYTES,
                });
            }
            Ok(decoded_len)
        })
        .transpose()?;
    if serde_json::to_string(&payload)? != encoded {
        return Err(IndexError::NonCanonicalCommitPayload);
    }
    let publication_metadata = payload
        .publication_metadata
        .zip(publication_metadata_decoded_len)
        .map(|(metadata, decoded_len)| {
            let decoded = STANDARD_NO_PAD
                .decode(metadata)
                .map_err(|_| IndexError::InvalidPublicationMetadataEncoding)?;
            if decoded.len() != decoded_len {
                return Err(IndexError::InvalidPublicationMetadataEncoding);
            }
            Ok(decoded)
        })
        .transpose()?;
    Ok(DecodedCommitPayload {
        generation_id: payload.generation_id.to_owned(),
        publication_metadata,
    })
}

fn unpadded_base64_decoded_len(encoded_len: usize) -> Result<usize> {
    let trailing = match encoded_len % 4 {
        0 => 0,
        2 => 1,
        3 => 2,
        _ => return Err(IndexError::InvalidPublicationMetadataEncoding),
    };
    encoded_len
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|prefix| prefix.checked_add(trailing))
        .ok_or(IndexError::CountOverflow)
}

pub(crate) fn reconcile_commit_error(
    index: &tantivy::Index,
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
        return Ok(metas.opstamp);
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
    Ok(Some(decode_commit_payload(payload)?.generation_id))
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
        let obsolete_integrity_sidecar = is_legacy_generation_integrity_sidecar(file_name);
        let should_remove = immutable_generation.is_some_and(|generation_id| {
            !retained_generation_ids
                .iter()
                .any(|retained| retained == generation_id)
        }) || corrupt_quarantine
            || obsolete_integrity_sidecar;
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

fn is_legacy_generation_integrity_sidecar(file_name: &str) -> bool {
    let Some(generation_uuid) = file_name
        .strip_prefix("generation-")
        .and_then(|name| name.strip_suffix(".integrity.json"))
    else {
        return false;
    };
    generation_uuid.len() == 32
        && generation_uuid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
