use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension};
use same_file::Handle;

use crate::{
    schema, search::projections::SearchProjectionCounts, JournalCheckpoint, Result, Store,
    StoreError, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION,
};

mod preflight;
#[cfg(test)]
mod preflight_tests;
mod publish;
mod target;

use preflight::{cold_target_state, prove_adjacent_hard_link_with, ColdTargetState};
use publish::{
    adjacent_retired_path, adjacent_stage_path, append_suffix, cleanup_orphaned_stage_files,
    fsync_directory, install_same_filesystem, link_absent, link_count, remove_database_sidecars,
    remove_path_if_same, remove_stage_sidecars, restore_retired_target,
};
#[cfg(test)]
use target::set_publication_lease_wait;
use target::{
    acquire_publication_lease, admit_empty_generation, publication_lease_wait,
    revalidate_empty_generation,
};

const COLD_LOCK_SUFFIX: &str = ".ctx-native-cold.lock";
const COLD_STAGE_MARKER: &str = ".ctx-native-cold-";
const COLD_RETIRED_TAIL: &str = ".retired.sqlite";
const DATABASE_SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
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
/// Eligibility is a property of the projection being written, not of whether
/// the destination file happens to exist: an absent target and an existing but
/// wholly empty generation both build here, and every destination that already
/// owns content stays on the ordinary incremental writer. The stage is
/// populated through the current NativePath Store APIs, validated, synced, and
/// published with an absent-target-only hard link.
#[doc(hidden)]
pub struct ColdStoreBuild {
    target_path: PathBuf,
    parent_path: PathBuf,
    stage_path: PathBuf,
    stage_identity: Option<Handle>,
    admission: TargetAdmission,
    _lock_file: File,
    store: Option<Store>,
    schema_signature: String,
    schema_prepare: Duration,
    load_started: Instant,
    measured_core_load: Option<Duration>,
    projection_journal_build: Duration,
    installed: bool,
}

/// The destination state this build was admitted against.
///
/// Publication re-proves the admitted state immediately before it installs, and
/// fails closed on any mismatch.
enum TargetAdmission {
    Absent,
    EmptyGeneration {
        identity: Handle,
        records_digest: [u8; 32],
    },
}

// Test-only injection point inside the window the publication lease protects:
// after the emptiness proof, before the destination name is touched.
#[cfg(test)]
std::thread_local! {
    static TEST_POST_PROOF_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_post_proof_hook(hook: Box<dyn FnOnce()>) {
    TEST_POST_PROOF_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

#[cfg(test)]
fn run_post_proof_hook() {
    let hook = TEST_POST_PROOF_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// Surfaces a retained retired generation ahead of the failure that caused it.
///
/// A publication that could neither install nor roll back leaves a real
/// database on disk that nothing else will claim; naming it is more urgent than
/// the underlying error, which is preserved in the message.
fn retained_generation_error(retained: Option<PathBuf>, cause: StoreError) -> StoreError {
    match retained {
        Some(path) => StoreError::ColdStoreRetiredGenerationRetained {
            path,
            cause: cause.to_string(),
        },
        None => cause,
    }
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
        // Reject non-regular destinations before taking the lock. Absent and
        // existing regular targets both stay admissible until the emptiness
        // proof below runs under the exclusive cold lock.
        cold_target_state(&target_path)?;

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
        if !restore_retired_target(&parent_path, file_name, &target_path)? {
            return Ok(None);
        }
        cleanup_orphaned_stage_files(&parent_path, file_name)?;
        if !prove_adjacent_hard_link_with(&target_path, hard_link)? {
            return Ok(None);
        }
        let (admission, carried_records) = match cold_target_state(&target_path)? {
            ColdTargetState::Absent => (TargetAdmission::Absent, Vec::new()),
            ColdTargetState::ExistingRegular => match admit_empty_generation(&target_path)? {
                None => return Ok(None),
                Some(admitted) => (
                    TargetAdmission::EmptyGeneration {
                        identity: admitted.identity,
                        records_digest: admitted.records_digest,
                    },
                    admitted.records,
                ),
            },
        };

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
            // Control records the retired generation owned move into the new
            // one before any provider content, so publication never drops a
            // row the destination already held. The search projection is
            // rebuilt from canonical rows at the end of the load.
            for record in &carried_records {
                store.upsert_record(record)?;
            }
            Ok((store, schema_signature))
        })();
        let (store, schema_signature) = match initialized {
            Ok(value) => value,
            Err(error) => {
                remove_path_if_same(&stage_path, &stage_identity);
                remove_stage_sidecars(&stage_path);
                return Err(error);
            }
        };

        Ok(Some(Self {
            target_path,
            parent_path,
            stage_path,
            stage_identity: Some(stage_identity),
            admission,
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
        // Everything from the emptiness proof through the install runs under one
        // continuous exclusive publication lease. Every writable `Store::open`
        // takes the same lease shared for the lifetime of the Store, so no
        // writable Store — and therefore no commit — can exist in this window.
        let lease = acquire_publication_lease(&self.target_path, publication_lease_wait())?;
        self.revalidate_target()?;
        self.revalidate_stage_link_count(1)?;
        #[cfg(test)]
        run_post_proof_hook();
        let retired_path = self.install()?;
        if let Err(error) = self
            .revalidate_installed_link()
            .and_then(|()| fsync_directory(&self.parent_path))
        {
            self.rollback_uninstalled_target();
            let retained = self.restore_retired_generation(retired_path.as_deref());
            drop(lease);
            return Err(retained_generation_error(retained, error));
        }
        if let Some(retired_path) = retired_path {
            let _ = fs::remove_file(retired_path);
            let _ = fsync_directory(&self.parent_path);
        }
        drop(lease);
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

    /// Re-proves the admitted destination state immediately before publication.
    ///
    /// The caller holds the exclusive publication lease across this proof and
    /// the install that follows, and every writable `Store::open` takes that
    /// same lease shared for the lifetime of the Store. No writable Store can
    /// therefore exist between the proof and the install, which is what makes
    /// the proof hold through publication rather than only at the instant it
    /// runs.
    ///
    /// An absent target must still be absent. An empty generation must still be
    /// the exact admitted object, must still be empty, and its carried rows must
    /// still digest identically, contents included.
    fn revalidate_target(&self) -> Result<()> {
        match &self.admission {
            TargetAdmission::Absent => match fs::symlink_metadata(&self.target_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(StoreError::ColdStoreTargetChanged(self.target_path.clone())),
            },
            TargetAdmission::EmptyGeneration {
                identity,
                records_digest,
            } => {
                self.revalidate_target_identity(identity)?;
                revalidate_empty_generation(&self.target_path, records_digest)?;
                self.revalidate_target_identity(identity)
            }
        }
    }

    fn revalidate_target_identity(&self, identity: &Handle) -> Result<()> {
        let changed = || StoreError::ColdStoreTargetChanged(self.target_path.clone());
        let metadata = fs::symlink_metadata(&self.target_path).map_err(|_| changed())?;
        if !metadata.file_type().is_file()
            || Handle::from_path(&self.target_path)
                .map(|current| current != *identity)
                .unwrap_or(true)
        {
            return Err(changed());
        }
        Ok(())
    }

    /// Publishes the stage under the target name.
    ///
    /// Both admissions end in the same absent-target-only hard link, which is
    /// the anti-clobber primitive: it fails closed the instant any other object
    /// owns the destination name. An admitted empty generation is first linked
    /// aside so the name can be made absent without ever losing the original
    /// object, and is restored if publication does not succeed.
    fn install(&self) -> Result<Option<PathBuf>> {
        let TargetAdmission::EmptyGeneration { identity, .. } = &self.admission else {
            install_same_filesystem(&self.stage_path, &self.target_path)?;
            return Ok(None);
        };
        // The name is minted under the exclusive cold lock with a fresh nonce,
        // so nothing else can own it and every failure below can drop it.
        let retired_path = adjacent_retired_path(&self.target_path);
        match self.retire_and_publish(identity, &retired_path) {
            Ok(()) => Ok(Some(retired_path)),
            Err(error) => {
                let retained = self.restore_retired_generation(Some(&retired_path));
                Err(retained_generation_error(retained, error))
            }
        }
    }

    /// Retires the admitted generation, then publishes the stage in its place.
    ///
    /// The backup link is made durable *before* the only other name for the
    /// object is removed, so no crash can leave the parent directory without a
    /// name for the admitted generation. Each subsequent directory mutation is
    /// synced before the next one, so every reachable crash state is one this
    /// builder's recovery can resolve.
    fn retire_and_publish(&self, identity: &Handle, retired_path: &Path) -> Result<()> {
        let changed = || StoreError::ColdStoreTargetChanged(self.target_path.clone());
        link_absent(&self.target_path, retired_path)?;
        if Handle::from_path(retired_path)
            .map(|current| current != *identity)
            .unwrap_or(true)
            || link_count(retired_path)?.is_some_and(|actual| actual != 2)
        {
            return Err(changed());
        }
        // The backup name must be durable before the original name is removed.
        fsync_directory(&self.parent_path)?;
        remove_path_if_same(&self.target_path, identity);
        if fs::symlink_metadata(&self.target_path).is_ok() {
            return Err(changed());
        }
        remove_database_sidecars(&self.target_path);
        fsync_directory(&self.parent_path)?;
        install_same_filesystem(&self.stage_path, &self.target_path)?;
        fsync_directory(&self.parent_path)
    }

    /// Puts an admitted generation back under the target name after a failed
    /// publication.
    ///
    /// The restore is itself absent-target-only, so a concurrent winner is never
    /// overwritten by the rollback. When the restore cannot succeed — which is
    /// exactly the designed case where another object took the destination name
    /// — the retired copy is **kept**, and its path is returned so the failure
    /// names it. The next lock owner resolves it rather than deleting it.
    #[must_use]
    fn restore_retired_generation(&self, retired_path: Option<&Path>) -> Option<PathBuf> {
        let retired_path = retired_path?;
        if !matches!(self.admission, TargetAdmission::EmptyGeneration { .. })
            || fs::symlink_metadata(retired_path).is_err()
        {
            return None;
        }
        if link_absent(retired_path, &self.target_path).is_err() {
            let _ = fsync_directory(&self.parent_path);
            return Some(retired_path.to_path_buf());
        }
        let _ = fsync_directory(&self.parent_path);
        let _ = fs::remove_file(retired_path);
        let _ = fsync_directory(&self.parent_path);
        None
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
                remove_path_if_same(&self.stage_path, identity);
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

#[cfg(test)]
mod tests;
