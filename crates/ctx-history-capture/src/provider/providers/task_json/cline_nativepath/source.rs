use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderSourceDirectory,
        ProviderSourceRoot,
    },
    CaptureError, CLINE_TASK_JSON_SOURCE_FORMAT, ROO_TASK_JSON_SOURCE_FORMAT,
};

use super::ClineNativePathError;

mod route;

use route::resolve_data_root;

const ROOT_INVENTORY_DOMAIN: &[u8] = b"ctx-cline-nativepath-root-inventory-v2\0";
const MAX_TASK_DIRECT_CHILDREN: usize = 4_096;
const MAX_TASK_DIRECTORIES: usize = 4_096;
pub(super) const API_FILE: &str = "api_conversation_history.json";
pub(super) const UI_FILE: &str = "ui_messages.json";
pub(super) const METADATA_FILE: &str = "task_metadata.json";
pub(super) const ROOT_INDEX_FILE: &str = "taskHistory.json";
pub(super) const ROO_HISTORY_ITEM_FILE: &str = "history_item.json";
pub(super) const ROO_TASK_INDEX_FILE: &str = "_index.json";
pub(super) const ROO_FALLBACK_FILE: &str = "claude_messages.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TaskJsonNativeDialect {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) display_name: &'static str,
    metadata_files: &'static [(&'static str, ClineComponent)],
    message_files: &'static [(&'static str, ClineComponent)],
    root_index_file: Option<&'static str>,
}

impl TaskJsonNativeDialect {
    pub(crate) const CLINE: Self = Self {
        provider: CaptureProvider::Cline,
        source_format: CLINE_TASK_JSON_SOURCE_FORMAT,
        display_name: "Cline",
        metadata_files: &[(METADATA_FILE, ClineComponent::TaskMetadata)],
        message_files: &[
            (API_FILE, ClineComponent::ApiHistory),
            (UI_FILE, ClineComponent::UiMessages),
        ],
        root_index_file: Some(ROOT_INDEX_FILE),
    };

    pub(crate) const ROO: Self = Self {
        provider: CaptureProvider::RooCode,
        source_format: ROO_TASK_JSON_SOURCE_FORMAT,
        display_name: "Roo Code",
        // Roo's history item is the strongest identity/workspace authority.
        // `_index.json` fills metadata when the history item is absent.
        metadata_files: &[
            (ROO_HISTORY_ITEM_FILE, ClineComponent::HistoryItem),
            (ROO_TASK_INDEX_FILE, ClineComponent::TaskIndex),
            (METADATA_FILE, ClineComponent::TaskMetadata),
        ],
        message_files: &[
            (API_FILE, ClineComponent::ApiHistory),
            (UI_FILE, ClineComponent::UiMessages),
            (ROO_FALLBACK_FILE, ClineComponent::FallbackHistory),
        ],
        root_index_file: None,
    };

    fn all_task_files(self) -> impl Iterator<Item = (&'static str, ClineComponent)> {
        self.metadata_files
            .iter()
            .chain(self.message_files.iter())
            .copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum ClineComponent {
    ApiHistory = 0,
    UiMessages = 1,
    TaskMetadata = 2,
    FallbackHistory = 4,
    HistoryItem = 5,
    TaskIndex = 6,
}

impl ClineComponent {
    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Self::ApiHistory => API_FILE,
            Self::UiMessages => UI_FILE,
            Self::TaskMetadata => METADATA_FILE,
            Self::FallbackHistory => ROO_FALLBACK_FILE,
            Self::HistoryItem => ROO_HISTORY_ITEM_FILE,
            Self::TaskIndex => ROO_TASK_INDEX_FILE,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClineFileStamp {
    len: u64,
    token: [u8; 32],
}

impl ClineFileStamp {
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn token(&self) -> String {
        hex(&self.token)
    }

    pub(super) fn token_bytes(&self) -> &[u8; 32] {
        &self.token
    }
}

impl PartialEq for ClineFileStamp {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.token == other.token
    }
}

impl Eq for ClineFileStamp {}

impl fmt::Debug for ClineFileStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClineFileStamp")
            .field("len", &self.len)
            .field("token", &self.token())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClineObservedFileState {
    Missing,
    Present(ClineFileStamp),
    Unavailable(Box<str>),
}

#[derive(Debug, Clone)]
pub(crate) struct ClineComponentObservation {
    pub(crate) component: ClineComponent,
    pub(crate) path: PathBuf,
    pub(crate) state: ClineObservedFileState,
    pub(super) authority: Option<ProviderSourceRoot>,
    pub(super) relative_path: Option<PathBuf>,
}

impl PartialEq for ClineComponentObservation {
    fn eq(&self, other: &Self) -> bool {
        self.component == other.component
            && self.path == other.path
            && self.state == other.state
            && self.relative_path == other.relative_path
    }
}

impl Eq for ClineComponentObservation {}

impl ClineComponentObservation {
    pub(crate) fn stamp(&self) -> Option<&ClineFileStamp> {
        match &self.state {
            ClineObservedFileState::Present(stamp) => Some(stamp),
            ClineObservedFileState::Missing | ClineObservedFileState::Unavailable(_) => None,
        }
    }

    pub(crate) fn revalidate(&self) -> Result<bool, ClineNativePathError> {
        let (Some(authority), Some(relative)) =
            (self.authority.as_ref(), self.relative_path.as_deref())
        else {
            return Ok(false);
        };
        if authority.revalidate().is_err() {
            return Ok(false);
        }
        let current = observe_component_optional(authority, relative, &self.path, self.component)?;
        Ok(current == *self)
    }

    pub(super) fn open_verified(&self) -> Result<OpenedProviderSourceFile, ClineNativePathError> {
        let expected = self
            .stamp()
            .ok_or_else(|| ClineNativePathError::SourceChanged {
                path: self.path.clone(),
            })?;
        let (Some(authority), Some(relative)) =
            (self.authority.as_ref(), self.relative_path.as_deref())
        else {
            return Err(ClineNativePathError::SourceChanged {
                path: self.path.clone(),
            });
        };
        authority.revalidate().map_err(|error| {
            capture_source_error(&self.path, "revalidate component root", error)
        })?;
        let opened = authority
            .open_file(relative)
            .map_err(|error| capture_source_error(&self.path, "reopen component", error))?;
        let token = opened_file_token(&opened, &self.path)?;
        if opened.len() != expected.len || token != expected.token {
            return Err(ClineNativePathError::SourceChanged {
                path: self.path.clone(),
            });
        }
        opened.revalidate().map_err(|error| {
            capture_source_error(&self.path, "revalidate reopened component", error)
        })?;
        Ok(opened)
    }

    pub(super) fn post_parse_revalidate(&self) -> Result<bool, ClineNativePathError> {
        self.revalidate()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ClineLiveTaskObservation {
    pub(crate) dialect: TaskJsonNativeDialect,
    pub(crate) requested_task_path: PathBuf,
    pub(crate) canonical_task_path: PathBuf,
    pub(crate) directory_task_id: Box<str>,
    pub(crate) api_history: ClineComponentObservation,
    pub(crate) ui_messages: ClineComponentObservation,
    pub(crate) fallback_history: ClineComponentObservation,
    pub(crate) task_metadata: ClineComponentObservation,
    pub(crate) history_item: ClineComponentObservation,
    pub(crate) task_index: ClineComponentObservation,
    authority: ProviderSourceRoot,
    task_relative: PathBuf,
}

impl PartialEq for ClineLiveTaskObservation {
    fn eq(&self, other: &Self) -> bool {
        self.dialect == other.dialect
            && self.requested_task_path == other.requested_task_path
            && self.canonical_task_path == other.canonical_task_path
            && self.directory_task_id == other.directory_task_id
            && self.api_history == other.api_history
            && self.ui_messages == other.ui_messages
            && self.fallback_history == other.fallback_history
            && self.task_metadata == other.task_metadata
            && self.history_item == other.history_item
            && self.task_index == other.task_index
            && self.task_relative == other.task_relative
    }
}

impl Eq for ClineLiveTaskObservation {}

impl ClineLiveTaskObservation {
    pub(crate) fn component(&self, component: ClineComponent) -> &ClineComponentObservation {
        match component {
            ClineComponent::ApiHistory => &self.api_history,
            ClineComponent::UiMessages => &self.ui_messages,
            ClineComponent::FallbackHistory => &self.fallback_history,
            ClineComponent::TaskMetadata => &self.task_metadata,
            ClineComponent::HistoryItem => &self.history_item,
            ClineComponent::TaskIndex => &self.task_index,
        }
    }

    pub(crate) fn metadata_authority(&self) -> &ClineComponentObservation {
        self.dialect
            .metadata_files
            .iter()
            .map(|(_, component)| self.component(*component))
            .find(|observation| observation.stamp().is_some())
            .unwrap_or_else(|| {
                let component = self.dialect.metadata_files[0].1;
                self.component(component)
            })
    }

    pub(crate) fn event_components(&self) -> impl Iterator<Item = ClineComponent> + '_ {
        self.dialect
            .message_files
            .iter()
            .map(|(_, component)| *component)
    }

    pub(crate) fn revalidate_directory(&self) -> Result<bool, ClineNativePathError> {
        match self.authority.open_directory(&self.task_relative) {
            Ok(directory) => {
                directory.revalidate().map_err(|error| {
                    capture_source_error(
                        &self.requested_task_path,
                        "revalidate task directory",
                        error,
                    )
                })?;
                Ok(self.authority.revalidate().is_ok())
            }
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(capture_source_error(
                &self.requested_task_path,
                "revalidate task directory",
                error,
            )),
        }
    }

    pub(crate) fn revalidate_all_components(&self) -> Result<bool, ClineNativePathError> {
        if !self.revalidate_directory()? {
            return Ok(false);
        }
        for component in [
            ClineComponent::ApiHistory,
            ClineComponent::UiMessages,
            ClineComponent::FallbackHistory,
            ClineComponent::TaskMetadata,
            ClineComponent::HistoryItem,
            ClineComponent::TaskIndex,
        ] {
            let expected = self.component(component);
            if !expected.revalidate()? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClineRootInventoryProof {
    entries: usize,
    digest: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) struct ClineRootAuthority {
    data_root: PathBuf,
    dialect: TaskJsonNativeDialect,
    inventory: ClineRootInventoryProof,
    authority: ProviderSourceRoot,
}

impl PartialEq for ClineRootAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.data_root == other.data_root
            && self.dialect == other.dialect
            && self.inventory == other.inventory
    }
}

impl Eq for ClineRootAuthority {}

impl ClineRootAuthority {
    pub(crate) fn source_backed_revision(&self) -> Vec<u8> {
        let mut revision = Vec::with_capacity(8 + 32);
        revision.extend_from_slice(
            &u64::try_from(self.inventory.entries)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        revision.extend_from_slice(&self.inventory.digest);
        revision
    }

    /// This is catalog authority only. Component pages never depend on it.
    pub(crate) fn revalidate_catalog(&self) -> Result<bool, ClineNativePathError> {
        if self.authority.revalidate().is_err() {
            return Ok(false);
        }
        Ok(observe_direct_child_inventory(&self.authority, self.dialect)?.proof == self.inventory)
    }
}

struct ClineRootInventory {
    proof: ClineRootInventoryProof,
    task_relatives: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineDiscovery {
    root_authority: ClineRootAuthority,
    task_routes: Box<[ClineLiveTaskObservation]>,
}

impl ClineDiscovery {
    pub(crate) fn root_authority(&self) -> &ClineRootAuthority {
        &self.root_authority
    }

    pub(crate) fn task_routes(&self) -> &[ClineLiveTaskObservation] {
        &self.task_routes
    }

    pub(crate) fn for_task(&self, task: ClineLiveTaskObservation) -> Self {
        Self {
            root_authority: self.root_authority.clone(),
            task_routes: vec![task].into_boxed_slice(),
        }
    }
}

pub(crate) fn discover_cline_root(root: &Path) -> Result<ClineDiscovery, ClineNativePathError> {
    discover_task_json_root(root, TaskJsonNativeDialect::CLINE)
}

pub(crate) fn discover_roo_root(root: &Path) -> Result<ClineDiscovery, ClineNativePathError> {
    discover_task_json_root(root, TaskJsonNativeDialect::ROO)
}

fn discover_task_json_root(
    root: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<ClineDiscovery, ClineNativePathError> {
    let (_, authority) = resolve_data_root(root, dialect)?;
    let tasks_root = authority.named_path().join("tasks");
    authority
        .open_directory(Path::new("tasks"))
        .and_then(|directory| directory.revalidate())
        .map_err(|error| capture_source_error(&tasks_root, "open tasks root", error))?;
    let inventory = observe_direct_child_inventory(&authority, dialect)?;
    let root_authority = ClineRootAuthority {
        data_root: authority.named_path().to_path_buf(),
        dialect,
        inventory: inventory.proof.clone(),
        authority: authority.clone(),
    };
    let mut routes = Vec::with_capacity(inventory.task_relatives.len());
    for relative in inventory.task_relatives {
        routes.push(observe_live_task(&authority, &relative, dialect)?);
    }
    routes.sort_by(|left, right| left.requested_task_path.cmp(&right.requested_task_path));
    Ok(ClineDiscovery {
        root_authority,
        task_routes: routes.into_boxed_slice(),
    })
}

fn observe_live_task(
    authority: &ProviderSourceRoot,
    task_relative: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<ClineLiveTaskObservation, ClineNativePathError> {
    let path = authority.named_path().join(task_relative);
    authority
        .open_directory(task_relative)
        .and_then(|directory| directory.revalidate())
        .map_err(|error| capture_source_error(&path, "open task directory", error))?;
    let canonical_task_path = path.clone();
    let directory_task_id = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| valid_task_id(value))
        .ok_or_else(|| ClineNativePathError::InvalidNativeIdentity {
            message: format!("invalid Cline task directory name: {}", path.display()),
        })?
        .to_owned()
        .into_boxed_str();
    Ok(ClineLiveTaskObservation {
        dialect,
        requested_task_path: path.to_path_buf(),
        canonical_task_path,
        directory_task_id,
        api_history: observe_task_component(
            authority,
            &task_relative.join(API_FILE),
            ClineComponent::ApiHistory,
        )?,
        ui_messages: observe_task_component(
            authority,
            &task_relative.join(UI_FILE),
            ClineComponent::UiMessages,
        )?,
        fallback_history: observe_task_component(
            authority,
            &task_relative.join(ROO_FALLBACK_FILE),
            ClineComponent::FallbackHistory,
        )?,
        task_metadata: observe_task_component(
            authority,
            &task_relative.join(METADATA_FILE),
            ClineComponent::TaskMetadata,
        )?,
        history_item: observe_task_component(
            authority,
            &task_relative.join(ROO_HISTORY_ITEM_FILE),
            ClineComponent::HistoryItem,
        )?,
        task_index: observe_task_component(
            authority,
            &task_relative.join(ROO_TASK_INDEX_FILE),
            ClineComponent::TaskIndex,
        )?,
        authority: authority.clone(),
        task_relative: task_relative.to_path_buf(),
    })
}

fn observe_component_optional(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
    path: &Path,
    component: ClineComponent,
) -> Result<ClineComponentObservation, ClineNativePathError> {
    let opened = match authority.open_path(relative_path) {
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ClineComponentObservation {
                component,
                path: path.to_path_buf(),
                state: ClineObservedFileState::Missing,
                authority: Some(authority.clone()),
                relative_path: Some(relative_path.to_path_buf()),
            });
        }
        Err(error) => {
            return Err(capture_source_error(
                path,
                "open component observation",
                error,
            ))
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => {
            directory.revalidate().map_err(|error| {
                capture_source_error(path, "revalidate component directory", error)
            })?;
            return Err(ClineNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Cline components must be ordinary files".to_owned(),
            });
        }
        Ok(OpenedProviderSourcePath::File(file)) => file,
    };
    let token = opened_file_token(&opened, path)?;
    opened
        .revalidate()
        .map_err(|error| capture_source_error(path, "revalidate component observation", error))?;
    Ok(ClineComponentObservation {
        component,
        path: path.to_path_buf(),
        state: ClineObservedFileState::Present(ClineFileStamp {
            len: opened.len(),
            token,
        }),
        authority: Some(authority.clone()),
        relative_path: Some(relative_path.to_path_buf()),
    })
}

fn observe_task_component(
    authority: &ProviderSourceRoot,
    relative_path: &Path,
    component: ClineComponent,
) -> Result<ClineComponentObservation, ClineNativePathError> {
    let path = authority.named_path().join(relative_path);
    match observe_component_optional(authority, relative_path, &path, component) {
        Ok(observation) => Ok(observation),
        Err(error) if is_component_local_error(&error) => Ok(ClineComponentObservation {
            component,
            path,
            state: ClineObservedFileState::Unavailable(error.to_string().into_boxed_str()),
            authority: Some(authority.clone()),
            relative_path: Some(relative_path.to_path_buf()),
        }),
        Err(error) => Err(error),
    }
}

fn observe_direct_child_inventory(
    authority: &ProviderSourceRoot,
    dialect: TaskJsonNativeDialect,
) -> Result<ClineRootInventory, ClineNativePathError> {
    let tasks_root = authority.named_path().join("tasks");
    let directory = authority
        .open_directory(Path::new("tasks"))
        .map_err(|error| capture_source_error(&tasks_root, "open tasks inventory", error))?;
    let mut children = Vec::new();
    let names = directory
        .entries(MAX_TASK_DIRECT_CHILDREN.saturating_add(1))
        .map_err(|error| capture_source_error(&tasks_root, "enumerate tasks inventory", error))?;
    if names.len() > MAX_TASK_DIRECT_CHILDREN {
        return Err(ClineNativePathError::SourceAccess {
            path: tasks_root,
            message: "Cline tasks root exceeds the 4096-child inventory bound".to_owned(),
        });
    }
    for name in names {
        let path = authority.named_path().join("tasks").join(&name);
        let opened = directory
            .open_child(&name)
            .map_err(|error| capture_source_error(&path, "open task inventory entry", error))?;
        let mut component_states = Vec::new();
        let mut identity = [0_u8; 32];
        let (is_directory, is_file, has_component) = match opened {
            OpenedProviderSourcePath::Directory(task_directory) => {
                let mut has_component = false;
                for (file, _) in dialect.all_task_files() {
                    let (state, present) =
                        inventory_component_state(&task_directory, file, &path.join(file))?;
                    component_states.extend_from_slice(&state);
                    has_component |= present;
                }
                identity = directory_inventory_identity(&name);
                task_directory.revalidate().map_err(|error| {
                    capture_source_error(&path, "revalidate task inventory directory", error)
                })?;
                (true, false, has_component)
            }
            OpenedProviderSourcePath::File(file) => {
                file.revalidate().map_err(|error| {
                    capture_source_error(&path, "revalidate task inventory file", error)
                })?;
                (false, true, false)
            }
        };
        children.push((
            path,
            is_directory,
            is_file,
            component_states,
            identity,
            has_component,
        ));
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(ROOT_INVENTORY_DOMAIN);
    let mut task_relatives = Vec::new();
    for (path, directory, file, states, identity, has_component) in &children {
        let name = path
            .file_name()
            .ok_or_else(|| ClineNativePathError::InvalidNativeIdentity {
                message: format!("inventory entry has no name: {}", path.display()),
            })?;
        let encoded = name.as_encoded_bytes();
        hasher.update(encoded.len().to_le_bytes());
        hasher.update(encoded);
        hasher.update([u8::from(*directory), u8::from(*file)]);
        hasher.update(states);
        hasher.update(identity);
        if *directory && *has_component {
            if task_relatives.len() == MAX_TASK_DIRECTORIES {
                return Err(ClineNativePathError::SourceAccess {
                    path: authority.named_path().join("tasks"),
                    message: "Cline tasks root exceeds the 4096-task authority bound".to_owned(),
                });
            }
            task_relatives.push(PathBuf::from("tasks").join(name));
        }
    }
    directory
        .revalidate()
        .and_then(|()| authority.revalidate())
        .map_err(|error| capture_source_error(&tasks_root, "revalidate tasks inventory", error))?;
    Ok(ClineRootInventory {
        proof: ClineRootInventoryProof {
            entries: children.len(),
            digest: hasher.finalize().into(),
        },
        task_relatives,
    })
}

fn directory_inventory_identity(name: &std::ffi::OsStr) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-directory-inventory-v1\0");
    hasher.update(name.as_encoded_bytes());
    hasher.finalize().into()
}

fn inventory_component_state(
    directory: &ProviderSourceDirectory,
    name: &str,
    path: &Path,
) -> Result<(Vec<u8>, bool), ClineNativePathError> {
    match directory.open_child(std::ffi::OsStr::new(name)) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            let mut state = vec![1];
            state.extend_from_slice(&opened_file_token(&file, path)?);
            file.revalidate().map_err(|error| {
                capture_source_error(path, "revalidate inventory component", error)
            })?;
            Ok((state, true))
        }
        Ok(OpenedProviderSourcePath::Directory(child)) => {
            child.revalidate().map_err(|error| {
                capture_source_error(path, "revalidate inventory component directory", error)
            })?;
            Ok((vec![2], true))
        }
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            Ok((vec![0], false))
        }
        Err(error) => {
            let classified = capture_source_error(path, "open inventory component", error);
            if is_component_local_error(&classified) {
                Ok((vec![3], true))
            } else {
                Err(classified)
            }
        }
    }
}

fn valid_task_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

fn opened_file_token(
    file: &OpenedProviderSourceFile,
    path: &Path,
) -> Result<[u8; 32], ClineNativePathError> {
    let metadata = file.metadata();
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-task-json-opened-file-token-v1\0");
    hasher.update(metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .map_err(|error| source_io(path, "read component modification time", error))?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            hasher.update([0]);
            hasher.update(duration.as_secs().to_le_bytes());
            hasher.update(duration.subsec_nanos().to_le_bytes());
        }
        Err(error) => {
            hasher.update([1]);
            hasher.update(error.duration().as_secs().to_le_bytes());
            hasher.update(error.duration().subsec_nanos().to_le_bytes());
        }
    }
    hasher.update([u8::from(metadata.permissions().readonly())]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.mode().to_le_bytes());
        hasher.update(metadata.ctime().to_le_bytes());
        hasher.update(metadata.ctime_nsec().to_le_bytes());
    }
    Ok(hasher.finalize().into())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

pub(super) fn is_component_local_error(error: &ClineNativePathError) -> bool {
    match error {
        ClineNativePathError::SourceAccess { .. } | ClineNativePathError::SourceChanged { .. } => {
            true
        }
        ClineNativePathError::SourceIo {
            kind, raw_os_error, ..
        } => component_local_io_error(*kind, *raw_os_error),
        ClineNativePathError::SystemicSource { .. }
        | ClineNativePathError::UnsupportedRoot { .. }
        | ClineNativePathError::InvalidNativeIdentity { .. }
        | ClineNativePathError::Invariant { .. } => false,
    }
}

fn component_local_io_error(kind: io::ErrorKind, raw_os_error: Option<i32>) -> bool {
    if matches!(
        kind,
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    ) {
        return true;
    }
    #[cfg(unix)]
    if raw_os_error.is_some_and(|code| {
        matches!(
            code,
            libc::ENOENT | libc::EACCES | libc::EPERM | libc::ENOTDIR | libc::ELOOP | libc::ESTALE
        )
    }) {
        return true;
    }
    #[cfg(not(unix))]
    let _ = raw_os_error;
    false
}

pub(super) fn capture_source_error(
    path: &Path,
    operation: &'static str,
    error: CaptureError,
) -> ClineNativePathError {
    match error {
        CaptureError::Io(error) => source_io(path, operation, error),
        CaptureError::SystemIo { source, .. } => source_io(path, operation, source),
        error @ CaptureError::InvalidProviderTranscriptPath { .. } => {
            ClineNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        }
        CaptureError::SourceChangedDuringCapture => ClineNativePathError::SourceChanged {
            path: path.to_path_buf(),
        },
        error => ClineNativePathError::SystemicSource {
            path: path.to_path_buf(),
            message: error.to_string(),
        },
    }
}

pub(super) fn source_io(
    path: &Path,
    operation: &'static str,
    error: io::Error,
) -> ClineNativePathError {
    ClineNativePathError::SourceIo {
        path: path.to_path_buf(),
        operation,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
        message: error.to_string(),
    }
}

pub(super) fn source_access(path: &Path, error: io::Error) -> ClineNativePathError {
    source_io(path, "source access", error)
}
