use std::{
    collections::{BTreeMap, VecDeque},
    fs::Metadata,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    CaptureError, Result,
};

use super::ordinary_file::{observe_opened_ordinary_file, OrdinaryFileObservation};

const GROUP_OBSERVATION_DOMAIN: &[u8] = b"ctx.event-files.group-observation.v1\0";
const INVENTORY_OBSERVATION_DOMAIN: &[u8] = b"ctx.event-files.inventory-observation.v1\0";

#[cfg(test)]
std::thread_local! {
    static INVENTORY_OPENS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static BODY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventFileLimits {
    pub max_depth: usize,
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_record_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EventFileCoordinates {
    pub group_key: String,
    pub relative_file_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventFileLimit {
    Depth,
    Entries,
    PathBytes,
}

impl std::fmt::Display for EventFileLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Depth => "depth",
            Self::Entries => "entries",
            Self::PathBytes => "path_bytes",
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum EventFileInventoryError {
    #[error("event-file source {path:?} is unavailable: {detail}")]
    Unavailable { path: PathBuf, detail: String },
    #[error("event-file source {path:?} changed while retained: {detail}")]
    SourceChanged { path: PathBuf, detail: String },
    #[error(
        "event-file inventory exceeded {limit} limit at {path:?}: observed {observed}, maximum {maximum}"
    )]
    LimitExceeded {
        path: PathBuf,
        limit: EventFileLimit,
        maximum: usize,
        observed: usize,
    },
    #[error("event-file path {path:?} is invalid: {detail}")]
    InvalidPath { path: PathBuf, detail: String },
    #[error(
        "event-file record {path:?} exceeds the bounded record limit: observed {observed}, maximum {maximum}"
    )]
    RecordTooLarge {
        path: PathBuf,
        maximum: usize,
        observed: u64,
    },
    #[error("selected event file {path:?} was not accepted by the provider classifier")]
    NoAcceptedExactFile { path: PathBuf },
    #[error(
        "event-file coordinate {group_key:?}/{relative_file_key:?} is provided by more than one retained leaf"
    )]
    DuplicateCoordinate {
        group_key: String,
        relative_file_key: String,
    },
    #[error("event-file group {0:?} is not present in the retained inventory")]
    MissingGroup(String),
}

pub(crate) type EventFileInventoryResult<T> = std::result::Result<T, EventFileInventoryError>;

#[derive(Debug)]
pub(crate) struct EventFileLeaf {
    selected_relative_path: PathBuf,
    display_path: PathBuf,
    coordinates: EventFileCoordinates,
    observation: OrdinaryFileObservation,
    opened: OpenedProviderSourceFile,
}

impl EventFileLeaf {
    pub(crate) fn selected_relative_path(&self) -> &Path {
        &self.selected_relative_path
    }

    pub(crate) fn display_path(&self) -> &Path {
        &self.display_path
    }

    pub(crate) fn coordinates(&self) -> &EventFileCoordinates {
        &self.coordinates
    }

    pub(crate) fn metadata(&self) -> &Metadata {
        self.opened.metadata()
    }
}

#[derive(Debug)]
struct OwnedEventFileGroup {
    group_key: String,
    leaves: Vec<EventFileLeaf>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EventFileGroup<'inventory> {
    inventory: &'inventory EventFileInventory,
    index: usize,
}

impl<'inventory> EventFileGroup<'inventory> {
    fn owned(&self) -> &'inventory OwnedEventFileGroup {
        &self.inventory.groups[self.index]
    }

    pub(crate) fn group_key(&self) -> &'inventory str {
        &self.owned().group_key
    }

    pub(crate) fn leaves(&self) -> &'inventory [EventFileLeaf] {
        &self.owned().leaves
    }

    pub(crate) fn observation_digest(&self) -> [u8; 32] {
        group_observation_digest(self.owned())
    }

    pub(crate) fn read_leaf(&self, leaf: &EventFileLeaf) -> EventFileInventoryResult<Vec<u8>> {
        #[cfg(test)]
        BODY_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
        leaf.opened
            .read_all_bounded(self.inventory.limits.max_record_bytes)
            .map_err(|error| changed(leaf.display_path(), error))
    }

    pub(crate) fn revalidate(&self) -> EventFileInventoryResult<()> {
        self.inventory.revalidate_group(self.group_key())
    }
}

#[derive(Debug)]
pub(crate) struct EventFileInventory {
    selected_path: PathBuf,
    selected_file: bool,
    root: Option<ProviderSourceRoot>,
    retained_directories: Vec<ProviderSourceDirectory>,
    groups: Vec<OwnedEventFileGroup>,
    limits: EventFileLimits,
}

impl EventFileInventory {
    pub(crate) fn open(
        selected: &Path,
        limits: EventFileLimits,
        mut classify: impl FnMut(&Path) -> Result<Option<EventFileCoordinates>>,
    ) -> EventFileInventoryResult<Self> {
        #[cfg(test)]
        INVENTORY_OPENS.with(|opens| opens.set(opens.get().saturating_add(1)));

        let selected_path = normalized_absolute_path(selected)?;
        validate_path(&selected_path, limits.max_path_bytes)?;
        let opened = open_provider_source_path(&selected_path)
            .map_err(|error| unavailable(&selected_path, error))?;
        match opened {
            OpenedProviderSourcePath::File(file) => {
                if limits.max_entries == 0 {
                    return Err(EventFileInventoryError::LimitExceeded {
                        path: selected_path,
                        limit: EventFileLimit::Entries,
                        maximum: 0,
                        observed: 1,
                    });
                }
                let coordinates = classify(&selected_path)
                    .map_err(|error| invalid(&selected_path, error.to_string()))?
                    .ok_or_else(|| EventFileInventoryError::NoAcceptedExactFile {
                        path: selected_path.clone(),
                    })?;
                let relative = selected_path
                    .file_name()
                    .map(PathBuf::from)
                    .ok_or_else(|| invalid(&selected_path, "selected file has no leaf name"))?;
                let leaf = admit_leaf(relative, selected_path.clone(), coordinates, file, limits)?;
                let groups = build_groups(vec![leaf])?;
                let inventory = Self {
                    selected_path,
                    selected_file: true,
                    root: None,
                    retained_directories: Vec::new(),
                    groups,
                    limits,
                };
                inventory.revalidate_all()?;
                Ok(inventory)
            }
            OpenedProviderSourcePath::Directory(directory) => {
                let root = directory.authority_root();
                let mut pending = VecDeque::from([(directory, 0_usize)]);
                let mut retained_directories = Vec::new();
                let mut leaves = Vec::new();
                let mut observed_entries = 0_usize;

                while let Some((directory, depth)) = pending.pop_front() {
                    if depth > limits.max_depth {
                        return Err(EventFileInventoryError::LimitExceeded {
                            path: root.named_path().join(directory.relative_path()),
                            limit: EventFileLimit::Depth,
                            maximum: limits.max_depth,
                            observed: depth,
                        });
                    }
                    let names = directory
                        .entries(limits.max_entries.saturating_add(1))
                        .map_err(|error| {
                            inventory_entries_error(
                                &root.named_path().join(directory.relative_path()),
                                error,
                                limits.max_entries,
                            )
                        })?;
                    let next_count = observed_entries.saturating_add(names.len());
                    if next_count > limits.max_entries {
                        return Err(EventFileInventoryError::LimitExceeded {
                            path: root.named_path().join(directory.relative_path()),
                            limit: EventFileLimit::Entries,
                            maximum: limits.max_entries,
                            observed: next_count,
                        });
                    }
                    observed_entries = next_count;

                    for name in names {
                        let relative_path = directory.relative_path().join(&name);
                        let display_path = root.named_path().join(&relative_path);
                        validate_path(&display_path, limits.max_path_bytes)?;
                        match directory
                            .open_child(&name)
                            .map_err(|error| unavailable(&display_path, error))?
                        {
                            OpenedProviderSourcePath::Directory(child) => {
                                let child_depth = depth.saturating_add(1);
                                if child_depth > limits.max_depth {
                                    return Err(EventFileInventoryError::LimitExceeded {
                                        path: display_path,
                                        limit: EventFileLimit::Depth,
                                        maximum: limits.max_depth,
                                        observed: child_depth,
                                    });
                                }
                                pending.push_back((child, child_depth));
                            }
                            OpenedProviderSourcePath::File(file) => {
                                let Some(coordinates) = classify(&display_path)
                                    .map_err(|error| invalid(&display_path, error.to_string()))?
                                else {
                                    file.revalidate()
                                        .map_err(|error| changed(&display_path, error))?;
                                    continue;
                                };
                                leaves.push(admit_leaf(
                                    relative_path,
                                    display_path,
                                    coordinates,
                                    file,
                                    limits,
                                )?);
                            }
                        }
                    }
                    directory.revalidate().map_err(|error| {
                        changed(&root.named_path().join(directory.relative_path()), error)
                    })?;
                    retained_directories.push(directory);
                }
                root.revalidate()
                    .map_err(|error| changed(root.named_path(), error))?;
                let groups = build_groups(leaves)?;
                let inventory = Self {
                    selected_path,
                    selected_file: false,
                    root: Some(root),
                    retained_directories,
                    groups,
                    limits,
                };
                inventory.revalidate_all()?;
                Ok(inventory)
            }
        }
    }

    pub(crate) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    pub(crate) fn selected_file(&self) -> bool {
        self.selected_file
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub(crate) fn groups(
        &self,
    ) -> impl ExactSizeIterator<Item = EventFileGroup<'_>> + DoubleEndedIterator {
        (0..self.groups.len()).map(|index| EventFileGroup {
            inventory: self,
            index,
        })
    }

    pub(crate) fn group(&self, group_key: &str) -> Option<EventFileGroup<'_>> {
        self.groups
            .binary_search_by(|group| group.group_key.as_str().cmp(group_key))
            .ok()
            .map(|index| EventFileGroup {
                inventory: self,
                index,
            })
    }

    pub(crate) fn observation_digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(INVENTORY_OBSERVATION_DOMAIN);
        digest.update((self.groups.len() as u64).to_be_bytes());
        for group in &self.groups {
            hash_text(&mut digest, &group.group_key);
            digest.update(group_observation_digest(group));
        }
        digest.finalize().into()
    }

    pub(crate) fn revalidate_group(&self, group_key: &str) -> EventFileInventoryResult<()> {
        let group = self
            .groups
            .iter()
            .find(|group| group.group_key == group_key)
            .ok_or_else(|| EventFileInventoryError::MissingGroup(group_key.to_owned()))?;
        for leaf in &group.leaves {
            leaf.opened
                .revalidate()
                .map_err(|error| changed(leaf.display_path(), error))?;
        }
        self.revalidate_directories_and_root()
    }

    pub(crate) fn revalidate_all(&self) -> EventFileInventoryResult<()> {
        for group in &self.groups {
            for leaf in &group.leaves {
                leaf.opened
                    .revalidate()
                    .map_err(|error| changed(leaf.display_path(), error))?;
            }
        }
        self.revalidate_directories_and_root()
    }

    fn revalidate_directories_and_root(&self) -> EventFileInventoryResult<()> {
        for directory in &self.retained_directories {
            let path = self
                .root
                .as_ref()
                .map(|root| root.named_path().join(directory.relative_path()))
                .unwrap_or_else(|| self.selected_path.clone());
            directory
                .revalidate()
                .map_err(|error| changed(&path, error))?;
        }
        if let Some(root) = &self.root {
            root.revalidate()
                .map_err(|error| changed(root.named_path(), error))?;
        }
        Ok(())
    }
}

fn admit_leaf(
    selected_relative_path: PathBuf,
    display_path: PathBuf,
    coordinates: EventFileCoordinates,
    opened: OpenedProviderSourceFile,
    limits: EventFileLimits,
) -> EventFileInventoryResult<EventFileLeaf> {
    validate_coordinates(&display_path, &coordinates, limits.max_path_bytes)?;
    let maximum_record_bytes = u64::try_from(limits.max_record_bytes).unwrap_or(u64::MAX);
    if opened.len() > maximum_record_bytes {
        return Err(EventFileInventoryError::RecordTooLarge {
            path: display_path,
            maximum: limits.max_record_bytes,
            observed: opened.len(),
        });
    }
    let observation = observe_opened_ordinary_file(&display_path, &opened)
        .map_err(|error| changed(&display_path, error))?;
    Ok(EventFileLeaf {
        selected_relative_path,
        display_path,
        coordinates,
        observation,
        opened,
    })
}

fn build_groups(leaves: Vec<EventFileLeaf>) -> EventFileInventoryResult<Vec<OwnedEventFileGroup>> {
    let mut grouped = BTreeMap::<String, BTreeMap<String, EventFileLeaf>>::new();
    for leaf in leaves {
        let group_key = leaf.coordinates.group_key.clone();
        let relative_file_key = leaf.coordinates.relative_file_key.clone();
        if grouped
            .entry(group_key.clone())
            .or_default()
            .insert(relative_file_key.clone(), leaf)
            .is_some()
        {
            return Err(EventFileInventoryError::DuplicateCoordinate {
                group_key,
                relative_file_key,
            });
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(group_key, leaves)| OwnedEventFileGroup {
            group_key,
            leaves: leaves.into_values().collect(),
        })
        .collect())
}

fn validate_coordinates(
    display_path: &Path,
    coordinates: &EventFileCoordinates,
    maximum_path_bytes: usize,
) -> EventFileInventoryResult<()> {
    if coordinates.group_key.is_empty() || coordinates.group_key.len() > maximum_path_bytes {
        return Err(invalid(
            display_path,
            "event-file group keys must be nonempty and bounded",
        ));
    }
    let relative = Path::new(&coordinates.relative_file_key);
    if coordinates.relative_file_key.is_empty()
        || coordinates.relative_file_key.len() > maximum_path_bytes
        || coordinates.relative_file_key.contains('\\')
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid(
            display_path,
            "event-file locator keys must contain only normal relative UTF-8 components",
        ));
    }
    Ok(())
}

fn normalized_absolute_path(path: &Path) -> EventFileInventoryResult<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| unavailable(path, CaptureError::Io(error)))?
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
                    return Err(invalid(
                        path,
                        "event-file selections cannot escape the filesystem root",
                    ));
                }
            }
        }
    }
    if !normalized.is_absolute() {
        return Err(invalid(
            path,
            "event-file selections must resolve to an absolute path",
        ));
    }
    Ok(normalized)
}

fn validate_path(path: &Path, maximum: usize) -> EventFileInventoryResult<()> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid(path, "event-file paths must be valid UTF-8"))?;
    if text.len() > maximum {
        return Err(EventFileInventoryError::LimitExceeded {
            path: path.to_path_buf(),
            limit: EventFileLimit::PathBytes,
            maximum,
            observed: text.len(),
        });
    }
    Ok(())
}

fn group_observation_digest(group: &OwnedEventFileGroup) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(GROUP_OBSERVATION_DOMAIN);
    hash_text(&mut digest, &group.group_key);
    digest.update((group.leaves.len() as u64).to_be_bytes());
    for leaf in &group.leaves {
        hash_text(&mut digest, &leaf.coordinates.relative_file_key);
        digest.update(leaf.observation.len().to_be_bytes());
        hash_system_time(&mut digest, leaf.observation.modified_at());
        digest.update(leaf.observation.token());
    }
    digest.finalize().into()
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hash_system_time(digest: &mut Sha256, value: SystemTime) {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            digest.update([0]);
            digest.update(duration.as_secs().to_be_bytes());
            digest.update(duration.subsec_nanos().to_be_bytes());
        }
        Err(error) => {
            let duration = error.duration();
            digest.update([1]);
            digest.update(duration.as_secs().to_be_bytes());
            digest.update(duration.subsec_nanos().to_be_bytes());
        }
    }
}

fn unavailable(path: &Path, error: CaptureError) -> EventFileInventoryError {
    EventFileInventoryError::Unavailable {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn inventory_entries_error(
    path: &Path,
    error: CaptureError,
    maximum: usize,
) -> EventFileInventoryError {
    if matches!(
        &error,
        CaptureError::InvalidProviderTranscriptPath { reason, .. }
            if *reason == "provider source directory exceeds its bounded entry budget"
    ) {
        EventFileInventoryError::LimitExceeded {
            path: path.to_path_buf(),
            limit: EventFileLimit::Entries,
            maximum,
            observed: maximum.saturating_add(1),
        }
    } else {
        unavailable(path, error)
    }
}

fn changed(path: &Path, error: CaptureError) -> EventFileInventoryError {
    EventFileInventoryError::SourceChanged {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn invalid(path: &Path, detail: impl Into<String>) -> EventFileInventoryError {
    EventFileInventoryError::InvalidPath {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EventFileIoCounts {
    pub inventory_opens: usize,
    pub body_reads: usize,
}

#[cfg(test)]
pub(crate) fn count_event_file_io<T>(operation: impl FnOnce() -> T) -> (T, EventFileIoCounts) {
    INVENTORY_OPENS.with(|opens| opens.set(0));
    BODY_READS.with(|reads| reads.set(0));
    let output = operation();
    let inventory_opens = INVENTORY_OPENS.with(|opens| opens.replace(0));
    let body_reads = BODY_READS.with(|reads| reads.replace(0));
    (
        output,
        EventFileIoCounts {
            inventory_opens,
            body_reads,
        },
    )
}

#[cfg(test)]
#[path = "event_files/tests.rs"]
mod tests;
