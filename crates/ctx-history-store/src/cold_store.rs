use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension};
use same_file::Handle;
use uuid::Uuid;

use crate::{
    schema, search::projections::SearchProjectionCounts, JournalCheckpoint, Result, Store,
    StoreError, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION,
};

mod preflight;
#[cfg(test)]
mod preflight_tests;

use preflight::{
    cold_target_state, create_absent_hard_link_with, prove_adjacent_hard_link_with,
    ColdTargetState, HardLinkOutcome,
};

const COLD_LOCK_SUFFIX: &str = ".ctx-native-cold.lock";
const COLD_STAGE_MARKER: &str = ".ctx-native-cold-";
const FTS_TABLES: [&str; 5] = [
    "ctx_history_search",
    "event_search",
    "artifact_search",
    "ctx_history_search_scriptgram",
    "event_search_scriptgram",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColdStoreBuildCounts {
    pub history_records: usize,
    pub sources: usize,
    pub capture_sources: usize,
    pub sessions: usize,
    pub session_edges: usize,
    pub runs: usize,
    pub events: usize,
    pub file_touches: usize,
    pub batches: usize,
    pub groups: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColdStoreBuildTimings {
    pub schema_prepare: Duration,
    pub core_load: Duration,
    pub projection_journal_build: Duration,
    pub index_and_fts_build: Duration,
    pub database_validation: Duration,
    pub search_validation: Duration,
    pub validation: Duration,
    pub durable_install: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdStoreBuildReceipt {
    pub target_path: PathBuf,
    pub counts: ColdStoreBuildCounts,
    pub database_bytes: u64,
    pub deferred_index_count: usize,
    pub timings: ColdStoreBuildTimings,
}

/// Adjacent builder for a final-format Store generation.
///
/// Existing destinations always remain on the ordinary incremental writer.
/// The stage is populated through the current NativePath Store APIs, validated,
/// synced, and installed with an absent-target-only hard-link publication.
#[doc(hidden)]
pub struct ColdStoreBuild {
    target_path: PathBuf,
    parent_path: PathBuf,
    stage_path: PathBuf,
    stage_identity: Option<Handle>,
    _lock_file: File,
    store: Option<Store>,
    schema_signature: String,
    schema_prepare: Duration,
    load_started: Instant,
    measured_core_load: Option<Duration>,
    projection_journal_build: Duration,
    installed: bool,
}

impl ColdStoreBuild {
    pub fn begin(target_path: impl AsRef<Path>) -> Result<Option<Self>> {
        Self::begin_with_hard_link_probe(target_path, |source, target| {
            fs::hard_link(source, target)
        })
    }

    #[doc(hidden)]
    pub fn begin_with_hard_link_probe<HardLink>(
        target_path: impl AsRef<Path>,
        hard_link: HardLink,
    ) -> Result<Option<Self>>
    where
        HardLink: FnOnce(&Path, &Path) -> std::io::Result<()>,
    {
        let prepare_started = Instant::now();
        let requested = target_path.as_ref();
        let file_name = requested
            .file_name()
            .ok_or_else(|| StoreError::ColdStoreTargetIneligible(requested.to_path_buf()))?;
        let requested_parent = requested.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(requested_parent)?;
        let parent_path = fs::canonicalize(requested_parent)?;
        let target_path = parent_path.join(file_name);
        match cold_target_state(&target_path)? {
            ColdTargetState::ExistingRegular => return Ok(None),
            ColdTargetState::Absent => {}
        }

        let lock_path = append_suffix(&target_path, COLD_LOCK_SUFFIX);
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        if let Err(error) = lock_file.try_lock_exclusive() {
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                return Err(StoreError::ColdStoreBuildBusy(target_path));
            }
            return Err(error.into());
        }
        cleanup_orphaned_stage_files(&parent_path, file_name)?;
        if !prove_adjacent_hard_link_with(&target_path, hard_link)? {
            return Ok(None);
        }
        match cold_target_state(&target_path)? {
            ColdTargetState::ExistingRegular => return Ok(None),
            ColdTargetState::Absent => {}
        }

        let stage_path = adjacent_stage_path(&target_path);
        let stage_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&stage_path)?;
        let stage_identity = Handle::from_file(stage_file)?;

        let initialized = (|| {
            let store = Store::open_new_cold_stage(&stage_path)?;
            if !store.fresh_provider_projection_eligible()? {
                return Err(StoreError::ColdStoreInvalidState);
            }
            let schema_signature = store.schema()?;
            store
                .conn
                .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            store.conn.pragma_update(None, "journal_mode", "DELETE")?;
            for table in FTS_TABLES {
                schema::fts::drop_fts_table_if_exists(&store.conn, table)?;
            }
            store.invalidate_event_search_projection_capabilities();
            store.conn.execute_batch(
                "PRAGMA journal_mode=OFF;
                 PRAGMA synchronous=OFF;
                 PRAGMA locking_mode=EXCLUSIVE;
                 PRAGMA temp_store=FILE;
                 PRAGMA cache_size=-131072;
                 PRAGMA foreign_keys=ON;",
            )?;
            store.begin_native_cold_load()?;
            Ok((store, schema_signature))
        })();
        let (store, schema_signature) = match initialized {
            Ok(value) => value,
            Err(error) => {
                remove_stage_if_same(&stage_path, &stage_identity);
                remove_stage_sidecars(&stage_path);
                return Err(error);
            }
        };

        Ok(Some(Self {
            target_path,
            parent_path,
            stage_path,
            stage_identity: Some(stage_identity),
            _lock_file: lock_file,
            store: Some(store),
            schema_signature,
            schema_prepare: prepare_started.elapsed(),
            load_started: Instant::now(),
            measured_core_load: None,
            projection_journal_build: Duration::ZERO,
            installed: false,
        }))
    }

    pub fn store(&self) -> Result<&Store> {
        self.store.as_ref().ok_or(StoreError::ColdStoreInvalidState)
    }

    pub fn store_mut(&mut self) -> Result<&mut Store> {
        self.store.as_mut().ok_or(StoreError::ColdStoreInvalidState)
    }

    pub fn stage_path(&self) -> Result<&Path> {
        Ok(&self.stage_path)
    }

    pub fn counts(&self) -> Result<ColdStoreBuildCounts> {
        store_counts(self.store()?)
    }

    #[doc(hidden)]
    pub fn activate_projection_journal(
        &mut self,
        contract_fingerprint: &str,
    ) -> Result<JournalCheckpoint> {
        if self.measured_core_load.is_some() {
            return Err(StoreError::ColdStoreInvalidState);
        }
        self.measured_core_load = Some(self.load_started.elapsed());
        let started = Instant::now();
        let checkpoint = self
            .store()?
            .activate_native_cold_projection_journal(contract_fingerprint)?;
        self.projection_journal_build = started.elapsed();
        Ok(checkpoint)
    }

    pub fn finish(self) -> Result<ColdStoreBuildReceipt> {
        self.finish_with_pre_install(|_| Ok(()))
    }

    #[doc(hidden)]
    pub fn finish_with_pre_install<BeforeInstall>(
        mut self,
        before_install: BeforeInstall,
    ) -> Result<ColdStoreBuildReceipt>
    where
        BeforeInstall: FnOnce(&Path) -> Result<()>,
    {
        let core_load = self
            .measured_core_load
            .unwrap_or_else(|| self.load_started.elapsed());
        let store = self
            .store
            .as_ref()
            .ok_or(StoreError::ColdStoreInvalidState)?;
        if !store.conn.is_autocommit()
            || store.batch_depth.get() != 0
            || store.connection_quarantined.get()
            || store
                .event_search_bulk_depth
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
        {
            return Err(StoreError::ColdStoreInvalidState);
        }
        let counts = store_counts(store)?;

        let index_started = Instant::now();
        store.finish_native_cold_load()?;
        store.conn.execute_batch(
            "PRAGMA locking_mode=NORMAL;
             PRAGMA synchronous=OFF;",
        )?;
        schema::create_fts_tables_if_supported(&store.conn)?;
        for table in FTS_TABLES {
            if !table_exists(&store.conn, table)? {
                return Err(StoreError::ColdStoreValidation(format!(
                    "required FTS table {table} is unavailable"
                )));
            }
        }
        let expected_search_counts = store.rebuild_search_projection_with_counts()?;
        store.conn.execute_batch(
            "PRAGMA synchronous=FULL;
             PRAGMA journal_mode=DELETE;
             PRAGMA locking_mode=NORMAL;",
        )?;
        let index_and_fts_build = index_started.elapsed();

        let validation_started = Instant::now();
        let validation_timings = validate_final_store(
            store,
            &self.schema_signature,
            counts,
            expected_search_counts,
        )?;
        let validation = validation_started.elapsed();

        self.store.take();
        self.revalidate_stage()?;
        let reopened = Store::open_read_only(&self.stage_path)?;
        validate_reopened_store(&reopened, &self.schema_signature)?;
        drop(reopened);
        self.revalidate_stage()?;
        before_install(&self.stage_path)?;
        self.revalidate_stage()?;
        remove_stage_sidecars(&self.stage_path);

        let install_started = Instant::now();
        let database = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.stage_path)?;
        database.sync_all()?;
        let database_bytes = database.metadata()?.len();
        drop(database);
        fsync_directory(&self.parent_path)?;
        self.revalidate_target()?;
        self.revalidate_stage_link_count(1)?;
        install_same_filesystem(&self.stage_path, &self.target_path)?;
        if let Err(error) = self
            .revalidate_installed_link()
            .and_then(|()| fsync_directory(&self.parent_path))
        {
            self.rollback_uninstalled_target();
            return Err(error);
        }
        self.installed = true;
        self.stage_identity.take();
        let _ = fs::remove_file(&self.stage_path);
        let _ = fsync_directory(&self.parent_path);
        let durable_install = install_started.elapsed();

        Ok(ColdStoreBuildReceipt {
            target_path: self.target_path.clone(),
            counts,
            database_bytes,
            deferred_index_count: 0,
            timings: ColdStoreBuildTimings {
                schema_prepare: self.schema_prepare,
                core_load,
                projection_journal_build: self.projection_journal_build,
                index_and_fts_build,
                database_validation: validation_timings.database,
                search_validation: validation_timings.search,
                validation,
                durable_install,
            },
        })
    }

    fn revalidate_target(&self) -> Result<()> {
        match fs::symlink_metadata(&self.target_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(StoreError::ColdStoreTargetChanged(self.target_path.clone())),
        }
    }

    fn revalidate_stage(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.stage_path)
            .map_err(|_| StoreError::ColdStoreInvalidState)?;
        let identity = self
            .stage_identity
            .as_ref()
            .ok_or(StoreError::ColdStoreInvalidState)?;
        if !metadata.file_type().is_file()
            || Handle::from_path(&self.stage_path)
                .map(|current| current != *identity)
                .unwrap_or(true)
        {
            return Err(StoreError::ColdStoreInvalidState);
        }
        Ok(())
    }

    fn revalidate_stage_link_count(&self, expected: u64) -> Result<()> {
        self.revalidate_stage()?;
        if link_count(&self.stage_path)?.is_some_and(|actual| actual != expected) {
            return Err(StoreError::ColdStoreInvalidState);
        }
        Ok(())
    }

    fn revalidate_installed_link(&self) -> Result<()> {
        self.revalidate_stage_link_count(2)?;
        let identity = self
            .stage_identity
            .as_ref()
            .ok_or(StoreError::ColdStoreInvalidState)?;
        let target =
            Handle::from_path(&self.target_path).map_err(|_| StoreError::ColdStoreInvalidState)?;
        if target != *identity || link_count(&self.target_path)?.is_some_and(|actual| actual != 2) {
            return Err(StoreError::ColdStoreInvalidState);
        }
        Ok(())
    }

    fn rollback_uninstalled_target(&self) {
        let Some(identity) = self.stage_identity.as_ref() else {
            return;
        };
        let exact_target = Handle::from_path(&self.target_path)
            .map(|target| target == *identity)
            .unwrap_or(false);
        if exact_target {
            let _ = fs::remove_file(&self.target_path);
            let _ = fsync_directory(&self.parent_path);
        }
    }
}

impl Drop for ColdStoreBuild {
    fn drop(&mut self) {
        self.store.take();
        if !self.installed {
            if let Some(identity) = self.stage_identity.as_ref() {
                remove_stage_if_same(&self.stage_path, identity);
            }
            remove_stage_sidecars(&self.stage_path);
        }
    }
}

fn store_counts(store: &Store) -> Result<ColdStoreBuildCounts> {
    Ok(ColdStoreBuildCounts {
        history_records: query_count(&store.conn, "SELECT COUNT(*) FROM history_records")?,
        sources: query_count(
            &store.conn,
            "SELECT COUNT(*) FROM provider_source_locators WHERE is_current = 1",
        )?,
        capture_sources: query_count(&store.conn, "SELECT COUNT(*) FROM capture_sources")?,
        sessions: query_count(&store.conn, "SELECT COUNT(*) FROM sessions")?,
        session_edges: query_count(&store.conn, "SELECT COUNT(*) FROM session_edges")?,
        runs: query_count(&store.conn, "SELECT COUNT(*) FROM runs")?,
        events: query_count(&store.conn, "SELECT COUNT(*) FROM events")?,
        file_touches: query_count(&store.conn, "SELECT COUNT(*) FROM files_touched")?,
        batches: query_count(&store.conn, "SELECT COUNT(*) FROM sync_cursors")?,
        groups: query_count(
            &store.conn,
            "SELECT COUNT(*) FROM projection_journal_chunks",
        )?,
    })
}

fn validate_final_store(
    store: &Store,
    schema_signature: &str,
    expected_counts: ColdStoreBuildCounts,
    expected_search_counts: SearchProjectionCounts,
) -> Result<ColdStoreValidationTimings> {
    let database_started = Instant::now();
    validate_store_identity(store, schema_signature)?;
    let integrity = {
        let mut statement = store.conn.prepare("PRAGMA integrity_check")?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    if integrity != ["ok"] {
        return Err(StoreError::ColdStoreValidation(format!(
            "integrity_check failed: {}",
            integrity.join("; ")
        )));
    }
    let foreign_key_errors =
        query_count(&store.conn, "SELECT COUNT(*) FROM pragma_foreign_key_check")?;
    if foreign_key_errors != 0 {
        return Err(StoreError::ColdStoreValidation(format!(
            "foreign_key_check reported {foreign_key_errors} rows"
        )));
    }
    validate_store_counts(store, expected_counts)?;
    let database = database_started.elapsed();

    let search_started = Instant::now();
    validate_search_projection(store, expected_search_counts)?;
    let search = search_started.elapsed();
    Ok(ColdStoreValidationTimings { database, search })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ColdStoreValidationTimings {
    database: Duration,
    search: Duration,
}

fn validate_search_projection(store: &Store, expected: SearchProjectionCounts) -> Result<()> {
    let actual = SearchProjectionCounts {
        history_search: query_count(&store.conn, "SELECT COUNT(*) FROM ctx_history_search")?,
        history_scriptgram: query_count(
            &store.conn,
            "SELECT COUNT(*) FROM ctx_history_search_scriptgram",
        )?,
        event_search: query_count(&store.conn, "SELECT COUNT(*) FROM event_search")?,
        event_lookup: query_count(&store.conn, "SELECT COUNT(*) FROM event_search_lookup")?,
        event_scriptgram: query_count(&store.conn, "SELECT COUNT(*) FROM event_search_scriptgram")?,
        artifact_search: query_count(&store.conn, "SELECT COUNT(*) FROM artifact_search")?,
    };
    if actual != expected {
        return Err(StoreError::ColdStoreValidation(
            "rebuilt search authority does not match canonical rows".to_owned(),
        ));
    }
    for table in FTS_TABLES {
        store
            .conn
            .query_row(
                &format!(
                    "SELECT rowid FROM {} WHERE {} MATCH ?1 LIMIT 1",
                    quoted_identifier(table),
                    quoted_identifier(table)
                ),
                ["ctx_cold_validation_impossible_6f669c28"],
                |_| Ok(()),
            )
            .optional()?;
    }
    Ok(())
}

fn validate_reopened_store(store: &Store, schema_signature: &str) -> Result<()> {
    validate_store_identity(store, schema_signature)?;
    for table in [
        "history_records",
        "provider_source_locators",
        "capture_sources",
        "sessions",
        "session_edges",
        "runs",
        "events",
        "files_touched",
        "sync_cursors",
        "projection_journal_chunks",
        "ctx_history_search",
        "event_search",
        "artifact_search",
        "ctx_history_search_scriptgram",
        "event_search_scriptgram",
    ] {
        store
            .conn
            .query_row(
                &format!("SELECT 1 FROM {} LIMIT 1", quoted_identifier(table)),
                [],
                |_| Ok(()),
            )
            .optional()?;
    }
    Ok(())
}

fn validate_store_identity(store: &Store, schema_signature: &str) -> Result<()> {
    if store.schema()? != schema_signature {
        return Err(StoreError::ColdStoreValidation(
            "final schema differs from canonical Store schema".to_owned(),
        ));
    }
    let user_version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version != SCHEMA_VERSION {
        return Err(StoreError::ColdStoreValidation(format!(
            "unexpected user_version {user_version}"
        )));
    }
    schema::verify_final_schema_identity(&store.conn)?;
    let identity: String = store.conn.query_row(
        "SELECT schema_identity FROM ctx_store_schema_identity WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if identity != FINAL_SCHEMA_IDENTITY {
        return Err(StoreError::ColdStoreValidation(
            "schema identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_store_counts(store: &Store, expected_counts: ColdStoreBuildCounts) -> Result<()> {
    let actual_counts = store_counts(store)?;
    if actual_counts != expected_counts {
        return Err(StoreError::ColdStoreValidation(format!(
            "final Store counts changed during index construction: expected {expected_counts:?}, found {actual_counts:?}"
        )));
    }
    if actual_counts.sources != actual_counts.batches
        || actual_counts.sources != actual_counts.capture_sources
    {
        return Err(StoreError::ColdStoreValidation(
            "current locator, capture-source, and cursor authority counts differ".to_owned(),
        ));
    }
    Ok(())
}

fn query_count(conn: &Connection, sql: &str) -> Result<usize> {
    let value: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    usize::try_from(value).map_err(|_| StoreError::ColdStoreInvalidState)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn adjacent_stage_path(target: &Path) -> PathBuf {
    loop {
        let candidate = append_suffix(
            target,
            &format!("{COLD_STAGE_MARKER}{}.sqlite", Uuid::new_v4()),
        );
        if !candidate.exists() {
            return candidate;
        }
    }
}

fn stage_sidecars(path: &Path) -> Vec<PathBuf> {
    let mut paths = ["-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| append_suffix(path, suffix))
        .collect::<Vec<_>>();
    for suffix in [
        ".event-search-bulk.lock.sqlite",
        ".source-inventory.lock.sqlite",
        ".migration.lock.sqlite",
    ] {
        let lock = append_suffix(path, suffix);
        paths.push(lock.clone());
        for sidecar in ["-wal", "-shm", "-journal"] {
            paths.push(append_suffix(&lock, sidecar));
        }
    }
    paths
}

fn remove_stage_sidecars(path: &Path) {
    for sidecar in stage_sidecars(path) {
        let _ = fs::remove_file(sidecar);
    }
}

fn remove_stage_if_same(path: &Path, identity: &Handle) {
    let matches = fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
        && Handle::from_path(path)
            .map(|current| current == *identity)
            .unwrap_or(false);
    if matches {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
fn fsync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn fsync_directory(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()?;
    Ok(())
}

fn install_same_filesystem(stage: &Path, target: &Path) -> Result<()> {
    match create_absent_hard_link_with(stage, target, |source, target| {
        fs::hard_link(source, target)
    })? {
        HardLinkOutcome::Linked => Ok(()),
        HardLinkOutcome::Unsupported => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "adjacent absent-target hard-link publication is unsupported",
        )
        .into()),
    }
}

fn cleanup_orphaned_stage_files(parent: &Path, target_name: &std::ffi::OsStr) -> Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !is_exact_orphaned_stage_name(target_name, &entry.file_name()) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn is_exact_orphaned_stage_name(
    target_name: &std::ffi::OsStr,
    entry_name: &std::ffi::OsStr,
) -> bool {
    const SUFFIXES: [&[u8]; 16] = [
        b".sqlite",
        b".sqlite-wal",
        b".sqlite-shm",
        b".sqlite-journal",
        b".sqlite.event-search-bulk.lock.sqlite",
        b".sqlite.event-search-bulk.lock.sqlite-wal",
        b".sqlite.event-search-bulk.lock.sqlite-shm",
        b".sqlite.event-search-bulk.lock.sqlite-journal",
        b".sqlite.source-inventory.lock.sqlite",
        b".sqlite.source-inventory.lock.sqlite-wal",
        b".sqlite.source-inventory.lock.sqlite-shm",
        b".sqlite.source-inventory.lock.sqlite-journal",
        b".sqlite.migration.lock.sqlite",
        b".sqlite.migration.lock.sqlite-wal",
        b".sqlite.migration.lock.sqlite-shm",
        b".sqlite.migration.lock.sqlite-journal",
    ];
    let name = entry_name.as_encoded_bytes();
    let target = target_name.as_encoded_bytes();
    let Some(rest) = name
        .strip_prefix(target)
        .and_then(|rest| rest.strip_prefix(COLD_STAGE_MARKER.as_bytes()))
    else {
        return false;
    };
    let Some((uuid, suffix)) = rest.split_at_checked(36) else {
        return false;
    };
    std::str::from_utf8(uuid)
        .ok()
        .and_then(|value| Uuid::parse_str(value).ok())
        .is_some()
        && SUFFIXES.contains(&suffix)
}

#[cfg(unix)]
fn link_count(path: &Path) -> Result<Option<u64>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(fs::metadata(path)?.nlink()))
}

#[cfg(target_os = "windows")]
fn link_count(_path: &Path) -> Result<Option<u64>> {
    // Stable file-ID equality on both names proves that CreateHardLinkW
    // published the exact staged object. Rust 1.88 does not expose the link
    // count needed for the additional Unix invariant.
    Ok(None)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use ctx_history_core::HistoryRecord;
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};

    use super::*;

    #[test]
    fn target_created_before_no_replace_install_preserves_target_and_stage() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage.sqlite");
        let target = temp.path().join("work.sqlite");
        fs::write(&stage, b"stage").unwrap();
        fs::write(&target, b"winner").unwrap();

        assert!(matches!(
            install_same_filesystem(&stage, &target),
            Err(StoreError::ColdStoreTargetChanged(path)) if path == target
        ));
        assert_eq!(fs::read(&target).unwrap(), b"winner");
        assert_eq!(fs::read(&stage).unwrap(), b"stage");
    }

    #[test]
    fn no_replace_install_publishes_the_exact_stage_object() {
        let temp = tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage.sqlite");
        let target = temp.path().join("work.sqlite");
        fs::write(&stage, b"stage").unwrap();
        let stage_identity = Handle::from_path(&stage).unwrap();

        install_same_filesystem(&stage, &target).unwrap();

        assert_eq!(Handle::from_path(&target).unwrap(), stage_identity);
        assert_eq!(fs::read(&target).unwrap(), b"stage");
        assert_eq!(fs::read(&stage).unwrap(), b"stage");
    }

    #[test]
    fn cold_stage_open_does_not_migrate_parent_legacy_store() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = temp.path().join("work-record").join("work.sqlite");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy-store-canary").unwrap();
        let target = temp.path().join("work.sqlite");

        let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();

        assert!(!target.exists());
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy-store-canary");
        drop(builder);
        assert_eq!(fs::read(&legacy).unwrap(), b"legacy-store-canary");
    }

    #[test]
    fn cold_load_retains_every_canonical_explicit_index() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("work.sqlite");
        let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
        let temp_store = builder
            .store()
            .unwrap()
            .conn
            .query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(temp_store, 1, "cold scratch storage must be disk-backed");
        let during_load = query_count(
            &builder.store().unwrap().conn,
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND sql IS NOT NULL",
        )
        .unwrap();
        assert!(during_load > 0);

        let receipt = builder.finish().unwrap();

        assert_eq!(receipt.deferred_index_count, 0);
        let reopened = Store::open_read_only(target).unwrap();
        let installed = query_count(
            &reopened.conn,
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND sql IS NOT NULL",
        )
        .unwrap();
        assert_eq!(installed, during_load);
    }

    #[test]
    fn cold_lock_owner_removes_only_exact_orphan_stage_names() {
        let temp = tempfile::tempdir().unwrap();
        let target_name = std::ffi::OsStr::new("work.sqlite");
        let uuid = "9ff4ee19-a3bf-4b8b-81ce-0b768335cfac";
        let stage = temp
            .path()
            .join(format!("work.sqlite{COLD_STAGE_MARKER}{uuid}.sqlite"));
        let sidecar = append_suffix(&stage, "-wal");
        let impostor = temp.path().join(format!(
            "work.sqlite{COLD_STAGE_MARKER}{uuid}.sqlite.backup"
        ));
        fs::write(&stage, b"orphan").unwrap();
        fs::write(&sidecar, b"orphan-sidecar").unwrap();
        fs::write(&impostor, b"keep").unwrap();

        cleanup_orphaned_stage_files(temp.path(), target_name).unwrap();

        assert!(!stage.exists());
        assert!(!sidecar.exists());
        assert_eq!(fs::read(impostor).unwrap(), b"keep");
    }

    #[test]
    fn cold_search_validation_is_read_only_and_detects_projection_count_drift() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("stage.sqlite")).unwrap();
        store
            .upsert_record(&HistoryRecord {
                id: Uuid::new_v4(),
                title: "cold validation".to_owned(),
                body: "検索投影の完全な入力".to_owned(),
                tags: vec!["cold".to_owned()],
                kind: "task".to_owned(),
                workspace: Some("/workspace".to_owned()),
                created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
                updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            })
            .unwrap();
        let expected = store.rebuild_search_projection_with_counts().unwrap();
        assert_eq!(expected.history_search, 1);
        assert_eq!(expected.history_scriptgram, 1);

        store
            .conn
            .authorizer(Some(|context: AuthContext<'_>| match context.action {
                AuthAction::Insert { .. }
                | AuthAction::Update { .. }
                | AuthAction::Delete { .. } => Authorization::Deny,
                _ => Authorization::Allow,
            }));
        let started = Instant::now();
        validate_search_projection(&store, expected).unwrap();
        let elapsed = started.elapsed();
        store
            .conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        eprintln!("bounded cold search validation: {elapsed:?}");

        store
            .conn
            .execute("DELETE FROM ctx_history_search", [])
            .unwrap();
        assert!(matches!(
            validate_search_projection(&store, expected),
            Err(StoreError::ColdStoreValidation(message))
                if message == "rebuilt search authority does not match canonical rows"
        ));
    }
}
