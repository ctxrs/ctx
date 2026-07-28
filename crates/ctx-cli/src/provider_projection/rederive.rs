use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};

use ctx_history_core::database_path;
use ctx_history_store::{publish_over_existing_store, Store};

use crate::analytics::{ImportTelemetry, ProviderRefreshTrigger};
use crate::commands::import::{run_import_internal, ImportRunOptions, ProviderRefreshCollector};
use crate::config::AppConfig;
use crate::output::JsonOutputFormat;
use crate::progress::{format_bytes, format_count, ProgressArg};
use crate::provider_args::NativeProviderArg;
use crate::provider_sources::SourceInfo;
use crate::ImportArgs;

use clap::ValueEnum;

use super::{disk, rebuild_lock_path, retired_store_path, sources, staging_root, ProjectionState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RebuildOutcome {
    /// Nothing to do. A native store is already addressable by this binary.
    AlreadyNative,
    /// No store on disk. A fresh install never rebuilds.
    Uninitialized,
    Rebuilt(RebuildReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RebuildReport {
    pub(crate) sources: usize,
    pub(crate) events: i64,
    pub(crate) sessions: i64,
    pub(crate) session_edges: i64,
    pub(crate) capture_sources: i64,
    pub(crate) installed_bytes: u64,
    pub(crate) rebuilt_bytes: u64,
    pub(crate) elapsed: Duration,
}

impl RebuildReport {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "rebuilt": true,
            "sources": self.sources,
            "events": self.events,
            "sessions": self.sessions,
            "session_edges": self.session_edges,
            "capture_sources": self.capture_sources,
            "previous_bytes": self.installed_bytes,
            "rebuilt_bytes": self.rebuilt_bytes,
            "elapsed_seconds": self.elapsed.as_secs_f64(),
        })
    }
}

/// Whether another process currently owns the rebuild for this store.
pub(crate) fn rebuild_is_running(data_root: &Path) -> bool {
    let path = rebuild_lock_path(data_root);
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

struct RebuildLock {
    file: File,
}

impl RebuildLock {
    fn acquire(data_root: &Path) -> Result<Self> {
        fs::create_dir_all(data_root).context("initialize ctx data root")?;
        let path = rebuild_lock_path(data_root);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open rebuild lock {}", path.display()))?;
        file.try_lock_exclusive().map_err(|_| {
            anyhow!("a provider history rebuild is already running for this ctx index")
        })?;
        Ok(Self { file })
    }
}

impl Drop for RebuildLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Re-derives the provider projection into a new store and installs it.
///
/// Never deletes before it rebuilds. The current store stays installed and
/// usable for the whole re-derivation; publication is one rename plus one hard
/// link at the end. Any failure or process death before publication leaves the
/// original store in place, and the next run starts over from a clean stage.
pub(crate) fn run_rebuild(
    data_root: &Path,
    config: &AppConfig,
    progress: ProgressArg,
    json: bool,
    quiet: bool,
) -> Result<RebuildOutcome> {
    let db_path = database_path(data_root.to_path_buf());
    let lock = RebuildLock::acquire(data_root)?;
    recover_interrupted_publication(&db_path)?;
    if !db_path.exists() {
        return Ok(RebuildOutcome::Uninitialized);
    }

    // A writable open records the generation for a store this binary has not
    // migrated yet, and is what makes a second rebuild a no-op.
    let store = Store::open(&db_path)
        .with_context(|| format!("open ctx index {} before rebuilding", db_path.display()))?;
    let requires_rebuild = store
        .provider_projection_state()?
        .is_some_and(|state| state.generation.requires_rederivation());
    drop(store);
    if !requires_rebuild {
        return Ok(RebuildOutcome::AlreadyNative);
    }

    let headroom = disk::ensure_headroom(&db_path)?;
    let recorded = sources::recorded_roots(&db_path)?;
    let discovered = sources::discovered_importable_sources();
    let replay = sources::ordered_for_cold_build(sources::replayable_roots(&recorded, &discovered));
    if discovered.is_empty() && replay.is_empty() {
        bail!(
            "cannot re-derive the provider index: none of the provider history this index was \
             built from is present on this machine any more. Re-import the history you want with \
             `ctx import` once it is available."
        );
    }

    let staging = staging_root(data_root);
    reset_staging(&staging)?;
    if !quiet && !json {
        eprintln!(
            "ctx: re-deriving the provider index from provider history \
             ({} of index replaced, {} free)",
            format_bytes(headroom.installed_bytes),
            format_bytes(headroom.available_bytes)
        );
    }

    let started = Instant::now();
    let build = build_staged_store(&staging, config, &discovered, &replay, progress, json);
    let sources_imported = match build {
        Ok(count) => count,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.context(
                "re-derivation failed; the existing ctx index is unchanged and still usable",
            ));
        }
    };

    let staged_db = database_path(staging.clone());
    let prepared = quiesce_staged_store(&staged_db)
        .and_then(|()| validate_staged_store(&staged_db))
        // Content-addressed objects move next to the installed Store before the
        // swap, so a failure here still leaves the original generation in place
        // rather than an index whose blobs are in a directory about to be
        // deleted.
        .and_then(|counts| merge_object_store(&staging, data_root).map(|()| counts));
    let counts = match prepared {
        Ok(counts) => counts,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.context(
                "the re-derived index failed validation and was discarded; the existing ctx \
                 index is unchanged and still usable",
            ));
        }
    };

    let rebuilt_bytes = fs::metadata(&staged_db)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    publish(&staged_db, &db_path)?;
    let _ = fs::remove_dir_all(&staging);
    drop(lock);

    Ok(RebuildOutcome::Rebuilt(RebuildReport {
        sources: sources_imported,
        events: counts.events,
        sessions: counts.sessions,
        session_edges: counts.session_edges,
        capture_sources: counts.capture_sources,
        installed_bytes: headroom.installed_bytes,
        rebuilt_bytes,
        elapsed: started.elapsed(),
    }))
}

/// Imports every provider source into an empty staging store.
///
/// The staging destination is absent, so the first Codex session tree takes the
/// cold builder exactly as a fresh install does; everything after it uses the
/// ordinary incremental writer, exactly as `ctx import --all` does today.
fn build_staged_store(
    staging: &Path,
    config: &AppConfig,
    discovered: &[SourceInfo],
    replay: &[SourceInfo],
    progress: ProgressArg,
    json: bool,
) -> Result<usize> {
    let options = ImportRunOptions {
        progress,
        json,
        print_human: false,
        allow_empty_sources: true,
        include_history_source_plugins: true,
        operation: "rebuild",
    };
    let mut imported = 0_usize;
    if !discovered.is_empty() {
        let args = rebuild_import_args(None);
        let mut telemetry = ImportTelemetry::from_args(&args);
        let mut refreshes = ProviderRefreshCollector::default();
        let report = run_import_internal(
            &args,
            staging.to_path_buf(),
            &mut telemetry,
            &mut refreshes,
            ProviderRefreshTrigger::Import,
            config,
            options.clone(),
        )?;
        imported = imported.saturating_add(report.totals.imported_sources);
    }
    for source in replay {
        let args = rebuild_import_args(Some(source));
        let mut telemetry = ImportTelemetry::from_args(&args);
        let mut refreshes = ProviderRefreshCollector::default();
        let report = run_import_internal(
            &args,
            staging.to_path_buf(),
            &mut telemetry,
            &mut refreshes,
            ProviderRefreshTrigger::Import,
            config,
            options.clone(),
        )
        .with_context(|| {
            format!(
                "re-derive {} history from {}",
                source.provider.as_str(),
                source.path.display()
            )
        })?;
        imported = imported.saturating_add(report.totals.imported_sources);
    }
    if imported == 0 {
        bail!("no provider history could be re-derived from any recorded or discovered source");
    }
    Ok(imported)
}

fn native_provider_arg(provider: ctx_history_core::CaptureProvider) -> Option<NativeProviderArg> {
    NativeProviderArg::value_variants()
        .iter()
        .copied()
        .find(|candidate| candidate.capture_provider() == provider)
}

fn rebuild_import_args(source: Option<&SourceInfo>) -> ImportArgs {
    ImportArgs {
        provider: source.and_then(|source| native_provider_arg(source.provider)),
        path: source.map(|source| source.path.clone()),
        history_source: None,
        history_source_manifest: Vec::new(),
        reset_cursor: false,
        input_format: None,
        all: source.is_none(),
        resume: false,
        partial: false,
        no_daemon: true,
        format: JsonOutputFormat::Text,
        progress: ProgressArg::None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedCounts {
    events: i64,
    sessions: i64,
    session_edges: i64,
    capture_sources: i64,
}

/// Folds the write-ahead log into the staged database file.
///
/// Publication installs one inode, so every committed byte has to be inside it
/// first. Closing the last connection normally checkpoints and removes the log,
/// but that is not something to assume about a file that is about to become the
/// user's index: a log that survives is a hard failure, not a file to delete.
fn quiesce_staged_store(staged_db: &Path) -> Result<()> {
    {
        let store = Store::open(staged_db).with_context(|| {
            format!("reopen re-derived index {} to quiesce", staged_db.display())
        })?;
        store.checkpoint_wal_truncate_required()?;
    }
    for suffix in ["-wal", "-journal"] {
        let path = sidecar_path(staged_db, suffix);
        let residual = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        };
        if residual != 0 {
            bail!(
                "re-derived index still holds {residual} bytes in {}; refusing to publish a \
                 generation whose committed data is outside the file",
                path.display()
            );
        }
        let _ = fs::remove_file(&path);
    }
    // The shared-memory file is derived from the log and is rebuilt on demand.
    let _ = fs::remove_file(sidecar_path(staged_db, "-shm"));
    Ok(())
}

/// Proves the re-derived store is installable before anything is replaced.
fn validate_staged_store(staged_db: &Path) -> Result<StagedCounts> {
    if !staged_db.exists() {
        bail!("the re-derivation produced no ctx index");
    }
    {
        // Reopen through the Store: the build's own connection proves nothing
        // about the bytes that ended up durable, and this checks schema
        // version and final schema identity on the way in.
        let store = Store::open_read_only(staged_db)
            .with_context(|| format!("reopen re-derived index {}", staged_db.display()))?;
        let findings = store.validate()?;
        if !findings.is_empty() {
            bail!(
                "re-derived index failed validation: {}",
                findings.join("; ")
            );
        }
        if store
            .provider_projection_state()?
            .is_some_and(|state| state.generation.requires_rederivation())
        {
            bail!("re-derived index is still a superseded provider projection");
        }
    }

    let conn = Connection::open_with_flags(
        staged_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let counts = StagedCounts {
        events: scalar(&conn, "SELECT COUNT(*) FROM events")?,
        sessions: scalar(&conn, "SELECT COUNT(*) FROM sessions")?,
        session_edges: scalar(&conn, "SELECT COUNT(*) FROM session_edges")?,
        capture_sources: scalar(&conn, "SELECT COUNT(*) FROM capture_sources")?,
    };
    if counts.events == 0 || counts.sessions == 0 {
        bail!(
            "re-derived index has {} sessions and {} events, which cannot be a complete \
             re-derivation",
            counts.sessions,
            counts.events
        );
    }
    for table in [
        "ctx_history_search",
        "event_search",
        "artifact_search",
        "ctx_history_search_scriptgram",
        "event_search_scriptgram",
    ] {
        conn.query_row(
            &format!("SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 1"),
            ["ctx_rebuild_validation_impossible_5f3a91c2"],
            |_| Ok(()),
        )
        .optional()
        .with_context(|| format!("query re-derived search index {table}"))?;
    }
    Ok(counts)
}

fn scalar(conn: &Connection, sql: &str) -> Result<i64> {
    Ok(conn.query_row(sql, [], |row| row.get(0))?)
}

/// Installs the re-derived generation with the cold-store publication.
fn publish(staged_db: &Path, db_path: &Path) -> Result<()> {
    let retired = retired_store_path(db_path);
    publish_over_existing_store(staged_db, db_path, &retired)
        .with_context(|| format!("install the re-derived ctx index at {}", db_path.display()))?;
    // Only past this point may anything belonging to the old generation go
    // away: the new generation is durable and linked at the Store path.
    remove_sqlite_sidecars(db_path);
    let _ = fs::remove_file(&retired);
    remove_sqlite_sidecars(&retired);
    Ok(())
}

/// Restores a store whose publication was interrupted between the rename and
/// the link, and clears a retirement left behind by a completed one.
pub(crate) fn recover_interrupted_publication(db_path: &Path) -> Result<()> {
    let retired = retired_store_path(db_path);
    if !retired.exists() {
        return Ok(());
    }
    if db_path.exists() {
        // Publication completed; the retired generation is superseded.
        let _ = fs::remove_file(&retired);
        remove_sqlite_sidecars(&retired);
        return Ok(());
    }
    fs::rename(&retired, db_path).with_context(|| {
        format!(
            "restore the ctx index from an interrupted rebuild at {}",
            retired.display()
        )
    })?;
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn remove_sqlite_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(sidecar_path(path, suffix));
    }
}

fn reset_staging(staging: &Path) -> Result<()> {
    match fs::remove_dir_all(staging) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("clear the previous rebuild stage {}", staging.display()))
        }
    }
    fs::create_dir_all(staging)
        .with_context(|| format!("create the rebuild stage {}", staging.display()))?;
    Ok(())
}

/// Moves content-addressed objects the rebuild produced next to the installed
/// store. Object names are content digests, so an existing name is the same
/// bytes and is left alone.
fn merge_object_store(staging: &Path, data_root: &Path) -> Result<()> {
    let source = staging.join("objects");
    if !source.is_dir() {
        return Ok(());
    }
    let destination = data_root.join("objects");
    fs::create_dir_all(&destination)?;
    merge_directory(&source, &destination)
}

fn merge_directory(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            fs::create_dir_all(&target)?;
            merge_directory(&entry.path(), &target)?;
        } else if !target.exists() {
            fs::rename(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub(crate) fn print_report(outcome: &RebuildOutcome, json: bool, quiet: bool) -> Result<()> {
    match outcome {
        RebuildOutcome::Uninitialized if json => crate::output::print_json(json!({
            "rebuilt": false,
            "state": ProjectionState::Uninitialized.as_str(),
        })),
        RebuildOutcome::AlreadyNative if json => crate::output::print_json(json!({
            "rebuilt": false,
            "state": ProjectionState::Native.as_str(),
        })),
        RebuildOutcome::Rebuilt(report) if json => crate::output::print_json(report.to_json()),
        RebuildOutcome::Uninitialized => {
            if !quiet {
                println!("No ctx index to rebuild. Run `ctx setup` or `ctx import` first.");
            }
            Ok(())
        }
        RebuildOutcome::AlreadyNative => {
            if !quiet {
                println!("The provider index is already current; nothing to rebuild.");
            }
            Ok(())
        }
        RebuildOutcome::Rebuilt(report) => {
            if !quiet {
                println!(
                    "Rebuilt the provider index from {} source{}: {} events across {} sessions in {:.1}s.",
                    format_count(report.sources),
                    if report.sources == 1 { "" } else { "s" },
                    format_count(report.events.max(0) as usize),
                    format_count(report.sessions.max(0) as usize),
                    report.elapsed.as_secs_f64(),
                );
            }
            Ok(())
        }
    }
}
