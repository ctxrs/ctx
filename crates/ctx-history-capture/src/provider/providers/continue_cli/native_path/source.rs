use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

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
mod watch;

use common::*;
use index::*;
use inventory::observe_inventory;
use watch::RootMutationWatch;

const SESSION_REVISION_DOMAIN: &[u8] = b"ctx-continue-nativepath-session-v2\0";
const INDEX_REVISION_DOMAIN: &[u8] = b"ctx-continue-nativepath-index-v2\0";
const INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx-continue-nativepath-inventory-v2\0";
const DOCUMENT_LEAF_DIGEST_DOMAIN: &[u8] = b"ctx-continue-document-leaf-v1\0";
const DOCUMENT_TREE_DIGEST_DOMAIN: &[u8] = b"ctx-continue-document-tree-v1\0";
const MAX_CONTINUE_SESSION_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_INDEX_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_SESSION_ID_BYTES: usize = 512;
const MAX_CONTINUE_INDEX_STRING_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
const MAX_CONTINUE_DIRECTORY_DEPTH: usize = 128;
const MAX_CONTINUE_INVENTORY_ENTRIES: usize = 8_192;
const MAX_CONTINUE_INDEX_ENTRIES: usize = MAX_CONTINUE_INVENTORY_ENTRIES;

#[derive(Debug)]
struct ExactFileSnapshot {
    path: PathBuf,
    canonical_path: PathBuf,
    file_token: [u8; 32],
    bytes: Box<[u8]>,
    revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueSourceObservation {
    requested_path: PathBuf,
    canonical_path: PathBuf,
    file_token: [u8; 32],
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
            opened,
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

impl ContinueIndexState {
    fn tag(self) -> u8 {
        match self {
            Self::Missing => 0,
            Self::Ready => 1,
            Self::Malformed => 2,
            Self::Unavailable => 3,
        }
    }
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

    pub(crate) fn dependency_revision(&self) -> &str {
        &self.dependency_revision
    }

    fn fingerprint(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-continue-index-observation-v1\0");
        digest.update([self.state.tag()]);
        digest.update((self.dependency_revision.len() as u64).to_be_bytes());
        digest.update(self.dependency_revision.as_bytes());
        digest.finalize().into()
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

    fn observe_current(&self) -> ContinueIndexObservation {
        observe_continue_index(&self.authority, self.relative_path.clone()).observation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueDocumentLeaf {
    relative_path: PathBuf,
    path: PathBuf,
    file_token: [u8; 32],
}

impl ContinueDocumentLeaf {
    pub(super) fn new(relative_path: PathBuf, path: PathBuf, file_token: [u8; 32]) -> Self {
        Self {
            relative_path,
            path,
            file_token,
        }
    }

    fn matches(&self, observation: &ContinueSourceObservation) -> bool {
        self.path == observation.requested_path && self.file_token == observation.file_token
    }
}

#[derive(Debug)]
pub(crate) struct ContinueTreeAuthority {
    authority: ProviderSourceRoot,
    selected_relative: PathBuf,
    selected_file: bool,
    inventory_fingerprint: [u8; 32],
    mutation_watch: RootMutationWatch,
    index: ContinueIndexSnapshot,
}

impl ContinueTreeAuthority {
    pub(crate) fn index(&self) -> &ContinueIndexSnapshot {
        &self.index
    }

    pub(crate) fn open_source(
        &self,
        leaf: &ContinueDocumentLeaf,
    ) -> Result<ContinueSourceSnapshot, ContinueNativePathError> {
        let opened = self
            .authority
            .open_file(&leaf.relative_path)
            .map_err(|error| capture_source_error(&leaf.path, "open Continue source", error))?;
        let snapshot = ContinueSourceSnapshot::read_opened(&leaf.path, opened)?;
        if !leaf.matches(snapshot.observation()) {
            return Err(ContinueNativePathError::SourceChanged {
                path: leaf.path.clone(),
            });
        }
        Ok(snapshot)
    }

    pub(crate) fn leaf_fingerprint(&self, leaf: &ContinueDocumentLeaf) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(DOCUMENT_LEAF_DIGEST_DOMAIN);
        digest.update(leaf.file_token);
        digest.update(self.index.observation.fingerprint());
        digest.finalize().into()
    }

    pub(crate) fn tree_fingerprint(&self) -> [u8; 32] {
        continue_tree_fingerprint(
            self.inventory_fingerprint,
            self.index.observation.fingerprint(),
        )
    }

    pub(crate) fn revalidate_fingerprint(
        &self,
    ) -> Result<Option<[u8; 32]>, ContinueNativePathError> {
        if self.mutation_watch.mutated() {
            return Ok(None);
        }
        let current = match observe_inventory(
            &self.authority,
            &self.selected_relative,
            self.selected_file,
            None,
            Some(&self.mutation_watch),
        ) {
            Ok(current) => current,
            Err(ContinueNativePathError::SourceChanged { .. })
            | Err(ContinueNativePathError::SourceIo {
                kind: io::ErrorKind::NotFound,
                ..
            })
            | Err(ContinueNativePathError::SourceAccess { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        if self.mutation_watch.mutated() {
            return Ok(None);
        }
        let index = self.index.observe_current();
        if self.mutation_watch.mutated() {
            return Ok(None);
        }
        Ok(Some(continue_tree_fingerprint(
            current.digest,
            index.fingerprint(),
        )))
    }
}

#[derive(Debug)]
pub(crate) struct ContinueDiscovery {
    leaves: Vec<ContinueDocumentLeaf>,
    authority: ContinueTreeAuthority,
}

impl ContinueDiscovery {
    pub(crate) fn into_parts(self) -> (Vec<ContinueDocumentLeaf>, ContinueTreeAuthority) {
        (self.leaves, self.authority)
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
    let mutation_watch = RootMutationWatch::new(if selected_file {
        authority.named_path()
    } else {
        &selected_path
    })?;
    let mut leaves = Vec::new();
    let inventory = observe_inventory(
        &authority,
        &selected_relative,
        selected_file,
        Some(&mut leaves),
        Some(&mutation_watch),
    )?;
    if mutation_watch.mutated() {
        return Err(ContinueNativePathError::SourceChanged {
            path: selected_path,
        });
    }
    let mut admitted_files = HashSet::with_capacity(leaves.len());
    leaves.retain(|leaf| admitted_files.insert(leaf.file_token));
    let index = ContinueIndexSnapshot::observe(&authority, index_relative);
    if mutation_watch.mutated() {
        return Err(ContinueNativePathError::SourceChanged {
            path: selected_path,
        });
    }
    Ok(ContinueDiscovery {
        leaves,
        authority: ContinueTreeAuthority {
            authority,
            selected_relative,
            selected_file,
            inventory_fingerprint: inventory.digest,
            mutation_watch,
            index,
        },
    })
}

fn continue_tree_fingerprint(
    inventory_fingerprint: [u8; 32],
    index_fingerprint: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(DOCUMENT_TREE_DIGEST_DOMAIN);
    digest.update(inventory_fingerprint);
    digest.update(index_fingerprint);
    digest.finalize().into()
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
