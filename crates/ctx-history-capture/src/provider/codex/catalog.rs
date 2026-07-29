use std::{
    io::BufReader,
    path::{Path, PathBuf},
    thread,
    time::SystemTime,
};

use ctx_history_core::{AgentType, CaptureProvider};
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
    CaptureError, CatalogSummary, ProviderImportFailure, Result, CODEX_SESSION_SOURCE_FORMAT,
};

use crate::provider::codex::nativepath::{opened_codex_file_observation, CodexFileObservation};
use crate::provider::codex::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION};
use crate::provider::provider_path_identity;

pub(crate) const CODEX_CATALOG_MAX_SOURCES: usize = 131_072;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatalogSession {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: String,
    pub(crate) source_root: String,
    pub(crate) source_path: String,
    pub(crate) external_session_id: Option<String>,
    pub(crate) parent_external_session_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) session_started_at_ms: Option<i64>,
    pub(crate) file_size_bytes: u64,
    pub(crate) file_modified_at_ms: i64,
    pub(crate) cataloged_at_ms: i64,
    pub(crate) metadata: Value,
}

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

pub(crate) fn ensure_catalog_source_bound(source_count: usize) -> Result<()> {
    if source_count > CODEX_CATALOG_MAX_SOURCES {
        return Err(CaptureError::InvalidPayload(format!(
            "Codex catalog contains {source_count} sources; maximum is {CODEX_CATALOG_MAX_SOURCES}"
        )));
    }
    Ok(())
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
    catalog_codex_session_opened(
        &source_file.path,
        &source_file.opened,
        source_root,
        observation,
        cataloged_at_ms,
    )
}

pub(crate) fn catalog_codex_explicit_session_opened(
    path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<CatalogSession> {
    let observation = opened_codex_file_observation(path, opened.file())?;
    opened.revalidate()?;
    catalog_codex_session_opened(
        path,
        opened,
        &path.display().to_string(),
        &observation,
        system_time_ms(SystemTime::now()),
    )
}

fn catalog_codex_session_opened(
    path: &Path,
    opened: &OpenedProviderSourceFile,
    source_root: &str,
    observation: &CodexFileObservation,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let session_meta = read_codex_session_meta_from_opened(opened)?;
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
    read_codex_session_meta_from_opened(&source.opened)
}

fn read_codex_session_meta_from_opened(opened: &OpenedProviderSourceFile) -> Result<Option<Value>> {
    let mut reader = BufReader::new(opened.file().try_clone()?);
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
            opened.revalidate()?;
            return Ok(Some(value));
        }
    }
    opened.revalidate()?;
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
