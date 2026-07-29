use std::{
    fmt, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineInjectedIoOperation {
    InventoryComponentStat,
    ComponentStat,
    ComponentOpen,
    ComponentRead,
    ComponentPostParseStat,
    RootAuthorityStat,
}

#[cfg(test)]
struct ClineInjectedIoFailure {
    operation: ClineInjectedIoOperation,
    path: PathBuf,
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
    remaining: usize,
}

#[cfg(test)]
std::thread_local! {
    static CLINE_INJECTED_IO_FAILURE: std::cell::RefCell<Option<ClineInjectedIoFailure>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_cline_io_failure(
    operation: ClineInjectedIoOperation,
    path: PathBuf,
    error: io::Error,
    repetitions: usize,
) {
    CLINE_INJECTED_IO_FAILURE.with(|failure| {
        *failure.borrow_mut() = Some(ClineInjectedIoFailure {
            operation,
            path,
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
            remaining: repetitions,
        });
    });
}

#[cfg(test)]
pub(crate) fn clear_cline_io_failure() {
    CLINE_INJECTED_IO_FAILURE.with(|failure| {
        failure.borrow_mut().take();
    });
}

#[cfg(test)]
pub(super) fn injected_io_failure(
    operation: ClineInjectedIoOperation,
    path: &Path,
) -> Option<io::Error> {
    CLINE_INJECTED_IO_FAILURE.with(|failure| {
        let mut failure = failure.borrow_mut();
        let configured = failure.as_mut()?;
        if configured.operation != operation || configured.path != path || configured.remaining == 0
        {
            return None;
        }
        configured.remaining -= 1;
        configured.raw_os_error.map_or_else(
            || Some(io::Error::new(configured.kind, configured.message.clone())),
            |raw| Some(io::Error::from_raw_os_error(raw)),
        )
    })
}

#[cfg(not(test))]
pub(super) fn injected_io_failure(
    _operation: ClineInjectedIoOperation,
    _path: &Path,
) -> Option<io::Error> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum ClineComponent {
    ApiHistory = 0,
    UiMessages = 1,
    TaskMetadata = 2,
    RootIndex = 3,
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
            Self::RootIndex => ROOT_INDEX_FILE,
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
    opened: Arc<OpenedProviderSourceFile>,
}

impl ClineFileStamp {
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(super) fn opened(&self) -> Arc<OpenedProviderSourceFile> {
        self.opened.clone()
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

    pub(crate) fn is_missing(&self) -> bool {
        matches!(self.state, ClineObservedFileState::Missing)
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
        if let Some(stamp) = self.stamp() {
            if stamp.opened.revalidate().is_err() {
                return Ok(false);
            }
        }
        let current = observe_component_optional(authority, relative, &self.path, self.component)?;
        Ok(current == *self)
    }

    pub(super) fn post_parse_revalidate(&self) -> Result<bool, ClineNativePathError> {
        if let Some(error) =
            injected_io_failure(ClineInjectedIoOperation::ComponentPostParseStat, &self.path)
        {
            return Err(source_io(&self.path, "post-parse component stat", error));
        }
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
            ClineComponent::RootIndex => {
                unreachable!("root index is not a task component")
            }
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
    tasks_root: PathBuf,
    dialect: TaskJsonNativeDialect,
    inventory: Option<ClineRootInventoryProof>,
    complete: bool,
    authority: ProviderSourceRoot,
}

impl PartialEq for ClineRootAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.data_root == other.data_root
            && self.tasks_root == other.tasks_root
            && self.dialect == other.dialect
            && self.inventory == other.inventory
            && self.complete == other.complete
    }
}

impl Eq for ClineRootAuthority {}

impl ClineRootAuthority {
    pub(crate) fn tasks_root(&self) -> &Path {
        &self.tasks_root
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn source_backed_revision(&self) -> Vec<u8> {
        let mut revision = Vec::with_capacity(1 + 8 + 32);
        revision.push(u8::from(self.complete));
        if let Some(inventory) = &self.inventory {
            revision.extend_from_slice(
                &u64::try_from(inventory.entries)
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            revision.extend_from_slice(&inventory.digest);
        } else {
            revision.extend_from_slice(&0_u64.to_le_bytes());
            revision.extend_from_slice(&[0_u8; 32]);
        }
        revision
    }

    /// This is catalog authority only. Component pages never depend on it.
    pub(crate) fn revalidate_catalog(&self) -> Result<bool, ClineNativePathError> {
        if self.authority.revalidate().is_err() {
            return Ok(false);
        }
        let Some(expected) = &self.inventory else {
            return Ok(true);
        };
        Ok(observe_direct_child_inventory(&self.authority, self.dialect)?.proof == *expected)
    }
}

struct ClineRootInventory {
    proof: ClineRootInventoryProof,
    task_relatives: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineDiscovery {
    dialect: TaskJsonNativeDialect,
    root_authority: ClineRootAuthority,
    root_index: ClineComponentObservation,
    task_routes: Box<[ClineLiveTaskObservation]>,
}

impl ClineDiscovery {
    pub(crate) fn dialect(&self) -> TaskJsonNativeDialect {
        self.dialect
    }

    pub(crate) fn root_authority(&self) -> &ClineRootAuthority {
        &self.root_authority
    }

    pub(crate) fn root_index(&self) -> &ClineComponentObservation {
        &self.root_index
    }

    pub(crate) fn task_routes(&self) -> &[ClineLiveTaskObservation] {
        &self.task_routes
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
    let (data_root, authority) = resolve_data_root(root, dialect)?;
    let tasks_root = authority.named_path().join("tasks");
    if let Some(error) =
        injected_io_failure(ClineInjectedIoOperation::RootAuthorityStat, &data_root)
    {
        return Err(source_io(&data_root, "stat root authority", error));
    }
    if let Some(error) =
        injected_io_failure(ClineInjectedIoOperation::RootAuthorityStat, &tasks_root)
    {
        return Err(source_io(&tasks_root, "stat root authority", error));
    }
    authority
        .open_directory(Path::new("tasks"))
        .and_then(|directory| directory.revalidate())
        .map_err(|error| capture_source_error(&tasks_root, "open tasks root", error))?;
    let inventory = observe_direct_child_inventory(&authority, dialect)?;
    let root_authority = ClineRootAuthority {
        data_root: authority.named_path().to_path_buf(),
        tasks_root,
        dialect,
        inventory: Some(inventory.proof.clone()),
        complete: true,
        authority: authority.clone(),
    };
    let root_index = match dialect.root_index_file {
        Some(file) => {
            let relative = PathBuf::from("state").join(file);
            observe_component_optional(
                &authority,
                &relative,
                &authority.named_path().join(&relative),
                ClineComponent::RootIndex,
            )?
        }
        None => ClineComponentObservation {
            component: ClineComponent::RootIndex,
            path: authority.named_path().join("state").join(ROOT_INDEX_FILE),
            state: ClineObservedFileState::Missing,
            authority: Some(authority.clone()),
            relative_path: Some(PathBuf::from("state").join(ROOT_INDEX_FILE)),
        },
    };
    let mut routes = Vec::with_capacity(inventory.task_relatives.len());
    for relative in inventory.task_relatives {
        routes.push(observe_live_task(&authority, &relative, dialect)?);
    }
    routes.sort_by(|left, right| left.requested_task_path.cmp(&right.requested_task_path));
    Ok(ClineDiscovery {
        dialect,
        root_authority,
        root_index,
        task_routes: routes.into_boxed_slice(),
    })
}

pub(crate) fn revalidate_cline_component_source(
    observation: &ClineComponentObservation,
    expected_stamp_token: &str,
) -> Result<bool, ClineNativePathError> {
    let current_token = observation
        .stamp()
        .map_or_else(|| "missing".to_owned(), ClineFileStamp::token);
    if current_token != expected_stamp_token || !observation.revalidate()? {
        return Ok(false);
    }
    Ok(true)
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
    if let Some(error) = injected_io_failure(ClineInjectedIoOperation::ComponentStat, path) {
        return Err(source_io(path, "stat component", error));
    }
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
            opened: Arc::new(opened),
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
    if let Some(error) = injected_io_failure(ClineInjectedIoOperation::InventoryComponentStat, path)
    {
        let classified = source_io(path, "inventory component stat", error);
        return if is_component_local_error(&classified) {
            Ok((vec![3], true))
        } else {
            Err(classified)
        };
    }
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

fn resolve_data_root(
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<(PathBuf, ProviderSourceRoot), ClineNativePathError> {
    let requested = normalized_task_json_authority_path(path)?;
    let (data_root, selected_route) =
        selected_task_json_route(&requested, dialect).ok_or_else(|| {
            ClineNativePathError::UnsupportedRoot {
                path: requested.clone(),
            }
        })?;
    let authority = ProviderSourceRoot::open(&data_root)
        .map_err(|error| capture_source_error(&data_root, "open task-json data root", error))?;
    let tasks = match authority.open_directory(Path::new("tasks")) {
        Ok(tasks) => tasks,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ClineNativePathError::UnsupportedRoot { path: requested });
        }
        Err(error) => {
            return Err(capture_source_error(
                &data_root.join("tasks"),
                "open selected tasks directory",
                error,
            ));
        }
    };
    tasks.revalidate().map_err(|error| {
        capture_source_error(
            &data_root.join("tasks"),
            "revalidate selected tasks directory",
            error,
        )
    })?;
    match selected_route {
        SelectedTaskJsonRoute::DataRoot | SelectedTaskJsonRoute::TasksRoot => {}
        SelectedTaskJsonRoute::TaskDirectory(relative) => {
            let directory = authority.open_directory(&relative).map_err(|error| {
                capture_source_error(&requested, "open selected task directory", error)
            })?;
            if !task_dir_has_component(&directory, &requested, dialect)? {
                return Err(ClineNativePathError::UnsupportedRoot { path: requested });
            }
            directory.revalidate().map_err(|error| {
                capture_source_error(&requested, "revalidate selected task directory", error)
            })?;
        }
        SelectedTaskJsonRoute::File(relative) => {
            authority
                .open_file(&relative)
                .and_then(|file| file.revalidate())
                .map_err(|error| {
                    capture_source_error(&requested, "open selected task-json file", error)
                })?;
        }
    }
    authority.revalidate().map_err(|error| {
        capture_source_error(&data_root, "revalidate task-json data root", error)
    })?;
    Ok((authority.named_path().to_path_buf(), authority))
}

enum SelectedTaskJsonRoute {
    DataRoot,
    TasksRoot,
    TaskDirectory(PathBuf),
    File(PathBuf),
}

fn selected_task_json_route(
    requested: &Path,
    dialect: TaskJsonNativeDialect,
) -> Option<(PathBuf, SelectedTaskJsonRoute)> {
    let file_name = requested.file_name().and_then(|value| value.to_str());
    if file_name.is_some_and(|name| dialect.root_index_file == Some(name)) {
        let data_root = requested.parent()?.parent()?.to_path_buf();
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::File(relative)));
    }
    if file_name.is_some_and(|name| {
        dialect
            .all_task_files()
            .any(|(candidate, _)| candidate == name)
    }) {
        let task_dir = requested.parent()?;
        let data_root = task_dir_data_root(task_dir)?;
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::File(relative)));
    }
    if file_name == Some("tasks") {
        return Some((
            requested.parent()?.to_path_buf(),
            SelectedTaskJsonRoute::TasksRoot,
        ));
    }
    if requested
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        == Some("tasks")
    {
        let data_root = task_dir_data_root(requested)?;
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::TaskDirectory(relative)));
    }
    Some((requested.to_path_buf(), SelectedTaskJsonRoute::DataRoot))
}

fn task_dir_data_root(task_dir: &Path) -> Option<PathBuf> {
    let tasks = task_dir
        .parent()
        .filter(|path| path.file_name().and_then(|value| value.to_str()) == Some("tasks"))?;
    tasks.parent().map(Path::to_path_buf)
}

fn task_dir_has_component(
    directory: &ProviderSourceDirectory,
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<bool, ClineNativePathError> {
    for (file, _) in dialect.all_task_files() {
        let component = path.join(file);
        match directory.open_child(std::ffi::OsStr::new(file)) {
            Ok(OpenedProviderSourcePath::File(opened)) => {
                opened.revalidate().map_err(|error| {
                    capture_source_error(&component, "revalidate task component", error)
                })?;
                return Ok(true);
            }
            Ok(OpenedProviderSourcePath::Directory(opened)) => {
                opened.revalidate().map_err(|error| {
                    capture_source_error(&component, "revalidate task component directory", error)
                })?;
                return Ok(true);
            }
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                let classified = capture_source_error(&component, "inspect task component", error);
                if is_component_local_error(&classified) {
                    return Ok(true);
                }
                return Err(classified);
            }
        }
    }
    Ok(false)
}

fn normalized_task_json_authority_path(path: &Path) -> Result<PathBuf, ClineNativePathError> {
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
                    return Err(ClineNativePathError::UnsupportedRoot {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
    }
    Ok(normalized)
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

fn source_access(path: &Path, error: io::Error) -> ClineNativePathError {
    source_io(path, "source access", error)
}
