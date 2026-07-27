//! Admission boundary for reopening provider-owned complete content.
//!
//! The Store supplies an [`AuthorizedSourceRoute`]. The broker validates and
//! freezes that route once, then resolvers receive only [`BrokeredSourceAccess`].
//! Paths never cross the resolver request boundary.

use std::{
    fmt, fs, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[cfg(not(target_os = "windows"))]
use std::fs::File;

use ctx_history_core::CaptureProvider;
use tempfile::TempDir;
use uuid::Uuid;

use super::{
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceFamily,
    CompleteContentSourceLocator, SourceSnapshot,
};
use crate::{
    native_source::NativeLocator,
    provider::{
        providers::nanoclaw,
        sqlite::{open_provider_sqlite_readonly, ReadOnlySqliteConnection},
    },
    CaptureError, NANOCLAW_SOURCE_FORMAT,
};

mod identity;
mod jsonl;
mod jsonl_auxiliary;
mod nanoclaw_snapshot;
#[cfg(unix)]
pub(crate) mod unix;
use identity::FrozenFile;
#[cfg(unix)]
use std::io::Read;

#[cfg(test)]
pub(crate) use nanoclaw_snapshot::set_before_source_set_revalidation as set_nanoclaw_before_source_set_revalidation;

#[cfg(target_os = "windows")]
#[path = "source_access/windows.rs"]
pub(crate) mod windows;

const SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES: u64 = 512 * 1024 * 1024;

/// Store-authorized route used only while admitting source access.
///
/// This is the sole path-bearing complete-content API. It is deliberately
/// separate from message/result resolver requests.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizedSourceRoute {
    pub source_id: Uuid,
    pub provider: CaptureProvider,
    pub source_format: String,
    pub family: CompleteContentSourceFamily,
    pub raw_source_path: PathBuf,
    pub source_root: Option<PathBuf>,
    pub source_identity: Option<String>,
    pub source_snapshot: SourceSnapshot,
}

impl fmt::Debug for AuthorizedSourceRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedSourceRoute")
            .field("source_id", &self.source_id)
            .field("provider", &self.provider)
            .field("source_format", &self.source_format)
            .field("family", &self.family)
            .field("source_snapshot", &self.source_snapshot)
            .finish_non_exhaustive()
    }
}

/// Stateless admission service. Future Store route revisions can be consumed
/// here without exposing their path representation to provider resolvers.
#[derive(Debug, Default, Clone, Copy)]
pub struct SourceAccessBroker;

impl SourceAccessBroker {
    pub const fn new() -> Self {
        Self
    }

    pub fn admit(
        &self,
        route: AuthorizedSourceRoute,
        event_id: Uuid,
    ) -> Result<BrokeredSourceAccess, CompleteContentError> {
        self.admit_for_source_locators(route, &[], event_id)
    }

    /// Admits one route together with the exact provider-native records that
    /// the caller will resolve. Compound providers use this selection to freeze
    /// only the required component sources before resolver code runs.
    pub fn admit_for_source_locators(
        &self,
        route: AuthorizedSourceRoute,
        locators: &[CompleteContentSourceLocator],
        event_id: Uuid,
    ) -> Result<BrokeredSourceAccess, CompleteContentError> {
        if route.source_identity.as_deref().is_some_and(str::is_empty) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        }
        admit_platform(route, locators, event_id)
    }
}

/// Opaque capability shared by every request for one admitted source.
#[derive(Clone)]
pub struct BrokeredSourceAccess {
    source_id: Uuid,
    inner: Arc<BrokeredSource>,
}

impl BrokeredSourceAccess {
    pub fn source_id(&self) -> Uuid {
        self.source_id
    }

    pub fn family(&self) -> CompleteContentSourceFamily {
        match self.inner.as_ref() {
            BrokeredSource::Jsonl(_) => CompleteContentSourceFamily::Jsonl,
            BrokeredSource::Sqlite(_) => CompleteContentSourceFamily::Sqlite,
            BrokeredSource::NanoClaw(_) => CompleteContentSourceFamily::Sqlite,
            BrokeredSource::Structured(_) => CompleteContentSourceFamily::Structured,
            #[cfg(test)]
            BrokeredSource::Fixture => CompleteContentSourceFamily::Fixture,
        }
    }

    pub(crate) fn open_sqlite_snapshot(
        &self,
        event_id: Uuid,
    ) -> Result<ReadOnlySqliteConnection, CompleteContentError> {
        let BrokeredSource::Sqlite(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        open_provider_sqlite_readonly(&source.path)
            .map_err(|cause| map_capture_error(event_id, cause))
    }

    pub(crate) fn open_nanoclaw_project(
        &self,
        locators: &[NativeLocator],
        query_budget: super::sqlite::CompleteContentSqliteQueryBudget,
        event_id: Uuid,
    ) -> Result<nanoclaw::NanoClawCompleteProject, CompleteContentError> {
        let BrokeredSource::NanoClaw(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.open(locators, query_budget, event_id)
    }

    pub(crate) fn structured_snapshot(
        &self,
        event_id: Uuid,
    ) -> Result<&super::structured::source_access::StructuredSourceSnapshot, CompleteContentError>
    {
        let BrokeredSource::Structured(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        Ok(source)
    }

    #[cfg(test)]
    pub(crate) fn fixture(source_id: Uuid) -> Self {
        Self {
            source_id,
            inner: Arc::new(BrokeredSource::Fixture),
        }
    }
}

impl PartialEq for BrokeredSourceAccess {
    fn eq(&self, other: &Self) -> bool {
        self.source_id == other.source_id && Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for BrokeredSourceAccess {}

impl fmt::Debug for BrokeredSourceAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokeredSourceAccess")
            .field("source_id", &self.source_id)
            .field("family", &self.family())
            .finish_non_exhaustive()
    }
}

// The enum already has one heap allocation and shared ownership through the
// enclosing `Arc`. On Windows, broker-retained handle identities enlarge the
// JSONL variant; adding a second box would only add indirection per source.
#[cfg_attr(
    target_os = "windows",
    allow(
        clippy::large_enum_variant,
        reason = "the enclosing Arc already bounds stack use"
    )
)]
enum BrokeredSource {
    Jsonl(jsonl::BrokeredJsonlSource),
    Sqlite(BrokeredSqliteSource),
    NanoClaw(BrokeredNanoClawSource),
    Structured(super::structured::source_access::StructuredSourceSnapshot),
    #[cfg(test)]
    Fixture,
}

struct BrokeredSqliteSource {
    _dir: TempDir,
    path: PathBuf,
}

struct BrokeredNanoClawSource(nanoclaw_snapshot::BrokeredNanoClawSnapshot);

impl BrokeredNanoClawSource {
    fn open(
        &self,
        locators: &[NativeLocator],
        query_budget: super::sqlite::CompleteContentSqliteQueryBudget,
        event_id: Uuid,
    ) -> Result<nanoclaw::NanoClawCompleteProject, CompleteContentError> {
        self.0.open(locators, query_budget, event_id)
    }
}

#[cfg(any(unix, target_os = "windows"))]
fn admit_platform(
    route: AuthorizedSourceRoute,
    locators: &[CompleteContentSourceLocator],
    event_id: Uuid,
) -> Result<BrokeredSourceAccess, CompleteContentError> {
    let source_id = route.source_id;
    let inner = match route.family {
        CompleteContentSourceFamily::Jsonl => {
            let selected = selected_source_path(&route, event_id)?;
            BrokeredSource::Jsonl(jsonl::admit(route, selected, event_id)?)
        }
        CompleteContentSourceFamily::Sqlite => {
            let selected = selected_source_path(&route, event_id)?;
            if route.provider == CaptureProvider::NanoClaw
                && route.source_format == NANOCLAW_SOURCE_FORMAT
            {
                BrokeredSource::NanoClaw(admit_nanoclaw(&route, &selected, locators, event_id)?)
            } else {
                BrokeredSource::Sqlite(admit_sqlite(&route, &selected, event_id)?)
            }
        }
        CompleteContentSourceFamily::Structured => BrokeredSource::Structured(
            super::structured::source_access::admit_structured_source(&route, event_id)?,
        ),
        #[cfg(test)]
        CompleteContentSourceFamily::Fixture => BrokeredSource::Fixture,
    };
    Ok(BrokeredSourceAccess {
        source_id,
        inner: Arc::new(inner),
    })
}

fn admit_nanoclaw(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    locators: &[CompleteContentSourceLocator],
    event_id: Uuid,
) -> Result<BrokeredNanoClawSource, CompleteContentError> {
    nanoclaw_snapshot::BrokeredNanoClawSnapshot::admit(route, selected_path, locators, event_id)
        .map(BrokeredNanoClawSource)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn admit_platform(
    _route: AuthorizedSourceRoute,
    _locators: &[CompleteContentSourceLocator],
    event_id: Uuid,
) -> Result<BrokeredSourceAccess, CompleteContentError> {
    Err(CompleteContentError::new(
        CompleteContentErrorKind::HydrationUnsupported,
        event_id,
    ))
}

#[cfg(unix)]
fn admit_sqlite(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    event_id: Uuid,
) -> Result<BrokeredSqliteSource, CompleteContentError> {
    use std::os::unix::fs::PermissionsExt;

    let main = open_brokered_file(selected_path).map_err(|cause| map_io_error(event_id, cause))?;
    let metadata = main
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if metadata.permissions().mode() & 0o444 == 0 {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    validate_source_snapshot(&route.source_snapshot, &metadata, event_id)?;
    let main_frozen = FrozenFile::from_metadata(&metadata).map_err(|_| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    let mut sidecars = Vec::new();
    for suffix in ["-wal", "-shm", "-journal"] {
        let source_path = sqlite_sidecar_path(selected_path, suffix);
        match open_brokered_file(&source_path) {
            Ok(file) => {
                let frozen = file
                    .metadata()
                    .and_then(|metadata| FrozenFile::from_metadata(&metadata))
                    .map_err(|cause| map_io_error(event_id, cause))?;
                sidecars.push((suffix, source_path, file, frozen));
            }
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => return Err(map_io_error(event_id, cause)),
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("ctx-complete-content-sqlite-")
        .tempdir()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let path = dir.path().join("source.sqlite");
    copy_bounded_handle(&main, &path, event_id)?;
    for (suffix, _, file, _) in &sidecars {
        copy_bounded_handle(file, &sqlite_sidecar_path(&path, suffix), event_id)?;
    }
    if !revalidate_opened_file(selected_path, &main, &main_frozen) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    for (suffix, source_path, file, frozen) in &sidecars {
        if !revalidate_opened_file(source_path, file, frozen) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceChanged,
                event_id,
            ));
        }
        debug_assert_eq!(source_path, &sqlite_sidecar_path(selected_path, suffix));
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let source_path = sqlite_sidecar_path(selected_path, suffix);
        let existed = sidecars
            .iter()
            .any(|(observed, _, _, _)| *observed == suffix);
        match open_brokered_file(&source_path) {
            Ok(_) if !existed => {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
            Ok(_) => {}
            Err(cause) if cause.kind() == io::ErrorKind::NotFound && !existed => {}
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
            Err(cause) => return Err(map_io_error(event_id, cause)),
        }
    }
    // Opening now proves that the copied DB/WAL set is coherent. Resolvers open
    // their own bounded read-only connection against the same immutable snapshot.
    open_provider_sqlite_readonly(&path).map_err(|cause| map_capture_error(event_id, cause))?;
    Ok(BrokeredSqliteSource { _dir: dir, path })
}

#[cfg(target_os = "windows")]
fn admit_sqlite(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    event_id: Uuid,
) -> Result<BrokeredSqliteSource, CompleteContentError> {
    let database =
        windows::admit_regular_file(selected_path, route.source_root.as_deref(), event_id)?;
    validate_source_snapshot(&route.source_snapshot, &database.metadata, event_id)?;
    let sidecar_paths =
        ["-wal", "-shm", "-journal"].map(|suffix| sqlite_sidecar_path(selected_path, suffix));
    let mut sidecars = Vec::with_capacity(sidecar_paths.len());
    for path in &sidecar_paths {
        sidecars.push(windows::admit_optional_regular_file(
            path,
            route.source_root.as_deref(),
            event_id,
        )?);
    }

    let dir = tempfile::Builder::new()
        .prefix("ctx-complete-content-sqlite-")
        .tempdir()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let path = dir.path().join("source.sqlite");
    windows::copy_bounded_handle(
        &database,
        &path,
        SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
        event_id,
    )?;
    for ((suffix, admitted), _source_path) in ["-wal", "-shm", "-journal"]
        .into_iter()
        .zip(sidecars.iter())
        .zip(sidecar_paths.iter())
    {
        if let Some(admitted) = admitted {
            let destination = sqlite_sidecar_path(&path, suffix);
            windows::copy_bounded_handle(
                admitted,
                &destination,
                SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
                event_id,
            )?;
        }
    }

    windows::verify_named_file_still_matches(
        selected_path,
        route.source_root.as_deref(),
        &database.identity,
        event_id,
    )?;
    for (source_path, admitted) in sidecar_paths.iter().zip(&sidecars) {
        windows::verify_optional_named_file_still_matches(
            source_path,
            route.source_root.as_deref(),
            admitted.as_ref().map(|file| &file.identity),
            event_id,
        )?;
    }
    open_provider_sqlite_readonly(&path).map_err(|cause| map_capture_error(event_id, cause))?;
    Ok(BrokeredSqliteSource { _dir: dir, path })
}

fn selected_source_path(
    route: &AuthorizedSourceRoute,
    event_id: Uuid,
) -> Result<PathBuf, CompleteContentError> {
    let raw = normalize_lexical(&route.raw_source_path).ok_or_else(|| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    let selected = if raw.is_absolute() {
        raw
    } else {
        let root = route
            .source_root
            .as_deref()
            .and_then(normalize_lexical)
            .ok_or_else(|| {
                CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
            })?;
        normalize_lexical(&root.join(raw)).ok_or_else(|| {
            CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
        })?
    };
    if let Some(root) = route.source_root.as_deref().and_then(normalize_lexical) {
        #[cfg(target_os = "windows")]
        let contained = windows::lexical_path_is_within(&selected, &root);
        #[cfg(not(target_os = "windows"))]
        let contained = selected == root || selected.starts_with(&root);
        if !contained {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceUnreadable,
                event_id,
            ));
        }
    }
    #[cfg(target_os = "windows")]
    windows::validate_local_qualified_path(&selected, event_id)?;
    Ok(selected)
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => return None,
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        Some(normalize_macos_fixed_root_alias(&normalized))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(normalized)
    }
}

/// Apple's compatibility aliases are stable filesystem-root spellings, not
/// provider-controlled symlinks. Translate only these exact prefixes so the
/// descriptor walk can continue to reject every other symlink component.
#[cfg(target_os = "macos")]
pub(super) fn normalize_macos_fixed_root_alias(path: &Path) -> PathBuf {
    for (alias, target) in [
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/tmp"), Path::new("/private/tmp")),
        (Path::new("/etc"), Path::new("/private/etc")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return target.join(suffix);
        }
    }
    path.to_path_buf()
}

fn validate_source_snapshot(
    snapshot: &SourceSnapshot,
    metadata: &fs::Metadata,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    if snapshot
        .size_bytes
        .is_some_and(|expected| metadata.len() < expected)
    {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    if let (Some(expected), Some(size), Ok(modified)) = (
        snapshot.modified_at_ms,
        snapshot.size_bytes,
        metadata.modified(),
    ) {
        if size == metadata.len() {
            use std::time::UNIX_EPOCH;
            let actual = modified
                .duration_since(UNIX_EPOCH)
                .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(i64::MIN);
            if actual != expected {
                return Err(CompleteContentError::new(
                    CompleteContentErrorKind::SourceChanged,
                    event_id,
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_bounded_handle(
    source: &File,
    destination: &Path,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let metadata = source
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if !metadata.file_type().is_file() || metadata.len() > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
        return Err(CompleteContentError::new(
            if metadata.len() > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
                CompleteContentErrorKind::ContentTooLarge
            } else {
                CompleteContentErrorKind::SourceUnreadable
            },
            event_id,
        ));
    }
    let mut output = File::create(destination).map_err(|cause| map_io_error(event_id, cause))?;
    let mut input = source;
    let copied = io::copy(
        &mut input
            .by_ref()
            .take(SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES.saturating_add(1)),
        &mut output,
    )
    .map_err(|cause| map_io_error(event_id, cause))?;
    if copied != metadata.len() {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn open_brokered_file(path: &Path) -> io::Result<File> {
    unix::open_path(path, unix::ExpectedType::File)
}

#[cfg(unix)]
pub(crate) fn open_brokered_directory(path: &Path) -> io::Result<File> {
    unix::open_path(path, unix::ExpectedType::Directory)
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn open_brokered_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "brokered complete-content source opening is unavailable on this platform",
    ))
}

#[cfg(not(any(unix, target_os = "windows")))]
pub(crate) fn open_brokered_directory(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "brokered complete-content directory opening is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn revalidate_opened_file(path: &Path, file: &File, frozen: &FrozenFile) -> bool {
    let opened = file
        .metadata()
        .ok()
        .and_then(|metadata| FrozenFile::from_metadata(&metadata).ok());
    let selected = open_brokered_file(path)
        .ok()
        .and_then(|file| file.metadata().ok())
        .and_then(|metadata| FrozenFile::from_metadata(&metadata).ok());
    opened.as_ref() == Some(frozen) && selected.as_ref() == Some(frozen)
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn map_io_error(event_id: Uuid, cause: io::Error) -> CompleteContentError {
    CompleteContentError::new(
        match cause.kind() {
            io::ErrorKind::NotFound => CompleteContentErrorKind::SourceMissing,
            io::ErrorKind::PermissionDenied => CompleteContentErrorKind::SourceUnreadable,
            _ => CompleteContentErrorKind::SourceUnreadable,
        },
        event_id,
    )
}

fn map_capture_error(event_id: Uuid, cause: CaptureError) -> CompleteContentError {
    let kind = match cause {
        CaptureError::Io(ref error) if error.kind() == io::ErrorKind::NotFound => {
            CompleteContentErrorKind::SourceMissing
        }
        CaptureError::InvalidPayload(ref message) if message.contains("exceeds") => {
            CompleteContentErrorKind::ContentTooLarge
        }
        CaptureError::InvalidProviderTranscriptPath { .. } => {
            CompleteContentErrorKind::SourceUnreadable
        }
        CaptureError::SourceChangedDuringCapture | CaptureError::InvalidPayload(_) => {
            CompleteContentErrorKind::SourceChanged
        }
        _ => CompleteContentErrorKind::SourceUnreadable,
    };
    CompleteContentError::new(kind, event_id)
}

#[cfg(test)]
mod tests;
