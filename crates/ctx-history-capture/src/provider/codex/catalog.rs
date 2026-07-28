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
    collect_jsonl_paths_bounded, read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
};
use crate::common::time::{parse_rfc3339_utc, system_time_ms};
use crate::provider_sources::{
    observe_ordinary_file_strong_metadata, open_ordinary_file_without_following,
};
use crate::{
    observe_ordinary_file, CaptureError, CatalogSummary, CodexSessionCatalogOptions,
    OrdinaryFileObservation, ProviderImportFailure, Result, CODEX_SESSION_SOURCE_FORMAT,
};

use crate::provider::codex::{CODEX_CAPTURE_REVISION, CODEX_POLICY_REVISION};
use crate::provider::importer::provider_path_identity;

pub(crate) const CODEX_CATALOG_MAX_SOURCES: usize = 131_072;

pub(crate) fn discover_codex_session_catalog(
    root: &Path,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    provider_path_identity(root)?;
    let mut paths = Vec::new();
    collect_jsonl_paths_bounded(root, &mut paths, CODEX_CATALOG_MAX_SOURCES)?;
    ensure_catalog_source_bound(paths.len())?;
    for path in &paths {
        provider_path_identity(path)?;
    }
    catalog_codex_session_paths(
        paths,
        &root.display().to_string(),
        system_time_ms(SystemTime::now()),
        None,
    )
}

fn apply_codex_session_import_bounds(
    paths: &mut Vec<PathBuf>,
    max_files: Option<usize>,
    max_total_bytes: Option<u64>,
) -> Result<usize> {
    paths.sort();
    if max_files.is_none() && max_total_bytes.is_none() {
        return Ok(0);
    }

    let original_len = paths.len();
    let mut selected = Vec::new();
    let mut total_bytes = 0_u64;
    for path in paths.iter().rev() {
        if max_files.is_some_and(|limit| selected.len() >= limit) {
            continue;
        }
        let len = std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if max_total_bytes.is_some_and(|limit| total_bytes.saturating_add(len) > limit) {
            continue;
        }
        total_bytes = total_bytes.saturating_add(len);
        selected.push(path.clone());
    }
    selected.sort();
    let skipped = original_len.saturating_sub(selected.len());
    *paths = selected;
    Ok(skipped)
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
    let root = root.as_ref();
    provider_path_identity(root)?;
    let source_root_path = options.source_root.as_deref().unwrap_or(root);
    provider_path_identity(source_root_path)?;
    let source_root = source_root_path.display().to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let mut paths = Vec::new();
    collect_jsonl_paths_bounded(root, &mut paths, CODEX_CATALOG_MAX_SOURCES)?;
    for path in &paths {
        provider_path_identity(path)?;
    }
    let skipped_by_bounds = apply_codex_session_import_bounds(
        &mut paths,
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
    let mut current_paths = Vec::with_capacity(paths.len());
    let mut cached_sessions = Vec::new();
    let mut paths_to_observe = Vec::new();
    let mut paths_to_parse = Vec::new();
    let mut metadata_failures = Vec::new();
    for path in paths {
        let source_path = path.display().to_string();
        match observe_ordinary_file_strong_metadata(&path) {
            Ok(Some(observation)) => {
                if let Some(session) = cached_catalog_session_if_unchanged(
                    existing.get(&source_path),
                    &observation,
                    cataloged_at_ms,
                ) {
                    summary.source_files += 1;
                    summary.source_bytes = summary.source_bytes.saturating_add(observation.len());
                    current_paths.push(source_path);
                    summary.cached_sessions += 1;
                    cached_sessions.push(session);
                    continue;
                }
            }
            Ok(None) => {}
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        }
        paths_to_observe.push(path);
    }
    for (path, observation) in observe_codex_session_paths(paths_to_observe, options.parallelism)? {
        let observation = match observation {
            Ok(observation) => observation,
            Err(err) => {
                summary.failed_sessions += 1;
                metadata_failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        summary.source_files += 1;
        summary.source_bytes = summary.source_bytes.saturating_add(observation.len());
        let source_path = path.display().to_string();
        current_paths.push(source_path.clone());
        if let Some(session) = cached_catalog_session_if_unchanged(
            existing.get(&source_path),
            &observation,
            cataloged_at_ms,
        ) {
            summary.cached_sessions += 1;
            cached_sessions.push(session);
        } else {
            paths_to_parse.push(path);
        }
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
    if paths_to_parse.is_empty()
        && metadata_failures.is_empty()
        && cached_sessions.len() == current_paths.len()
        && existing.len() == current_paths.len()
        && !has_missing_existing_paths
        && stale_session_count == 0
    {
        summary.cataloged_sessions = cached_sessions.len();
        return Ok(summary);
    }
    let (scan_summary, sessions) = catalog_codex_session_paths(
        paths_to_parse,
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
    Ok(summary)
}

type CodexPathObservation = (PathBuf, Result<OrdinaryFileObservation>);

fn observe_codex_session_paths(
    paths: Vec<PathBuf>,
    requested_parallelism: Option<usize>,
) -> Result<Vec<CodexPathObservation>> {
    let parallelism = catalog_parallelism(paths.len(), requested_parallelism);
    if parallelism <= 1 {
        return Ok(paths
            .into_iter()
            .map(|path| {
                let observation = observe_ordinary_file(&path);
                (path, observation)
            })
            .collect());
    }

    let chunk_size = paths.len().div_ceil(parallelism).max(1);
    thread::scope(|scope| -> Result<Vec<CodexPathObservation>> {
        let mut handles = Vec::new();
        for chunk in paths.chunks(chunk_size) {
            let chunk = chunk.to_vec();
            handles.push(scope.spawn(move || {
                chunk
                    .into_iter()
                    .map(|path| {
                        let observation = observe_ordinary_file(&path);
                        (path, observation)
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let mut observations = Vec::with_capacity(paths.len());
        for handle in handles {
            observations.extend(
                handle
                    .join()
                    .map_err(|_| CaptureError::WorkerPanicked("Codex observation"))?,
            );
        }
        Ok(observations)
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
    for path in &paths {
        provider_path_identity(path)?;
    }
    let source_root = source_root_path.display().to_string();
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let (scan_summary, sessions) =
        catalog_codex_session_paths(paths, &source_root, cataloged_at_ms, options.parallelism)?;
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
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
) -> Option<CatalogSession> {
    let session = session?;
    let modified_at_ms = system_time_ms(observation.modified_at());
    let observation_token = observation.token_hex();
    if session.provider == CaptureProvider::Codex
        && session.source_format == CODEX_SESSION_SOURCE_FORMAT
        && session.file_size_bytes == observation.len()
        && session.file_modified_at_ms == modified_at_ms
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
pub(crate) fn catalog_codex_session_paths(
    paths: Vec<PathBuf>,
    source_root: &str,
    cataloged_at_ms: i64,
    requested_parallelism: Option<usize>,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    let parallelism = catalog_parallelism(paths.len(), requested_parallelism);
    let batches = if parallelism <= 1 {
        vec![catalog_codex_session_chunk(
            paths,
            source_root.to_owned(),
            cataloged_at_ms,
        )]
    } else {
        let chunk_size = paths.len().div_ceil(parallelism).max(1);
        thread::scope(|scope| -> Result<Vec<CatalogWorkerBatch>> {
            let mut handles = Vec::new();
            for chunk in paths.chunks(chunk_size) {
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
pub(crate) fn catalog_codex_session_chunk(
    paths: Vec<PathBuf>,
    source_root: String,
    cataloged_at_ms: i64,
) -> CatalogWorkerBatch {
    let mut batch = CatalogWorkerBatch {
        sessions: Vec::with_capacity(paths.len()),
        ..CatalogWorkerBatch::default()
    };
    for path in paths {
        let observation = match observe_ordinary_file(&path) {
            Ok(observation) => observation,
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch.failures.push(format!("{}: {err}", path.display()));
                continue;
            }
        };
        batch.summary.source_files += 1;
        batch.summary.source_bytes = batch.summary.source_bytes.saturating_add(observation.len());
        match catalog_codex_session_file(&path, source_root.as_str(), &observation, cataloged_at_ms)
        {
            Ok(session) => {
                batch.summary.parsed_sessions += 1;
                batch.sessions.push(session);
            }
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch.failures.push(format!("{}: {err}", path.display()));
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
pub(crate) fn catalog_codex_session_file(
    path: &Path,
    source_root: &str,
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let session_meta = read_codex_session_meta(path)?;
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
        file_size_bytes: observation.len(),
        file_modified_at_ms: system_time_ms(observation.modified_at()),
        cataloged_at_ms,
        metadata: json!({
            "inventory_file_change_token_v1": observation.token_hex(),
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
    let file = open_ordinary_file_without_following(path)?;
    let mut reader = BufReader::new(file);
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
            return Ok(Some(value));
        }
    }
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
