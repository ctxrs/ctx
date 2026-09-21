//! Atomic publication of bounded plaintext attribution manifests.
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::filesystem;
use crate::{
    ManifestError, SegmentFile, SegmentFileError, SegmentManifest, SegmentWriter, decode_manifest,
    encode_manifest,
};

mod locking;
use locking::PublicationLock;

const ACTIVE_MANIFEST_FILE: &str = "attribution-manifest.ctxm";
const CANDIDATE_PREFIX: &str = ".attribution-manifest-";
const CANDIDATE_SUFFIX: &str = ".candidate";

#[cfg(any(test, feature = "test-support"))]
static POST_ACTIVATION_SYNC_FAILURES: std::sync::OnceLock<
    std::sync::Mutex<std::collections::BTreeSet<PathBuf>>,
> = std::sync::OnceLock::new();

#[derive(Debug, Error)]
pub enum SegmentStoreError {
    #[error("attribution segment store I/O failed while {operation} {path}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("attribution manifest is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("attribution manifest publication compare-and-swap failed")]
    CompareAndSwap {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("attribution manifest publication is already active")]
    PublicationBusy,
    #[error("attribution manifest was activated but its directory sync failed at {path}")]
    ActivationDurabilityUncertain {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    File(#[from] SegmentFileError),
}

pub struct SegmentStore {
    root: PathBuf,
}

impl SegmentStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn active_manifest_path(&self) -> PathBuf {
        self.root.join(ACTIVE_MANIFEST_FILE)
    }

    pub fn load_active(&self) -> Result<Option<SegmentManifest>, SegmentStoreError> {
        let path = self.active_manifest_path();
        match filesystem::open(&path) {
            Ok(file) => read_manifest(file, &path).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io_error("open manifest", &path, source)),
        }
    }

    pub fn stage_manifest(
        &self,
        manifest: &SegmentManifest,
    ) -> Result<ManifestCandidate, SegmentStoreError> {
        filesystem::create_private_directory_all(&self.root)
            .map_err(|source| io_error("create directory", &self.root, source))?;
        let bytes = encode_manifest(manifest)?;
        let path = self.root.join(format!(
            "{CANDIDATE_PREFIX}{}{CANDIDATE_SUFFIX}",
            manifest.generation_id
        ));
        let mut writer = SegmentWriter::create(
            &path,
            manifest.generation_bytes()?,
            0,
            crate::SEGMENT_CHUNK_BYTES,
        )?;
        // Only own the pathname after create_new succeeds.
        let candidate = ManifestCandidate {
            path,
            manifest: manifest.clone(),
        };
        writer
            .write_all(&bytes)
            .map_err(|source| io_error("write candidate", &candidate.path, source))?;
        writer.finish()?;
        if read_manifest(
            filesystem::open(&candidate.path)
                .map_err(|source| io_error("open candidate", &candidate.path, source))?,
            &candidate.path,
        )? != *manifest
        {
            return Err(SegmentStoreError::Corrupt(
                "candidate changed while staging",
            ));
        }
        Ok(candidate)
    }

    pub fn publish_candidate(
        &self,
        candidate: ManifestCandidate,
    ) -> Result<SegmentManifest, SegmentStoreError> {
        if candidate.path.parent() != Some(self.root.as_path()) {
            return Err(SegmentStoreError::Corrupt(
                "candidate belongs to a different store",
            ));
        }
        let lock = PublicationLock::acquire(&self.root)?;
        let verified = read_manifest(
            filesystem::open(&candidate.path)
                .map_err(|source| io_error("open candidate", &candidate.path, source))?,
            &candidate.path,
        )?;
        if verified != candidate.manifest {
            return Err(SegmentStoreError::Corrupt(
                "candidate changed after staging",
            ));
        }
        let current = self.load_active()?;
        let actual = current
            .as_ref()
            .map(|manifest| manifest.generation_id.clone());
        let expected_generation = current
            .as_ref()
            .map_or(0, |manifest| manifest.graph_generation)
            .checked_add(1)
            .ok_or(SegmentStoreError::Corrupt("generation overflow"))?;
        if verified.prior_generation_id != actual
            || verified.graph_generation != expected_generation
        {
            return Err(SegmentStoreError::CompareAndSwap {
                expected: verified.prior_generation_id.clone(),
                actual,
            });
        }
        if verified.predecessor_segments
            != current
                .as_ref()
                .map_or(&[][..], |manifest| manifest.segments.as_slice())
        {
            return Err(SegmentStoreError::Corrupt(
                "predecessor reachability changed",
            ));
        }
        lock.verify_identity()?;
        // Make creation of immutable segment names durable before the active
        // manifest can name them. File content is synced by SegmentWriter.
        filesystem::sync_directory(&self.root)
            .map_err(|source| io_error("sync candidates", &self.root, source))?;
        filesystem::replace(&candidate.path, &self.active_manifest_path())
            .map_err(|source| io_error("publish", &self.active_manifest_path(), source))?;
        self.sync_after_activation().map_err(|source| {
            SegmentStoreError::ActivationDurabilityUncertain {
                path: self.root.clone(),
                source,
            }
        })?;
        Ok(verified)
    }

    fn sync_after_activation(&self) -> io::Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        if POST_ACTIVATION_SYNC_FAILURES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| io::Error::other("sync failure hook poisoned"))?
            .remove(&self.active_manifest_path())
        {
            return Err(io::Error::other(
                "injected post-activation directory sync failure",
            ));
        }
        filesystem::sync_directory(&self.root)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_next_post_activation_sync_for_test(&self) -> Result<(), SegmentStoreError> {
        let inserted = POST_ACTIVATION_SYNC_FAILURES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| SegmentStoreError::Corrupt("sync failure hook poisoned"))?
            .insert(self.active_manifest_path());
        if !inserted {
            return Err(SegmentStoreError::Corrupt(
                "sync failure hook already installed",
            ));
        }
        Ok(())
    }

    /// Removes abandoned candidates. The caller must hold its materializer
    /// writer lock, so it cannot remove another writer's staged generation.
    pub fn cleanup_candidates(&self) -> Result<usize, SegmentStoreError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(source) => return Err(io_error("read directory", &self.root, source)),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry.map_err(|source| io_error("read directory", &self.root, source))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !is_candidate(name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|source| io_error("stat candidate", &entry.path(), source))?;
            if !metadata.is_file() {
                return Err(SegmentStoreError::Corrupt(
                    "candidate is not a regular file",
                ));
            }
            fs::remove_file(entry.path())
                .map_err(|source| io_error("remove candidate", &entry.path(), source))?;
            removed += 1;
        }
        if removed != 0 {
            filesystem::sync_directory(&self.root)
                .map_err(|source| io_error("sync directory", &self.root, source))?;
        }
        Ok(removed)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn active_manifest_path_for_test(&self) -> PathBuf {
        self.active_manifest_path()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn load_active_for_test(&self) -> Result<Option<SegmentManifest>, SegmentStoreError> {
        self.load_active()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_manifest_for_test(
        &self,
        manifest: &SegmentManifest,
    ) -> Result<(), SegmentStoreError> {
        let candidate = self.stage_manifest(manifest)?;
        filesystem::replace(&candidate.path, &self.active_manifest_path()).map_err(|source| {
            io_error(
                "install test manifest",
                &self.active_manifest_path(),
                source,
            )
        })?;
        filesystem::sync_directory(&self.root)
            .map_err(|source| io_error("sync test manifest", &self.root, source))
    }
}

pub struct ManifestCandidate {
    path: PathBuf,
    manifest: SegmentManifest,
}
impl ManifestCandidate {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for ManifestCandidate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_manifest(mut file: File, path: &Path) -> Result<SegmentManifest, SegmentStoreError> {
    let length = file
        .metadata()
        .map_err(|source| io_error("stat manifest", path, source))?
        .len();
    // The maximum encoded manifest is 8 MiB plus 512 checksums and header.
    if !(crate::SEGMENT_HEADER_BYTES..=9 * 1024 * 1024).contains(&length) {
        return Err(SegmentStoreError::Corrupt("manifest length"));
    }
    let mut header = [0_u8; 96];
    file.read_exact(&mut header)
        .map_err(|source| io_error("read manifest header", path, source))?;
    let generation: [u8; 32] = header[12..44]
        .try_into()
        .map_err(|_| SegmentStoreError::Corrupt("manifest generation"))?;
    let mut file = SegmentFile::open_file_region(file, path, 0, length, generation, 0)?;
    if file.plaintext_len() > crate::MAX_MANIFEST_PLAINTEXT_BYTES as u64 {
        return Err(SegmentStoreError::Corrupt("manifest length"));
    }
    let manifest = decode_manifest(&file.read_all()?)?;
    if manifest.generation_bytes()? != generation {
        return Err(SegmentStoreError::Corrupt("manifest generation mismatch"));
    }
    Ok(manifest)
}

fn is_candidate(name: &str) -> bool {
    name.strip_prefix(CANDIDATE_PREFIX)
        .and_then(|value| value.strip_suffix(CANDIDATE_SUFFIX))
        .is_some_and(|generation| crate::decode_generation_id(generation).is_ok())
}
fn io_error(operation: &'static str, path: &Path, source: io::Error) -> SegmentStoreError {
    SegmentStoreError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests;
