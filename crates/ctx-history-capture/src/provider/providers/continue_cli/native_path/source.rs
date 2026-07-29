use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
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
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceRoot,
    },
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
use inventory::{observe_inventory, opened_file_token};
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

#[derive(Debug)]
struct ExactFileSnapshot {
    path: PathBuf,
    canonical_path: PathBuf,
    file_token: [u8; 32],
    bytes: Box<[u8]>,
    revision: String,
    opened: Arc<OpenedProviderSourceFile>,
}

#[derive(Debug, Clone)]
pub(crate) struct ContinueSourceObservation {
    requested_path: PathBuf,
    canonical_path: PathBuf,
    file_token: [u8; 32],
    session_revision: String,
    raw_bytes: u64,
    opened: Arc<OpenedProviderSourceFile>,
}

impl PartialEq for ContinueSourceObservation {
    fn eq(&self, other: &Self) -> bool {
        self.requested_path == other.requested_path
            && self.canonical_path == other.canonical_path
            && self.file_token == other.file_token
            && self.session_revision == other.session_revision
            && self.raw_bytes == other.raw_bytes
    }
}

impl Eq for ContinueSourceObservation {}

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
        let snapshot = match read_opened_exact_file(
            &self.requested_path,
            self.opened.clone(),
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
            && snapshot.file_token == self.file_token
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
    fn read_opened(
        path: &Path,
        opened: OpenedProviderSourceFile,
    ) -> Result<Self, ContinueNativePathError> {
        let snapshot = read_opened_exact_file(
            path,
            Arc::new(opened),
            MAX_CONTINUE_SESSION_BYTES,
            SESSION_REVISION_DOMAIN,
        )?;
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
                file_token: snapshot.file_token,
                session_revision: snapshot.revision,
                raw_bytes,
                opened: snapshot.opened,
            },
            bytes: snapshot.bytes,
        })
    }

    pub(crate) fn observation(&self) -> &ContinueSourceObservation {
        &self.observation
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
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
    authority: ProviderSourceRoot,
    relative_path: PathBuf,
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
    fn observe(authority: &ProviderSourceRoot, relative_path: PathBuf) -> Self {
        observe_continue_index(authority, relative_path)
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
        observe_continue_index(&self.authority, self.relative_path.clone()).observation
            == self.observation
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContinueRootAuthority {
    root: PathBuf,
    authority: ProviderSourceRoot,
    selected_relative: PathBuf,
    selected_file: bool,
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
        let current = match observe_inventory(
            &self.authority,
            &self.selected_relative,
            self.selected_file,
            None,
            self.mutation_watch.as_deref(),
        ) {
            Ok(current) => current,
            Err(ContinueNativePathError::SourceChanged { .. })
            | Err(ContinueNativePathError::SourceIo {
                kind: io::ErrorKind::NotFound,
                ..
            })
            | Err(ContinueNativePathError::SourceAccess { .. }) => {
                return Ok(ContinueRootRevalidation {
                    authoritative: false,
                    inventory_entries: 0,
                    inventory_digest: String::new(),
                    before_token: [0; 32],
                    after_token: [0; 32],
                });
            }
            Err(error) => return Err(error),
        };
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

    pub(crate) fn open_source(
        &self,
        path: &Path,
    ) -> Result<ContinueSourceSnapshot, ContinueNativePathError> {
        let relative = path
            .strip_prefix(self.root_authority.authority.named_path())
            .map_err(|_| ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue source escaped its retained root authority".to_owned(),
            })?;
        let opened = self
            .root_authority
            .authority
            .open_file(relative)
            .map_err(|error| capture_source_error(path, "open Continue source", error))?;
        ContinueSourceSnapshot::read_opened(path, opened)
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> ContinueDiscoveryStats {
        self.stats
    }
}

pub(crate) fn discover_continue_root(
    root: &Path,
) -> Result<ContinueDiscovery, ContinueNativePathError> {
    let requested_root = normalized_continue_authority_path(root)?;
    let opened = open_provider_source_path(&requested_root)
        .map_err(|error| capture_source_error(root, "open Continue root", error))?;
    let (authority, selected_relative, selected_path, selected_file) = match opened {
        OpenedProviderSourcePath::Directory(directory) => {
            let authority = directory.authority_root();
            let selected_path = authority.named_path().to_path_buf();
            (authority, PathBuf::new(), selected_path, false)
        }
        OpenedProviderSourcePath::File(file) => {
            let name = requested_root.file_name().ok_or_else(|| {
                ContinueNativePathError::SourceAccess {
                    path: root.to_path_buf(),
                    message: "Continue file roots require a named parent entry".to_owned(),
                }
            })?;
            let parent =
                requested_root
                    .parent()
                    .ok_or_else(|| ContinueNativePathError::SourceAccess {
                        path: root.to_path_buf(),
                        message: "Continue file roots require a parent directory".to_owned(),
                    })?;
            let authority = ProviderSourceRoot::open(parent)
                .map_err(|error| capture_source_error(root, "open Continue root parent", error))?;
            let selected_relative = PathBuf::from(name);
            let selected_path = authority.named_path().join(&selected_relative);
            authority
                .open_file(&selected_relative)
                .and_then(|opened| opened.revalidate())
                .map_err(|error| capture_source_error(root, "bind Continue file root", error))?;
            file.revalidate().map_err(|error| {
                capture_source_error(root, "revalidate Continue file root", error)
            })?;
            (authority, selected_relative, selected_path, true)
        }
    };
    let index_relative = if selected_file {
        PathBuf::from("sessions.json")
    } else {
        selected_relative.join("sessions.json")
    };
    let mut spool = ContinuePathSpool::new(&selected_path)?;
    let mutation_watch = Arc::new(RootMutationWatch::new(&selected_path)?);
    let inventory = observe_inventory(
        &authority,
        &selected_relative,
        selected_file,
        Some(&mut spool),
        Some(&mutation_watch),
    )?;
    if inventory.before_token != inventory.after_token || mutation_watch.mutated() {
        return Err(ContinueNativePathError::SourceChanged {
            path: selected_path,
        });
    }
    let index = ContinueIndexSnapshot::observe(&authority, index_relative);
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
            root: selected_path,
            authority,
            selected_relative,
            selected_file,
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
    let normalized_root = normalized_continue_authority_path(root)?;
    let authority = ProviderSourceRoot::open(&normalized_root)
        .map_err(|error| capture_source_error(root, "open pending Continue root", error))?;
    let canonical_root = authority.named_path().to_path_buf();
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
        let canonical_path = normalized_continue_authority_path(&path)?;
        let relative = canonical_path.strip_prefix(&canonical_root).map_err(|_| {
            ContinueNativePathError::SourceAccess {
                path: path.clone(),
                message: "pending Continue observation must be a direct child of its retained root"
                    .to_owned(),
            }
        })?;
        if relative.components().count() != 1
            || !matches!(relative.components().next(), Some(Component::Normal(_)))
        {
            return Err(ContinueNativePathError::SourceAccess {
                path,
                message: "pending Continue observation must be a direct child of its retained root"
                    .to_owned(),
            });
        }
        if !super::super::continue_session_json_path(&canonical_path) {
            return Err(ContinueNativePathError::SourceAccess {
                path,
                message: "pending Continue observation is not a session JSON document".to_owned(),
            });
        }
        authority
            .open_file(relative)
            .and_then(|opened| opened.revalidate())
            .map_err(|error| {
                capture_source_error(&path, "validate pending Continue source", error)
            })?;
        canonical_paths.push(canonical_path);
    }
    canonical_paths.sort();
    canonical_paths.dedup();
    for path in canonical_paths {
        spool.push(&path)?;
    }
    let index = ContinueIndexSnapshot::observe(&authority, PathBuf::from("sessions.json"));
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
            authority,
            selected_relative: PathBuf::new(),
            selected_file: false,
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

fn normalized_continue_authority_path(path: &Path) -> Result<PathBuf, ContinueNativePathError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| source_access(path, error))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ContinueNativePathError::SourceAccess {
                        path: path.to_path_buf(),
                        message: "Continue root cannot escape the filesystem root".to_owned(),
                    });
                }
            }
        }
    }
    Ok(normalized)
}
