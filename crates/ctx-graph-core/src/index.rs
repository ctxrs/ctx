use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    ingest::{self, IngestOptions},
    languages,
    model::*,
    parser::{MAX_SOURCE_BYTES, PythonContext, empty_facts, parse_python_with_source_root},
    project_context::{ProjectContext, add_document_aliases},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use ctx_graph_types::EXTRACTOR_REVISION;
const SCAN_MANIFEST_VERSION: u32 = 3;
const MAX_SCAN_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCAN_CACHE_BYTES: usize = 512 * 1024 * 1024;
const MAX_IGNORE_BYTES: u64 = 1024 * 1024;
const SOURCE_IDENTITY_SETTLE_NANOS: u128 = 2_000_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanManifest {
    version: u32,
    root: String,
    database: DatabaseIdentity,
    generation: u64,
    stored_options: serde_json::Value,
    index_options: serde_json::Value,
    extractor_revision: u32,
    language_revision: String,
    ingest_fingerprint: String,
    scan: ScanProof,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseIdentity {
    length: u64,
    modified: String,
    file_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanProof {
    supported_files: usize,
    unsupported_files: usize,
    ignore_fingerprint: String,
    sources: Vec<SourceProof>,
    managed_sources: Vec<SourceProof>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceProof {
    path: String,
    digest: String,
    identity: Option<ManifestSourceIdentity>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSourceIdentity {
    length: u64,
    modified: String,
    file_id: String,
}

struct Discovery {
    files: Vec<(PathBuf, String)>,
    coverage: Coverage,
    ignore_files: Option<Vec<(Vec<u8>, String)>>,
    context_files: Option<Vec<(PathBuf, String)>>,
}

struct PreparedScan {
    files: Vec<(PathBuf, String)>,
    coverage: Coverage,
    managed: Vec<FileFacts>,
    proof: Option<ScanProof>,
    cached_sources: Option<HashMap<String, CachedSource>>,
    freshly_read: BTreeSet<String>,
}

struct CachedSource {
    digest: String,
    content: Option<Vec<u8>>,
    version: SourceVersion,
}

#[derive(Clone, PartialEq, Eq)]
struct SourceVersion {
    length: u64,
    modified: std::time::SystemTime,
    file_id: String,
}

thread_local! {
    static PROJECT_CONTEXT_DISCOVERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A debug-build seam used by integration tests to prove the manifest bypasses context parsing.
#[doc(hidden)]
pub fn project_context_discoveries_for_tests() -> usize {
    PROJECT_CONTEXT_DISCOVERIES.with(std::cell::Cell::get)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexOptions {
    pub code_only: bool,
    pub no_gitignore: bool,
    pub include_generated: bool,
    pub max_semantic_files: usize,
    /// Corpus-wide attempted-call and reserved-output limits, in addition to per-file limits.
    pub max_semantic_calls: Option<usize>,
    pub max_semantic_output_tokens: Option<u64>,
    pub semantic_code: bool,
    /// Repository-relative Python import roots; the most specific matching root wins.
    pub python_source_roots: Vec<String>,
    /// Explicit Swift module names mapped to repository-relative source roots.
    pub swift_modules: BTreeMap<String, String>,
    /// Explicit Python interpreter with the official Robot Framework parser installed.
    pub robot_python: Option<PathBuf>,
    /// Re-extract local files for this invocation; never persisted in the index.
    #[serde(skip)]
    pub force: bool,
    /// Accept a smaller semantic result for this invocation, after saving a snapshot.
    #[serde(skip)]
    pub allow_semantic_shrink: bool,
    /// Report wall-clock extraction phase timings for this invocation only.
    #[serde(skip)]
    pub timing: bool,
    pub ingest: IngestOptions,
}
impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            code_only: false,
            no_gitignore: false,
            include_generated: false,
            max_semantic_files: 32,
            max_semantic_calls: None,
            max_semantic_output_tokens: None,
            semantic_code: false,
            python_source_roots: vec![],
            swift_modules: BTreeMap::new(),
            robot_python: None,
            force: false,
            allow_semantic_shrink: false,
            timing: false,
            ingest: IngestOptions::default(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Freshness {
    pub generation: u64,
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub fresh: bool,
}

/// Run-local reservations and provider receipts survive a failed extraction or
/// commit. Available through anyhow downcasting; never written into the graph.
#[derive(Debug, Serialize)]
pub struct FailedSemanticUsage {
    pub semantic_usage: Option<ingest::SemanticUsage>,
    pub provider_usage: Vec<ingest::ProviderUsage>,
    pub usage_unavailable: bool,
    #[serde(skip)]
    message: String,
}
impl std::fmt::Display for FailedSemanticUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}; semantic usage before failure: {}",
            self.message,
            serde_json::to_string(self).map_err(|_| std::fmt::Error)?
        )
    }
}
impl std::error::Error for FailedSemanticUsage {}

pub(crate) fn retain_semantic_usage(error: anyhow::Error, options: &IndexOptions) -> anyhow::Error {
    if error.downcast_ref::<FailedSemanticUsage>().is_some() {
        return error;
    }
    let Some(semantic) = &options.ingest.semantic else {
        return error;
    };
    let reserved = semantic
        .runtime_budget
        .as_ref()
        .map(|b| b.usage())
        .transpose();
    let actual = semantic
        .runtime_usage
        .as_ref()
        .map(|r| r.snapshot())
        .transpose();
    let usage = FailedSemanticUsage {
        message: error.to_string(),
        usage_unavailable: reserved.is_err() || actual.is_err(),
        semantic_usage: reserved.ok().flatten(),
        provider_usage: actual.ok().flatten().unwrap_or_default(),
    };
    if usage.usage_unavailable
        || usage.semantic_usage.is_some_and(|u| u.calls > 0)
        || !usage.provider_usage.is_empty()
    {
        error.context(usage)
    } else {
        error
    }
}

pub fn stored_options(db: &Path) -> Result<IndexOptions> {
    if !db.try_exists()? {
        return Ok(IndexOptions::default());
    }
    let value = Store::open_read_only(db)?.graph_metadata()?;
    match value.get("graf_index_options") {
        Some(options) => {
            serde_json::from_value(options.clone()).context("invalid stored index options")
        }
        None => Ok(IndexOptions::default()),
    }
}

pub fn run(root: &Path, db: &Path) -> Result<IndexReport> {
    run_with_options(root, db, &stored_options(db)?)
}

pub fn run_with_options(root: &Path, db: &Path, options: &IndexOptions) -> Result<IndexReport> {
    run_with_reserved_semantic_files(root, db, options, 0)
}

pub(crate) fn run_with_reserved_semantic_files(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    reserved: usize,
) -> Result<IndexReport> {
    let prepared = prepare_semantic_budget(options);
    run_prepared(root, db, &prepared, reserved)
        .map_err(|error| retain_semantic_usage(error, &prepared))
}

pub(crate) fn prepare_semantic_budget(options: &IndexOptions) -> IndexOptions {
    let mut prepared = options.clone();
    if let Some(semantic) = &mut prepared.ingest.semantic {
        if semantic.runtime_budget.is_none() {
            semantic.runtime_budget = Some(std::sync::Arc::new(ingest::SemanticBudget::new(
                options.max_semantic_calls,
                options.max_semantic_output_tokens,
            )));
        }
        if semantic.runtime_usage.is_none() {
            semantic.runtime_usage =
                Some(std::sync::Arc::new(ingest::SemanticUsageRecorder::default()));
        }
    }
    prepared
}

pub(crate) use ctx_graph_storage::stamps::semantic_counts;

/// Portable graph evidence, written before an explicitly accepted semantic loss.
/// Native ownership/index state is not restored by importing this snapshot.
pub(crate) fn preserve_snapshot(root: &Path, store: &Store) -> Result<PathBuf> {
    let snapshot = store.snapshot()?;
    let bytes = serde_json::to_vec(&snapshot)?;
    preserve_backup(root, &format!("graph-{}", snapshot.generation), &bytes)
}

pub(crate) fn preserve_backup(root: &Path, prefix: &str, bytes: &[u8]) -> Result<PathBuf> {
    ensure!(
        !prefix.is_empty()
            && prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid backup name"
    );
    ensure!(
        bytes.len() <= 256 * 1024 * 1024,
        "semantic backup exceeds snapshot size limit"
    );
    let directory = root.join(".graf/backups");
    for path in [root.join(".graf"), directory.clone()] {
        match std::fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "backup path must be a regular directory"
                );
            }
            Err(error) => return Err(error).context("cannot create backup directory"),
        }
    }
    let name = format!("{prefix}-{}.json", blake3::hash(bytes).to_hex());
    let destination = directory.join(name);
    if destination.try_exists()? {
        let metadata = std::fs::symlink_metadata(&destination)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "backup must be a regular file"
        );
        let existing = read_source(&destination, 256 * 1024 * 1024)?.1;
        ensure!(
            existing.as_deref() == Some(bytes),
            "existing backup content differs"
        );
        return Ok(destination);
    }
    let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(&destination)
        .map_err(|e| e.error)
        .context("cannot publish semantic backup")?;
    Ok(destination)
}

pub fn check_update(root: &Path, db: &Path) -> Result<Freshness> {
    let root = root.canonicalize()?;
    let store = Store::open_read_only(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind == "native",
        "check-update requires a native graph"
    );
    ensure!(
        stats.root.as_deref() == root.to_str(),
        "index root differs from the stored root"
    );
    let options = stored_options(db)?;
    let db = db.canonicalize()?;
    let ingest_fingerprint = ingest::config_fingerprint(&options.ingest)?;
    let index_options = serde_json::to_value(&options)?;
    let stored_options = stored_options_value(&store)?;
    let database = database_identity(&db);
    let manifest = read_manifest(&scan_manifest_path(&db));
    let root_text = root
        .to_str()
        .context("index root must be UTF-8")?
        .to_owned();
    let reusable = manifest.as_ref().and_then(|manifest| {
        reusable_scan(
            manifest,
            &root_text,
            database.as_ref(),
            stats.generation,
            &stored_options,
            &index_options,
            &ingest_fingerprint,
        )
    });
    let scan = prepare_scan(&root, &db, &options, false, reusable, None)?;
    if let Some(proof) = &scan.proof {
        let exact = database_identity(&db).is_some_and(|database| {
            manifest_matches(
                &scan_manifest_path(&db),
                &ScanManifest {
                    version: SCAN_MANIFEST_VERSION,
                    root: root_text,
                    database,
                    generation: stats.generation,
                    stored_options,
                    index_options,
                    extractor_revision: EXTRACTOR_REVISION,
                    language_revision: languages::revision().into(),
                    ingest_fingerprint: ingest_fingerprint.clone(),
                    scan: proof.clone(),
                },
            )
        });
        if exact || reusable.is_some_and(|previous| scan_content_matches(previous, proof)) {
            return Ok(Freshness {
                generation: stats.generation,
                changed: vec![],
                added: vec![],
                deleted: vec![],
                fresh: true,
            });
        }
    }
    let PreparedScan { files, managed, .. } = scan;
    let context = discover_project_context(
        &root,
        &files.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
        &options.swift_modules,
    )?;
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let python = python_inventory(&files, &options, &old, &context, &ingest_fingerprint)?;
    let mut result = Freshness {
        generation: stats.generation,
        changed: vec![],
        added: vec![],
        deleted: vec![],
        fresh: false,
    };
    for (path, relative) in files {
        let maximum =
            if (is_code(&relative) || content_probe(&relative)) && !relative.ends_with(".dmi") {
                MAX_SOURCE_BYTES as u64
            } else {
                options.ingest.max_input_bytes
            };
        let (hash, _) = read_source(&path, maximum)?;
        context.validate_source(&relative, &hash)?;
        python.validate_source(&relative, &hash)?;
        let hash = stamp(
            &relative,
            &hash,
            &context,
            &ingest_fingerprint,
            &options,
            python.context_token(&relative),
        );
        match old.remove(&relative) {
            None => result.added.push(relative),
            Some(previous) if previous != hash => result.changed.push(relative),
            _ => (),
        }
    }
    for facts in managed {
        match old.remove(&facts.path) {
            None => result.added.push(facts.path),
            Some(previous) if previous != facts.hash => result.changed.push(facts.path),
            _ => (),
        }
    }
    result.added.sort();
    result.changed.sort();
    result.deleted = old.into_keys().collect();
    result.deleted.sort();
    result.fresh =
        result.added.is_empty() && result.changed.is_empty() && result.deleted.is_empty();
    Ok(result)
}

#[derive(Default)]
struct PythonInventory {
    facts: HashMap<String, FileFacts>,
    source_hashes: HashMap<String, String>,
    context_tokens: HashMap<String, String>,
}

const PYTHON_TERMINAL_CONTEXT: &str = "terminal-v2";

/// Recover the raw local-source digest from formats produced by this indexer.
/// Managed captures hash a saved extraction record instead, so never qualify.
// Project context deliberately reads nearby manifests and explicitly named
// source modules even when ignore rules exclude them from the graph. A cache
// proof must therefore observe those possible inputs too. This second walk
// ignores repository ignore files but retains Graf's fixed generated-directory
// boundaries; any traversal uncertainty simply disables the shortcut.
pub(crate) use ctx_graph_types::read_source;

#[cfg(test)]
mod tests;

mod pythoninventory_validate_source;

mod run_prepared;
use run_prepared::*;
mod unchanged_report;
use unchanged_report::*;
