use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use tantivy::directory::Directory as _;
use tantivy::directory::Lock;
use uuid::Uuid;

use ctx_history_platform::platform_security::{ensure_private_directory, ensure_private_file};

use crate::retention::{
    ensure_generation_read_lease_coordinator, try_generation_id_reclaim_authority,
};
use crate::{
    is_generation_id, manifest_path, sha256_hex, sync_directory, DurableMmapDirectory,
    GenerationError, Result, ACTIVE_GENERATION_POINTER_FILE, GENERATION_WRITER_LOCK_FILE,
    INDEX_GENERATIONS_DIRECTORY, MANIFEST_DIRECTORY,
};

/// Repairs legacy generation control files before any reader captures their
/// immutable identities. The writer lock serializes this metadata-only repair
/// with publication.
pub fn ensure_generation_control_state_private(root: &Path) -> Result<PathBuf> {
    ensure_private_directory(root)?;
    let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let root = directory.root_path().to_path_buf();
    ensure_private_directory(&root.join(MANIFEST_DIRECTORY))?;
    let lock = Lock {
        filepath: GENERATION_WRITER_LOCK_FILE.into(),
        is_blocking: false,
    };
    let _guard = crate::lock::acquire_generation_writer_lock_with_retry(&directory, &lock)?;
    ensure_generation_control_files_private_with_writer_lock_held(&root)?;
    Ok(root)
}

/// Repairs every generation control file while the caller holds the writer
/// lock. This includes flat-delta ancestors that are not named by the active
/// pointer or retention lease directly.
#[doc(hidden)]
pub fn ensure_generation_control_files_private_with_writer_lock_held(root: &Path) -> Result<()> {
    ensure_private_directory(root)?;
    let manifest_directory = root.join(MANIFEST_DIRECTORY);
    ensure_private_directory(&manifest_directory)?;
    ensure_private_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_optional_private_file(&root.join(ACTIVE_GENERATION_POINTER_FILE))?;
    for entry in fs::read_dir(&manifest_directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let is_manifest = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .is_some_and(is_generation_id);
        if !is_manifest {
            continue;
        }
        ensure_private_file(&entry.path())?;
    }
    Ok(())
}

fn ensure_optional_private_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_private_file(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Loads the exact immutable bytes named by a generation manifest digest.
pub fn load_manifest_bytes(root: &Path, generation_id: &str) -> Result<Vec<u8>> {
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    let bytes = match crate::read_root::read_registered_file(root, &relative_path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => fs::read(root.join(&relative_path)).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                GenerationError::MissingManifest(generation_id.to_owned())
            }
            _ => GenerationError::Io(error),
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(GenerationError::MissingManifest(generation_id.to_owned()));
        }
        Err(error) => return Err(GenerationError::Io(error)),
    };
    let actual = sha256_hex(&bytes);
    if actual != generation_id {
        return Err(GenerationError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    Ok(bytes)
}

/// Loads metadata for the exact immutable manifest through the active opened
/// read-root capability when one is installed for this thread.
pub fn load_manifest_metadata(root: &Path, generation_id: &str) -> Result<Metadata> {
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    match crate::read_root::registered_file_metadata(root, &relative_path) {
        Ok(Some(metadata)) => Ok(metadata),
        Ok(None) => {
            fs::symlink_metadata(root.join(relative_path)).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => {
                    GenerationError::MissingManifest(generation_id.to_owned())
                }
                _ => GenerationError::Io(error),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(GenerationError::MissingManifest(generation_id.to_owned()))
        }
        Err(error) => Err(GenerationError::Io(error)),
    }
}

/// Durably publishes already-canonical manifest bytes under their SHA-256 id.
pub fn write_manifest_bytes(root: &Path, generation_id: &str, bytes: &[u8]) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual != generation_id {
        return Err(GenerationError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    let directory = root.join(MANIFEST_DIRECTORY);
    ensure_private_directory(&directory)?;
    let path = manifest_path(root, generation_id);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let existing = fs::read(&path)?;
            if existing == bytes {
                ensure_private_file(&path)?;
                File::open(&path)?.sync_all()?;
                sync_directory(&directory)?;
                return Ok(());
            }
            if existing != bytes {
                quarantine_manifest(&directory, &path, generation_id)?;
            }
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            quarantine_manifest(&directory, &path, generation_id)?;
        }
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "generation manifest path is not a regular file",
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let durable_directory =
        DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
    let relative_path = Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"));
    durable_directory.atomic_write(&relative_path, bytes)?;
    Ok(())
}

fn quarantine_manifest(directory: &Path, path: &Path, generation_id: &str) -> Result<()> {
    let quarantine = directory.join(format!(
        ".{generation_id}.corrupt-{}",
        Uuid::now_v7().simple()
    ));
    fs::rename(path, quarantine)?;
    sync_directory(directory)?;
    Ok(())
}

pub fn reclaim_unreferenced_manifests(
    root: &Path,
    retained_generation_ids: &[String],
) -> Result<()> {
    ensure_generation_read_lease_coordinator(root)?;
    let directory = root.join(MANIFEST_DIRECTORY);
    ensure_private_directory(&directory)?;
    let retained_generation_ids = retained_manifest_closure(root, retained_generation_ids)?;
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
        let unretained_generation = immutable_generation
            .filter(|generation_id| !retained_generation_ids.contains(*generation_id));
        let should_remove =
            unretained_generation.is_some() || corrupt_quarantine || obsolete_integrity_sidecar;
        if should_remove {
            let _reclaim_authority = if let Some(generation_id) = unretained_generation {
                let Some(authority) = try_generation_id_reclaim_authority(root, generation_id)?
                else {
                    continue;
                };
                Some(authority)
            } else {
                None
            };
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        sync_directory(&directory)?;
    }
    Ok(())
}

const MANIFEST_FLAT_DELTA_PREFIX: &[u8] = br#"{"storage_format":"ctx-manifest-flat-delta-v1","#;

#[derive(Deserialize)]
struct ManifestDeltaReference {
    storage_format: String,
    base_generation_id: String,
}

fn retained_manifest_closure(
    root: &Path,
    retained_generation_ids: &[String],
) -> Result<BTreeSet<String>> {
    let mut retained = BTreeSet::new();
    for generation_id in retained_generation_ids {
        if !is_generation_id(generation_id) {
            return Err(GenerationError::InvalidGenerationId);
        }
        if retained.insert(generation_id.clone()) {
            if let Some(base_generation_id) = referenced_base_generation_id(root, generation_id)? {
                if referenced_base_generation_id(root, &base_generation_id)?.is_some() {
                    return Err(GenerationError::InvalidGenerationId);
                }
                retained.insert(base_generation_id);
            }
        }
    }
    Ok(retained)
}

pub(crate) fn referenced_base_generation_id(
    root: &Path,
    generation_id: &str,
) -> Result<Option<String>> {
    let path = manifest_path(root, generation_id);
    let mut file = File::open(&path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => GenerationError::MissingManifest(generation_id.to_owned()),
        _ => GenerationError::Io(error),
    })?;
    let mut prefix = [0_u8; 64];
    let prefix_bytes = file.read(&mut prefix)?;
    let prefix = &prefix[..prefix_bytes];
    if !prefix.starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        return Ok(None);
    }
    let bytes = load_manifest_bytes(root, generation_id)?;
    let reference: ManifestDeltaReference = serde_json::from_slice(&bytes)?;
    if reference.storage_format != "ctx-manifest-flat-delta-v1"
        || !is_generation_id(&reference.base_generation_id)
    {
        return Err(GenerationError::InvalidGenerationId);
    }
    Ok(Some(reference.base_generation_id))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip_preserves_exact_bytes() {
        let root = tempfile::tempdir().unwrap();
        let bytes = br#"{"manifest_version":11,"indexed_documents":0}"#;
        let generation_id = sha256_hex(bytes);

        write_manifest_bytes(root.path(), &generation_id, bytes).unwrap();

        assert_eq!(
            load_manifest_bytes(root.path(), &generation_id).unwrap(),
            bytes
        );
        assert_eq!(
            fs::read(manifest_path(root.path(), &generation_id)).unwrap(),
            bytes
        );
    }

    #[test]
    fn manifest_digest_mismatch_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let expected = "0".repeat(64);

        assert!(matches!(
            write_manifest_bytes(root.path(), &expected, b"not the expected manifest"),
            Err(GenerationError::ManifestDigestMismatch { expected: value, .. }) if value == expected
        ));
    }

    #[cfg(unix)]
    #[test]
    fn matching_manifest_with_permissive_mode_is_repaired_privately() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().unwrap();
        let bytes = b"legacy manifest";
        let generation_id = sha256_hex(bytes);
        write_manifest_bytes(root.path(), &generation_id, bytes).unwrap();
        let path = manifest_path(root.path(), &generation_id);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();

        write_manifest_bytes(root.path(), &generation_id, bytes).unwrap();

        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_manifest_is_replaced_without_changing_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let root = tempfile::tempdir().unwrap();
        let bytes = b"canonical manifest";
        let generation_id = sha256_hex(bytes);
        let directory = root.path().join(MANIFEST_DIRECTORY);
        ensure_private_directory(&directory).unwrap();
        let target = root.path().join("outside");
        fs::write(&target, b"preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o664)).unwrap();
        let path = manifest_path(root.path(), &generation_id);
        symlink(&target, &path).unwrap();

        write_manifest_bytes(root.path(), &generation_id, bytes).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o664
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn reclamation_retains_only_named_manifest_bytes() {
        let root = tempfile::tempdir().unwrap();
        let retained_bytes = b"retained";
        let reclaimed_bytes = b"reclaimed";
        let retained = sha256_hex(retained_bytes);
        let reclaimed = sha256_hex(reclaimed_bytes);
        write_manifest_bytes(root.path(), &retained, retained_bytes).unwrap();
        write_manifest_bytes(root.path(), &reclaimed, reclaimed_bytes).unwrap();
        fs::write(
            root.path()
                .join(MANIFEST_DIRECTORY)
                .join("generation-0123456789abcdef0123456789abcdef.integrity.json"),
            b"obsolete",
        )
        .unwrap();

        reclaim_unreferenced_manifests(root.path(), std::slice::from_ref(&retained)).unwrap();

        assert!(manifest_path(root.path(), &retained).is_file());
        assert!(!manifest_path(root.path(), &reclaimed).exists());
        assert_eq!(
            load_manifest_bytes(root.path(), &retained).unwrap(),
            retained_bytes
        );
    }

    #[test]
    fn reclamation_retains_flat_delta_anchor_without_reading_anchor_body() {
        let root = tempfile::tempdir().unwrap();
        let base_bytes = vec![b'x'; 2 * 1024 * 1024];
        let base = sha256_hex(&base_bytes);
        write_manifest_bytes(root.path(), &base, &base_bytes).unwrap();
        let delta_bytes = format!(
            "{{\"storage_format\":\"ctx-manifest-flat-delta-v1\",\"base_generation_id\":\"{base}\",\"other\":true}}"
        )
        .into_bytes();
        let delta = sha256_hex(&delta_bytes);
        write_manifest_bytes(root.path(), &delta, &delta_bytes).unwrap();
        let reclaimed_bytes = b"reclaimed";
        let reclaimed = sha256_hex(reclaimed_bytes);
        write_manifest_bytes(root.path(), &reclaimed, reclaimed_bytes).unwrap();

        reclaim_unreferenced_manifests(root.path(), std::slice::from_ref(&delta)).unwrap();

        assert!(manifest_path(root.path(), &delta).is_file());
        assert!(manifest_path(root.path(), &base).is_file());
        assert!(!manifest_path(root.path(), &reclaimed).exists());
    }
}
