use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use sha2::{Digest, Sha256};

use crate::{
    common::io::ensure_provider_path_parents_are_not_symlinks,
    provider_sources::{observe_ordinary_file, OrdinaryFileObservation},
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
    pub(crate) publication_revision: &'static str,
    pub(crate) component_cursor_stream_format: &'static str,
    pub(crate) task_cursor_stream_format: &'static str,
    pub(crate) root_cursor_stream_format: &'static str,
    pub(crate) page_publication_domain: &'static [u8],
    pub(crate) page_publication_prefix: &'static str,
    pub(crate) task_publication_domain: &'static [u8],
    pub(crate) task_publication_prefix: &'static str,
    pub(crate) root_publication_domain: &'static [u8],
    pub(crate) root_publication_prefix: &'static str,
    pub(crate) retirement_publication_domain: &'static [u8],
    pub(crate) retirement_publication_prefix: &'static str,
    metadata_files: &'static [(&'static str, ClineComponent)],
    message_files: &'static [(&'static str, ClineComponent)],
    root_index_file: Option<&'static str>,
}

impl TaskJsonNativeDialect {
    pub(crate) const CLINE: Self = Self {
        provider: CaptureProvider::Cline,
        source_format: CLINE_TASK_JSON_SOURCE_FORMAT,
        display_name: "Cline",
        publication_revision: "cline-v1",
        component_cursor_stream_format: "cline_nativepath_component_v1",
        task_cursor_stream_format: "cline_nativepath_task_v1",
        root_cursor_stream_format: "cline_nativepath_root_v1",
        page_publication_domain: b"ctx-cline-nativepath-publication-v1\0",
        page_publication_prefix: "cline-nativepath-v1:",
        task_publication_domain: b"ctx-cline-nativepath-task-checkpoint-v1\0",
        task_publication_prefix: "cline-nativepath-task-v1:",
        root_publication_domain: b"ctx-cline-nativepath-root-manifest-v1\0",
        root_publication_prefix: "cline-nativepath-root-v1:",
        retirement_publication_domain: b"ctx-cline-nativepath-route-retirement-v1\0",
        retirement_publication_prefix: "cline-nativepath-retire-v1:",
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
        publication_revision: "roo-code-v1",
        component_cursor_stream_format: "roo_nativepath_component_v1",
        task_cursor_stream_format: "roo_nativepath_task_v1",
        root_cursor_stream_format: "roo_nativepath_root_v1",
        page_publication_domain: b"ctx-roo-nativepath-publication-v1\0",
        page_publication_prefix: "roo-nativepath-v1:",
        task_publication_domain: b"ctx-roo-nativepath-task-checkpoint-v1\0",
        task_publication_prefix: "roo-nativepath-task-v1:",
        root_publication_domain: b"ctx-roo-nativepath-root-manifest-v1\0",
        root_publication_prefix: "roo-nativepath-root-v1:",
        retirement_publication_domain: b"ctx-roo-nativepath-route-retirement-v1\0",
        retirement_publication_prefix: "roo-nativepath-retire-v1:",
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

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClineFileStamp {
    len: u64,
    ordinary: OrdinaryFileObservation,
}

impl ClineFileStamp {
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(super) fn ordinary(&self) -> &OrdinaryFileObservation {
        &self.ordinary
    }

    pub(crate) fn token(&self) -> String {
        self.ordinary.token_hex()
    }
}

impl fmt::Debug for ClineFileStamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClineFileStamp")
            .field("len", &self.len)
            .field("ordinary_token", &self.ordinary.token_hex())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClineObservedFileState {
    Missing,
    Present(ClineFileStamp),
    Unavailable(Box<str>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineComponentObservation {
    pub(crate) component: ClineComponent,
    pub(crate) path: PathBuf,
    pub(crate) state: ClineObservedFileState,
}

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
        let current = observe_component_optional(&self.path, self.component)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

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
        let canonical = match fs::canonicalize(&self.requested_task_path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(source_access(&self.requested_task_path, error)),
        };
        Ok(canonical == self.canonical_task_path)
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
            if observe_task_component(&expected.path, component)? != *expected {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineRootAuthority {
    data_root: PathBuf,
    tasks_root: PathBuf,
    dialect: TaskJsonNativeDialect,
    inventory: Option<ClineRootInventoryProof>,
    complete: bool,
}

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
        ensure_directory_without_symlink(&self.data_root)?;
        ensure_directory_without_symlink(&self.tasks_root)?;
        let Some(expected) = &self.inventory else {
            return Ok(true);
        };
        Ok(observe_direct_child_inventory(&self.tasks_root, self.dialect)?.proof == *expected)
    }
}

struct ClineRootInventory {
    proof: ClineRootInventoryProof,
    task_paths: Vec<PathBuf>,
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
    let data_root = resolve_data_root(root, dialect)?;
    let tasks_root = data_root.join("tasks");
    ensure_directory_without_symlink(&data_root)?;
    ensure_directory_without_symlink(&tasks_root)?;
    let inventory = observe_direct_child_inventory(&tasks_root, dialect)?;
    let root_authority = ClineRootAuthority {
        data_root: fs::canonicalize(&data_root)
            .map_err(|error| source_access(&data_root, error))?,
        tasks_root: fs::canonicalize(&tasks_root)
            .map_err(|error| source_access(&tasks_root, error))?,
        dialect,
        inventory: Some(inventory.proof.clone()),
        complete: true,
    };
    let root_index = match dialect.root_index_file {
        Some(file) => observe_component_optional(
            &data_root.join("state").join(file),
            ClineComponent::RootIndex,
        )?,
        None => ClineComponentObservation {
            component: ClineComponent::RootIndex,
            path: data_root.join("state").join(ROOT_INDEX_FILE),
            state: ClineObservedFileState::Missing,
        },
    };
    let mut routes = Vec::with_capacity(inventory.task_paths.len());
    for path in inventory.task_paths {
        routes.push(observe_live_task(&path, dialect)?);
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
    path: &Path,
    component: ClineComponent,
    expected_stamp_token: &str,
) -> Result<bool, ClineNativePathError> {
    let current = observe_component_optional(path, component)?;
    let current_token = current
        .stamp()
        .map_or_else(|| "missing".to_owned(), ClineFileStamp::token);
    Ok(current_token == expected_stamp_token)
}

fn observe_live_task(
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<ClineLiveTaskObservation, ClineNativePathError> {
    ensure_directory_without_symlink(path)?;
    let canonical_task_path = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
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
        api_history: observe_task_component(&path.join(API_FILE), ClineComponent::ApiHistory)?,
        ui_messages: observe_task_component(&path.join(UI_FILE), ClineComponent::UiMessages)?,
        fallback_history: observe_task_component(
            &path.join(ROO_FALLBACK_FILE),
            ClineComponent::FallbackHistory,
        )?,
        task_metadata: observe_task_component(
            &path.join(METADATA_FILE),
            ClineComponent::TaskMetadata,
        )?,
        history_item: observe_task_component(
            &path.join(ROO_HISTORY_ITEM_FILE),
            ClineComponent::HistoryItem,
        )?,
        task_index: observe_task_component(
            &path.join(ROO_TASK_INDEX_FILE),
            ClineComponent::TaskIndex,
        )?,
    })
}

fn observe_component_optional(
    path: &Path,
    component: ClineComponent,
) -> Result<ClineComponentObservation, ClineNativePathError> {
    if let Some(error) = injected_io_failure(ClineInjectedIoOperation::ComponentStat, path) {
        return Err(source_io(path, "stat component", error));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ClineComponentObservation {
                component,
                path: path.to_path_buf(),
                state: ClineObservedFileState::Missing,
            });
        }
        Err(error) => return Err(source_access(path, error)),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(ClineNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Cline components must be ordinary files".to_owned(),
            });
        }
        Ok(_) => {}
    }
    ensure_provider_path_parents_are_not_symlinks(path)
        .map_err(|error| capture_source_error(path, "validate component parents", error))?;
    let ordinary = observe_ordinary_file(path)
        .map_err(|error| capture_source_error(path, "observe component metadata", error))?;
    Ok(ClineComponentObservation {
        component,
        path: path.to_path_buf(),
        state: ClineObservedFileState::Present(ClineFileStamp {
            len: ordinary.len(),
            ordinary,
        }),
    })
}

fn observe_task_component(
    path: &Path,
    component: ClineComponent,
) -> Result<ClineComponentObservation, ClineNativePathError> {
    match observe_component_optional(path, component) {
        Ok(observation) => Ok(observation),
        Err(error) if is_component_local_error(&error) => Ok(ClineComponentObservation {
            component,
            path: path.to_path_buf(),
            state: ClineObservedFileState::Unavailable(error.to_string().into_boxed_str()),
        }),
        Err(error) => Err(error),
    }
}

fn observe_direct_child_inventory(
    tasks_root: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<ClineRootInventory, ClineNativePathError> {
    let mut children = Vec::new();
    for entry in fs::read_dir(tasks_root).map_err(|error| source_access(tasks_root, error))? {
        let entry = entry.map_err(|error| source_access(tasks_root, error))?;
        if children.len() == MAX_TASK_DIRECT_CHILDREN {
            return Err(ClineNativePathError::SourceAccess {
                path: tasks_root.to_path_buf(),
                message: "Cline tasks root exceeds the 4096-child inventory bound".to_owned(),
            });
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| source_access(&path, error))?;
        if file_type.is_symlink() {
            return Err(ClineNativePathError::SourceAccess {
                path,
                message: "symlinked Cline task inventory entries are rejected".to_owned(),
            });
        }
        let mut component_states = Vec::new();
        let mut identity = [0_u8; 32];
        if file_type.is_dir() {
            for (file, _) in dialect.all_task_files() {
                component_states.push(inventory_component_state(&path.join(file))?);
            }
            identity = directory_inventory_identity(&path)?;
        }
        children.push((
            path,
            file_type.is_dir(),
            file_type.is_file(),
            component_states,
            identity,
        ));
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(ROOT_INVENTORY_DOMAIN);
    let mut task_paths = Vec::new();
    for (path, directory, file, states, identity) in &children {
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
        if *directory && states.iter().any(|state| *state != 0) {
            if task_paths.len() == MAX_TASK_DIRECTORIES {
                return Err(ClineNativePathError::SourceAccess {
                    path: tasks_root.to_path_buf(),
                    message: "Cline tasks root exceeds the 4096-task authority bound".to_owned(),
                });
            }
            task_paths.push(path.clone());
        }
    }
    Ok(ClineRootInventory {
        proof: ClineRootInventoryProof {
            entries: children.len(),
            digest: hasher.finalize().into(),
        },
        task_paths,
    })
}

fn directory_inventory_identity(path: &Path) -> Result<[u8; 32], ClineNativePathError> {
    let canonical = fs::canonicalize(path).map_err(|error| source_access(path, error))?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|error| source_access(&canonical, error))?;
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-directory-inventory-v1\0");
    hasher.update(canonical.as_os_str().as_encoded_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        hasher.update(metadata.mode().to_le_bytes());
        hasher.update(metadata.uid().to_le_bytes());
        hasher.update(metadata.gid().to_le_bytes());
    }
    #[cfg(not(unix))]
    hasher.update([u8::from(metadata.permissions().readonly())]);
    Ok(hasher.finalize().into())
}

fn inventory_component_state(path: &Path) -> Result<u8, ClineNativePathError> {
    let metadata = injected_io_failure(ClineInjectedIoOperation::InventoryComponentStat, path)
        .map_or_else(|| fs::symlink_metadata(path), Err);
    match metadata {
        Ok(metadata) if metadata.file_type().is_file() => Ok(1),
        Ok(_) => Ok(2),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => {
            let classified = source_io(path, "inventory component stat", error);
            if is_component_local_error(&classified) {
                Ok(3)
            } else {
                Err(classified)
            }
        }
    }
}

fn resolve_data_root(
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<PathBuf, ClineNativePathError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() {
        return Err(ClineNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "symlinked Cline roots are rejected".to_owned(),
        });
    }
    if metadata.file_type().is_file() {
        return match path.file_name().and_then(|value| value.to_str()) {
            Some(file) if dialect.root_index_file == Some(file) => path
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .ok_or_else(|| ClineNativePathError::UnsupportedRoot {
                    path: path.to_path_buf(),
                }),
            Some(file)
                if dialect
                    .all_task_files()
                    .any(|(candidate, _)| candidate == file) =>
            {
                task_dir_data_root(path.parent().unwrap_or(path))
            }
            _ => Err(ClineNativePathError::UnsupportedRoot {
                path: path.to_path_buf(),
            }),
        };
    }
    if !metadata.file_type().is_dir() {
        return Err(ClineNativePathError::UnsupportedRoot {
            path: path.to_path_buf(),
        });
    }
    if path.file_name().and_then(|value| value.to_str()) == Some("tasks") {
        return path.parent().map(Path::to_path_buf).ok_or_else(|| {
            ClineNativePathError::UnsupportedRoot {
                path: path.to_path_buf(),
            }
        });
    }
    if task_dir_has_component(path, dialect)? {
        return task_dir_data_root(path);
    }
    if path.join("tasks").is_dir() {
        return Ok(path.to_path_buf());
    }
    Err(ClineNativePathError::UnsupportedRoot {
        path: path.to_path_buf(),
    })
}

fn task_dir_data_root(task_dir: &Path) -> Result<PathBuf, ClineNativePathError> {
    let tasks = task_dir
        .parent()
        .filter(|path| path.file_name().and_then(|value| value.to_str()) == Some("tasks"))
        .ok_or_else(|| ClineNativePathError::UnsupportedRoot {
            path: task_dir.to_path_buf(),
        })?;
    tasks
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ClineNativePathError::UnsupportedRoot {
            path: task_dir.to_path_buf(),
        })
}

fn ensure_directory_without_symlink(path: &Path) -> Result<(), ClineNativePathError> {
    if let Some(error) = injected_io_failure(ClineInjectedIoOperation::RootAuthorityStat, path) {
        return Err(source_io(path, "stat root authority", error));
    }
    ensure_provider_path_parents_are_not_symlinks(path)
        .map_err(|error| capture_source_error(path, "validate directory parents", error))?;
    let metadata = fs::symlink_metadata(path).map_err(|error| source_access(path, error))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ClineNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Cline task roots must be ordinary directories".to_owned(),
        });
    }
    Ok(())
}

fn task_dir_has_component(
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<bool, ClineNativePathError> {
    for (file, _) in dialect.all_task_files() {
        let component = path.join(file);
        match fs::symlink_metadata(&component) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                let classified = source_io(&component, "stat task component", error);
                if is_component_local_error(&classified) {
                    return Ok(true);
                }
                return Err(classified);
            }
        }
    }
    Ok(false)
}

fn valid_task_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
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
