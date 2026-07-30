//! Admission boundary for reopening provider-owned complete content.
//!
//! The Store supplies an [`AuthorizedSourceRoute`]. The broker validates and
//! freezes that route once, then resolvers receive only [`BrokeredSourceAccess`].
//! Paths never cross the resolver request boundary.

use std::{
    fmt, fs, io,
    ops::Deref,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[cfg(not(target_os = "windows"))]
use std::fs::File;

use ctx_history_core::{platform_security::create_private_directory_all, CaptureProvider};
use rusqlite::Connection;
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
        sqlite::{
            map_sqlite_source_access_error, open_provider_sqlite_readonly, ReadOnlySqliteConnection,
        },
    },
    provider_sources::{
        open_ctx_owned_sqlite_read_snapshot, CtxOwnedSqliteReadSnapshot, SqliteSourceEvidence,
    },
    CaptureError, NANOCLAW_SOURCE_FORMAT,
};

mod identity;
mod jsonl;
mod jsonl_auxiliary;
mod nanoclaw_snapshot;
#[cfg(any(unix, target_os = "windows"))]
mod sqlite_admission;
#[cfg(unix)]
pub(crate) mod unix;
use identity::FrozenFile;
#[cfg(any(unix, target_os = "windows"))]
use sqlite_admission::admit as admit_sqlite;
#[cfg(unix)]
use std::io::Read;

#[cfg(test)]
// Retain the race-injection seam for focused source-revalidation tests.
#[allow(unused_imports)]
pub(crate) use nanoclaw_snapshot::set_before_source_set_revalidation as set_nanoclaw_before_source_set_revalidation;

#[cfg(target_os = "windows")]
#[path = "source_access/windows.rs"]
pub(crate) mod windows;

const SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES: u64 = 512 * 1024 * 1024;
pub const COMPLETE_CONTENT_MAX_ADMITTED_SOURCES: usize = 8;
pub const COMPLETE_CONTENT_MAX_SNAPSHOT_BYTES: u64 = nanoclaw_snapshot::SNAPSHOT_MAX_TOTAL_BYTES;

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

/// Root-bound admission service. Future Store route revisions can be consumed
/// here without exposing their path representation to provider resolvers.
#[derive(Debug, Clone)]
pub struct SourceAccessBroker {
    data_root: Arc<PathBuf>,
}

/// Opaque result of bounded route inspection. It owns the exact route that was
/// measured, so admission cannot substitute a caller-supplied byte count or a
/// different path after the aggregate gate accepts it.
pub struct PreparedSourceAdmission {
    route: AuthorizedSourceRoute,
    reserved_snapshot_bytes: u64,
    event_id: Uuid,
}

impl PreparedSourceAdmission {
    pub const fn reserved_snapshot_bytes(&self) -> u64 {
        self.reserved_snapshot_bytes
    }
}

impl fmt::Debug for PreparedSourceAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSourceAdmission")
            .field("source_id", &self.route.source_id)
            .field("family", &self.route.family)
            .field("reserved_snapshot_bytes", &self.reserved_snapshot_bytes)
            .finish_non_exhaustive()
    }
}

impl SourceAccessBroker {
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: Arc::new(data_root.into()),
        }
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
        admit_platform(&self.data_root, route, locators, None, event_id)
    }

    /// Returns the bytes that must be reserved before admitting this source.
    ///
    /// Ordinary SQLite sources report the exact currently named database and
    /// sidecar bytes. Compound SQLite sources reserve their complete family
    /// bound because discovering their selected files is itself admission
    /// work. Single-file JSONL retains a handle and reserves no copied snapshot
    /// bytes.
    pub fn prepare(
        &self,
        route: AuthorizedSourceRoute,
        event_id: Uuid,
    ) -> Result<PreparedSourceAdmission, CompleteContentError> {
        if route.source_identity.as_deref().is_some_and(str::is_empty) {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        }
        let reserved_snapshot_bytes = snapshot_reservation_bytes_platform(&route, event_id)?;
        Ok(PreparedSourceAdmission {
            route,
            reserved_snapshot_bytes,
            event_id,
        })
    }

    /// Admits a route only if it still has the reservation measured before the
    /// caller accepted its aggregate source set.
    pub fn admit_prepared_for_source_locators(
        &self,
        prepared: PreparedSourceAdmission,
        locators: &[CompleteContentSourceLocator],
    ) -> Result<BrokeredSourceAccess, CompleteContentError> {
        admit_platform(
            &self.data_root,
            prepared.route,
            locators,
            Some(prepared.reserved_snapshot_bytes),
            prepared.event_id,
        )
    }
}

#[cfg(any(unix, target_os = "windows"))]
fn snapshot_reservation_bytes_platform(
    route: &AuthorizedSourceRoute,
    event_id: Uuid,
) -> Result<u64, CompleteContentError> {
    match route.family {
        CompleteContentSourceFamily::Jsonl => Ok(0),
        CompleteContentSourceFamily::Sqlite
            if route.provider == CaptureProvider::NanoClaw
                && route.source_format == NANOCLAW_SOURCE_FORMAT =>
        {
            Ok(nanoclaw_snapshot::SNAPSHOT_MAX_TOTAL_BYTES)
        }
        CompleteContentSourceFamily::Sqlite => {
            let selected = selected_source_path(route, event_id)?;
            sqlite_snapshot_reservation_bytes(route, &selected, event_id)
        }
        CompleteContentSourceFamily::Structured => Err(CompleteContentError::new(
            CompleteContentErrorKind::HydrationUnsupported,
            event_id,
        )),
        #[cfg(test)]
        CompleteContentSourceFamily::Fixture => Ok(0),
    }
}

#[cfg(not(any(unix, target_os = "windows")))]
fn snapshot_reservation_bytes_platform(
    _route: &AuthorizedSourceRoute,
    event_id: Uuid,
) -> Result<u64, CompleteContentError> {
    Err(CompleteContentError::new(
        CompleteContentErrorKind::HydrationUnsupported,
        event_id,
    ))
}

fn validate_fixed_snapshot_reservation(
    reserved: Option<u64>,
    expected: u64,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    if reserved.is_some_and(|reserved| reserved != expected) {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceChanged,
            event_id,
        ));
    }
    Ok(())
}

fn validate_observed_snapshot_reservation(
    reserved: Option<u64>,
    observed: u64,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    validate_fixed_snapshot_reservation(reserved, observed, event_id)
}

fn bounded_sqlite_component_bytes(
    metadata: &fs::Metadata,
    event_id: Uuid,
) -> Result<u64, CompleteContentError> {
    if !metadata.file_type().is_file() {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::SourceUnreadable,
            event_id,
        ));
    }
    if metadata.len() > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
        return Err(CompleteContentError::new(
            CompleteContentErrorKind::ContentTooLarge,
            event_id,
        ));
    }
    Ok(metadata.len())
}

#[cfg(unix)]
fn sqlite_snapshot_reservation_bytes(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    event_id: Uuid,
) -> Result<u64, CompleteContentError> {
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
    let mut bytes = bounded_sqlite_component_bytes(&metadata, event_id)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        match open_brokered_file(&sqlite_sidecar_path(selected_path, suffix)) {
            Ok(file) => {
                let metadata = file
                    .metadata()
                    .map_err(|cause| map_io_error(event_id, cause))?;
                bytes = bytes
                    .checked_add(bounded_sqlite_component_bytes(&metadata, event_id)?)
                    .ok_or_else(|| {
                        CompleteContentError::new(
                            CompleteContentErrorKind::ContentTooLarge,
                            event_id,
                        )
                    })?;
            }
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => return Err(map_io_error(event_id, cause)),
        }
    }
    Ok(bytes)
}

#[cfg(target_os = "windows")]
fn sqlite_snapshot_reservation_bytes(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    event_id: Uuid,
) -> Result<u64, CompleteContentError> {
    let main = windows::admit_regular_file(selected_path, route.source_root.as_deref(), event_id)?;
    validate_source_snapshot(&route.source_snapshot, &main.metadata, event_id)?;
    let mut bytes = bounded_sqlite_component_bytes(&main.metadata, event_id)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(file) = windows::admit_optional_regular_file(
            &sqlite_sidecar_path(selected_path, suffix),
            route.source_root.as_deref(),
            event_id,
        )? {
            bytes = bytes
                .checked_add(bounded_sqlite_component_bytes(&file.metadata, event_id)?)
                .ok_or_else(|| {
                    CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
                })?;
        }
    }
    Ok(bytes)
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
            #[cfg(test)]
            BrokeredSource::Fixture => CompleteContentSourceFamily::Fixture,
        }
    }

    pub(crate) fn open_sqlite_snapshot(
        &self,
        event_id: Uuid,
    ) -> Result<BrokeredSqliteReadSnapshot, CompleteContentError> {
        let BrokeredSource::Sqlite(source) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        source.open(event_id)
    }

    pub(crate) fn finish_sqlite_snapshot(
        &self,
        snapshot: BrokeredSqliteReadSnapshot,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        let BrokeredSource::Sqlite(_) = self.inner.as_ref() else {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        };
        snapshot.finish(event_id)
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
    #[cfg(test)]
    Fixture,
}

enum BrokeredSqliteSource {
    Provider {
        data_root: Arc<PathBuf>,
        path: PathBuf,
        evidence: SqliteSourceEvidence,
    },
    CtxOwned {
        _dir: TempDir,
        path: PathBuf,
    },
}

pub(crate) enum BrokeredSqliteReadSnapshot {
    Provider {
        connection: Box<ReadOnlySqliteConnection>,
        expected: SqliteSourceEvidence,
    },
    CtxOwned(CtxOwnedSqliteReadSnapshot),
}

impl BrokeredSqliteSource {
    fn open(&self, event_id: Uuid) -> Result<BrokeredSqliteReadSnapshot, CompleteContentError> {
        match self {
            Self::Provider {
                data_root,
                path,
                evidence,
            } => {
                let connection = open_provider_sqlite_readonly(data_root, path)
                    .map_err(|cause| map_capture_error(event_id, cause))?;
                if connection
                    .evidence()
                    .map_err(|cause| map_capture_error(event_id, cause))?
                    != evidence
                {
                    return Err(CompleteContentError::new(
                        CompleteContentErrorKind::SourceChanged,
                        event_id,
                    ));
                }
                Ok(BrokeredSqliteReadSnapshot::Provider {
                    connection: Box::new(connection),
                    expected: evidence.clone(),
                })
            }
            Self::CtxOwned { path, .. } => open_ctx_owned_sqlite_read_snapshot(path)
                .map(BrokeredSqliteReadSnapshot::CtxOwned)
                .map_err(map_sqlite_source_access_error)
                .map_err(|cause| map_capture_error(event_id, cause)),
        }
    }
}

impl BrokeredSqliteReadSnapshot {
    fn finish(self, event_id: Uuid) -> Result<(), CompleteContentError> {
        match self {
            Self::Provider {
                connection,
                expected,
            } => {
                let evidence = (*connection)
                    .finish()
                    .map_err(|cause| map_capture_error(event_id, cause))?;
                if evidence == expected {
                    Ok(())
                } else {
                    Err(CompleteContentError::new(
                        CompleteContentErrorKind::SourceChanged,
                        event_id,
                    ))
                }
            }
            Self::CtxOwned(snapshot) => snapshot
                .finish()
                .map_err(map_sqlite_source_access_error)
                .map_err(|cause| map_capture_error(event_id, cause)),
        }
    }
}

impl Deref for BrokeredSqliteReadSnapshot {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Provider { connection, .. } => connection,
            Self::CtxOwned(snapshot) => snapshot,
        }
    }
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
    data_root: &Path,
    route: AuthorizedSourceRoute,
    locators: &[CompleteContentSourceLocator],
    reserved_snapshot_bytes: Option<u64>,
    event_id: Uuid,
) -> Result<BrokeredSourceAccess, CompleteContentError> {
    let source_id = route.source_id;
    let inner = match route.family {
        CompleteContentSourceFamily::Jsonl => {
            validate_fixed_snapshot_reservation(reserved_snapshot_bytes, 0, event_id)?;
            let selected = selected_source_path(&route, event_id)?;
            BrokeredSource::Jsonl(jsonl::admit(route, selected, event_id)?)
        }
        CompleteContentSourceFamily::Sqlite => {
            let selected = selected_source_path(&route, event_id)?;
            if route.provider == CaptureProvider::NanoClaw
                && route.source_format == NANOCLAW_SOURCE_FORMAT
            {
                validate_fixed_snapshot_reservation(
                    reserved_snapshot_bytes,
                    nanoclaw_snapshot::SNAPSHOT_MAX_TOTAL_BYTES,
                    event_id,
                )?;
                BrokeredSource::NanoClaw(admit_nanoclaw(
                    data_root, &route, &selected, locators, event_id,
                )?)
            } else {
                BrokeredSource::Sqlite(admit_sqlite(
                    data_root,
                    &route,
                    &selected,
                    reserved_snapshot_bytes,
                    event_id,
                )?)
            }
        }
        CompleteContentSourceFamily::Structured => {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::HydrationUnsupported,
                event_id,
            ));
        }
        #[cfg(test)]
        CompleteContentSourceFamily::Fixture => {
            validate_fixed_snapshot_reservation(reserved_snapshot_bytes, 0, event_id)?;
            BrokeredSource::Fixture
        }
    };
    Ok(BrokeredSourceAccess {
        source_id,
        inner: Arc::new(inner),
    })
}

fn admit_nanoclaw(
    data_root: &Path,
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    locators: &[CompleteContentSourceLocator],
    event_id: Uuid,
) -> Result<BrokeredNanoClawSource, CompleteContentError> {
    nanoclaw_snapshot::BrokeredNanoClawSnapshot::admit(
        data_root,
        route,
        selected_path,
        locators,
        event_id,
    )
    .map(BrokeredNanoClawSource)
}

fn ctx_sqlite_snapshot_tempdir(
    data_root: &Path,
    event_id: Uuid,
) -> Result<TempDir, CompleteContentError> {
    let staging_root = data_root.join("tmp").join("provider-sqlite");
    create_private_directory_all(&staging_root).map_err(|cause| map_io_error(event_id, cause))?;
    tempfile::Builder::new()
        .prefix("complete-content-sqlite-")
        .tempdir_in(&staging_root)
        .map_err(|cause| map_io_error(event_id, cause))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn admit_platform(
    _data_root: &Path,
    _route: AuthorizedSourceRoute,
    _locators: &[CompleteContentSourceLocator],
    _reserved_snapshot_bytes: Option<u64>,
    event_id: Uuid,
) -> Result<BrokeredSourceAccess, CompleteContentError> {
    Err(CompleteContentError::new(
        CompleteContentErrorKind::HydrationUnsupported,
        event_id,
    ))
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
    expected_bytes: u64,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let metadata = source
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if !metadata.file_type().is_file()
        || metadata.len() > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES
        || metadata.len() != expected_bytes
    {
        return Err(CompleteContentError::new(
            if metadata.len() > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
                CompleteContentErrorKind::ContentTooLarge
            } else if metadata.len() != expected_bytes {
                CompleteContentErrorKind::SourceChanged
            } else {
                CompleteContentErrorKind::SourceUnreadable
            },
            event_id,
        ));
    }
    let mut output = File::create(destination).map_err(|cause| map_io_error(event_id, cause))?;
    let mut input = source;
    let copied = io::copy(&mut input.by_ref().take(expected_bytes), &mut output)
        .map_err(|cause| map_io_error(event_id, cause))?;
    if copied != expected_bytes {
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
