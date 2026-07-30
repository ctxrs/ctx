//! Bounded physical inventory for trees whose logical sources group event files.
//!
//! The inventory retains one root authority plus sorted leaf paths, metadata,
//! and strong observations. Discovery and terminal re-enumeration walk
//! depth-first, so directory handles are bounded by depth and leaf handles are
//! closed immediately after observation. Body reads reopen one exact leaf
//! through the retained authority, verify it before and after the bounded read,
//! and close it before returning bytes to the provider projector.

use std::{
    collections::BTreeMap,
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
    static ACTIVE_LEAF_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PEAK_LEAF_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ACTIVE_DIRECTORY_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PEAK_DIRECTORY_HANDLES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
type EventFileClassifier = fn(&Path) -> Result<Option<EventFileCoordinates>>;

#[derive(Debug)]
pub(crate) struct EventFileLeaf {
    selected_relative_path: PathBuf,
    display_path: PathBuf,
    coordinates: EventFileCoordinates,
    observation: OrdinaryFileObservation,
    metadata: Metadata,
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
        &self.metadata
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
        self.inventory.read_leaf(self.group_key(), leaf)
    }

    pub(crate) fn revalidate(&self) -> EventFileInventoryResult<()> {
        self.inventory.revalidate_group(self.group_key())
    }
}

#[derive(Debug)]
pub(crate) struct EventFileInventory {
    selected_path: PathBuf,
    selected_relative_path: PathBuf,
    selected_file: bool,
    root: ProviderSourceRoot,
    groups: Vec<OwnedEventFileGroup>,
    limits: EventFileLimits,
    classify: EventFileClassifier,
}

impl EventFileInventory {
    pub(crate) fn open(
        selected: &Path,
        limits: EventFileLimits,
        classify: EventFileClassifier,
    ) -> EventFileInventoryResult<Self> {
        #[cfg(test)]
        INVENTORY_OPENS.with(|opens| opens.set(opens.get().saturating_add(1)));

        let selected_path = normalized_absolute_path(selected)?;
        validate_path(&selected_path, limits.max_path_bytes)?;
        let opened = open_provider_source_path(&selected_path)
            .map_err(|error| unavailable(&selected_path, error))?;
        let (root, selected_relative_path, selected_file, initial_observation) = match opened {
            OpenedProviderSourcePath::File(file) => {
                if limits.max_entries == 0 {
                    return Err(EventFileInventoryError::LimitExceeded {
                        path: selected_path,
                        limit: EventFileLimit::Entries,
                        maximum: 0,
                        observed: 1,
                    });
                }
                let relative = selected_path
                    .file_name()
                    .map(PathBuf::from)
                    .ok_or_else(|| invalid(&selected_path, "selected file has no leaf name"))?;
                let parent = selected_path
                    .parent()
                    .ok_or_else(|| invalid(&selected_path, "selected file has no parent"))?;
                let initial_observation = {
                    let _handle = transient_handle(TransientHandleKind::Leaf);
                    let observation = observe_opened_ordinary_file(&selected_path, &file)
                        .map_err(|error| changed(&selected_path, error))?;
                    drop(file);
                    observation
                };
                let root =
                    ProviderSourceRoot::open(parent).map_err(|error| unavailable(parent, error))?;
                (root, relative, true, Some(initial_observation))
            }
            OpenedProviderSourcePath::Directory(directory) => {
                let root = directory.authority_root();
                drop(directory);
                (root, PathBuf::new(), false, None)
            }
        };
        let groups = discover_groups(
            &root,
            &selected_path,
            &selected_relative_path,
            selected_file,
            limits,
            classify,
            DiscoveryErrorMode::Unavailable,
        )?;
        if let Some(initial_observation) = initial_observation {
            let discovered = groups
                .first()
                .and_then(|group| group.leaves.first())
                .map(|leaf| &leaf.observation);
            if discovered != Some(&initial_observation) {
                return Err(source_changed(
                    &selected_path,
                    "selected event file changed while its retained parent authority was opened",
                ));
            }
        }
        Ok(Self {
            selected_path,
            selected_relative_path,
            selected_file,
            root,
            groups,
            limits,
            classify,
        })
    }

    pub(crate) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    #[cfg(test)]
    pub(crate) fn selected_file(&self) -> bool {
        self.selected_file
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn retained_authority_handles(&self) -> usize {
        1
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
            self.verify_leaf(leaf)?;
        }
        self.root
            .revalidate()
            .map_err(|error| changed(self.root.named_path(), error))
    }

    pub(crate) fn revalidate_all(&self) -> EventFileInventoryResult<()> {
        let current = discover_groups(
            &self.root,
            &self.selected_path,
            &self.selected_relative_path,
            self.selected_file,
            self.limits,
            self.classify,
            DiscoveryErrorMode::Changed,
        )
        .map_err(|error| match error {
            error @ EventFileInventoryError::SourceChanged { .. } => error,
            error => source_changed(
                &self.selected_path,
                format!("event-file terminal inventory changed: {error}"),
            ),
        })?;
        if !same_snapshot(&self.groups, &current) {
            return Err(source_changed(
                &self.selected_path,
                "event-file paths or metadata changed after inventory",
            ));
        }
        Ok(())
    }

    fn read_leaf(
        &self,
        group_key: &str,
        leaf: &EventFileLeaf,
    ) -> EventFileInventoryResult<Vec<u8>> {
        let group = self
            .groups
            .iter()
            .find(|group| group.group_key == group_key)
            .ok_or_else(|| EventFileInventoryError::MissingGroup(group_key.to_owned()))?;
        if !group
            .leaves
            .iter()
            .any(|candidate| std::ptr::eq(candidate, leaf))
        {
            return Err(invalid(
                leaf.display_path(),
                "event-file leaf does not belong to the requested retained group",
            ));
        }
        let opened = self.open_verified_leaf(leaf)?;
        let _handle = transient_handle(TransientHandleKind::Leaf);
        #[cfg(test)]
        BODY_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
        let bytes = opened
            .read_all_bounded(self.limits.max_record_bytes)
            .map_err(|error| changed(leaf.display_path(), error))?;
        let closing = observe_opened_ordinary_file(leaf.display_path(), &opened)
            .map_err(|error| changed(leaf.display_path(), error))?;
        if closing != leaf.observation {
            return Err(source_changed(
                leaf.display_path(),
                "event-file leaf fingerprint changed while its body was read",
            ));
        }
        Ok(bytes)
    }

    fn verify_leaf(&self, leaf: &EventFileLeaf) -> EventFileInventoryResult<()> {
        let _opened = self.open_verified_leaf(leaf)?;
        let _handle = transient_handle(TransientHandleKind::Leaf);
        Ok(())
    }

    fn open_verified_leaf(
        &self,
        leaf: &EventFileLeaf,
    ) -> EventFileInventoryResult<OpenedProviderSourceFile> {
        let opened = self
            .root
            .open_file(leaf.selected_relative_path())
            .map_err(|error| changed(leaf.display_path(), error))?;
        let _handle = transient_handle(TransientHandleKind::Leaf);
        let observation = observe_opened_ordinary_file(leaf.display_path(), &opened)
            .map_err(|error| changed(leaf.display_path(), error))?;
        if observation != leaf.observation {
            return Err(source_changed(
                leaf.display_path(),
                "event-file leaf fingerprint no longer matches its inventory",
            ));
        }
        Ok(opened)
    }
}

#[derive(Clone, Copy)]
enum DiscoveryErrorMode {
    Unavailable,
    Changed,
}

struct DiscoveryState {
    leaves: Vec<EventFileLeaf>,
    observed_entries: usize,
}

fn discover_groups(
    root: &ProviderSourceRoot,
    selected_path: &Path,
    selected_relative_path: &Path,
    selected_file: bool,
    limits: EventFileLimits,
    classify: EventFileClassifier,
    error_mode: DiscoveryErrorMode,
) -> EventFileInventoryResult<Vec<OwnedEventFileGroup>> {
    let mut state = DiscoveryState {
        leaves: Vec::new(),
        observed_entries: 0,
    };
    if selected_file {
        if limits.max_entries == 0 {
            return Err(EventFileInventoryError::LimitExceeded {
                path: selected_path.to_path_buf(),
                limit: EventFileLimit::Entries,
                maximum: 0,
                observed: 1,
            });
        }
        let opened = root
            .open_file(selected_relative_path)
            .map_err(|error| discovery_error(selected_path, error, error_mode))?;
        let _handle = transient_handle(TransientHandleKind::Leaf);
        let coordinates = classify(selected_path)
            .map_err(|error| invalid(selected_path, error.to_string()))?
            .ok_or_else(|| match error_mode {
                DiscoveryErrorMode::Unavailable => EventFileInventoryError::NoAcceptedExactFile {
                    path: selected_path.to_path_buf(),
                },
                DiscoveryErrorMode::Changed => source_changed(
                    selected_path,
                    "selected event file is no longer accepted by its classifier",
                ),
            })?;
        state.leaves.push(admit_leaf(
            selected_relative_path.to_path_buf(),
            selected_path.to_path_buf(),
            coordinates,
            opened,
            limits,
        )?);
    } else {
        let directory = root
            .open_directory(selected_relative_path)
            .map_err(|error| discovery_error(selected_path, error, error_mode))?;
        discover_directory(root, directory, 0, limits, classify, error_mode, &mut state)?;
    }
    root.revalidate()
        .map_err(|error| discovery_error(root.named_path(), error, error_mode))?;
    build_groups(state.leaves)
}

fn discover_directory(
    root: &ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    depth: usize,
    limits: EventFileLimits,
    classify: EventFileClassifier,
    error_mode: DiscoveryErrorMode,
    state: &mut DiscoveryState,
) -> EventFileInventoryResult<()> {
    let _handle = transient_handle(TransientHandleKind::Directory);
    let directory_path = root.named_path().join(directory.relative_path());
    if depth > limits.max_depth {
        return Err(EventFileInventoryError::LimitExceeded {
            path: directory_path,
            limit: EventFileLimit::Depth,
            maximum: limits.max_depth,
            observed: depth,
        });
    }
    let remaining = limits.max_entries.saturating_sub(state.observed_entries);
    let names = directory
        .entries(remaining.saturating_add(1))
        .map_err(|error| {
            inventory_entries_error(&directory_path, error, limits.max_entries, error_mode)
        })?;
    let next_count = state.observed_entries.saturating_add(names.len());
    if next_count > limits.max_entries {
        return Err(EventFileInventoryError::LimitExceeded {
            path: directory_path,
            limit: EventFileLimit::Entries,
            maximum: limits.max_entries,
            observed: next_count,
        });
    }
    state.observed_entries = next_count;

    for name in names {
        let relative_path = directory.relative_path().join(&name);
        let display_path = root.named_path().join(&relative_path);
        validate_path(&display_path, limits.max_path_bytes)?;
        match directory
            .open_child(&name)
            .map_err(|error| discovery_error(&display_path, error, error_mode))?
        {
            OpenedProviderSourcePath::Directory(child) => {
                discover_directory(
                    root,
                    child,
                    depth.saturating_add(1),
                    limits,
                    classify,
                    error_mode,
                    state,
                )?;
            }
            OpenedProviderSourcePath::File(file) => {
                let _handle = transient_handle(TransientHandleKind::Leaf);
                let Some(coordinates) = classify(&display_path)
                    .map_err(|error| invalid(&display_path, error.to_string()))?
                else {
                    continue;
                };
                state.leaves.push(admit_leaf(
                    relative_path,
                    display_path,
                    coordinates,
                    file,
                    limits,
                )?);
            }
        }
    }
    directory
        .revalidate()
        .map_err(|error| discovery_error(&directory_path, error, error_mode))
}

fn same_snapshot(left: &[OwnedEventFileGroup], right: &[OwnedEventFileGroup]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left_group, right_group)| {
            left_group.group_key == right_group.group_key
                && left_group.leaves.len() == right_group.leaves.len()
                && left_group.leaves.iter().zip(&right_group.leaves).all(
                    |(left_leaf, right_leaf)| {
                        left_leaf.selected_relative_path == right_leaf.selected_relative_path
                            && left_leaf.display_path == right_leaf.display_path
                            && left_leaf.coordinates == right_leaf.coordinates
                            && left_leaf.observation == right_leaf.observation
                    },
                )
        })
}

fn admit_leaf(
    selected_relative_path: PathBuf,
    display_path: PathBuf,
    coordinates: EventFileCoordinates,
    file: OpenedProviderSourceFile,
    limits: EventFileLimits,
) -> EventFileInventoryResult<EventFileLeaf> {
    validate_coordinates(&display_path, &coordinates, limits.max_path_bytes)?;
    let maximum_record_bytes = u64::try_from(limits.max_record_bytes).unwrap_or(u64::MAX);
    if file.len() > maximum_record_bytes {
        return Err(EventFileInventoryError::RecordTooLarge {
            path: display_path,
            maximum: limits.max_record_bytes,
            observed: file.len(),
        });
    }
    let observation = observe_opened_ordinary_file(&display_path, &file)
        .map_err(|error| changed(&display_path, error))?;
    let metadata = file.metadata().clone();
    Ok(EventFileLeaf {
        selected_relative_path,
        display_path,
        coordinates,
        observation,
        metadata,
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
    error_mode: DiscoveryErrorMode,
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
        discovery_error(path, error, error_mode)
    }
}

fn discovery_error(
    path: &Path,
    error: CaptureError,
    error_mode: DiscoveryErrorMode,
) -> EventFileInventoryError {
    match error_mode {
        DiscoveryErrorMode::Unavailable => unavailable(path, error),
        DiscoveryErrorMode::Changed => changed(path, error),
    }
}

fn changed(path: &Path, error: CaptureError) -> EventFileInventoryError {
    EventFileInventoryError::SourceChanged {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn source_changed(path: &Path, detail: impl Into<String>) -> EventFileInventoryError {
    EventFileInventoryError::SourceChanged {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

fn invalid(path: &Path, detail: impl Into<String>) -> EventFileInventoryError {
    EventFileInventoryError::InvalidPath {
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

#[derive(Clone, Copy)]
enum TransientHandleKind {
    Leaf,
    Directory,
}

#[cfg(not(test))]
struct TransientHandleGuard;

#[cfg(not(test))]
fn transient_handle(_kind: TransientHandleKind) -> TransientHandleGuard {
    TransientHandleGuard
}

#[cfg(test)]
struct TransientHandleGuard {
    kind: TransientHandleKind,
}

#[cfg(test)]
fn transient_handle(kind: TransientHandleKind) -> TransientHandleGuard {
    let (active, peak) = match kind {
        TransientHandleKind::Leaf => (&ACTIVE_LEAF_HANDLES, &PEAK_LEAF_HANDLES),
        TransientHandleKind::Directory => (&ACTIVE_DIRECTORY_HANDLES, &PEAK_DIRECTORY_HANDLES),
    };
    active.with(|active| {
        let next = active.get().saturating_add(1);
        active.set(next);
        peak.with(|peak| peak.set(peak.get().max(next)));
    });
    TransientHandleGuard { kind }
}

#[cfg(test)]
impl Drop for TransientHandleGuard {
    fn drop(&mut self) {
        let active = match self.kind {
            TransientHandleKind::Leaf => &ACTIVE_LEAF_HANDLES,
            TransientHandleKind::Directory => &ACTIVE_DIRECTORY_HANDLES,
        };
        active.with(|active| active.set(active.get().saturating_sub(1)));
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EventFileIoCounts {
    pub inventory_opens: usize,
    pub body_reads: usize,
    pub peak_transient_leaf_handles: usize,
    pub peak_transient_directory_handles: usize,
    pub active_transient_leaf_handles: usize,
    pub active_transient_directory_handles: usize,
}

#[cfg(test)]
pub(crate) fn count_event_file_io<T>(operation: impl FnOnce() -> T) -> (T, EventFileIoCounts) {
    INVENTORY_OPENS.with(|opens| opens.set(0));
    BODY_READS.with(|reads| reads.set(0));
    ACTIVE_LEAF_HANDLES.with(|handles| handles.set(0));
    PEAK_LEAF_HANDLES.with(|handles| handles.set(0));
    ACTIVE_DIRECTORY_HANDLES.with(|handles| handles.set(0));
    PEAK_DIRECTORY_HANDLES.with(|handles| handles.set(0));
    let output = operation();
    let inventory_opens = INVENTORY_OPENS.with(|opens| opens.replace(0));
    let body_reads = BODY_READS.with(|reads| reads.replace(0));
    let peak_transient_leaf_handles = PEAK_LEAF_HANDLES.with(|handles| handles.replace(0));
    let peak_transient_directory_handles =
        PEAK_DIRECTORY_HANDLES.with(|handles| handles.replace(0));
    let active_transient_leaf_handles = ACTIVE_LEAF_HANDLES.with(|handles| handles.replace(0));
    let active_transient_directory_handles =
        ACTIVE_DIRECTORY_HANDLES.with(|handles| handles.replace(0));
    (
        output,
        EventFileIoCounts {
            inventory_opens,
            body_reads,
            peak_transient_leaf_handles,
            peak_transient_directory_handles,
            active_transient_leaf_handles,
            active_transient_directory_handles,
        },
    )
}

#[cfg(test)]
#[path = "event_files/tests.rs"]
mod tests;
