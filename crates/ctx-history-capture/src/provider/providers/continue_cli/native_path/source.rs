use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use crate::{
    provider_sources::{observe_ordinary_file, open_ordinary_file_without_following},
    CaptureError, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    decode::{decode_string, decode_u64, validate_and_root, JsonKind, JsonSpan},
    ContinueNativePathError,
};

mod common;
mod index;
mod inventory;
mod spool;

use common::*;
use index::*;
#[cfg(all(test, windows))]
pub(super) use inventory::metadata_identity;
use inventory::observe_inventory;
pub(crate) use spool::ContinuePathIter;
use spool::{ContinuePathSpool, RootMutationWatch};

const SESSION_REVISION_DOMAIN: &[u8] = b"ctx-continue-nativepath-session-v2\0";
const INDEX_REVISION_DOMAIN: &[u8] = b"ctx-continue-nativepath-index-v2\0";
const INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx-continue-nativepath-inventory-v1\0";
const METADATA_TOKEN_DOMAIN: &[u8] = b"ctx-continue-nativepath-metadata-token-v1\0";
const MAX_CONTINUE_SESSION_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_INDEX_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_SESSION_ID_BYTES: usize = 512;
const MAX_CONTINUE_INDEX_STRING_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_DIRECTORY_DEPTH: usize = 128;
const MAX_CONTINUE_INVENTORY_ENTRIES: usize = 8_192;
// Keep optional-index residency bounded by the same fixed corpus ceiling.
const MAX_CONTINUE_INDEX_ENTRIES: usize = MAX_CONTINUE_INVENTORY_ENTRIES;
#[cfg(test)]
const MAX_CONTINUE_PENDING_PAGE_PATHS: usize = 256;
const MAX_SPOOLED_PATH_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueInjectedIoOperation {
    SourceRead,
    SpoolWrite,
}

#[cfg(test)]
struct ContinueInjectedIoFailure {
    operation: ContinueInjectedIoOperation,
    path: PathBuf,
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

#[cfg(test)]
std::thread_local! {
    static CONTINUE_INJECTED_IO_FAILURE:
        std::cell::RefCell<Option<ContinueInjectedIoFailure>> =
            const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_continue_io_failure(
    operation: ContinueInjectedIoOperation,
    path: PathBuf,
    error: io::Error,
) {
    CONTINUE_INJECTED_IO_FAILURE.with(|failure| {
        *failure.borrow_mut() = Some(ContinueInjectedIoFailure {
            operation,
            path,
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        });
    });
}

#[cfg(test)]
pub(crate) fn clear_continue_io_failure() {
    CONTINUE_INJECTED_IO_FAILURE.with(|failure| {
        failure.borrow_mut().take();
    });
}

#[cfg(test)]
fn injected_io_failure(operation: ContinueInjectedIoOperation, path: &Path) -> Option<io::Error> {
    CONTINUE_INJECTED_IO_FAILURE.with(|failure| {
        let mut failure = failure.borrow_mut();
        let configured = failure.as_ref()?;
        if configured.operation != operation || configured.path != path {
            return None;
        }
        let configured = failure.take()?;
        configured.raw_os_error.map_or_else(
            || Some(io::Error::new(configured.kind, configured.message)),
            |raw| Some(io::Error::from_raw_os_error(raw)),
        )
    })
}

#[cfg(not(test))]
fn injected_io_failure(_operation: ContinueInjectedIoOperation, _path: &Path) -> Option<io::Error> {
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExactFileSnapshot {
    path: PathBuf,
    canonical_path: PathBuf,
    ordinary_observation: crate::provider_sources::OrdinaryFileObservation,
    bytes: Box<[u8]>,
    revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueSourceObservation {
    requested_path: PathBuf,
    canonical_path: PathBuf,
    ordinary_observation: crate::provider_sources::OrdinaryFileObservation,
    session_revision: String,
    raw_bytes: u64,
}

impl ContinueSourceObservation {
    pub(crate) fn requested_path(&self) -> &Path {
        &self.requested_path
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn session_revision(&self) -> &str {
        &self.session_revision
    }

    pub(crate) fn raw_bytes(&self) -> u64 {
        self.raw_bytes
    }

    pub(crate) fn revalidate(&self) -> Result<bool, ContinueNativePathError> {
        let snapshot = match read_exact_file(
            &self.requested_path,
            MAX_CONTINUE_SESSION_BYTES,
            SESSION_REVISION_DOMAIN,
        ) {
            Ok(snapshot) => snapshot,
            Err(ContinueNativePathError::SourceIo {
                kind: io::ErrorKind::NotFound,
                ..
            })
            | Err(ContinueNativePathError::SourceTooLarge { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(snapshot.canonical_path == self.canonical_path
            && snapshot.ordinary_observation == self.ordinary_observation
            && snapshot.revision == self.session_revision
            && u64::try_from(snapshot.bytes.len()).ok() == Some(self.raw_bytes))
    }
}

#[derive(Debug)]
pub(crate) struct ContinueSourceSnapshot {
    observation: ContinueSourceObservation,
    pub(super) bytes: Box<[u8]>,
}

impl ContinueSourceSnapshot {
    pub(crate) fn read(path: &Path) -> Result<Self, ContinueNativePathError> {
        let snapshot = read_exact_file(path, MAX_CONTINUE_SESSION_BYTES, SESSION_REVISION_DOMAIN)?;
        let raw_bytes = u64::try_from(snapshot.bytes.len()).map_err(|_| {
            ContinueNativePathError::SourceTooLarge {
                path: path.to_path_buf(),
                limit: MAX_CONTINUE_SESSION_BYTES,
                observed: u64::MAX,
            }
        })?;
        Ok(Self {
            observation: ContinueSourceObservation {
                requested_path: snapshot.path,
                canonical_path: snapshot.canonical_path,
                ordinary_observation: snapshot.ordinary_observation,
                session_revision: snapshot.revision,
                raw_bytes,
            },
            bytes: snapshot.bytes,
        })
    }

    pub(crate) fn observation(&self) -> &ContinueSourceObservation {
        &self.observation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueIndexState {
    Missing,
    Ready,
    Malformed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueIndexObservation {
    path: PathBuf,
    state: ContinueIndexState,
    dependency_revision: String,
}

impl ContinueIndexObservation {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> ContinueIndexState {
        self.state
    }

    pub(crate) fn dependency_revision(&self) -> &str {
        &self.dependency_revision
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ContinueIndexMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(rename = "dateCreated", skip_serializing_if = "Option::is_none")]
    pub(crate) date_created: Option<String>,
    #[serde(rename = "workspaceDirectory", skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_directory: Option<String>,
    #[serde(rename = "messageCount", skip_serializing_if = "Option::is_none")]
    pub(crate) message_count: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct ContinueIndexSnapshot {
    observation: ContinueIndexObservation,
    metadata_entries: Vec<ContinueIndexEntry>,
    #[cfg(test)]
    entry_count: usize,
    #[cfg(test)]
    content_read: bool,
}

#[derive(Debug)]
struct ContinueIndexEntry {
    session_id: String,
    metadata: ContinueIndexMetadata,
}

pub(super) struct ContinueIndexMetadataLookup(Option<ContinueIndexMetadata>);

impl ContinueIndexMetadataLookup {
    pub(super) fn cloned(self) -> Option<ContinueIndexMetadata> {
        self.0
    }
}

impl ContinueIndexSnapshot {
    fn observe(root: &Path) -> Self {
        observe_continue_index(root.join("sessions.json"))
    }

    pub(crate) fn observation(&self) -> &ContinueIndexObservation {
        &self.observation
    }

    pub(super) fn metadata(&self, session_id: &str) -> ContinueIndexMetadataLookup {
        ContinueIndexMetadataLookup(
            self.metadata_entries
                .binary_search_by(|entry| entry.session_id.as_str().cmp(session_id))
                .ok()
                .map(|index| self.metadata_entries[index].metadata.clone()),
        )
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.entry_count
    }

    #[cfg(test)]
    pub(crate) fn resident_metadata_entries(&self) -> usize {
        self.metadata_entries.len()
    }

    pub(crate) fn revalidate(&self) -> bool {
        observe_continue_index(self.observation.path.clone()).observation == self.observation
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContinueRootAuthority {
    root: PathBuf,
    complete: bool,
    #[cfg(test)]
    discovered_sources: usize,
    inventory_entries: usize,
    inventory_digest: String,
    before_token: [u8; 32],
    after_token: [u8; 32],
    mutation_watch: Option<Arc<RootMutationWatch>>,
}

impl ContinueRootAuthority {
    #[cfg(test)]
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    #[cfg(test)]
    pub(crate) fn discovered_sources(&self) -> usize {
        self.discovered_sources
    }

    #[cfg(test)]
    pub(crate) fn inventory_digest(&self) -> &str {
        &self.inventory_digest
    }

    #[cfg(test)]
    pub(crate) fn before_token(&self) -> &[u8; 32] {
        &self.before_token
    }

    #[cfg(test)]
    pub(crate) fn after_token(&self) -> &[u8; 32] {
        &self.after_token
    }

    pub(crate) fn revalidate(&self) -> Result<ContinueRootRevalidation, ContinueNativePathError> {
        if !self.complete {
            return Ok(ContinueRootRevalidation {
                authoritative: false,
                inventory_entries: 0,
                inventory_digest: String::new(),
                before_token: [0; 32],
                after_token: [0; 32],
            });
        }
        let mutated_before = self
            .mutation_watch
            .as_ref()
            .is_some_and(|watch| watch.mutated());
        let current = observe_inventory(&self.root, None, self.mutation_watch.as_deref())?;
        let mutated_after = self
            .mutation_watch
            .as_ref()
            .is_some_and(|watch| watch.mutated());
        Ok(ContinueRootRevalidation {
            authoritative: !mutated_before
                && !mutated_after
                && current.before_token == current.after_token
                && current.before_token == self.before_token
                && current.after_token == self.after_token
                && current.entries == self.inventory_entries
                && current.digest == self.inventory_digest,
            inventory_entries: current.entries,
            inventory_digest: current.digest,
            before_token: current.before_token,
            after_token: current.after_token,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueRootRevalidation {
    pub(crate) authoritative: bool,
    pub(crate) inventory_entries: usize,
    pub(crate) inventory_digest: String,
    pub(crate) before_token: [u8; 32],
    pub(crate) after_token: [u8; 32],
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContinueDiscoveryStats {
    pub(crate) scanned_session_paths: usize,
    pub(crate) inventory_entries: usize,
    pub(crate) index_observations: usize,
    pub(crate) index_content_reads: usize,
    pub(crate) index_entries: usize,
    pub(crate) index_resident_metadata_entries: usize,
    pub(crate) spooled_path_bytes: u64,
    pub(crate) maximum_spool_record_bytes: usize,
    pub(crate) maximum_directory_sort_entries: usize,
    pub(crate) maximum_directory_sort_key_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct ContinueDiscovery {
    spool: ContinuePathSpool,
    index: ContinueIndexSnapshot,
    root_authority: ContinueRootAuthority,
    #[cfg(test)]
    stats: ContinueDiscoveryStats,
}

impl ContinueDiscovery {
    pub(crate) fn paths(&self) -> Result<ContinuePathIter, ContinueNativePathError> {
        self.spool.iter()
    }

    pub(crate) fn index(&self) -> &ContinueIndexSnapshot {
        &self.index
    }

    pub(crate) fn root_authority(&self) -> &ContinueRootAuthority {
        &self.root_authority
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ContinueDiscoveryStats {
        self.stats
    }
}

pub(crate) fn discover_continue_root(
    root: &Path,
) -> Result<ContinueDiscovery, ContinueNativePathError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| source_access(root, error))?;
    let index_root = if metadata.file_type().is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };
    let mut spool = ContinuePathSpool::new(root)?;
    let mutation_watch = Arc::new(RootMutationWatch::new(root)?);
    let inventory = observe_inventory(root, Some(&mut spool), Some(&mutation_watch))?;
    if inventory.before_token != inventory.after_token || mutation_watch.mutated() {
        return Err(ContinueNativePathError::SourceChanged {
            path: root.to_path_buf(),
        });
    }
    let canonical_root = fs::canonicalize(root).map_err(|error| source_access(root, error))?;
    let index = ContinueIndexSnapshot::observe(index_root);
    #[cfg(test)]
    let stats = ContinueDiscoveryStats {
        scanned_session_paths: spool.entries,
        inventory_entries: inventory.entries,
        index_observations: 1,
        index_content_reads: usize::from(index.content_read),
        index_entries: index.entry_count(),
        index_resident_metadata_entries: index.resident_metadata_entries(),
        spooled_path_bytes: spool.bytes,
        maximum_spool_record_bytes: spool.maximum_record_bytes,
        maximum_directory_sort_entries: inventory.maximum_directory_sort_entries,
        maximum_directory_sort_key_bytes: inventory.maximum_directory_sort_key_bytes,
    };
    Ok(ContinueDiscovery {
        root_authority: ContinueRootAuthority {
            root: canonical_root,
            complete: true,
            #[cfg(test)]
            discovered_sources: spool.entries,
            inventory_entries: inventory.entries,
            inventory_digest: inventory.digest,
            before_token: inventory.before_token,
            after_token: inventory.after_token,
            mutation_watch: Some(mutation_watch),
        },
        spool,
        index,
        #[cfg(test)]
        stats,
    })
}

#[cfg(test)]
pub(crate) fn observe_continue_pending_paths(
    root: &Path,
    source_paths: impl IntoIterator<Item = PathBuf>,
) -> Result<ContinueDiscovery, ContinueNativePathError> {
    let metadata = fs::symlink_metadata(root).map_err(|error| source_access(root, error))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ContinueNativePathError::SourceAccess {
            path: root.to_path_buf(),
            message: "pending Continue observations require a regular directory root".to_owned(),
        });
    }
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(root)
        .map_err(|error| capture_source_error(root, "validate Continue root parents", error))?;
    let canonical_root = fs::canonicalize(root).map_err(|error| source_access(root, error))?;
    let mut bounded_paths = Vec::with_capacity(MAX_CONTINUE_PENDING_PAGE_PATHS);
    for path in source_paths {
        if bounded_paths.len() >= MAX_CONTINUE_PENDING_PAGE_PATHS {
            return Err(ContinueNativePathError::PendingPageTooLarge {
                limit: MAX_CONTINUE_PENDING_PAGE_PATHS,
                observed: bounded_paths.len().saturating_add(1),
            });
        }
        bounded_paths.push(path);
    }
    let mut canonical_paths = Vec::with_capacity(bounded_paths.len());
    let mut spool = ContinuePathSpool::new(root)?;
    for path in bounded_paths {
        let canonical_path =
            fs::canonicalize(&path).map_err(|error| source_access(&path, error))?;
        if canonical_path.parent() != Some(canonical_root.as_path()) {
            return Err(ContinueNativePathError::SourceAccess {
                path,
                message:
                    "pending Continue observation must be a direct child of its canonical root"
                        .to_owned(),
            });
        }
        if !super::super::continue_session_json_path(&canonical_path) {
            return Err(ContinueNativePathError::SourceAccess {
                path,
                message: "pending Continue observation is not a session JSON document".to_owned(),
            });
        }
        crate::common::io::ensure_regular_provider_transcript_file(&path).map_err(|error| {
            capture_source_error(&path, "validate pending Continue source", error)
        })?;
        canonical_paths.push(canonical_path);
    }
    canonical_paths.sort();
    canonical_paths.dedup();
    for path in canonical_paths {
        spool.push(&path)?;
    }
    let index = ContinueIndexSnapshot::observe(&canonical_root);
    let stats = ContinueDiscoveryStats {
        scanned_session_paths: spool.entries,
        inventory_entries: 0,
        index_observations: 1,
        index_content_reads: usize::from(index.content_read),
        index_entries: index.entry_count(),
        index_resident_metadata_entries: index.resident_metadata_entries(),
        spooled_path_bytes: spool.bytes,
        maximum_spool_record_bytes: spool.maximum_record_bytes,
        maximum_directory_sort_entries: 0,
        maximum_directory_sort_key_bytes: 0,
    };
    Ok(ContinueDiscovery {
        root_authority: ContinueRootAuthority {
            root: canonical_root,
            complete: false,
            discovered_sources: spool.entries,
            inventory_entries: 0,
            inventory_digest: String::new(),
            before_token: [0; 32],
            after_token: [0; 32],
            mutation_watch: None,
        },
        spool,
        index,
        stats,
    })
}
