use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    provider_sources::{observe_ordinary_file, open_ordinary_file_without_following},
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    decode::{decode_string, decode_u64, validate_and_root, JsonKind, JsonSpan},
    ContinueNativePathError,
};

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
            Err(ContinueNativePathError::SourceAccess { .. })
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
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(root).map_err(|error| {
        ContinueNativePathError::SourceAccess {
            path: root.to_path_buf(),
            message: error.to_string(),
        }
    })?;
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
            ContinueNativePathError::SourceAccess {
                path: path.clone(),
                message: error.to_string(),
            }
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

#[derive(Debug)]
struct ContinuePathSpool {
    file: File,
    entries: usize,
    bytes: u64,
    maximum_record_bytes: usize,
}

impl ContinuePathSpool {
    fn new(root: &Path) -> Result<Self, ContinueNativePathError> {
        let file = tempfile::tempfile().map_err(|error| source_access(root, error))?;
        Ok(Self {
            file,
            entries: 0,
            bytes: 0,
            maximum_record_bytes: 0,
        })
    }

    fn push(&mut self, path: &Path) -> Result<(), ContinueNativePathError> {
        let encoded = encode_path(path).ok_or_else(|| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue source path cannot be represented in the private path spool"
                .to_owned(),
        })?;
        if encoded.len() > MAX_SPOOLED_PATH_BYTES {
            return Err(ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue source path exceeds the private path-spool record limit"
                    .to_owned(),
            });
        }
        let length =
            u32::try_from(encoded.len()).map_err(|_| ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue source path exceeds u32 path-spool encoding".to_owned(),
            })?;
        self.file
            .write_all(&length.to_le_bytes())
            .and_then(|_| self.file.write_all(&encoded))
            .map_err(|error| source_access(path, error))?;
        self.entries = self.entries.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(u64::from(length).saturating_add(4));
        self.maximum_record_bytes = self.maximum_record_bytes.max(encoded.len());
        Ok(())
    }

    fn iter(&self) -> Result<ContinuePathIter, ContinueNativePathError> {
        let file = self
            .file
            .try_clone()
            .map_err(|error| source_access(Path::new("<continue-path-spool>"), error))?;
        Ok(ContinuePathIter {
            file,
            offset: 0,
            remaining: self.entries,
        })
    }
}

pub(crate) struct ContinuePathIter {
    file: File,
    offset: u64,
    remaining: usize,
}

impl Iterator for ContinuePathIter {
    type Item = Result<PathBuf, ContinueNativePathError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let mut length = [0_u8; 4];
        if let Err(error) = read_exact_at(&self.file, &mut length, self.offset) {
            self.remaining = 0;
            return Some(Err(source_access(
                Path::new("<continue-path-spool>"),
                error,
            )));
        }
        self.offset = self.offset.saturating_add(4);
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_SPOOLED_PATH_BYTES {
            self.remaining = 0;
            return Some(Err(ContinueNativePathError::SourceAccess {
                path: PathBuf::from("<continue-path-spool>"),
                message: "private Continue path spool contains an oversized record".to_owned(),
            }));
        }
        let mut encoded = vec![0_u8; length];
        if let Err(error) = read_exact_at(&self.file, &mut encoded, self.offset) {
            self.remaining = 0;
            return Some(Err(source_access(
                Path::new("<continue-path-spool>"),
                error,
            )));
        }
        self.offset = self
            .offset
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
        self.remaining = self.remaining.saturating_sub(1);
        Some(
            decode_path(encoded).ok_or_else(|| ContinueNativePathError::SourceAccess {
                path: PathBuf::from("<continue-path-spool>"),
                message: "private Continue path spool contains an invalid path record".to_owned(),
            }),
        )
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    let mut remaining = buffer;
    while !remaining.is_empty() {
        let read = file.read_at(remaining, offset)?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        remaining = &mut remaining[read..];
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;

    let mut remaining = buffer;
    while !remaining.is_empty() {
        let read = file.seek_read(remaining, offset)?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        remaining = &mut remaining[read..];
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}

#[cfg(target_os = "linux")]
struct RootMutationWatch {
    file: Mutex<File>,
    mutated: AtomicBool,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for RootMutationWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootMutationWatch")
            .field("mutated", &self.mutated.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl RootMutationWatch {
    fn new(root: &Path) -> Result<Self, ContinueNativePathError> {
        use std::os::fd::FromRawFd;

        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if descriptor < 0 {
            return Err(source_access(root, std::io::Error::last_os_error()));
        }
        let watch = Self {
            file: Mutex::new(unsafe { File::from_raw_fd(descriptor) }),
            mutated: AtomicBool::new(false),
        };
        watch.add(root)?;
        Ok(watch)
    }

    fn add(&self, path: &Path) -> Result<(), ContinueNativePathError> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, os::unix::io::AsRawFd};

        let path_bytes = path.as_os_str().as_bytes();
        let path = CString::new(path_bytes).map_err(|_| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue inventory path contains an interior NUL".to_owned(),
        })?;
        let guard = self
            .file
            .lock()
            .map_err(|_| ContinueNativePathError::SourceAccess {
                path: PathBuf::from("<continue-inventory-watch>"),
                message: "Continue inventory mutation watch is poisoned".to_owned(),
            })?;
        let mask = libc::IN_ATTRIB
            | libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_DELETE_SELF
            | libc::IN_MODIFY
            | libc::IN_MOVE_SELF
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO;
        let result = unsafe { libc::inotify_add_watch(guard.as_raw_fd(), path.as_ptr(), mask) };
        if result < 0 {
            return Err(source_access(
                Path::new(path.to_str().unwrap_or("<continue-inventory-watch>")),
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    fn mutated(&self) -> bool {
        if self.mutated.load(Ordering::Acquire) {
            return true;
        }
        let Ok(mut file) = self.file.lock() else {
            self.mutated.store(true, Ordering::Release);
            return true;
        };
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {
                    self.mutated.store(true, Ordering::Release);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.mutated.store(true, Ordering::Release);
                    break;
                }
            }
        }
        self.mutated.load(Ordering::Acquire)
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct RootMutationWatch;

#[cfg(not(target_os = "linux"))]
impl RootMutationWatch {
    fn new(_root: &Path) -> Result<Self, ContinueNativePathError> {
        Ok(Self)
    }

    fn add(&self, _path: &Path) -> Result<(), ContinueNativePathError> {
        Ok(())
    }

    fn mutated(&self) -> bool {
        false
    }
}

struct InventoryObservation {
    entries: usize,
    digest: String,
    before_token: [u8; 32],
    after_token: [u8; 32],
    #[cfg(test)]
    maximum_directory_sort_entries: usize,
    #[cfg(test)]
    maximum_directory_sort_key_bytes: usize,
}

#[derive(Default)]
struct InventoryScratch {
    #[cfg(test)]
    maximum_directory_sort_entries: usize,
    #[cfg(test)]
    maximum_directory_sort_key_bytes: usize,
}

struct DirectoryChild {
    order_key: Vec<u8>,
    path: PathBuf,
}

fn observe_inventory(
    root: &Path,
    mut spool: Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
) -> Result<InventoryObservation, ContinueNativePathError> {
    let before_token = metadata_token(root)?;
    let mut hasher = Sha256::new();
    hasher.update(INVENTORY_DIGEST_DOMAIN);
    let mut entries = 0_usize;
    let mut scratch = InventoryScratch::default();
    visit_inventory(
        root,
        root,
        0,
        &mut entries,
        &mut hasher,
        &mut spool,
        mutation_watch,
        &mut scratch,
    )?;
    let after_token = metadata_token(root)?;
    Ok(InventoryObservation {
        entries,
        digest: digest_to_hex(hasher.finalize()),
        before_token,
        after_token,
        #[cfg(test)]
        maximum_directory_sort_entries: scratch.maximum_directory_sort_entries,
        #[cfg(test)]
        maximum_directory_sort_key_bytes: scratch.maximum_directory_sort_key_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn visit_inventory(
    root: &Path,
    path: &Path,
    depth: usize,
    entries: &mut usize,
    hasher: &mut Sha256,
    spool: &mut Option<&mut ContinuePathSpool>,
    mutation_watch: Option<&RootMutationWatch>,
    scratch: &mut InventoryScratch,
) -> Result<(), ContinueNativePathError> {
    if depth > MAX_CONTINUE_DIRECTORY_DEPTH {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue session tree exceeds the supported depth".to_owned(),
        });
    }
    if *entries >= MAX_CONTINUE_INVENTORY_ENTRIES {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue session tree exceeds the supported inventory limit".to_owned(),
        });
    }
    *entries = entries.saturating_add(1);
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "symlinked Continue inventory entries are rejected".to_owned(),
        });
    }
    if metadata.file_type().is_dir() {
        if let Some(watch) = mutation_watch {
            watch.add(path)?;
        }
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    hash_inventory_entry(hasher, relative, &metadata, path)?;

    if metadata.file_type().is_file() {
        if super::super::continue_session_json_path(path) {
            if let Some(spool) = spool.as_deref_mut() {
                spool.push(path)?;
            }
        }
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        return Ok(());
    }

    let mut children = Vec::new();
    #[cfg(test)]
    let mut sort_key_bytes = 0_usize;
    for child in fs::read_dir(path).map_err(|error| source_access(path, error))? {
        let child = child.map_err(|error| source_access(path, error))?;
        if *entries + children.len() >= MAX_CONTINUE_INVENTORY_ENTRIES {
            return Err(ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue session tree exceeds the supported inventory limit".to_owned(),
            });
        }
        let candidate = DirectoryChild {
            order_key: os_order_key(&child.file_name()),
            path: child.path(),
        };
        #[cfg(test)]
        {
            sort_key_bytes = sort_key_bytes.saturating_add(candidate.order_key.len());
        }
        children.push(candidate);
    }
    children.sort_by(|left, right| left.order_key.cmp(&right.order_key));
    #[cfg(test)]
    {
        scratch.maximum_directory_sort_entries =
            scratch.maximum_directory_sort_entries.max(children.len());
        scratch.maximum_directory_sort_key_bytes =
            scratch.maximum_directory_sort_key_bytes.max(sort_key_bytes);
    }
    for child in children {
        visit_inventory(
            root,
            &child.path,
            depth.saturating_add(1),
            entries,
            hasher,
            spool,
            mutation_watch,
            scratch,
        )?;
    }
    Ok(())
}

fn hash_inventory_entry(
    hasher: &mut Sha256,
    relative: &Path,
    metadata: &Metadata,
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    let encoded = encode_path(relative).ok_or_else(|| ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "Continue inventory path cannot be encoded".to_owned(),
    })?;
    hasher.update((encoded.len() as u64).to_le_bytes());
    hasher.update(encoded);
    let kind = if metadata.file_type().is_file() {
        b'f'
    } else if metadata.file_type().is_dir() {
        b'd'
    } else {
        b'o'
    };
    hasher.update([kind]);
    if metadata.file_type().is_file() {
        let observation =
            observe_ordinary_file(path).map_err(|error| source_access(path, error))?;
        hasher.update(observation.len().to_le_bytes());
        hasher.update(observation.token());
    } else {
        hasher.update(metadata_identity(metadata, path)?);
    }
    Ok(())
}

fn metadata_token(path: &Path) -> Result<[u8; 32], ContinueNativePathError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "symlinked Continue inventory roots are rejected".to_owned(),
        });
    }
    if metadata.file_type().is_file() {
        return observe_ordinary_file(path)
            .map(|observation| *observation.token())
            .map_err(|error| source_access(path, error));
    }
    let mut hasher = Sha256::new();
    hasher.update(METADATA_TOKEN_DOMAIN);
    hasher.update(metadata_identity(&metadata, path)?);
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
pub(super) fn metadata_identity(
    metadata: &Metadata,
    _path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    use std::os::unix::fs::MetadataExt;

    let mut bytes = Vec::with_capacity(13 * 8);
    for value in [
        metadata.dev(),
        metadata.ino(),
        u64::from(metadata.mode()),
        metadata.nlink(),
        u64::from(metadata.uid()),
        u64::from(metadata.gid()),
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

#[cfg(windows)]
pub(super) fn metadata_identity(
    metadata: &Metadata,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    windows_metadata_identity(path, metadata).map_err(|error| source_access(path, error))
}

#[cfg(windows)]
fn windows_metadata_identity(path: &Path, metadata: &Metadata) -> std::io::Result<Vec<u8>> {
    use std::{
        fs::OpenOptions,
        mem::size_of,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if metadata.file_type().is_dir() {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)?;
    let current = file.metadata()?;
    if current.file_type().is_file() != metadata.file_type().is_file()
        || current.file_type().is_dir() != metadata.file_type().is_dir()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Continue inventory entry changed kind during identity observation",
        ));
    }

    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "reparse-point Continue inventory entries are rejected",
        ));
    }
    let mut id = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut bytes = Vec::with_capacity(68);
    bytes.extend_from_slice(&id.VolumeSerialNumber.to_le_bytes());
    bytes.extend_from_slice(&id.FileId.Identifier);
    bytes.extend_from_slice(&basic.ChangeTime.to_le_bytes());
    bytes.extend_from_slice(&basic.LastWriteTime.to_le_bytes());
    bytes.extend_from_slice(&u64::from(basic.FileAttributes).to_le_bytes());
    bytes.extend_from_slice(&current.len().to_le_bytes());
    Ok(bytes)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn metadata_identity(
    _metadata: &Metadata,
    path: &Path,
) -> Result<Vec<u8>, ContinueNativePathError> {
    Err(ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "exact Continue root authority is unavailable without stable file identity"
            .to_owned(),
    })
}

fn observe_continue_index(path: PathBuf) -> ContinueIndexSnapshot {
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            index_without_entries(path, ContinueIndexState::Missing, b"missing", false)
        }
        Err(error) => index_without_entries(
            path,
            ContinueIndexState::Unavailable,
            format!("io:{:?}", error.kind()).as_bytes(),
            false,
        ),
        Ok(metadata) if !metadata.file_type().is_file() => {
            index_without_entries(path, ContinueIndexState::Unavailable, b"not-regular", false)
        }
        Ok(_) => match read_exact_file(&path, MAX_CONTINUE_INDEX_BYTES, INDEX_REVISION_DOMAIN) {
            Ok(snapshot) => match parse_index_entries(&snapshot.bytes) {
                Ok(metadata_entries) => ContinueIndexSnapshot {
                    observation: ContinueIndexObservation {
                        path,
                        state: ContinueIndexState::Ready,
                        dependency_revision: snapshot.revision,
                    },
                    #[cfg(test)]
                    entry_count: metadata_entries.len(),
                    metadata_entries,
                    #[cfg(test)]
                    content_read: true,
                },
                Err(_) => ContinueIndexSnapshot {
                    observation: ContinueIndexObservation {
                        path,
                        state: ContinueIndexState::Malformed,
                        dependency_revision: snapshot.revision,
                    },
                    metadata_entries: Vec::new(),
                    #[cfg(test)]
                    entry_count: 0,
                    #[cfg(test)]
                    content_read: true,
                },
            },
            Err(error) => index_without_entries(
                path,
                ContinueIndexState::Unavailable,
                error.to_string().as_bytes(),
                false,
            ),
        },
    }
}

fn parse_index_entries(
    bytes: &[u8],
) -> Result<Vec<ContinueIndexEntry>, Box<dyn std::error::Error>> {
    // Parse the bounded index once, then sort the retained metadata for binary
    // search by session identity during source preparation.
    let root = validate_and_root(bytes)?;
    if root.kind() != JsonKind::Array {
        return Err("Continue index is not an array".into());
    }
    let mut entries = Vec::new();
    for entry in root.as_array()? {
        let entry = entry?;
        if entry.kind() != JsonKind::Object {
            continue;
        }
        if let Some((session_id, metadata)) = parse_index_entry(entry)? {
            if entries.len() >= MAX_CONTINUE_INDEX_ENTRIES {
                return Err("Continue index exceeds the supported entry limit".into());
            }
            entries.push(ContinueIndexEntry {
                session_id,
                metadata,
            });
        }
    }
    entries.sort_unstable_by(|left, right| left.session_id.cmp(&right.session_id));
    if entries
        .windows(2)
        .any(|pair| pair[0].session_id == pair[1].session_id)
    {
        return Err("Continue index contains duplicate session IDs".into());
    }
    Ok(entries)
}

fn parse_index_entry(
    entry: JsonSpan<'_>,
) -> Result<Option<(String, ContinueIndexMetadata)>, Box<dyn std::error::Error>> {
    let mut session_id = None;
    let mut title = None;
    let mut date_created = None;
    let mut workspace_directory = None;
    let mut message_count = None;
    for field in entry.as_object()? {
        let (key, value) = field?;
        if key.is("sessionId") {
            session_id = decode_string(value, MAX_CONTINUE_SESSION_ID_BYTES)?;
        } else if key.is("title") {
            title = decode_string(value, MAX_CONTINUE_INDEX_STRING_BYTES)?;
        } else if key.is("dateCreated") {
            date_created = decode_string(value, 128)?;
        } else if key.is("workspaceDirectory") {
            workspace_directory = decode_string(value, MAX_CONTINUE_INDEX_STRING_BYTES)?;
        } else if key.is("messageCount") {
            message_count = decode_u64(value);
        }
        // Unknown and result-like index fields remain borrowed spans and are
        // discarded here without constructing Value or String payloads.
    }
    Ok(session_id
        .filter(|value| valid_identity_string(value, MAX_CONTINUE_SESSION_ID_BYTES))
        .map(|session_id| {
            (
                session_id,
                ContinueIndexMetadata {
                    title: title.filter(|value| valid_metadata_string(value)),
                    date_created,
                    workspace_directory: workspace_directory
                        .filter(|value| valid_metadata_string(value)),
                    message_count,
                },
            )
        }))
}

fn index_without_entries(
    path: PathBuf,
    state: ContinueIndexState,
    revision_evidence: &[u8],
    _content_read: bool,
) -> ContinueIndexSnapshot {
    ContinueIndexSnapshot {
        observation: ContinueIndexObservation {
            path,
            state,
            dependency_revision: sha256_hex(INDEX_REVISION_DOMAIN, revision_evidence),
        },
        metadata_entries: Vec::new(),
        #[cfg(test)]
        entry_count: 0,
        #[cfg(test)]
        content_read: _content_read,
    }
}

fn read_exact_file(
    path: &Path,
    max_bytes: usize,
    revision_domain: &[u8],
) -> Result<ExactFileSnapshot, ContinueNativePathError> {
    let ordinary_before =
        observe_ordinary_file(path).map_err(|error| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if ordinary_before.len() > max_bytes as u64 {
        return Err(ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: ordinary_before.len(),
        });
    }
    let canonical_before = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
    let file = open_ordinary_file_without_following(path).map_err(|error| {
        ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(ordinary_before.len())
            .unwrap_or(max_bytes)
            .min(max_bytes),
    );
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| source_access(path, error))?;
    if bytes.len() > max_bytes {
        return Err(ContinueNativePathError::SourceTooLarge {
            path: path.to_path_buf(),
            limit: max_bytes,
            observed: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        });
    }
    let ordinary_after =
        observe_ordinary_file(path).map_err(|error| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let canonical_after = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
    if ordinary_before != ordinary_after
        || canonical_before != canonical_after
        || ordinary_after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    {
        return Err(ContinueNativePathError::SourceChanged {
            path: path.to_path_buf(),
        });
    }
    Ok(ExactFileSnapshot {
        path: path.to_path_buf(),
        canonical_path: canonical_after,
        ordinary_observation: ordinary_after,
        revision: sha256_hex(revision_domain, &bytes),
        bytes: bytes.into_boxed_slice(),
    })
}

fn valid_identity_string(value: &str, maximum_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_bytes && !value.chars().any(char::is_control)
}

fn valid_metadata_string(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(char::is_control)
}

fn sha256_hex(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    digest_to_hex(hasher.finalize())
}

fn digest_to_hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn source_access(path: &Path, error: impl std::fmt::Display) -> ContinueNativePathError {
    ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

fn os_order_key(name: &OsStr) -> Vec<u8> {
    encode_os_string(name).unwrap_or_default()
}

fn encode_path(path: &Path) -> Option<Vec<u8>> {
    encode_os_string(path.as_os_str())
}

#[cfg(unix)]
fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    Some(value.as_bytes().to_vec())
}

#[cfg(unix)]
fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    Some(PathBuf::from(OsString::from_vec(value)))
}

#[cfg(windows)]
fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;

    Some(
        value
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[cfg(windows)]
fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    if value.len() % 2 != 0 {
        return None;
    }
    let units = value
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();
    Some(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
fn encode_os_string(value: &OsStr) -> Option<Vec<u8>> {
    value.to_str().map(|value| value.as_bytes().to_vec())
}

#[cfg(not(any(unix, windows)))]
fn decode_path(value: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(value).ok().map(PathBuf::from)
}
