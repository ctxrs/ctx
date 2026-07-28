use std::{
    collections::{BTreeMap, BTreeSet},
    io::BufReader,
    path::{Path, PathBuf},
    thread,
    time::SystemTime,
};

use ctx_history_core::{AgentType, CaptureProvider};
use ctx_history_store::{CatalogSession, Store};
use serde_json::{json, Value};

use crate::common::io::{
    open_provider_source_file, read_provider_jsonl_line_or_skip_oversized,
    OpenedProviderSourceFile, OpenedProviderSourcePath, ProviderJsonlLineRead,
    ProviderSourceDirectory, ProviderSourceRoot, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
    PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES, PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use crate::common::time::{parse_rfc3339_utc, system_time_ms};
use crate::{
    CaptureError, CatalogSummary, CodexSessionCatalogOptions, ProviderImportFailure, Result,
    CODEX_SESSION_SOURCE_FORMAT,
};

use crate::provider::codex::nativepath::{opened_codex_file_observation, CodexFileObservation};
use crate::provider::codex::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION};
use crate::provider::importer::provider_path_identity;

pub(crate) const CODEX_CATALOG_MAX_SOURCES: usize = 131_072;

fn authority_path(path: &Path) -> Result<PathBuf> {
    use std::path::Component;

    let absolute = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CaptureError::InvalidProviderTranscriptPath {
                        path: path.to_path_buf(),
                        reason: "Codex catalog authority escapes the filesystem root",
                    });
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

#[derive(Debug, Clone)]
struct CodexCatalogRoute {
    path: PathBuf,
    root: Option<ProviderSourceRoot>,
    relative_path: Option<PathBuf>,
}

impl CodexCatalogRoute {
    fn open(&self) -> Result<CodexCatalogFile> {
        let opened = match (&self.root, &self.relative_path) {
            (Some(root), Some(relative_path)) => root
                .open_file(relative_path)
                .map_err(|error| map_codex_catalog_open_error(&self.path, error))?,
            (None, None) => open_provider_source_file(&authority_path(&self.path)?)
                .map_err(|error| map_codex_catalog_open_error(&self.path, error))?,
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "Codex catalog route authority is incomplete",
                ))
            }
        };
        Ok(CodexCatalogFile {
            path: self.path.clone(),
            opened,
        })
    }
}

fn map_codex_catalog_open_error(path: &Path, error: CaptureError) -> CaptureError {
    match error {
        CaptureError::InvalidProviderTranscriptPath { .. } => {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "linked provider transcript path components are rejected",
            }
        }
        CaptureError::Io(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "linked provider transcript path components are rejected",
            }
        }
        other => other,
    }
}

#[derive(Debug)]
struct CodexCatalogFile {
    path: PathBuf,
    opened: OpenedProviderSourceFile,
}

#[derive(Debug)]
pub(crate) struct RetainedCodexSessionCatalog {
    pub(crate) summary: CatalogSummary,
    pub(crate) sessions: Vec<CatalogSession>,
    pub(crate) root: ProviderSourceRoot,
}

#[derive(Debug)]
pub(crate) struct RetainedCodexCatalogTree {
    pub(crate) summary: CatalogSummary,
    pub(crate) live_paths: BTreeSet<String>,
    pub(crate) root: ProviderSourceRoot,
}

pub(crate) fn discover_codex_session_catalog(
    root: &Path,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    let retained = discover_codex_session_catalog_retained(root)?;
    Ok((retained.summary, retained.sessions))
}

pub(crate) fn discover_codex_session_catalog_retained(
    root: &Path,
) -> Result<RetainedCodexSessionCatalog> {
    provider_path_identity(root)?;
    let (authority, files) = discover_codex_catalog_files(root)?;
    build_retained_codex_session_catalog(root, authority, files)
}

pub(crate) fn rediscover_codex_session_catalog_retained(
    root: &Path,
    authority: &ProviderSourceRoot,
) -> Result<RetainedCodexSessionCatalog> {
    authority.revalidate()?;
    let files = discover_codex_catalog_files_from_root(root, authority)?;
    build_retained_codex_session_catalog(root, authority.clone(), files)
}

fn build_retained_codex_session_catalog(
    root: &Path,
    authority: ProviderSourceRoot,
    routes: Vec<CodexCatalogRoute>,
) -> Result<RetainedCodexSessionCatalog> {
    let (summary, sessions) = catalog_codex_session_routes(
        routes,
        &root.display().to_string(),
        system_time_ms(SystemTime::now()),
        None,
    )?;
    authority.revalidate()?;
    Ok(RetainedCodexSessionCatalog {
        summary,
        sessions,
        root: authority,
    })
}

fn apply_codex_session_import_bounds(
    routes: &mut Vec<CodexCatalogRoute>,
    max_files: Option<usize>,
    max_total_bytes: Option<u64>,
) -> Result<usize> {
    routes.sort_by(|left, right| left.path.cmp(&right.path));
    if max_files.is_none() && max_total_bytes.is_none() {
        return Ok(0);
    }

    let original_len = routes.len();
    let mut selected = Vec::new();
    let mut total_bytes = 0_u64;
    for route in routes.iter().rev() {
        if max_files.is_some_and(|limit| selected.len() >= limit) {
            continue;
        }
        let len = route.open().map(|source| source.opened.len()).unwrap_or(0);
        if max_total_bytes.is_some_and(|limit| total_bytes.saturating_add(len) > limit) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(len);
        selected.push(route.clone());
    }
    selected.sort_by(|left, right| left.path.cmp(&right.path));
    let skipped = original_len.saturating_sub(selected.len());
    *routes = selected;
    Ok(skipped)
}

fn discover_codex_catalog_files(
    configured_root: &Path,
) -> Result<(ProviderSourceRoot, Vec<CodexCatalogRoute>)> {
    let authority_path = authority_path(configured_root)?;
    let root = ProviderSourceRoot::open(&authority_path)?;
    let routes = discover_codex_catalog_files_from_root(configured_root, &root)?;
    Ok((root, routes))
}

fn discover_codex_catalog_files_from_root(
    configured_root: &Path,
    root: &ProviderSourceRoot,
) -> Result<Vec<CodexCatalogRoute>> {
    let mut routes = Vec::new();
    let mut visited_directories = 0_usize;
    let mut visited_entries = 0_usize;
    discover_codex_catalog_directory(
        configured_root,
        root.directory()?,
        0,
        &mut visited_directories,
        &mut visited_entries,
        &mut routes,
    )?;
    ensure_catalog_source_bound(routes.len())?;
    root.revalidate()?;
    Ok(routes)
}

fn discover_codex_catalog_directory(
    display_path: &Path,
    directory: ProviderSourceDirectory,
    depth: usize,
    visited_directories: &mut usize,
    visited_entries: &mut usize,
    routes: &mut Vec<CodexCatalogRoute>,
) -> Result<()> {
    if depth > PROVIDER_JSONL_INVENTORY_MAX_DEPTH {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog directory depth exceeds the provider inventory bound".to_owned(),
        ));
    }
    *visited_directories = visited_directories.saturating_add(1);
    if *visited_directories > PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog directory count exceeds the provider inventory bound".to_owned(),
        ));
    }
    let names = directory.entries(
        PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
            .saturating_sub(*visited_entries)
            .saturating_add(1),
    )?;
    *visited_entries = visited_entries.saturating_add(names.len());
    if *visited_entries > PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog entry count exceeds the provider inventory bound".to_owned(),
        ));
    }
    for name in names {
        let path = display_path.join(&name);
        if path.as_os_str().as_encoded_bytes().len() > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Codex catalog path exceeds the provider inventory bound".to_owned(),
            ));
        }
        match directory
            .open_child(&name)
            .map_err(|error| map_codex_catalog_open_error(&path, error))?
        {
            OpenedProviderSourcePath::Directory(child) => discover_codex_catalog_directory(
                &path,
                child,
                depth.saturating_add(1),
                visited_directories,
                visited_entries,
                routes,
            )?,
            OpenedProviderSourcePath::File(opened)
                if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") =>
            {
                provider_path_identity(&path)?;
                let relative_path = directory.relative_path().join(&name);
                routes.push(CodexCatalogRoute {
                    path,
                    root: Some(directory.authority_root()),
                    relative_path: Some(relative_path),
                });
                drop(opened);
                ensure_catalog_source_bound(routes.len())?;
            }
            OpenedProviderSourcePath::File(_) => {}
        }
    }
    directory.revalidate()?;
    Ok(())
}

fn codex_catalog_observation(source: &CodexCatalogFile) -> Result<CodexFileObservation> {
    let observation = opened_codex_file_observation(&source.path, source.opened.file())?;
    source.opened.revalidate()?;
    Ok(observation)
}

fn hex_digest(value: &[u8; 32]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

pub fn catalog_codex_session_tree(
    root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    Ok(catalog_codex_session_tree_retained(root, store, options)?.summary)
}

pub(crate) fn catalog_codex_session_tree_retained(
    root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<RetainedCodexCatalogTree> {
    let root = root.as_ref();
    provider_path_identity(root)?;
    let source_root_path = options.source_root.as_deref().unwrap_or(root);
    provider_path_identity(source_root_path)?;
    let source_root = source_root_path.display().to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let (authority, mut routes) = discover_codex_catalog_files(root)?;
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut routes,
        options.max_session_files,
        options.max_total_bytes,
    )?;

    let mut summary = CatalogSummary {
        skipped_sessions: skipped_by_bounds,
        ..CatalogSummary::default()
    };
    let existing = store
        .list_catalog_sessions_for_source_bounded(
            CaptureProvider::Codex,
            &source_root,
            CODEX_CATALOG_MAX_SOURCES,
        )?
        .into_iter()
        .map(|session| (session.source_path.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut current_paths = Vec::with_capacity(routes.len());
    let mut cached_sessions = Vec::new();
    let mut sources_to_parse = Vec::new();
    let mut metadata_failures = Vec::new();
    for route in routes {
        let source = match route.open() {
            Ok(source) => source,
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", route.path.display()));
                continue;
            }
        };
        let source_path = source.path.display().to_string();
        let observation = match codex_catalog_observation(&source) {
            Ok(observation) => observation,
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", source.path.display()));
                continue;
            }
        };
        summary.source_files += 1;
        summary.source_bytes = summary.source_bytes.saturating_add(observation.len);
        current_paths.push(source_path.clone());
        if let Some(session) = cached_catalog_session_if_unchanged(
            existing.get(&source_path),
            &observation,
            cataloged_at_ms,
        ) {
            summary.cached_sessions += 1;
            cached_sessions.push(session);
            continue;
        }
        sources_to_parse.push(route);
    }
    summary.failures.extend(
        metadata_failures
            .iter()
            .cloned()
            .map(|error| ProviderImportFailure { line: 0, error }),
    );
    let stale_session_count =
        store.catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)?;
    let current_path_set = current_paths.iter().cloned().collect::<BTreeSet<_>>();
    let has_missing_existing_paths = existing
        .keys()
        .any(|source_path| !current_path_set.contains(source_path));
    if sources_to_parse.is_empty()
        && metadata_failures.is_empty()
        && cached_sessions.len() == current_paths.len()
        && existing.len() == current_paths.len()
        && !has_missing_existing_paths
        && stale_session_count == 0
    {
        summary.cataloged_sessions = cached_sessions.len();
        authority.revalidate()?;
        return Ok(RetainedCodexCatalogTree {
            summary,
            live_paths: current_path_set,
            root: authority,
        });
    }
    let (scan_summary, sessions) = catalog_codex_session_routes(
        sources_to_parse,
        &source_root,
        cataloged_at_ms,
        options.parallelism,
    )?;
    summary.failed_sessions += scan_summary.failed_sessions;
    summary.failures.extend(scan_summary.failures);
    summary.parsed_sessions += scan_summary.parsed_sessions;
    let parsed_session_count = sessions.len();
    let cached_session_count = cached_sessions.len();
    let mut sessions_to_persist = sessions;
    if stale_session_count > 0 {
        sessions_to_persist.extend(cached_sessions);
    }
    summary.cataloged_sessions = parsed_session_count.saturating_add(cached_session_count);

    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        if !sessions_to_persist.is_empty() {
            store.upsert_catalog_sessions(&sessions_to_persist)?;
        }
        if stale_session_count > 0 || has_missing_existing_paths {
            store.mark_catalog_source_missing_paths_stale(
                CaptureProvider::Codex,
                &source_root,
                &current_paths,
                cataloged_at_ms,
            )?;
        }
        Ok(())
    })();
    match persist {
        Ok(()) => {
            store.commit_batch()?;
        }
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    authority.revalidate()?;
    Ok(RetainedCodexCatalogTree {
        summary,
        live_paths: current_path_set,
        root: authority,
    })
}

pub fn catalog_codex_session_files(
    paths: Vec<PathBuf>,
    source_root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    ensure_catalog_source_bound(paths.len())?;
    let source_root_path = options
        .source_root
        .as_deref()
        .unwrap_or(source_root.as_ref());
    provider_path_identity(source_root_path)?;
    let mut routes = Vec::with_capacity(paths.len());
    for path in paths {
        provider_path_identity(&path)?;
        routes.push(CodexCatalogRoute {
            path,
            root: None,
            relative_path: None,
        });
    }
    let source_root = source_root_path.display().to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let (scan_summary, sessions) =
        catalog_codex_session_routes(routes, &source_root, cataloged_at_ms, options.parallelism)?;
    let mut summary = scan_summary;
    summary.cataloged_sessions = sessions.len();
    if !sessions.is_empty() {
        store.upsert_catalog_sessions(&sessions)?;
    }
    Ok(summary)
}

pub(crate) fn ensure_catalog_source_bound(source_count: usize) -> Result<()> {
    if source_count > CODEX_CATALOG_MAX_SOURCES {
        return Err(CaptureError::InvalidPayload(format!(
            "Codex catalog contains {source_count} sources; maximum is {CODEX_CATALOG_MAX_SOURCES}"
        )));
    }
    Ok(())
}

pub(crate) fn cached_catalog_session_if_unchanged(
    session: Option<&CatalogSession>,
    observation: &CodexFileObservation,
    cataloged_at_ms: i64,
) -> Option<CatalogSession> {
    let session = session?;
    let observation_token = hex_digest(&observation.change_token);
    if session.provider == CaptureProvider::Codex
        && session.source_format == CODEX_SESSION_SOURCE_FORMAT
        && session.file_size_bytes == observation.len
        && session.file_modified_at_ms == observation.modified_at_ms
        && session
            .metadata
            .get("inventory_file_change_token_v1")
            .and_then(Value::as_str)
            == Some(observation_token.as_str())
        && session
            .metadata
            .get("normalization_capture_revision")
            .and_then(Value::as_u64)
            == Some(u64::from(CODEX_CAPTURE_REVISION))
        && session
            .metadata
            .get("normalization_policy_revision")
            .and_then(Value::as_u64)
            == Some(u64::from(CODEX_POLICY_REVISION))
    {
        let mut session = session.clone();
        session.cataloged_at_ms = cataloged_at_ms;
        Some(session)
    } else {
        None
    }
}
#[derive(Debug, Default)]
pub(crate) struct CatalogWorkerBatch {
    pub(crate) summary: CatalogSummary,
    pub(crate) sessions: Vec<CatalogSession>,
    pub(crate) failures: Vec<String>,
}
fn catalog_codex_session_routes(
    routes: Vec<CodexCatalogRoute>,
    source_root: &str,
    cataloged_at_ms: i64,
    requested_parallelism: Option<usize>,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    let parallelism = catalog_parallelism(routes.len(), requested_parallelism);
    let batches = if parallelism <= 1 {
        vec![catalog_codex_session_chunk(
            routes,
            source_root.to_owned(),
            cataloged_at_ms,
        )]
    } else {
        let chunk_size = routes.len().div_ceil(parallelism).max(1);
        thread::scope(|scope| -> Result<Vec<CatalogWorkerBatch>> {
            let mut handles = Vec::new();
            for chunk in routes.chunks(chunk_size) {
                let chunk = chunk.to_vec();
                let source_root = source_root.to_owned();
                handles.push(scope.spawn(move || {
                    catalog_codex_session_chunk(chunk, source_root, cataloged_at_ms)
                }));
            }
            let mut batches = Vec::with_capacity(handles.len());
            for handle in handles {
                batches.push(
                    handle
                        .join()
                        .map_err(|_| CaptureError::WorkerPanicked("Codex catalog"))?,
                );
            }
            Ok(batches)
        })?
    };

    let mut summary = CatalogSummary::default();
    let mut sessions = Vec::new();
    for mut batch in batches {
        summary.source_files += batch.summary.source_files;
        summary.source_bytes = summary
            .source_bytes
            .saturating_add(batch.summary.source_bytes);
        summary.parsed_sessions += batch.summary.parsed_sessions;
        summary.failed_sessions += batch.summary.failed_sessions;
        sessions.append(&mut batch.sessions);
        summary.failures.extend(
            batch
                .failures
                .drain(..)
                .map(|error| ProviderImportFailure { line: 0, error }),
        );
    }
    Ok((summary, sessions))
}
fn catalog_codex_session_chunk(
    routes: Vec<CodexCatalogRoute>,
    source_root: String,
    cataloged_at_ms: i64,
) -> CatalogWorkerBatch {
    let mut batch = CatalogWorkerBatch {
        sessions: Vec::with_capacity(routes.len()),
        ..CatalogWorkerBatch::default()
    };
    for route in routes {
        let source = match route.open() {
            Ok(source) => source,
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch
                    .failures
                    .push(format!("{}: {err}", route.path.display()));
                continue;
            }
        };
        let observation = match codex_catalog_observation(&source) {
            Ok(observation) => observation,
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch
                    .failures
                    .push(format!("{}: {err}", source.path.display()));
                continue;
            }
        };
        batch.summary.source_files += 1;
        batch.summary.source_bytes = batch.summary.source_bytes.saturating_add(observation.len);
        match catalog_codex_session_file(
            &source,
            source_root.as_str(),
            &observation,
            cataloged_at_ms,
        ) {
            Ok(session) => {
                batch.summary.parsed_sessions += 1;
                batch.sessions.push(session);
            }
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch
                    .failures
                    .push(format!("{}: {err}", source.path.display()));
            }
        }
    }
    batch
}
pub(crate) fn catalog_parallelism(
    path_count: usize,
    requested_parallelism: Option<usize>,
) -> usize {
    if path_count <= 1 {
        return 1;
    }
    requested_parallelism
        .or_else(|| thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .clamp(1, 32)
        .min(path_count)
}
fn catalog_codex_session_file(
    source_file: &CodexCatalogFile,
    source_root: &str,
    observation: &CodexFileObservation,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let path = &source_file.path;
    let session_meta = read_codex_session_meta_opened(source_file)?;
    let payload = session_meta.as_ref().and_then(|value| value.get("payload"));
    let source = payload
        .and_then(|payload| payload.get("source"))
        .cloned()
        .unwrap_or(Value::Null);
    let parent_external_session_id = codex_parent_session_id(&source);
    let external_session_id = payload
        .and_then(|payload| payload.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| codex_session_id_from_path(path));
    let session_started_at_ms = payload
        .and_then(|payload| payload.get("timestamp"))
        .and_then(Value::as_str)
        .or_else(|| {
            session_meta
                .as_ref()
                .and_then(|value| value.get("timestamp"))
                .and_then(Value::as_str)
        })
        .and_then(parse_rfc3339_utc)
        .map(|timestamp| timestamp.timestamp_millis());
    let agent_type = if parent_external_session_id.is_some() {
        AgentType::Subagent
    } else {
        AgentType::Primary
    };
    let role_hint = payload
        .and_then(|payload| payload.get("agent_role"))
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| Some(agent_type.as_str().to_owned()));

    Ok(CatalogSession {
        provider: CaptureProvider::Codex,
        source_format: CODEX_SESSION_SOURCE_FORMAT.to_owned(),
        source_root: source_root.to_owned(),
        source_path: path.display().to_string(),
        external_session_id,
        parent_external_session_id,
        agent_type,
        role_hint,
        external_agent_id: payload
            .and_then(|payload| payload.get("agent_nickname"))
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty())
            .map(str::to_owned),
        cwd: payload
            .and_then(|payload| payload.get("cwd"))
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.trim().is_empty())
            .map(str::to_owned),
        session_started_at_ms,
        file_size_bytes: observation.len,
        file_modified_at_ms: observation.modified_at_ms,
        cataloged_at_ms,
        metadata: json!({
            "inventory_file_change_token_v1": hex_digest(&observation.change_token),
            "normalization_capture_revision": CODEX_CAPTURE_REVISION,
            "normalization_policy_revision": CODEX_POLICY_REVISION,
            "originator": payload.and_then(|payload| payload.get("originator")).and_then(Value::as_str),
            "cli_version": payload.and_then(|payload| payload.get("cli_version")).and_then(Value::as_str),
            "model_provider": payload.and_then(|payload| payload.get("model_provider")).and_then(Value::as_str),
            "source_kind": codex_source_kind(&source),
            "source": source,
            "catalog_scope": "session_meta",
        }),
    })
}
pub(crate) fn read_codex_session_meta(path: &Path) -> Result<Option<Value>> {
    let authority_path = authority_path(path)?;
    let source = CodexCatalogFile {
        path: path.to_path_buf(),
        opened: open_provider_source_file(&authority_path)?,
    };
    read_codex_session_meta_opened(&source)
}

fn read_codex_session_meta_opened(source: &CodexCatalogFile) -> Result<Option<Value>> {
    let mut reader = BufReader::new(source.opened.file().try_clone()?);
    let mut line = Vec::new();
    for _ in 0..32 {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)? {
            ProviderJsonlLineRead::Eof => break,
            ProviderJsonlLineRead::Line { .. } => {}
            ProviderJsonlLineRead::Oversized { .. } => continue,
        }
        if !line.contains(&b'{') || !contains_bytes(&line, br#""session_meta""#) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("session_meta") {
            source.opened.revalidate()?;
            return Ok(Some(value));
        }
    }
    source.opened.revalidate()?;
    Ok(None)
}
pub(crate) fn codex_parent_session_id(source: &Value) -> Option<String> {
    source
        .pointer("/subagent/thread_spawn/parent_thread_id")
        .or_else(|| source.pointer("/thread_spawn/parent_thread_id"))
        .or_else(|| source.get("parent_thread_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}
pub(crate) fn codex_source_kind(source: &Value) -> Option<String> {
    if let Some(value) = source.as_str().filter(|value| !value.trim().is_empty()) {
        return Some(value.to_owned());
    }
    if source.pointer("/subagent/thread_spawn").is_some() {
        return Some("subagent".to_owned());
    }
    if source.pointer("/thread_spawn").is_some() {
        return Some("thread_spawn".to_owned());
    }
    source
        .as_object()
        .and_then(|object| object.keys().next().cloned())
}
pub(crate) fn codex_session_id_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if stem.len() >= 36 {
        let tail = &stem[stem.len() - 36..];
        if tail.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-') {
            return Some(tail.to_owned());
        }
    }
    (!stem.trim().is_empty()).then(|| stem.to_owned())
}
