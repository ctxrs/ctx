//! Bounded source discovery, immutable reads, and structured-container scanning.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::{fs::File, io::Read};

use ctx_history_core::CaptureProvider;

use super::verification::{ResolutionBudget, StructuredBounds, STRUCTURED_MAX_COMPOUND_FILE_BYTES};
#[cfg(unix)]
use crate::complete_content::source_access;
use crate::complete_content::AuthorizedSourceRoute;
use crate::complete_content::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceLocator,
    CompleteMessageRequest,
};
use crate::provider::provider_safe_path_segment;
use crate::{CODEBUDDY_SOURCE_FORMAT, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT, ROVODEV_SOURCE_FORMAT};
use uuid::Uuid;

mod manifest;
mod parsing;

pub(super) use parsing::{
    parse_bounded_json, task_json_records, validate_json_shape, TaskJsonRecord,
};

pub(super) trait ContentErrorContext {
    fn content_event_id(&self) -> Uuid;
}

impl ContentErrorContext for CompleteMessageRequest {
    fn content_event_id(&self) -> Uuid {
        self.event_id
    }
}

struct AdmissionContext<'a> {
    route: &'a AuthorizedSourceRoute,
    event_id: Uuid,
}

impl ContentErrorContext for AdmissionContext<'_> {
    fn content_event_id(&self) -> Uuid {
        self.event_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StructuredAdmissionTestStage {
    RootOpened,
    ChildOpened,
}

#[cfg(test)]
type StructuredAdmissionHook = Box<dyn FnMut(&Path, StructuredAdmissionTestStage)>;

#[cfg(test)]
thread_local! {
    static STRUCTURED_ADMISSION_TEST_HOOK: std::cell::RefCell<Option<StructuredAdmissionHook>>
        = std::cell::RefCell::new(None);
}

#[cfg(all(test, unix))]
pub(super) fn set_structured_admission_test_hook(hook: Option<StructuredAdmissionHook>) {
    STRUCTURED_ADMISSION_TEST_HOOK.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

#[cfg(test)]
fn test_hook(path: &Path, stage: StructuredAdmissionTestStage) {
    STRUCTURED_ADMISSION_TEST_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().as_mut() {
            hook(path, stage);
        }
    });
}

#[cfg(not(test))]
fn test_hook(_path: &Path, _stage: StructuredAdmissionTestStage) {}

pub(crate) struct StructuredSourceSnapshot {
    files: Vec<StructuredSnapshotFile>,
    observed_entries: usize,
    observed_bytes: usize,
    observed_max_depth: usize,
}

impl std::fmt::Debug for StructuredSourceSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredSourceSnapshot")
            .field("file_count", &self.files.len())
            .field("observed_entries", &self.observed_entries)
            .field("observed_bytes", &self.observed_bytes)
            .field("observed_max_depth", &self.observed_max_depth)
            .finish_non_exhaustive()
    }
}

impl StructuredSourceSnapshot {
    pub(super) fn files(&self) -> &[StructuredSnapshotFile] {
        &self.files
    }

    pub(super) fn validate_bounds(
        &self,
        bounds: StructuredBounds,
        request: &CompleteMessageRequest,
    ) -> std::result::Result<(), CompleteContentError> {
        if self.files.len() > bounds.max_files
            || self.observed_entries > bounds.max_entries
            || self.observed_bytes > bounds.max_total_read_bytes
            || self.observed_max_depth > bounds.max_depth
        {
            return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
        }
        if bounds.deadline.is_zero() {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
        Ok(())
    }
}

pub(super) struct StructuredSnapshotFile {
    logical_path: PathBuf,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for StructuredSnapshotFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StructuredSnapshotFile")
            .field("byte_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl StructuredSnapshotFile {
    pub(super) fn path(&self) -> &Path {
        &self.logical_path
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) fn admit_structured_source(
    route: &AuthorizedSourceRoute,
    locators: &[CompleteContentSourceLocator],
    event_id: Uuid,
) -> std::result::Result<StructuredSourceSnapshot, CompleteContentError> {
    let request = AdmissionContext { route, event_id };
    let bounds = StructuredBounds::default();
    let deadline = std::time::Instant::now() + bounds.deadline;
    let mut budget = ResolutionBudget::new(bounds, deadline);
    let roots = selected_roots(&request, locators, &mut budget)?;
    let files = candidate_files(&request, &roots, &mut budget)?;
    if files.is_empty() {
        return Err(error(&request, CompleteContentErrorKind::SourceMissing));
    }
    Ok(StructuredSourceSnapshot {
        files,
        observed_entries: budget.entries,
        observed_bytes: budget.bytes,
        observed_max_depth: budget.max_depth_seen,
    })
}

fn selected_roots(
    request: &AdmissionContext<'_>,
    locators: &[CompleteContentSourceLocator],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    if exact_raw_source_route(request.route) {
        if !exact_path_allowed_by_root(request, &request.route.raw_source_path) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        return Ok(vec![request.route.raw_source_path.clone()]);
    }
    if request.route.provider == CaptureProvider::CodeBuddy
        && request.route.source_format == CODEBUDDY_SOURCE_FORMAT
    {
        return exact_codebuddy_message_paths(request, locators);
    }

    let mut roots = BTreeSet::new();
    if request
        .route
        .raw_source_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| matches!(ext, "json5" | "xml"))
    {
        let bytes = read_frozen_file_with_limit(
            request,
            &request.route.raw_source_path,
            budget,
            STRUCTURED_MAX_COMPOUND_FILE_BYTES,
        )?;
        let configured = match request
            .route
            .raw_source_path
            .extension()
            .and_then(|value| value.to_str())
        {
            Some("json5") => manifest::profile_roots_from_json5(request, &bytes, budget)?,
            Some("xml") => manifest::profile_roots_from_xml(request, &bytes, budget)?,
            _ => Vec::new(),
        };
        for root in configured {
            if path_allowed_by_root(request, &root) {
                roots.insert(root);
            }
        }
    } else {
        roots.insert(request.route.raw_source_path.clone());
    }
    if let Some(root) = request.route.source_root.as_ref() {
        roots.insert(root.clone());
    }
    if roots.is_empty() {
        return Err(error(request, CompleteContentErrorKind::SourceMissing));
    }
    Ok(roots.into_iter().collect())
}

fn exact_raw_source_route(route: &AuthorizedSourceRoute) -> bool {
    (route.provider == CaptureProvider::OpenHands
        && route.source_format == OPENHANDS_FILE_EVENTS_SOURCE_FORMAT)
        || (route.provider == CaptureProvider::RovoDev
            && route.source_format == ROVODEV_SOURCE_FORMAT)
}

fn exact_file_route(route: &AuthorizedSourceRoute) -> bool {
    exact_raw_source_route(route)
        || (route.provider == CaptureProvider::CodeBuddy
            && route.source_format == CODEBUDDY_SOURCE_FORMAT)
}

fn exact_codebuddy_message_paths(
    request: &AdmissionContext<'_>,
    locators: &[CompleteContentSourceLocator],
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    let mut paths = BTreeSet::new();
    for locator in locators {
        let Some((provider, _, subrecord, native_id)) =
            super::contracts::decode_structured_locator(locator.value())
        else {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        };
        if provider != CaptureProvider::CodeBuddy || subrecord != 0 {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let Some((_, message_id)) = native_id.rsplit_once(':') else {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        };
        if !provider_safe_path_segment(message_id) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let raw = &request.route.raw_source_path;
        let raw_is_exact_message = raw
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            == Some("messages")
            && raw.file_stem().and_then(|value| value.to_str()) == Some(message_id);
        let exact = if raw_is_exact_message {
            raw.clone()
        } else {
            raw.join("messages").join(format!("{message_id}.json"))
        };
        if !exact_path_allowed_by_root(request, &exact)
            || (raw_is_exact_message && exact != *raw)
            || (!raw_is_exact_message && !exact.starts_with(raw))
        {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        paths.insert(exact);
    }
    if paths.is_empty() {
        return Err(error(
            request,
            CompleteContentErrorKind::HydrationUnsupported,
        ));
    }
    Ok(paths.into_iter().collect())
}

fn exact_path_allowed_by_root(request: &AdmissionContext<'_>, path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    #[cfg(target_os = "windows")]
    return match request.route.source_root.as_ref() {
        Some(root) => windows_path_within(path, root),
        None => windows_local_qualified(path),
    };
    #[cfg(target_os = "macos")]
    return match request.route.source_root.as_deref() {
        Some(root) => {
            let path = source_access::normalize_macos_fixed_root_alias(path);
            let root = source_access::normalize_macos_fixed_root_alias(root);
            path.starts_with(root)
        }
        None => true,
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    return request
        .route
        .source_root
        .as_ref()
        .is_none_or(|root| path.starts_with(root));
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = (request, path);
        false
    }
}

fn candidate_files(
    request: &AdmissionContext<'_>,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<StructuredSnapshotFile>, CompleteContentError> {
    let mut files: BTreeMap<PathBuf, StructuredSnapshotFile> = BTreeMap::new();
    for root in roots {
        if !path_allowed_by_root(request, root) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        #[cfg(unix)]
        {
            let opened = match source_access::unix::open_path_any(root) {
                Ok(opened) => opened,
                Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => continue,
                Err(cause) => return Err(map_unix_source_error(request, &cause)),
            };
            let root_before = FrozenFile::from_file(opened.file())
                .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
            test_hook(root, StructuredAdmissionTestStage::RootOpened);
            match opened {
                source_access::unix::OpenedPath::File(file) => {
                    budget.observe_file(request)?;
                    let bytes = read_opened_unix_file_with_limit(
                        request,
                        &file,
                        budget,
                        STRUCTURED_MAX_COMPOUND_FILE_BYTES,
                    )?;
                    files.insert(
                        root.clone(),
                        StructuredSnapshotFile {
                            logical_path: root.clone(),
                            bytes,
                        },
                    );
                }
                source_access::unix::OpenedPath::Directory(directory) => {
                    if exact_file_route(request.route) {
                        return Err(error(
                            request,
                            CompleteContentErrorKind::ContentVerificationFailed,
                        ));
                    }
                    collect_unix_files(request, root, &directory, 0, &mut files, budget)?;
                }
            }
            let selected_after = source_access::unix::open_path_any(root)
                .and_then(|selected| FrozenFile::from_file(selected.file()))
                .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
            if !root_before.same(&selected_after) {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
        }

        #[cfg(target_os = "windows")]
        {
            use crate::complete_content::source_access::windows::{self, AdmittedWindowsPath};
            let admitted = match windows::admit_path(
                root,
                windows_containment_root(request, root),
                request.event_id,
            ) {
                Ok(admitted) => admitted,
                Err(cause) if cause.kind == CompleteContentErrorKind::SourceMissing => continue,
                Err(cause) => return Err(cause),
            };
            let root_identity = match &admitted {
                AdmittedWindowsPath::File(file) => file.identity.clone(),
                AdmittedWindowsPath::Directory(directory) => directory.identity.clone(),
            };
            test_hook(root, StructuredAdmissionTestStage::RootOpened);
            match admitted {
                AdmittedWindowsPath::File(file) => {
                    budget.observe_file(request)?;
                    let file_bytes = usize::try_from(file.metadata.len()).unwrap_or(usize::MAX);
                    budget.observe_bytes(request, file_bytes)?;
                    let bytes = windows::read_bounded_admitted_regular_file(
                        &file,
                        STRUCTURED_MAX_COMPOUND_FILE_BYTES,
                        request.event_id,
                    )?;
                    files.insert(
                        root.clone(),
                        StructuredSnapshotFile {
                            logical_path: root.clone(),
                            bytes,
                        },
                    );
                }
                AdmittedWindowsPath::Directory(directory) => {
                    if exact_file_route(request.route) {
                        return Err(error(
                            request,
                            CompleteContentErrorKind::ContentVerificationFailed,
                        ));
                    }
                    collect_windows_files(
                        request,
                        root,
                        &directory,
                        &root_identity,
                        0,
                        &mut files,
                        budget,
                    )?;
                }
            }
            windows::verify_named_admitted_path_still_matches(
                root,
                &root_identity,
                request.event_id,
            )?;
        }

        #[cfg(not(any(unix, target_os = "windows")))]
        {
            let _ = (root, budget);
            return Err(error(
                request,
                CompleteContentErrorKind::HydrationUnsupported,
            ));
        }
    }
    Ok(files.into_values().collect())
}

#[cfg(unix)]
fn collect_unix_files(
    request: &AdmissionContext<'_>,
    logical_directory: &Path,
    directory: &File,
    depth: usize,
    files: &mut BTreeMap<PathBuf, StructuredSnapshotFile>,
    budget: &mut ResolutionBudget,
) -> std::result::Result<(), CompleteContentError> {
    if depth > budget.bounds.max_depth {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    budget.observe_depth(request, depth)?;
    budget.check(request)?;
    let before = FrozenFile::from_file(directory)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let entries = source_access::unix::directory_entries(
        directory,
        budget.bounds.max_entries.saturating_sub(budget.entries),
        budget.deadline,
    )
    .map_err(|cause| map_unix_directory_error(request, cause))?;
    for name in entries {
        budget.observe_entries(request, 1)?;
        budget.check(request)?;
        let logical_path = logical_directory.join(&name);
        let child = source_access::unix::open_child(directory, &name)
            .map_err(|cause| map_unix_source_error(request, &cause))?;
        let child_before = FrozenFile::from_file(child.file())
            .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
        test_hook(&logical_path, StructuredAdmissionTestStage::ChildOpened);
        match child {
            source_access::unix::OpenedPath::Directory(child_directory) => {
                collect_unix_files(
                    request,
                    &logical_path,
                    &child_directory,
                    depth.saturating_add(1),
                    files,
                    budget,
                )?;
            }
            source_access::unix::OpenedPath::File(file) => {
                budget.observe_file(request)?;
                let bytes = read_opened_unix_file_with_limit(
                    request,
                    &file,
                    budget,
                    STRUCTURED_MAX_COMPOUND_FILE_BYTES,
                )?;
                files.insert(
                    logical_path.clone(),
                    StructuredSnapshotFile {
                        logical_path: logical_path.clone(),
                        bytes,
                    },
                );
            }
        }
        let selected_after = source_access::unix::open_child(directory, &name)
            .and_then(|selected| FrozenFile::from_file(selected.file()))
            .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
        if !child_before.same(&selected_after) {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
    }
    let after = FrozenFile::from_file(directory)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if !before.same(&after) {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn collect_windows_files(
    request: &AdmissionContext<'_>,
    root: &Path,
    directory: &crate::complete_content::source_access::windows::AdmittedWindowsDirectory,
    retained_root: &crate::complete_content::source_access::windows::WindowsFileIdentity,
    depth: usize,
    files: &mut BTreeMap<PathBuf, StructuredSnapshotFile>,
    budget: &mut ResolutionBudget,
) -> std::result::Result<(), CompleteContentError> {
    if depth > budget.bounds.max_depth {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    budget.observe_depth(request, depth)?;
    budget.check(request)?;
    use crate::complete_content::source_access::windows::{self, AdmittedWindowsPath};

    windows::verify_named_directory_still_matches(root, &directory.identity, request.event_id)?;
    let entries = windows::directory_entries(
        directory,
        budget.bounds.max_entries.saturating_sub(budget.entries),
        budget.deadline,
        request.event_id,
    )?;
    for entry in entries {
        budget.observe_entries(request, 1)?;
        let path = root.join(&entry.name);
        let admitted = windows::admit_path_under_retained_directory(
            &path,
            &directory.identity,
            retained_root,
            entry.file_id,
            entry.attributes,
            request.event_id,
        )?;
        let child_identity = match &admitted {
            AdmittedWindowsPath::File(file) => file.identity.clone(),
            AdmittedWindowsPath::Directory(directory) => directory.identity.clone(),
        };
        test_hook(&path, StructuredAdmissionTestStage::ChildOpened);
        match admitted {
            AdmittedWindowsPath::Directory(child_directory) => {
                collect_windows_files(
                    request,
                    &path,
                    &child_directory,
                    retained_root,
                    depth.saturating_add(1),
                    files,
                    budget,
                )?;
            }
            AdmittedWindowsPath::File(file) => {
                budget.observe_file(request)?;
                let file_bytes = usize::try_from(file.metadata.len()).unwrap_or(usize::MAX);
                budget.observe_bytes(request, file_bytes)?;
                let bytes = windows::read_bounded_admitted_regular_file(
                    &file,
                    STRUCTURED_MAX_COMPOUND_FILE_BYTES,
                    request.event_id,
                )?;
                files.insert(
                    path.clone(),
                    StructuredSnapshotFile {
                        logical_path: path.clone(),
                        bytes,
                    },
                );
            }
        }
        windows::verify_named_admitted_path_still_matches(
            &path,
            &child_identity,
            request.event_id,
        )?;
    }
    windows::verify_named_directory_still_matches(root, &directory.identity, request.event_id)?;
    Ok(())
}

fn path_allowed_by_root(request: &AdmissionContext<'_>, path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    #[cfg(target_os = "windows")]
    return match request.route.source_root.as_ref() {
        Some(root) => {
            windows_path_within(path, root)
                || windows_path_equal(path, &request.route.raw_source_path)
        }
        None => windows_local_qualified(path),
    };
    #[cfg(target_os = "macos")]
    return match request.route.source_root.as_deref() {
        Some(root) => {
            let path = source_access::normalize_macos_fixed_root_alias(path);
            let root = source_access::normalize_macos_fixed_root_alias(root);
            let raw =
                source_access::normalize_macos_fixed_root_alias(&request.route.raw_source_path);
            path.starts_with(root) || path == raw
        }
        None => true,
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    return match request.route.source_root.as_ref() {
        Some(root) => path.starts_with(root) || path == request.route.raw_source_path,
        None => true,
    };
    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = (request, path);
        false
    }
}

#[cfg(target_os = "windows")]
fn windows_containment_root<'a>(
    request: &'a AdmissionContext<'_>,
    path: &Path,
) -> Option<&'a Path> {
    request
        .route
        .source_root
        .as_deref()
        .filter(|root| windows_path_within(path, root) || windows_path_equal(path, root))
}

#[cfg(target_os = "windows")]
fn windows_local_qualified(path: &Path) -> bool {
    use std::path::Prefix;
    let mut components = path.components();
    matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && matches!(components.next(), Some(Component::RootDir))
        && !components.any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
}

#[cfg(target_os = "windows")]
fn windows_path_within(path: &Path, root: &Path) -> bool {
    let path = windows_path_components(path);
    let root = windows_path_components(root);
    root.len() <= path.len()
        && root
            .iter()
            .zip(path.iter())
            .all(|(root, path)| windows_component_equal(root, path))
}

#[cfg(target_os = "windows")]
fn windows_path_equal(left: &Path, right: &Path) -> bool {
    let left = windows_path_components(left);
    let right = windows_path_components(right);
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| windows_component_equal(left, right))
}

#[cfg(target_os = "windows")]
fn windows_path_components(path: &Path) -> Vec<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Prefix;
    path.components()
        .map(|component| match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    vec![windows_ascii_lower(letter as u16), b':' as u16]
                }
                _ => component.as_os_str().encode_wide().collect(),
            },
            _ => component.as_os_str().encode_wide().collect(),
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn windows_component_equal(left: &[u16], right: &[u16]) -> bool {
    left.iter()
        .copied()
        .map(windows_ascii_lower)
        .eq(right.iter().copied().map(windows_ascii_lower))
}

#[cfg(target_os = "windows")]
fn windows_ascii_lower(value: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

pub(super) fn whole_json_path_candidate(provider: CaptureProvider, path: &Path) -> bool {
    let file_name = path.file_name().and_then(|value| value.to_str());
    match provider {
        CaptureProvider::Auggie => {
            path.extension().and_then(|value| value.to_str()) == Some("json")
        }
        CaptureProvider::Continue => {
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && file_name != Some("sessions.json")
        }
        CaptureProvider::RovoDev => file_name == Some("session_context.json"),
        _ => false,
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct FrozenFile {
    length: u64,
    modified: Option<std::time::SystemTime>,
    device: u64,
    inode: u64,
    ctime: i64,
    ctime_nsec: i64,
}

#[cfg(unix)]
impl FrozenFile {
    fn from_file(file: &File) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            device: metadata.dev(),
            inode: metadata.ino(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        })
    }

    fn same(&self, other: &Self) -> bool {
        self.length == other.length
            && self.modified == other.modified
            && self.device == other.device
            && self.inode == other.inode
            && self.ctime == other.ctime
            && self.ctime_nsec == other.ctime_nsec
    }
}

#[cfg(unix)]
fn read_frozen_file_with_limit(
    request: &AdmissionContext<'_>,
    path: &Path,
    budget: &mut ResolutionBudget,
    max_file_bytes: usize,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    let opened = source_access::unix::open_path_any(path)
        .map_err(|cause| map_unix_source_error(request, &cause))?;
    let source_access::unix::OpenedPath::File(file) = opened else {
        return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
    };
    let bytes = read_opened_unix_file_with_limit(request, &file, budget, max_file_bytes)?;
    let path_after = source_access::unix::open_path_any(path)
        .and_then(|selected| FrozenFile::from_file(selected.file()))
        .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
    let opened_after = FrozenFile::from_file(&file)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if !opened_after.same(&path_after) {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_opened_unix_file_with_limit(
    request: &AdmissionContext<'_>,
    file: &File,
    budget: &mut ResolutionBudget,
    max_file_bytes: usize,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    let opened = FrozenFile::from_file(file)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let file_bytes = usize::try_from(opened.length).unwrap_or(usize::MAX);
    budget.observe_bytes(request, file_bytes)?;
    if file_bytes > max_file_bytes {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let mut reader = file
        .try_clone()
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let mut bytes = Vec::with_capacity(file_bytes);
    reader
        .by_ref()
        .take((max_file_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let after = FrozenFile::from_file(file)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if bytes.len() > max_file_bytes
        || u64::try_from(bytes.len()).ok() != Some(opened.length)
        || !opened.same(&after)
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn map_unix_source_error(
    request: &AdmissionContext<'_>,
    cause: &std::io::Error,
) -> CompleteContentError {
    if source_path_replaced(cause) {
        error(request, CompleteContentErrorKind::SourceChanged)
    } else {
        match cause.kind() {
            std::io::ErrorKind::NotFound => error(request, CompleteContentErrorKind::SourceMissing),
            _ => error(request, CompleteContentErrorKind::SourceUnreadable),
        }
    }
}

#[cfg(unix)]
fn map_unix_directory_error(
    request: &AdmissionContext<'_>,
    cause: source_access::unix::DirectoryEntriesError,
) -> CompleteContentError {
    match cause {
        source_access::unix::DirectoryEntriesError::Io(cause) => {
            map_unix_source_error(request, &cause)
        }
        source_access::unix::DirectoryEntriesError::ContentTooLarge => {
            error(request, CompleteContentErrorKind::ContentTooLarge)
        }
        source_access::unix::DirectoryEntriesError::Deadline => {
            error(request, CompleteContentErrorKind::SourceChanged)
        }
    }
}

#[cfg(target_os = "windows")]
fn read_frozen_file_with_limit(
    request: &AdmissionContext<'_>,
    path: &Path,
    budget: &mut ResolutionBudget,
    max_file_bytes: usize,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    use crate::complete_content::source_access::windows::{self, AdmittedWindowsPath};

    if !path_allowed_by_root(request, path) {
        return Err(error(
            request,
            CompleteContentErrorKind::ContentVerificationFailed,
        ));
    }
    let admitted = windows::admit_path(
        path,
        windows_containment_root(request, path),
        request.event_id,
    )?;
    let AdmittedWindowsPath::File(file) = admitted else {
        return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
    };
    let file_bytes = usize::try_from(file.metadata.len()).unwrap_or(usize::MAX);
    budget.observe_bytes(request, file_bytes)?;
    if file_bytes > max_file_bytes {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let bytes =
        windows::read_bounded_admitted_regular_file(&file, max_file_bytes, request.event_id)?;
    windows::verify_named_admitted_path_still_matches(path, &file.identity, request.event_id)?;
    Ok(bytes)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn read_frozen_file_with_limit(
    request: &AdmissionContext<'_>,
    _path: &Path,
    _budget: &mut ResolutionBudget,
    _max_file_bytes: usize,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    Err(error(
        request,
        CompleteContentErrorKind::HydrationUnsupported,
    ))
}

#[cfg(unix)]
fn source_path_replaced(error: &std::io::Error) -> bool {
    let replaced = matches!(
        error.raw_os_error(),
        Some(libc::ELOOP) | Some(libc::ENOTDIR)
    );
    #[cfg(target_os = "freebsd")]
    let replaced = replaced || error.raw_os_error() == Some(libc::EMLINK);
    replaced
}

#[cfg(all(test, target_os = "freebsd"))]
#[test]
fn freebsd_emlink_is_a_replaced_source_path() {
    // FreeBSD reports EMLINK, rather than ELOOP, when O_NOFOLLOW rejects a
    // symlink opened with O_DIRECTORY.
    let cause = std::io::Error::from_raw_os_error(libc::EMLINK);
    assert!(source_path_replaced(&cause));
}

pub(super) fn error(
    request: &(impl ContentErrorContext + ?Sized),
    kind: CompleteContentErrorKind,
) -> CompleteContentError {
    CompleteContentError::new(kind, request.content_event_id())
}
