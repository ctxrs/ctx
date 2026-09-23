use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::OpenOptions,
    io::{Read, Write},
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

const EXTRACTOR_REVISION: u32 = 12;
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

fn run_prepared(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    reserved: usize,
) -> Result<IndexReport> {
    let started = std::time::Instant::now();
    for source_root in &options.python_source_roots {
        ensure!(
            source_root.is_empty()
                || (!source_root.contains(['\\', ':'])
                    && source_root
                        .split('/')
                        .all(|part| !matches!(part, "" | "." | ".."))),
            "Python source roots must be normalized repository-relative directories (empty means repository root)"
        );
    }
    ensure!(
        !options.semantic_code || options.ingest.semantic.is_some(),
        "deep code extraction requires an explicit semantic provider"
    );
    ensure!(
        options.max_semantic_files <= 100_000,
        "max_semantic_files must be at most 100000"
    );
    ensure!(
        reserved <= options.max_semantic_files,
        "semantic extraction exceeds configured file budget"
    );
    let root = root
        .canonicalize()
        .context("cannot canonicalize index root")?;
    ensure!(root.is_dir(), "index root must be a directory");
    let root_text = root.to_str().context("index root must be UTF-8")?;
    let mut store = Store::create(db)?;
    let stats = store.stats()?;
    ensure!(
        stats.kind != "imported",
        "cannot index native sources into an imported graph"
    );
    ensure!(
        stats.root.as_deref().is_none_or(|old| old == root_text),
        "index root differs from the stored root"
    );
    let db = db.canonicalize()?;
    let index_options = serde_json::to_value(options)?;
    let stored_options = stored_options_value(&store)?;
    let ingest_fingerprint = ingest::config_fingerprint(&options.ingest)?;
    let mut old: HashMap<_, _> = store
        .file_stamps()?
        .into_iter()
        .map(|f| (f.path, f.hash))
        .collect();
    let manifest_path = scan_manifest_path(&db);
    let existing_manifest = read_manifest(&manifest_path);
    let database = database_identity(&db);
    let reusable = existing_manifest.as_ref().and_then(|manifest| {
        reusable_scan(
            manifest,
            root_text,
            database.as_ref(),
            stats.generation,
            &stored_options,
            &index_options,
            &ingest_fingerprint,
        )
    });
    let scan = prepare_scan(&root, &db, options, true, reusable, None)?;
    if !options.force
        && let Some(proof) = &scan.proof
        && let Some(database) = database_identity(&db)
    {
        let current = ScanManifest {
            version: SCAN_MANIFEST_VERSION,
            root: root_text.to_owned(),
            database,
            generation: stats.generation,
            stored_options: stored_options.clone(),
            index_options: index_options.clone(),
            extractor_revision: EXTRACTOR_REVISION,
            language_revision: languages::revision().into(),
            ingest_fingerprint: ingest_fingerprint.clone(),
            scan: proof.clone(),
        };
        if manifest_matches(&manifest_path, &current) {
            return unchanged_report(&stats, options, started);
        }
        if reusable.is_some_and(|previous| scan_content_matches(previous, proof)) {
            write_manifest(&manifest_path, &current)?;
            return unchanged_report(&stats, options, started);
        }
    }
    let initial_proof = scan.proof.clone();
    let PreparedScan {
        files,
        mut coverage,
        managed,
        mut cached_sources,
        mut freshly_read,
        ..
    } = scan;
    let mut context = discover_project_context(
        &root,
        &files.iter().map(|f| f.1.clone()).collect::<Vec<_>>(),
        &options.swift_modules,
    )?;
    let mut python = python_inventory(&files, options, &old, &context, &ingest_fingerprint)?;
    #[cfg(test)]
    tests::after_python_inventory();
    let detect_ms = started.elapsed().as_secs_f64() * 1000.0;
    let extracting = std::time::Instant::now();
    let mut changed = vec![];
    let mut semantic_files = reserved;
    for (path, relative) in files {
        let code = is_code(&relative) || content_probe(&relative);
        let maximum = if code && !relative.ends_with(".dmi") {
            MAX_SOURCE_BYTES as u64
        } else {
            options.ingest.max_input_bytes
        };
        let cached = cached_sources
            .as_mut()
            .and_then(|sources| sources.remove(&relative))
            .filter(|source| source_version(&path).as_ref() == Some(&source.version));
        let cached_version = cached.as_ref().map(|source| source.version.clone());
        let (content_hash, mut content) = cached
            .map(|source| Ok((source.digest, source.content)))
            .unwrap_or_else(|| read_source(&path, maximum))?;
        context.validate_source(&relative, &content_hash)?;
        python.validate_source(&relative, &content_hash)?;
        let hash = stamp(
            &relative,
            &content_hash,
            &context,
            &ingest_fingerprint,
            options,
            python.context_token(&relative),
        );
        let unchanged = old
            .remove(&relative)
            .is_some_and(|previous| previous == hash);
        if unchanged && !options.force {
            coverage.unchanged_files += 1;
            continue;
        }
        if content.is_none() {
            freshly_read.insert(relative.clone());
            let (fresh_hash, fresh_content) = read_source(&path, maximum)?;
            ensure!(
                fresh_hash == content_hash,
                "source changed during indexing; previous graph retained"
            );
            content = fresh_content;
        }
        let mut facts = if code {
            match content {
                Some(bytes) if relative.ends_with(".dmi") => {
                    languages::extended::parse_dmi(&relative, &bytes, &hash)?
                }
                Some(bytes) if relative.ends_with(".dfm") => {
                    languages::extended::parse_pascal_form_bytes(&relative, &bytes, &hash)?
                }
                Some(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(source) => {
                        let mut facts = if relative.ends_with(".py")
                            || (Path::new(&relative).extension().is_none()
                                && languages::scripted::shebang_language(source) == Some("python"))
                        {
                            let mut facts = python
                                .facts
                                .remove(&relative)
                                .context("Python inventory changed during scan; retry indexing")?;
                            facts.hash = hash.clone();
                            facts
                        } else if is_robot(&relative) && options.robot_python.is_some() {
                            let python = options.robot_python.as_ref().unwrap();
                            let python = if python.is_relative() && python.components().count() > 1
                            {
                                root.join(python)
                            } else {
                                python.clone()
                            };
                            languages::templates::parse_robot_official(
                                &relative, source, &hash, &python,
                            )?
                        } else if let Some(mut facts) =
                            context.take_cached_facts(&relative, &content_hash)?
                        {
                            facts.hash = hash.clone();
                            facts
                        } else {
                            languages::parse(&relative, source, &hash)?.unwrap_or_else(|| {
                                diagnostic(
                                    &relative,
                                    &hash,
                                    "file content does not match a supported language",
                                )
                            })
                        };
                        if options.semantic_code && !facts.nodes.is_empty() {
                            semantic_files += 1;
                            ensure!(
                                semantic_files <= options.max_semantic_files,
                                "semantic extraction exceeds configured file budget"
                            );
                            ingest::enrich_facts(&mut facts, source, &options.ingest)?;
                        }
                        facts
                    }
                    Err(_) => diagnostic(&relative, &hash, "source is not UTF-8"),
                },
                None => diagnostic(
                    &relative,
                    &hash,
                    if relative.ends_with(".dmi") {
                        "source exceeds configured input byte limit"
                    } else {
                        "source exceeds the 4 MiB limit"
                    },
                ),
            }
        } else {
            if options.ingest.semantic.is_some() {
                semantic_files += 1;
                ensure!(
                    semantic_files <= options.max_semantic_files,
                    "semantic extraction exceeds the configured file budget; increase max_semantic_files explicitly"
                );
            }
            let bytes = content
                .context("document exceeds configured input byte limit; previous graph retained")?;
            let suffix = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| format!(".{s}"))
                .unwrap_or_default();
            // Converters receive immutable task-owned bytes, not a source path
            // that could change after its fingerprint was computed.
            let mut staged = tempfile::Builder::new()
                .prefix("graf-ingest-")
                .suffix(&suffix)
                .tempfile()?;
            staged.write_all(&bytes)?;
            staged.flush()?;
            ingest::extract(staged.path(), &relative, &hash, &options.ingest)?
        };
        if let Some(version) = cached_version {
            ensure!(
                source_version(&path).as_ref() == Some(&version),
                "source changed during cached extraction; retry indexing"
            );
        }
        context.apply(&mut facts);
        add_document_aliases(&mut facts);
        changed.push(facts);
    }
    for mut facts in managed {
        coverage.supported_files += 1;
        if old
            .remove(&facts.path)
            .is_some_and(|previous| previous == facts.hash)
        {
            coverage.unchanged_files += 1;
        } else {
            add_document_aliases(&mut facts);
            changed.push(facts);
        }
    }
    changed.sort_by(|a, b| a.path.cmp(&b.path));
    let mut deleted: Vec<_> = old.into_keys().collect();
    deleted.sort();
    #[cfg(test)]
    tests::before_publish_validation();
    if let Some(initial_proof) = &initial_proof {
        let current = prepare_scan(
            &root,
            &db,
            options,
            false,
            Some(initial_proof),
            Some(&freshly_read),
        )?;
        ensure!(
            current
                .proof
                .as_ref()
                .is_some_and(|proof| scan_content_matches(initial_proof, proof)),
            "source tree changed during indexing; previous graph retained"
        );
    }
    let extract_ms = extracting.elapsed().as_secs_f64() * 1000.0;
    let committing = std::time::Instant::now();
    let losses = store.semantic_losses(&changed)?;
    if !losses.is_empty() {
        ensure!(
            options.allow_semantic_shrink,
            "semantic extraction would remove recorded facts from {}; previous graph retained. Review the result and use --allow-semantic-shrink to accept it with a backup",
            losses.join(", ")
        );
        preserve_snapshot(&root, &store)?;
    }
    let prepared_native_write = stats.kind == "empty" && !changed.is_empty();
    if prepared_native_write {
        store.prepare_native_index_write()?;
    }
    let applied = store.apply_native_with_options(
        root_text,
        changed,
        deleted,
        coverage,
        index_options.clone(),
    );
    let mut report = if prepared_native_write {
        store.finish_native_index_write(applied)?
    } else {
        applied?
    };
    report.semantic_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_budget.as_ref())
        .map(|budget| budget.usage())
        .transpose()?;
    report.provider_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_usage.as_ref())
        .map(|recorder| recorder.snapshot())
        .transpose()?;
    if options.timing {
        report.timings = Some(IndexTimings {
            detect_ms,
            extract_ms,
            commit_ms: committing.elapsed().as_secs_f64() * 1000.0,
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            capture_ms: None,
        });
    }
    drop(store);
    if let Some(initial_proof) = initial_proof
        && let Some(database) = database_identity(&db)
    {
        let manifest = ScanManifest {
            version: SCAN_MANIFEST_VERSION,
            root: root_text.to_owned(),
            database,
            generation: report.generation,
            stored_options: index_options.clone(),
            index_options,
            extractor_revision: EXTRACTOR_REVISION,
            language_revision: languages::revision().into(),
            ingest_fingerprint,
            scan: initial_proof,
        };
        if serde_json::to_vec(&manifest)
            .is_ok_and(|bytes| bytes.len() as u64 <= MAX_SCAN_MANIFEST_BYTES)
        {
            let _ = write_manifest(&scan_manifest_path(&db), &manifest);
        }
    }
    Ok(report)
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

pub(crate) fn semantic_counts(facts: &FileFacts) -> (usize, usize) {
    (
        facts
            .nodes
            .iter()
            .filter(|n| semantic_provenance(&n.metadata))
            .count(),
        facts
            .edges
            .iter()
            .filter(|e| semantic_provenance(&e.metadata))
            .count(),
    )
}

pub(crate) fn semantic_provenance(metadata: &serde_json::Value) -> bool {
    metadata["inferred"] == true
        && matches!(
            metadata["provenance"].as_str(),
            Some("semantic" | "visual_inference")
        )
}

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

fn discover_project_context(
    root: &Path,
    files: &[String],
    swift_modules: &BTreeMap<String, String>,
) -> Result<ProjectContext> {
    PROJECT_CONTEXT_DISCOVERIES.with(|count| count.set(count.get() + 1));
    ProjectContext::discover_with_swift_modules(root, files, swift_modules)
}

fn source_maximum(relative: &str, options: &IndexOptions) -> u64 {
    if (is_code(relative) || content_probe(relative)) && !relative.ends_with(".dmi") {
        MAX_SOURCE_BYTES as u64
    } else {
        options.ingest.max_input_bytes
    }
}

fn prepare_scan(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
    cache_sources: bool,
    previous: Option<&ScanProof>,
    force_read: Option<&BTreeSet<String>>,
) -> Result<PreparedScan> {
    let Discovery {
        files,
        coverage,
        ignore_files,
        context_files,
    } = discover(root, db, options)?;
    let mut cacheable = ignore_files.is_some() && context_files.is_some();
    let mut sources = BTreeMap::new();
    let previous: BTreeMap<_, _> = previous
        .into_iter()
        .flat_map(|proof| &proof.sources)
        .map(|proof| (proof.path.as_str(), proof))
        .collect();
    let mut freshly_read = BTreeSet::new();
    let source_paths: BTreeSet<_> = files.iter().map(|(_, relative)| relative.clone()).collect();
    let mut inputs = BTreeMap::new();
    for (path, relative) in files
        .iter()
        .chain(context_files.as_deref().unwrap_or_default())
    {
        inputs
            .entry(relative.clone())
            .or_insert_with(|| path.clone());
    }
    let mut cached_sources = cache_sources.then(HashMap::new);
    let mut cached_bytes = 0usize;
    for (relative, path) in inputs {
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            cacheable = false;
            continue;
        }
        let version = source_version_from_metadata(&metadata);
        let identity = manifest_source_identity(&metadata);
        let reusable = force_read
            .is_none_or(|paths| !paths.contains(&relative))
            .then(|| {
                identity.as_ref().and_then(|identity| {
                    previous
                        .get(relative.as_str())
                        .copied()
                        .filter(|proof| proof.identity.as_ref() == Some(identity))
                })
            })
            .flatten();
        let (digest, content) = if let Some(proof) = reusable {
            (proof.digest.clone(), None)
        } else {
            freshly_read.insert(relative.clone());
            read_source(&path, source_maximum(&relative, options))?
        };
        if source_paths.contains(&relative) && cached_sources.is_some() {
            cached_bytes = cached_bytes.saturating_add(content.as_ref().map_or(0, Vec::len));
            if cached_bytes <= MAX_SCAN_CACHE_BYTES
                && let Some(version) =
                    version.filter(|version| source_version(&path).as_ref() == Some(version))
            {
                cached_sources.as_mut().unwrap().insert(
                    relative.clone(),
                    CachedSource {
                        digest: digest.clone(),
                        content,
                        version,
                    },
                );
            } else if cached_bytes > MAX_SCAN_CACHE_BYTES {
                cached_sources = None;
            }
        }
        sources.insert(
            relative.clone(),
            SourceProof {
                path: relative,
                digest,
                identity,
            },
        );
    }
    let managed = crate::sources::read(root)?;
    let managed_sources = managed
        .iter()
        .map(|facts| SourceProof {
            path: facts.path.clone(),
            digest: facts.hash.clone(),
            identity: None,
        })
        .collect();
    let proof = cacheable.then(|| ScanProof {
        supported_files: coverage.supported_files + managed.len(),
        unsupported_files: coverage.unsupported_files,
        ignore_fingerprint: ignore_fingerprint(ignore_files.unwrap()),
        sources: sources.into_values().collect(),
        managed_sources,
    });
    if proof.is_none() {
        cached_sources = None;
    }
    Ok(PreparedScan {
        files,
        coverage,
        managed,
        proof,
        cached_sources,
        freshly_read,
    })
}

#[cfg(unix)]
fn manifest_source_identity(metadata: &std::fs::Metadata) -> Option<ManifestSourceIdentity> {
    let modified_nanos = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        let changed_seconds = u128::try_from(metadata.ctime()).ok()?;
        let changed_subsecond = u128::try_from(metadata.ctime_nsec()).ok()?;
        if changed_subsecond >= 1_000_000_000 {
            return None;
        }
        let changed_nanos = changed_seconds
            .checked_mul(1_000_000_000)?
            .checked_add(changed_subsecond)?;
        let observed_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        // Timestamp metadata is a safe digest cache key only after its
        // filesystem-resolution window has closed. If a scan overlaps that
        // window, persist no identity: a later run must hash once more before
        // it can establish a reusable proof.
        if !source_identity_settled(modified_nanos, changed_nanos, observed_nanos) {
            return None;
        }
        format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )
    };
    Some(ManifestSourceIdentity {
        length: metadata.len(),
        modified: modified_nanos.to_string(),
        file_id,
    })
}

#[cfg(unix)]
fn source_identity_settled(
    modified_nanos: u128,
    changed_nanos: u128,
    observed_nanos: u128,
) -> bool {
    observed_nanos.saturating_sub(changed_nanos.max(modified_nanos)) >= SOURCE_IDENTITY_SETTLE_NANOS
}

#[cfg(not(unix))]
fn manifest_source_identity(_metadata: &std::fs::Metadata) -> Option<ManifestSourceIdentity> {
    // The portable metadata surface has no change counter independent of mtime.
    // Keep content hashing on those platforms rather than trust a restorable timestamp.
    None
}

fn source_version(path: &Path) -> Option<SourceVersion> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    source_version_from_metadata(&metadata)
}

fn source_version_from_metadata(metadata: &std::fs::Metadata) -> Option<SourceVersion> {
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(windows)]
    let file_id = {
        use std::os::windows::fs::MetadataExt;
        metadata.creation_time().to_string()
    };
    #[cfg(not(any(unix, windows)))]
    let file_id = String::new();
    Some(SourceVersion {
        length: metadata.len(),
        modified: metadata.modified().ok()?,
        file_id,
    })
}

fn ignore_fingerprint(mut files: Vec<(Vec<u8>, String)>) -> String {
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash = blake3::Hasher::new();
    hash.update(b"graf-ignore-v1");
    for (path, digest) in files {
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(&path);
        hash.update(digest.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

fn stored_options_value(store: &Store) -> Result<serde_json::Value> {
    Ok(store
        .graph_metadata()?
        .get("graf_index_options")
        .cloned()
        .unwrap_or(serde_json::to_value(IndexOptions::default())?))
}

fn scan_manifest_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(".scan-manifest.json");
    PathBuf::from(name)
}

fn database_identity(db: &Path) -> Option<DatabaseIdentity> {
    let metadata = std::fs::symlink_metadata(db).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos()
        .to_string();
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    };
    #[cfg(windows)]
    let file_id = {
        use std::os::windows::fs::MetadataExt;
        metadata.creation_time().to_string()
    };
    #[cfg(not(any(unix, windows)))]
    let file_id = modified.clone();
    Some(DatabaseIdentity {
        length: metadata.len(),
        modified,
        file_id,
    })
}

fn manifest_matches(path: &Path, expected: &ScanManifest) -> bool {
    read_manifest(path).is_some_and(|actual| actual == *expected)
}

fn scan_content_matches(left: &ScanProof, right: &ScanProof) -> bool {
    fn sources_match(left: &[SourceProof], right: &[SourceProof]) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| left.path == right.path && left.digest == right.digest)
    }

    left.supported_files == right.supported_files
        && left.unsupported_files == right.unsupported_files
        && left.ignore_fingerprint == right.ignore_fingerprint
        && sources_match(&left.sources, &right.sources)
        && sources_match(&left.managed_sources, &right.managed_sources)
}

fn read_manifest(path: &Path) -> Option<ScanManifest> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return None;
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SCAN_MANIFEST_BYTES
    {
        return None;
    }
    let Ok((_, Some(bytes))) = read_source(path, MAX_SCAN_MANIFEST_BYTES) else {
        return None;
    };
    serde_json::from_slice(&bytes).ok()
}

fn reusable_scan<'a>(
    manifest: &'a ScanManifest,
    root: &str,
    database: Option<&DatabaseIdentity>,
    generation: u64,
    stored_options: &serde_json::Value,
    index_options: &serde_json::Value,
    ingest_fingerprint: &str,
) -> Option<&'a ScanProof> {
    (manifest.version == SCAN_MANIFEST_VERSION
        && manifest.root == root
        && Some(&manifest.database) == database
        && manifest.generation == generation
        && &manifest.stored_options == stored_options
        && &manifest.index_options == index_options
        && manifest.extractor_revision == EXTRACTOR_REVISION
        && manifest.language_revision == languages::revision()
        && manifest.ingest_fingerprint == ingest_fingerprint)
        .then_some(&manifest.scan)
}

fn write_manifest(path: &Path, manifest: &ScanManifest) -> Result<()> {
    let bytes = serde_json::to_vec(manifest)?;
    ensure!(
        bytes.len() as u64 <= MAX_SCAN_MANIFEST_BYTES,
        "scan manifest exceeds size limit"
    );
    let parent = path.parent().context("scan manifest has no parent")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist(path)
        .map_err(|error| error.error)
        .context("cannot publish scan manifest")?;
    Ok(())
}

fn unchanged_report(
    stats: &Stats,
    options: &IndexOptions,
    started: std::time::Instant,
) -> Result<IndexReport> {
    let semantic_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_budget.as_ref())
        .map(|budget| budget.usage())
        .transpose()?;
    let provider_usage = options
        .ingest
        .semantic
        .as_ref()
        .and_then(|semantic| semantic.runtime_usage.as_ref())
        .map(|recorder| recorder.snapshot())
        .transpose()?;
    let timings = options.timing.then(|| {
        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        IndexTimings {
            detect_ms: total_ms,
            extract_ms: 0.0,
            commit_ms: 0.0,
            total_ms,
            capture_ms: None,
        }
    });
    Ok(IndexReport {
        schema_version: stats.schema_version,
        generation: stats.generation,
        parsed_files: 0,
        unchanged_files: stats.coverage.supported_files,
        deleted_files: 0,
        nodes: stats.nodes,
        edges: stats.edges,
        diagnostics: stats.diagnostics.clone(),
        semantic_usage,
        provider_usage,
        timings,
    })
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

fn is_code(path: &str) -> bool {
    path.ends_with(".py") || languages::supports(path)
}
fn is_robot(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("robot") || extension.eq_ignore_ascii_case("resource")
        })
}
fn generic_json(path: &str) -> bool {
    path.ends_with(".json") || path.ends_with(".jsonc")
}
fn content_probe(path: &str) -> bool {
    generic_json(path) || Path::new(path).extension().is_none()
}
fn python_source_root<'a>(path: &str, options: &'a IndexOptions) -> Option<&'a str> {
    options
        .python_source_roots
        .iter()
        .filter(|root| {
            root.is_empty()
                || path
                    .strip_prefix(root.as_str())
                    .is_some_and(|tail| tail.starts_with('/'))
        })
        .max_by_key(|root| root.len())
        .map(String::as_str)
}

#[derive(Default)]
struct PythonInventory {
    facts: HashMap<String, FileFacts>,
    source_hashes: HashMap<String, String>,
    context_tokens: HashMap<String, String>,
}

const PYTHON_TERMINAL_CONTEXT: &str = "terminal-v2";

impl PythonInventory {
    fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == hash),
            "Python source changed during scan; retry indexing"
        );
        Ok(())
    }

    fn context_token(&self, path: &str) -> &str {
        self.context_tokens
            .get(path)
            .map_or(PYTHON_TERMINAL_CONTEXT, String::as_str)
    }
}

fn python_inventory(
    files: &[(PathBuf, String)],
    options: &IndexOptions,
    old: &HashMap<String, String>,
    project: &ProjectContext,
    ingest_fingerprint: &str,
) -> Result<PythonInventory> {
    let possible_python = |path: &str| {
        !path.starts_with(".graf/sources/")
            && (path.ends_with(".py") || Path::new(path).extension().is_none())
    };
    let paths: BTreeSet<_> = files
        .iter()
        .map(|(_, path)| path.as_str())
        .filter(|p| possible_python(p))
        .collect();
    let old_paths: BTreeSet<_> = old
        .keys()
        .map(String::as_str)
        .filter(|p| possible_python(p))
        .collect();
    // Reuse only the stamp, never old parsed facts. If any possible Python file,
    // option, inventory member or extractor revision differs, rebuild context.
    // This still hashes source bytes, so equal timestamps cannot conceal edits.
    if !options.force && !paths.is_empty() && paths == old_paths {
        let mut inventory = PythonInventory::default();
        let mut unchanged = true;
        for (path, relative) in files.iter().filter(|(_, path)| possible_python(path)) {
            let Some(token) = old
                .get(relative)
                .and_then(|stamp| previous_python_context(stamp))
            else {
                unchanged = false;
                break;
            };
            let (hash, _) = read_source(path, MAX_SOURCE_BYTES as u64)?;
            if old.get(relative)
                != Some(&stamp(
                    relative,
                    &hash,
                    project,
                    ingest_fingerprint,
                    options,
                    token,
                ))
            {
                unchanged = false;
                break;
            }
            inventory.source_hashes.insert(relative.clone(), hash);
            inventory
                .context_tokens
                .insert(relative.clone(), token.into());
        }
        if unchanged {
            return Ok(inventory);
        }
    }
    let mut inventory = PythonInventory::default();
    let mut facts = Vec::new();
    for (path, relative) in files {
        if !possible_python(relative) {
            continue;
        }
        let (hash, bytes) = read_source(path, MAX_SOURCE_BYTES as u64)?;
        // Even an invalid source or non-Python shebang contributed to discovery.
        inventory
            .source_hashes
            .insert(relative.clone(), hash.clone());
        let Some(bytes) = bytes else { continue };
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !relative.ends_with(".py")
            && languages::scripted::shebang_language(source) != Some("python")
        {
            continue;
        }
        facts.push(parse_python_with_source_root(
            relative,
            source,
            &hash,
            python_source_root(relative, options),
        )?);
    }
    let context = PythonContext::from_facts(&facts);
    let modules: BTreeSet<_> = facts
        .iter()
        .flat_map(|facts| &facts.nodes)
        .filter_map(|node| node.binding_key.as_deref())
        .filter(|key| key.starts_with("module:"))
        .map(str::to_owned)
        .collect();
    for mut facts in facts {
        context.apply(&mut facts);
        for reference in &mut facts.references {
            // An unavailable submodule is not an eligible fallback. Keeping the
            // terminal key lets Store rebind ordinary definition additions and
            // deletions; an actual submodule addition changes this file's token.
            if reference.relation == "imports"
                && let [_, fallback] = reference.candidate_keys.as_slice()
                && fallback.starts_with("module:")
                && !modules.contains(fallback)
            {
                reference.candidate_keys.pop();
            }
        }
        inventory
            .context_tokens
            .insert(facts.path.clone(), python_binding_token(&facts)?);
        inventory.facts.insert(facts.path.clone(), facts);
    }
    Ok(inventory)
}

fn python_binding_token(facts: &FileFacts) -> Result<String> {
    // Hash the context outcome, not the corpus or target availability. Stable
    // candidate keys remain Store-owned even when their terminal targets vanish.
    let mut references: Vec<_> = facts
        .references
        .iter()
        .map(|reference| {
            (
                &reference.id,
                &reference.relation,
                &reference.candidate_keys,
            )
        })
        .collect();
    let mut bindings: Vec<_> = facts
        .nodes
        .iter()
        .map(|node| {
            (
                &node.id,
                &node.binding_key,
                &node.metadata["binding_aliases"],
            )
        })
        .collect();
    // Export references are emitted from a HashMap; enumeration is not identity.
    references.sort_by(|a, b| a.0.cmp(b.0));
    bindings.sort_by(|a, b| a.0.cmp(b.0));
    let outcome = serde_json::to_vec(&(references, bindings))?;
    Ok(format!(
        "{PYTHON_TERMINAL_CONTEXT}-{}",
        blake3::hash(&outcome)
    ))
}

fn previous_python_context(stamp: &str) -> Option<&str> {
    let context = if let Some(rest) = stamp.strip_prefix("python-v") {
        let (_, rest) = rest.split_once(':')?;
        rest.split_once(':').map_or(rest, |(context, _)| context)
    } else if stamp.starts_with("languages-") {
        stamp.split(':').nth(2)?
    } else {
        return None;
    };
    (context == PYTHON_TERMINAL_CONTEXT
        || context.strip_prefix("terminal-v2-").is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
        }))
    .then_some(context)
}

/// Recover the raw local-source digest from formats produced by this indexer.
/// Managed captures hash a saved extraction record instead, so never qualify.
pub(crate) fn indexed_source_digest(stamp: &str) -> Option<&str> {
    let (family, _) = stamp.split_once(':')?;
    let known = if let Some(version) = family.strip_prefix("python-v") {
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=EXTRACTOR_REVISION).contains(&v))
    } else if let Some(version) = family.strip_prefix("languages-native-languages-") {
        let current = languages::revision()
            .strip_prefix("native-languages-")?
            .parse::<u32>()
            .ok()?;
        version
            .parse::<u32>()
            .ok()
            .is_some_and(|v| (1..=current).contains(&v))
    } else {
        family == "ingest-v1"
    };
    if !known || stamp.split(':').count() < 3 || stamp.contains(":oversized:") {
        return None;
    }
    let digest = stamp.rsplit(':').next()?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(digest)
}

fn stamp(
    path: &str,
    hash: &str,
    context: &ProjectContext,
    ingest_fingerprint: &str,
    options: &IndexOptions,
    python_context: &str,
) -> String {
    let hash = if options.semantic_code {
        format!("deep:{ingest_fingerprint}:{hash}")
    } else {
        hash.to_owned()
    };
    let hash = if let Some(root) = python_source_root(path, options)
        .filter(|_| path.ends_with(".py") || Path::new(path).extension().is_none())
    {
        format!(
            "source-root:{}:{hash}",
            blake3::hash(root.as_bytes()).to_hex()
        )
    } else {
        hash
    };
    let hash = if let Some(python) = options.robot_python.as_ref().filter(|_| is_robot(path)) {
        format!(
            "robot-official-v1:{}:{hash}",
            blake3::hash(python.as_os_str().as_encoded_bytes()).to_hex()
        )
    } else {
        hash
    };
    if path.ends_with(".py") {
        format!("python-v{EXTRACTOR_REVISION}:{python_context}:{hash}")
    } else if languages::supports(path) || content_probe(path) {
        format!(
            "languages-{}:{}:{}:{hash}",
            languages::revision(),
            context.fingerprint(path),
            if Path::new(path).extension().is_none() {
                python_context
            } else {
                ""
            },
        )
    } else {
        format!("ingest-v1:{ingest_fingerprint}:{hash}")
    }
}

fn discover(root: &Path, db: &Path, options: &IndexOptions) -> Result<Discovery> {
    let db = db.to_path_buf();
    let context_files = discover_context_files(root, &db, options);
    let manifest = scan_manifest_path(&db);
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    let include_generated = options.include_generated;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .parents(false)
        .git_ignore(!options.no_gitignore)
        .git_exclude(!options.no_gitignore)
        .add_custom_ignore_filename(".graphifyignore")
        .add_custom_ignore_filename(".grafignore")
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|t| t.is_symlink()) {
                return false;
            }
            if entry.path() == db
                || entry.path() == manifest
                || sidecars.iter().any(|p| p == entry.path())
            {
                return false;
            }
            !entry.file_type().is_some_and(|t| {
                t.is_dir()
                    && match entry.file_name().to_str() {
                        Some(".git" | ".graf") => true,
                        Some(
                            ".venv" | "venv" | "env" | "__pycache__" | "node_modules"
                            | "site-packages" | "target" | "build" | "dist" | ".tox" | ".nox"
                            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache",
                        ) => !include_generated,
                        _ => false,
                    }
            })
        });
    let mut coverage = Coverage::default();
    let mut files = vec![];
    let mut ignore_files = Some(vec![]);
    for entry in walker.build() {
        let entry = entry.context("cannot walk index root")?;
        if let Some(error) = entry.error() {
            bail!("cannot apply ignore rules: {error}");
        }
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            check_ignore_files(entry.path(), options.no_gitignore, &mut ignore_files)?;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("source path must be UTF-8")?
            .to_owned();
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        let recognized_content = if content_probe(&relative) && !languages::supports(&relative) {
            let (_, content) = read_source(entry.path(), MAX_SOURCE_BYTES as u64)?;
            content
                .as_deref()
                .and_then(|b| std::str::from_utf8(b).ok())
                .is_some_and(|s| languages::recognizes(&relative, s))
        } else {
            false
        };
        if !is_code(&relative)
            && !recognized_content
            && (options.code_only || !ingest::supports(Path::new(&relative)))
        {
            coverage.unsupported_files += 1;
            continue;
        }
        coverage.supported_files += 1;
        files.push((entry.path().to_path_buf(), relative));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(Discovery {
        files,
        coverage,
        ignore_files,
        context_files,
    })
}

// Project context deliberately reads nearby manifests and explicitly named
// source modules even when ignore rules exclude them from the graph. A cache
// proof must therefore observe those possible inputs too. This second walk
// ignores repository ignore files but retains Graf's fixed generated-directory
// boundaries; any traversal uncertainty simply disables the shortcut.
fn discover_context_files(
    root: &Path,
    db: &Path,
    options: &IndexOptions,
) -> Option<Vec<(PathBuf, String)>> {
    let db = db.to_path_buf();
    let manifest = scan_manifest_path(&db);
    let sidecars: Vec<_> = ["-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| {
            let mut path = db.as_os_str().to_owned();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect();
    let include_generated = options.include_generated;
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .ignore(false)
        .git_global(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                return false;
            }
            if entry.path() == db
                || entry.path() == manifest
                || sidecars.iter().any(|path| path == entry.path())
            {
                return false;
            }
            !entry.file_type().is_some_and(|kind| {
                kind.is_dir()
                    && match entry.file_name().to_str() {
                        Some(".git" | ".graf") => true,
                        Some(
                            ".venv" | "venv" | "env" | "__pycache__" | "node_modules"
                            | "site-packages" | "target" | "build" | "dist" | ".tox" | ".nox"
                            | ".mypy_cache" | ".pytest_cache" | ".ruff_cache",
                        ) => !include_generated,
                        _ => false,
                    }
            })
        });
    let mut files = vec![];
    for entry in walker.build() {
        let entry = entry.ok()?;
        if entry.error().is_some() {
            return None;
        }
        if entry.file_type().is_some_and(|kind| kind.is_dir()) {
            continue;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            return None;
        }
        let relative = entry.path().strip_prefix(root).ok()?.to_str()?.to_owned();
        #[cfg(windows)]
        let relative = relative.replace('\\', "/");
        if is_code(&relative) || content_probe(&relative) {
            files.push((entry.path().to_path_buf(), relative));
        }
    }
    files.sort_by(|left, right| left.1.cmp(&right.1));
    Some(files)
}

fn check_ignore_files(
    directory: &Path,
    no_gitignore: bool,
    manifest_files: &mut Option<Vec<(Vec<u8>, String)>>,
) -> Result<()> {
    // WalkBuilder suppresses ignore-file I/O errors, including invalid UTF-8.
    // Validate each visited directory so partial rules cannot publish a graph
    // that silently includes excluded files. Pruned subtrees need no validation.
    for name in [
        ".ignore",
        ".gitignore",
        ".git/info/exclude",
        ".grafignore",
        ".graphifyignore",
    ] {
        if no_gitignore && matches!(name, ".gitignore" | ".git/info/exclude") {
            continue;
        }
        let path = directory.join(name);
        match std::fs::metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file(),
                "ignore rules must be a regular file: {}",
                path.display()
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("cannot inspect ignore rules"),
        }
        if let Some(error) = ignore::gitignore::GitignoreBuilder::new(directory).add(&path) {
            bail!("cannot apply ignore rules: {error}");
        }
        if manifest_files.is_some() {
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                *manifest_files = None;
                continue;
            }
            let (digest, content) = read_source(&path, MAX_IGNORE_BYTES)?;
            if content.is_none() {
                *manifest_files = None;
                continue;
            }
            manifest_files
                .as_mut()
                .unwrap()
                .push((path.as_os_str().as_encoded_bytes().to_vec(), digest));
        }
    }
    Ok(())
}

fn diagnostic(path: &str, hash: &str, message: &str) -> FileFacts {
    let mut facts = empty_facts(path, hash);
    facts.diagnostics.push(Diagnostic {
        file: path.into(),
        line: None,
        message: message.into(),
    });
    facts
}

pub(crate) fn read_source(path: &Path, maximum: u64) -> Result<(String, Option<Vec<u8>>)> {
    for _ in 0..2 {
        if !std::fs::symlink_metadata(path)?.is_file() {
            bail!("source is no longer a regular file: {}", path.display());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let before = file.metadata()?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink(),
            "source is no longer a regular file: {}",
            path.display()
        );
        if before.len() > maximum {
            if file.metadata()?.len() > maximum {
                let size = if maximum == MAX_SOURCE_BYTES as u64 {
                    "4MiB".to_owned()
                } else {
                    maximum.to_string()
                };
                return Ok((format!("oversized:{size}"), None));
            }
            continue;
        }
        let modified = before.modified()?;
        let mut hasher = blake3::Hasher::new();
        let mut content = Some(Vec::new());
        let mut buffer = [0; 64 * 1024];
        let mut count = 0u64;
        // One extra byte detects growth without chasing an indefinitely growing file.
        let mut bounded = (&mut file).take(before.len().saturating_add(1));
        loop {
            let size = bounded
                .read(&mut buffer)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if size == 0 {
                break;
            }
            count += size as u64;
            hasher.update(&buffer[..size]);
            if let Some(bytes) = &mut content {
                if bytes.len() + size <= maximum as usize {
                    bytes.extend_from_slice(&buffer[..size]);
                } else {
                    content = None;
                }
            }
        }
        let after = file.metadata()?;
        if count == before.len() && before.len() == after.len() && modified == after.modified()? {
            return Ok((hasher.finalize().to_hex().to_string(), content));
        }
    }
    bail!(
        "source changed during both read attempts: {}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, fs};

    thread_local! {
        static AFTER_PYTHON_INVENTORY: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
        static BEFORE_PUBLISH_VALIDATION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) fn after_python_inventory() {
        let hook = AFTER_PYTHON_INVENTORY.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn before_publish_validation() {
        let hook = BEFORE_PUBLISH_VALIDATION.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(unix)]
    #[test]
    fn source_identity_reuse_waits_past_the_timestamp_collision_window() {
        let old = 1_000_000_000_000_u128;
        assert!(!source_identity_settled(
            old,
            old,
            old + SOURCE_IDENTITY_SETTLE_NANOS - 1
        ));
        assert!(source_identity_settled(
            old,
            old,
            old + SOURCE_IDENTITY_SETTLE_NANOS
        ));
        assert!(!source_identity_settled(old, old + 1, old));
        assert!(!source_identity_settled(old + 1, old, old));
    }

    #[test]
    fn publish_scan_content_match_ignores_only_promoted_source_identities() {
        let source = |digest: &str, identity| SourceProof {
            path: "app.py".into(),
            digest: digest.into(),
            identity,
        };
        let proof = |source| ScanProof {
            supported_files: 1,
            unsupported_files: 0,
            ignore_fingerprint: "ignore".into(),
            sources: vec![source],
            managed_sources: vec![],
        };
        let initial = proof(source("same", None));
        let promoted = proof(source(
            "same",
            Some(ManifestSourceIdentity {
                length: 1,
                modified: "2".into(),
                file_id: "3".into(),
            }),
        ));
        let changed = proof(source("changed", None));
        assert!(scan_content_matches(&initial, &promoted));
        assert!(!scan_content_matches(&initial, &changed));
    }

    #[cfg(unix)]
    #[test]
    fn publish_allows_fresh_source_identity_to_settle_after_bytes_are_read() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join(".graf/index.db");
        let source = root.path().join("app.py");
        fs::write(&source, "def first():\n    pass\n").unwrap();
        let initial = run(root.path(), &db).unwrap();

        fs::write(&source, "def second():\n    pass\n").unwrap();
        assert!(manifest_source_identity(&fs::metadata(&source).unwrap()).is_none());
        BEFORE_PUBLISH_VALIDATION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|| {
                std::thread::sleep(std::time::Duration::from_millis(2_100));
            }));
        });

        let updated = run(root.path(), &db).unwrap();
        assert_eq!(updated.parsed_files, 1);
        assert_eq!(updated.generation, initial.generation + 1);
        let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
        assert!(snapshot.nodes.iter().any(|node| node.label == "second"));
        assert!(snapshot.nodes.iter().all(|node| node.label != "first"));
    }

    #[test]
    fn python_inventory_restore_before_unchanged_check_preserves_generation() {
        for api_name in ["api.py", "api"] {
            let root = tempfile::tempdir().unwrap();
            let db = root.path().join(".graf/index.db");
            let api = root.path().join(api_name);
            let original = "#!/usr/bin/env python3\nfrom impl import first as entry\n";
            let temporary = "#!/usr/bin/env python3\nfrom impl import second as entry\n";
            fs::write(&api, original).unwrap();
            fs::write(
                root.path().join("impl.py"),
                "def first(): return 1\ndef second(): return 2\n",
            )
            .unwrap();
            fs::write(
                root.path().join("consumer.py"),
                "from api import entry\ndef run(): return entry()\n",
            )
            .unwrap();
            let initial = run(root.path(), &db).unwrap();
            let snapshot = || Store::open_read_only(&db).unwrap().snapshot().unwrap();
            let before = serde_json::to_value(snapshot()).unwrap();
            let target = |graph: &GraphSnapshot| {
                let call = graph
                    .edges
                    .iter()
                    .find(|edge| edge.relation == "calls")
                    .unwrap();
                graph
                    .nodes
                    .iter()
                    .find(|node| node.id == call.target)
                    .unwrap()
                    .qualified_name
                    .clone()
                    .unwrap()
            };
            assert_eq!(target(&snapshot()), "first");

            fs::write(&api, temporary).unwrap();
            let restored = api.clone();
            AFTER_PYTHON_INVENTORY.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move || fs::write(restored, original).unwrap()));
            });
            let error = run(root.path(), &db).unwrap_err();
            // Extensionless sources are also probed for a JavaScript shebang.
            // That earlier inventory guard detects this same byte mismatch first.
            let expected_error = if api_name.ends_with(".py") {
                "Python source changed during scan"
            } else {
                "source changed during JavaScript context discovery"
            };
            assert!(
                error.to_string().contains(expected_error),
                "{api_name}: {error}"
            );
            assert_eq!(snapshot().generation, initial.generation);
            assert_eq!(serde_json::to_value(snapshot()).unwrap(), before);
            assert_eq!(
                run(root.path(), &db).unwrap().generation,
                initial.generation
            );

            // The nearest ordinary case still updates the actual target.
            fs::write(&api, temporary).unwrap();
            run(root.path(), &db).unwrap();
            assert_eq!(target(&snapshot()), "second");
            assert_eq!(run(root.path(), &db).unwrap().parsed_files, 0);
        }
    }

    #[test]
    fn cached_document_change_with_restored_metadata_aborts_before_publish() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join(".graf/index.db");
        let source = root.path().join("notes.md");
        fs::write(&source, "alpha\n").unwrap();
        let initial = run(root.path(), &db).unwrap();
        let before =
            serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap();

        fs::write(&source, "bravo\n").unwrap();
        let metadata = fs::metadata(&source).unwrap();
        let times = std::fs::FileTimes::new()
            .set_accessed(metadata.accessed().unwrap())
            .set_modified(metadata.modified().unwrap());
        let changed = source.clone();
        BEFORE_PUBLISH_VALIDATION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::write(&changed, "cider\n").unwrap();
                OpenOptions::new()
                    .write(true)
                    .open(&changed)
                    .unwrap()
                    .set_times(times)
                    .unwrap();
            }));
        });

        let error = run(root.path(), &db).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("source tree changed during indexing"),
            "{error:#}"
        );
        let after = Store::open_read_only(&db).unwrap().snapshot().unwrap();
        assert_eq!(after.generation, initial.generation);
        assert_eq!(serde_json::to_value(after).unwrap(), before);
    }
}
