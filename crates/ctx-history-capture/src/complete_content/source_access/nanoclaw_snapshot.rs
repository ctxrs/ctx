//! Bounded, locator-selected NanoClaw project snapshots.
//!
//! NanoClaw is a compound SQLite source: a central database maps session rows
//! to inbound/outbound component databases. This module is the only complete-
//! content code allowed to touch the live project. Resolver code receives an
//! immutable private snapshot containing only the central database and the
//! components selected by caller locators.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use uuid::Uuid;

#[cfg(test)]
use std::cell::RefCell;

use super::{
    map_capture_error, map_io_error, nanoclaw, sqlite_sidecar_path, AuthorizedSourceRoute,
    CompleteContentError, CompleteContentErrorKind, CompleteContentSourceLocator, NativeLocator,
    SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
};
use crate::{
    complete_content::sqlite::{
        configure_complete_content_sqlite_connection, map_bounded_sqlite_error_for_event,
        CompleteContentSqliteQueryBudget,
    },
    provider::sqlite::open_provider_sqlite_readonly,
};

const SNAPSHOT_MAX_COMPONENT_DATABASES: usize = 256;
const SNAPSHOT_MAX_FILES: usize = (SNAPSHOT_MAX_COMPONENT_DATABASES + 1) * 4;
const SNAPSHOT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(5);
const SQLITE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

mod platform;

use platform::{admit_optional_file, admit_root, revalidate_optional, AdmittedFile};

#[cfg(test)]
thread_local! {
    static BEFORE_SOURCE_SET_REVALIDATION: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct NanoClawRevalidationHook;

#[cfg(test)]
impl Drop for NanoClawRevalidationHook {
    fn drop(&mut self) {
        BEFORE_SOURCE_SET_REVALIDATION.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn set_before_source_set_revalidation(
    hook: impl FnOnce() + 'static,
) -> NanoClawRevalidationHook {
    BEFORE_SOURCE_SET_REVALIDATION.with(|installed| {
        *installed.borrow_mut() = Some(Box::new(hook));
    });
    NanoClawRevalidationHook
}

#[cfg(test)]
fn run_before_source_set_revalidation() {
    BEFORE_SOURCE_SET_REVALIDATION.with(|installed| {
        if let Some(hook) = installed.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_source_set_revalidation() {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ManifestLocator {
    kind: String,
    value: Vec<u8>,
}

impl ManifestLocator {
    fn from_native(locator: &NativeLocator) -> Self {
        Self {
            kind: locator.kind().to_owned(),
            value: locator.value().to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NanoClawSnapshotManifest {
    locators: BTreeSet<ManifestLocator>,
    component_ids: BTreeSet<(i64, String, String, String)>,
}

/// Path-free capability retained after admission. `_snapshot` owns the private
/// directory; the only IDs retained are immutable locator/component manifest
/// entries. No provider path survives this boundary.
pub(super) struct BrokeredNanoClawSnapshot {
    _snapshot: tempfile::TempDir,
    manifest: NanoClawSnapshotManifest,
}

impl BrokeredNanoClawSnapshot {
    pub(super) fn admit(
        route: &AuthorizedSourceRoute,
        selected_path: &Path,
        source_locators: &[CompleteContentSourceLocator],
        event_id: Uuid,
    ) -> Result<Self, CompleteContentError> {
        if source_locators.is_empty() || source_locators.len() > SNAPSHOT_MAX_COMPONENT_DATABASES {
            return Err(content_error(
                event_id,
                if source_locators.is_empty() {
                    CompleteContentErrorKind::HydrationUnsupported
                } else {
                    CompleteContentErrorKind::ContentTooLarge
                },
            ));
        }
        let locators = source_locators
            .iter()
            .map(|locator| {
                NativeLocator::new(locator.kind(), locator.value().to_vec()).map_err(|_| {
                    content_error(
                        event_id,
                        CompleteContentErrorKind::ContentVerificationFailed,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let deadline = Instant::now() + SNAPSHOT_DEADLINE;
        let project_root = project_root(selected_path, event_id)?;
        if let Some(authorized_root) = route
            .source_root
            .as_deref()
            .and_then(super::normalize_lexical)
        {
            #[cfg(target_os = "windows")]
            let contained = super::windows::lexical_path_is_within(&project_root, &authorized_root);
            #[cfg(not(target_os = "windows"))]
            let contained =
                project_root == authorized_root || project_root.starts_with(&authorized_root);
            if !contained {
                return Err(content_error(
                    event_id,
                    CompleteContentErrorKind::SourceUnreadable,
                ));
            }
        }
        let root = admit_root(&project_root, route.source_root.as_deref(), event_id)?;
        let snapshot = create_snapshot_tempdir(&std::env::temp_dir(), event_id)?;
        fs::create_dir_all(snapshot.path().join("data").join("v2-sessions"))
            .map_err(|cause| map_io_error(event_id, cause))?;

        let mut budget = SnapshotBudget::new(deadline);
        let central_source = project_root.join("data").join("v2.db");
        let central_destination = snapshot.path().join("data").join("v2.db");
        let central = snapshot_sqlite_set(
            &central_source,
            &central_destination,
            route.source_root.as_deref(),
            true,
            &mut budget,
            event_id,
        )?
        .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceMissing))?;
        budget.admit_database(&central.database, event_id)?;
        validate_snapshot_schema(&central_destination, &mut budget, event_id)?;

        let central_connection = open_provider_sqlite_readonly(&central_destination)
            .map_err(|cause| map_capture_error(event_id, cause))?;
        configure_complete_content_sqlite_connection(
            &central_connection,
            CompleteContentSqliteQueryBudget::new(),
        )
        .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))?;
        let addresses = nanoclaw::selected_component_addresses(&central_connection, &locators)
            .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))?;
        drop(central_connection);
        if addresses.len() > SNAPSHOT_MAX_COMPONENT_DATABASES {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::ContentTooLarge,
            ));
        }

        let mut component_paths = BTreeSet::new();
        let mut database_identities = vec![central.database.frozen.clone()];
        let mut component_ids = BTreeSet::new();
        let mut observations = vec![central];
        for address in addresses {
            budget.check(event_id)?;
            let source_path = project_root
                .join("data")
                .join("v2-sessions")
                .join(&address.agent_group_id)
                .join(&address.session_id)
                .join(address.source.file_name());
            if !component_paths.insert(source_path.clone()) {
                return Err(content_error(
                    event_id,
                    CompleteContentErrorKind::SourceChanged,
                ));
            }
            let destination = snapshot
                .path()
                .join("data")
                .join("v2-sessions")
                .join(&address.agent_group_id)
                .join(&address.session_id)
                .join(address.source.file_name());
            let observed = snapshot_sqlite_set(
                &source_path,
                &destination,
                route.source_root.as_deref(),
                false,
                &mut budget,
                event_id,
            )?;
            if let Some(observed) = observed {
                budget.admit_database(&observed.database, event_id)?;
                if database_identities
                    .iter()
                    .any(|existing| existing.same_object(&observed.database.frozen))
                {
                    return Err(content_error(
                        event_id,
                        CompleteContentErrorKind::SourceChanged,
                    ));
                }
                database_identities.push(observed.database.frozen.clone());
                validate_snapshot_schema(&destination, &mut budget, event_id)?;
                observations.push(observed);
            }
            component_ids.insert((
                address.session_rowid,
                address.source.label().to_owned(),
                address.agent_group_id,
                address.session_id,
            ));
        }

        // Revalidate the complete selected set only after every copy/schema
        // inspection has finished, then bind the project root again. This
        // catches leaf, sidecar, and ancestor replacement during admission.
        run_before_source_set_revalidation();
        for observed in &observations {
            observed.revalidate(route.source_root.as_deref(), event_id)?;
        }
        root.revalidate(&project_root, route.source_root.as_deref(), event_id)?;
        budget.check(event_id)?;

        let project = nanoclaw::NanoClawCompleteProject::open(
            snapshot.path(),
            &locators,
            CompleteContentSqliteQueryBudget::new(),
        )
        .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))?;
        if !project
            .revalidate()
            .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))?
        {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::SourceChanged,
            ));
        }
        drop(project);

        Ok(Self {
            _snapshot: snapshot,
            manifest: NanoClawSnapshotManifest {
                locators: locators.iter().map(ManifestLocator::from_native).collect(),
                component_ids,
            },
        })
    }

    pub(super) fn open(
        &self,
        locators: &[NativeLocator],
        query_budget: CompleteContentSqliteQueryBudget,
        event_id: Uuid,
    ) -> Result<nanoclaw::NanoClawCompleteProject, CompleteContentError> {
        if locators.is_empty()
            || locators.iter().any(|locator| {
                !self
                    .manifest
                    .locators
                    .contains(&ManifestLocator::from_native(locator))
            })
        {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        let _ = &self.manifest.component_ids;
        nanoclaw::NanoClawCompleteProject::open(self._snapshot.path(), locators, query_budget)
            .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))
    }
}

struct SnapshotBudget {
    deadline: Instant,
    files: usize,
    bytes: u64,
    databases: usize,
}

impl SnapshotBudget {
    fn new(deadline: Instant) -> Self {
        Self {
            deadline,
            files: 0,
            bytes: 0,
            databases: 0,
        }
    }

    fn admit_file(
        &mut self,
        file: &AdmittedFile,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        self.check(event_id)?;
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(file.frozen.length);
        if self.files > SNAPSHOT_MAX_FILES
            || self.bytes > SNAPSHOT_MAX_TOTAL_BYTES
            || file.frozen.length > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES
        {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::ContentTooLarge,
            ));
        }
        Ok(())
    }

    fn admit_database(
        &mut self,
        _database: &AdmittedFile,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        self.databases = self.databases.saturating_add(1);
        if self.databases > SNAPSHOT_MAX_COMPONENT_DATABASES + 1 {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::ContentTooLarge,
            ));
        }
        self.check(event_id)
    }

    fn check(&self, event_id: Uuid) -> Result<(), CompleteContentError> {
        if Instant::now() > self.deadline {
            Err(content_error(
                event_id,
                CompleteContentErrorKind::ContentTooLarge,
            ))
        } else {
            Ok(())
        }
    }
}

struct ObservedSqliteSet {
    database: AdmittedFile,
    sidecars: Vec<(PathBuf, Option<AdmittedFile>)>,
}

impl ObservedSqliteSet {
    fn revalidate(
        &self,
        containment_root: Option<&Path>,
        event_id: Uuid,
    ) -> Result<(), CompleteContentError> {
        self.database.revalidate(containment_root, event_id)?;
        for (path, file) in &self.sidecars {
            revalidate_optional(path, file.as_ref(), containment_root, event_id)?;
        }
        Ok(())
    }
}

fn snapshot_sqlite_set(
    source_path: &Path,
    destination: &Path,
    containment_root: Option<&Path>,
    required: bool,
    budget: &mut SnapshotBudget,
    event_id: Uuid,
) -> Result<Option<ObservedSqliteSet>, CompleteContentError> {
    budget.check(event_id)?;
    let database = match admit_optional_file(source_path, containment_root, event_id)? {
        Some(database) => database,
        None if required => {
            return Err(content_error(
                event_id,
                CompleteContentErrorKind::SourceMissing,
            ));
        }
        None => {
            for suffix in SQLITE_SIDECAR_SUFFIXES {
                let path = sqlite_sidecar_path(source_path, suffix);
                if admit_optional_file(&path, containment_root, event_id)?.is_some() {
                    return Err(content_error(
                        event_id,
                        CompleteContentErrorKind::SourceChanged,
                    ));
                }
            }
            return Ok(None);
        }
    };
    budget.admit_file(&database, event_id)?;
    let mut sidecars = Vec::with_capacity(SQLITE_SIDECAR_SUFFIXES.len());
    for suffix in SQLITE_SIDECAR_SUFFIXES {
        let path = sqlite_sidecar_path(source_path, suffix);
        let admitted = admit_optional_file(&path, containment_root, event_id)?;
        if let Some(file) = admitted.as_ref() {
            budget.admit_file(file, event_id)?;
        }
        sidecars.push((path, admitted));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|cause| map_io_error(event_id, cause))?;
    }
    database.copy_to(destination, event_id)?;
    for ((_, admitted), suffix) in sidecars.iter().zip(SQLITE_SIDECAR_SUFFIXES) {
        if let Some(file) = admitted {
            file.copy_to(&sqlite_sidecar_path(destination, suffix), event_id)?;
        }
    }
    budget.check(event_id)?;
    Ok(Some(ObservedSqliteSet { database, sidecars }))
}

fn validate_snapshot_schema(
    path: &Path,
    budget: &mut SnapshotBudget,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    budget.check(event_id)?;
    let conn =
        open_provider_sqlite_readonly(path).map_err(|cause| map_capture_error(event_id, cause))?;
    configure_complete_content_sqlite_connection(&conn, CompleteContentSqliteQueryBudget::new())
        .map_err(|cause| map_bounded_sqlite_error_for_event(event_id, cause))?;
    budget.check(event_id)
}

fn project_root(selected_path: &Path, event_id: Uuid) -> Result<PathBuf, CompleteContentError> {
    if selected_path.file_name().and_then(|name| name.to_str()) == Some("v2.db")
        && selected_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("data")
    {
        return selected_path
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable));
    }
    Ok(selected_path.to_path_buf())
}

fn content_error(event_id: Uuid, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, event_id)
}

fn create_snapshot_tempdir(
    temp_root: &Path,
    event_id: Uuid,
) -> Result<tempfile::TempDir, CompleteContentError> {
    // `temp_dir()` uses `/var/folders/...` on macOS even though `/var` is a
    // compatibility symlink to `/private/var`. Normalize only Apple's fixed
    // root aliases before the private snapshot is created. Do not canonicalize
    // arbitrary ancestors: the provider SQLite path checks must continue to
    // reject every provider-controlled symlink.
    let temp_root = super::normalize_lexical(temp_root)
        .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
    tempfile::Builder::new()
        .prefix("ctx-complete-content-nanoclaw-")
        .tempdir_in(temp_root)
        .map_err(|cause| map_io_error(event_id, cause))
}

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use std::{fs, os::unix::fs::symlink, path::Path};

    use rusqlite::Connection;
    use uuid::Uuid;

    use super::create_snapshot_tempdir;
    use crate::{provider::sqlite::open_provider_sqlite_readonly, CaptureError};

    fn create_sqlite(path: &Path) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("create table proof(value text); insert into proof values ('ok');")
            .unwrap();
    }

    #[test]
    fn broker_snapshot_normalizes_fixed_tmp_alias_but_not_arbitrary_symlink_ancestors() {
        let trusted_root = tempfile::Builder::new()
            .prefix("ctx-nanoclaw-macos-alias-")
            .tempdir_in("/private/tmp")
            .unwrap();
        let fixed_alias_root = Path::new("/tmp").join(trusted_root.path().file_name().unwrap());
        let fixed_snapshot = create_snapshot_tempdir(&fixed_alias_root, Uuid::new_v4()).unwrap();
        assert!(fixed_snapshot.path().starts_with("/private/tmp"));
        let fixed_database = fixed_snapshot.path().join("fixed.sqlite");
        create_sqlite(&fixed_database);
        open_provider_sqlite_readonly(&fixed_database).unwrap();

        let real_root = trusted_root.path().join("real");
        let arbitrary_alias_root = trusted_root.path().join("arbitrary-link");
        fs::create_dir(&real_root).unwrap();
        symlink(&real_root, &arbitrary_alias_root).unwrap();
        let arbitrary_snapshot =
            create_snapshot_tempdir(&arbitrary_alias_root, Uuid::new_v4()).unwrap();
        let arbitrary_database = arbitrary_snapshot.path().join("arbitrary.sqlite");
        create_sqlite(&arbitrary_database);
        let error = open_provider_sqlite_readonly(&arbitrary_database)
            .err()
            .expect("arbitrary symlink ancestor must remain rejected");
        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { .. }
        ));
    }
}
