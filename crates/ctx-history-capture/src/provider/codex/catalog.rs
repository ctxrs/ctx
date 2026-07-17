use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    thread,
};

use ctx_history_core::{AgentType, CaptureProvider};
use ctx_history_store::{CatalogSession, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::common::io::{
    collect_jsonl_paths, read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
};
use crate::common::time::{parse_rfc3339_utc, system_time_ms};
use crate::{
    provider_sources::{
        open_observed_ordinary_file, provider_import_revision, OrdinaryFileObservation,
    },
    CaptureError, CatalogSummary, CodexSessionCatalogOptions, DurableSourceInventoryJournalEntry,
    ProviderImportFailure, Result, CODEX_SESSION_SOURCE_FORMAT,
};

use crate::provider::codex::session::{apply_codex_session_import_bounds, contains_bytes};

const CATALOG_PERSIST_BATCH_SESSIONS: usize = 64;
const CATALOG_PERSIST_BATCH_BYTES: usize = 8 * 1024 * 1024;
const CATALOG_PERSIST_ROW_OVERHEAD_BYTES: u64 = 256;
const QUIET_CATALOG_MAX_PARALLELISM: usize = 2;
const INTERACTIVE_CATALOG_MAX_PARALLELISM: usize = 8;
const DURABLE_CATALOG_OBSERVATION_PAGE: usize = 64;

#[derive(Debug, Clone)]
pub enum CodexCatalogObservationOutcome {
    Cataloged(Box<CatalogSession>),
    Failed(ProviderImportFailure),
}

#[derive(Debug, Clone)]
pub struct CodexCatalogJournalObservation {
    pub journal: DurableSourceInventoryJournalEntry,
    pub effect_fingerprint: [u8; 32],
    pub source_files: u64,
    pub source_bytes: u64,
    pub outcome: CodexCatalogObservationOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct CodexCatalogObservationPage {
    pub observations: Vec<CodexCatalogJournalObservation>,
    pub summary: CatalogSummary,
}

pub fn observe_codex_catalog_journal_page(
    entries: &[DurableSourceInventoryJournalEntry],
    source_root: &str,
    cataloged_at_ms: i64,
) -> Result<CodexCatalogObservationPage> {
    if entries.len() > DURABLE_CATALOG_OBSERVATION_PAGE {
        return Err(CaptureError::SystemInvariant(
            "Codex durable catalog observation page exceeds its row bound",
        ));
    }
    validate_codex_catalog_session_paths(
        &entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
    )?;
    let mut page = CodexCatalogObservationPage {
        observations: Vec::with_capacity(entries.len()),
        ..CodexCatalogObservationPage::default()
    };
    for journal in entries {
        let (file, observation) = match open_observed_ordinary_file(&journal.path) {
            Ok(observed) => observed,
            Err(error) => {
                let failure = ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {error}", journal.path.display()),
                };
                page.summary.failed_sessions = page.summary.failed_sessions.saturating_add(1);
                page.summary.sample_failure(failure.clone());
                let outcome = CodexCatalogObservationOutcome::Failed(failure);
                page.observations.push(CodexCatalogJournalObservation {
                    journal: journal.clone(),
                    effect_fingerprint: codex_catalog_observation_fingerprint(
                        journal, 0, 0, &outcome,
                    )?,
                    source_files: 0,
                    source_bytes: 0,
                    outcome,
                });
                continue;
            }
        };
        let source_bytes = observation.len();
        page.summary.source_files = page.summary.source_files.saturating_add(1);
        page.summary.source_bytes = page.summary.source_bytes.saturating_add(source_bytes);
        match catalog_codex_session_file(
            &journal.path,
            file,
            source_root,
            &observation,
            cataloged_at_ms,
        ) {
            Ok(session) => {
                page.summary.parsed_sessions = page.summary.parsed_sessions.saturating_add(1);
                page.summary.cataloged_sessions = page.summary.cataloged_sessions.saturating_add(1);
                let outcome = CodexCatalogObservationOutcome::Cataloged(Box::new(session));
                page.observations.push(CodexCatalogJournalObservation {
                    journal: journal.clone(),
                    effect_fingerprint: codex_catalog_observation_fingerprint(
                        journal,
                        1,
                        source_bytes,
                        &outcome,
                    )?,
                    source_files: 1,
                    source_bytes,
                    outcome,
                });
            }
            Err(error) => {
                let failure = ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {error}", journal.path.display()),
                };
                page.summary.failed_sessions = page.summary.failed_sessions.saturating_add(1);
                page.summary.sample_failure(failure.clone());
                let outcome = CodexCatalogObservationOutcome::Failed(failure);
                page.observations.push(CodexCatalogJournalObservation {
                    journal: journal.clone(),
                    effect_fingerprint: codex_catalog_observation_fingerprint(
                        journal,
                        1,
                        source_bytes,
                        &outcome,
                    )?,
                    source_files: 1,
                    source_bytes,
                    outcome,
                });
            }
        }
    }
    Ok(page)
}

fn codex_catalog_observation_fingerprint(
    journal: &DurableSourceInventoryJournalEntry,
    source_files: u64,
    source_bytes: u64,
    outcome: &CodexCatalogObservationOutcome,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-codex-catalog-observation-v1\0");
    hasher.update(journal.journal_identity);
    hasher.update(source_files.to_be_bytes());
    hasher.update(source_bytes.to_be_bytes());
    match outcome {
        CodexCatalogObservationOutcome::Cataloged(session) => {
            hasher.update([1]);
            for value in [
                session.provider.as_str(),
                session.source_format.as_str(),
                session.source_root.as_str(),
                session.source_path.as_str(),
                session.agent_type.as_str(),
            ] {
                hash_catalog_field(&mut hasher, value.as_bytes());
            }
            for value in [
                session.external_session_id.as_deref(),
                session.parent_external_session_id.as_deref(),
                session.role_hint.as_deref(),
                session.external_agent_id.as_deref(),
                session.cwd.as_deref(),
            ] {
                hash_optional_catalog_field(&mut hasher, value);
            }
            hasher.update(
                session
                    .session_started_at_ms
                    .unwrap_or(i64::MIN)
                    .to_be_bytes(),
            );
            hasher.update(session.file_size_bytes.to_be_bytes());
            hasher.update(session.file_modified_at_ms.to_be_bytes());
            hasher.update(session.import_revision.to_be_bytes());
            hash_catalog_field(&mut hasher, &serde_json::to_vec(&session.metadata)?);
        }
        CodexCatalogObservationOutcome::Failed(failure) => {
            hasher.update([2]);
            hasher.update((failure.line as u64).to_be_bytes());
        }
    }
    Ok(hasher.finalize().into())
}

fn hash_optional_catalog_field(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_catalog_field(hasher, value.as_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_catalog_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub fn catalog_codex_session_tree(
    root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    let root = root.as_ref();
    codex_catalog_root_identity(root)?;
    let source_root_path = options.source_root.as_deref().unwrap_or(root);
    let source_root = codex_catalog_root_identity(source_root_path)?.to_owned();
    let observation_generation = match options.observation_generation {
        Some(generation) => generation,
        None => {
            store.allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?
        }
    };
    let import_revision =
        provider_import_revision(CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT);
    let initial_inventory =
        prepare_initial_catalog_inventory(store, &source_root, observation_generation)?;
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let mut paths = Vec::new();
    collect_jsonl_paths(root, &mut paths)?;
    validate_codex_catalog_session_paths(&paths)?;
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
        .list_catalog_sessions_for_source(CaptureProvider::Codex, &source_root)?
        .into_iter()
        .map(|session| (session.source_path.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut current_paths = Vec::with_capacity(paths.len());
    let mut cached_sessions = Vec::new();
    let mut paths_to_parse = Vec::new();
    let mut has_metadata_failures = false;
    for path in paths {
        let observation = match crate::observe_ordinary_file(&path) {
            Ok(observation) => observation,
            Err(err) => {
                summary.failed_sessions += 1;
                has_metadata_failures = true;
                summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {err}", path.display()),
                });
                continue;
            }
        };
        summary.source_files += 1;
        summary.source_bytes = summary.source_bytes.saturating_add(observation.len());
        let source_path = codex_catalog_session_identity(&path)?.to_owned();
        current_paths.push(source_path.clone());
        if let Some(session) = cached_catalog_session_if_unchanged(
            existing.get(&source_path),
            &observation,
            cataloged_at_ms,
            import_revision,
        ) {
            summary.cached_sessions += 1;
            cached_sessions.push(session);
        } else {
            paths_to_parse.push(path);
        }
    }
    let stale_session_count =
        store.catalog_source_stale_session_count(CaptureProvider::Codex, &source_root)?;
    let current_path_set = current_paths.iter().cloned().collect::<BTreeSet<_>>();
    let has_missing_existing_paths = existing
        .keys()
        .any(|source_path| !current_path_set.contains(source_path));
    if paths_to_parse.is_empty()
        && !has_metadata_failures
        && cached_sessions.len() == current_paths.len()
        && existing.len() == current_paths.len()
        && !has_missing_existing_paths
        && stale_session_count == 0
    {
        summary.cataloged_sessions = cached_sessions.len();
        store.begin_immediate_batch()?;
        let completed = store.complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            &source_root,
            observation_generation,
        );
        match completed {
            Ok(true) => store.commit_batch()?,
            Ok(false) => {
                let _ = store.rollback_batch();
                return Err(CaptureError::InventorySuperseded);
            }
            Err(err) => {
                let _ = store.rollback_batch();
                return Err(err.into());
            }
        }
        return Ok(summary);
    }
    let (mut scan_summary, sessions) = catalog_codex_session_paths(
        paths_to_parse,
        &source_root,
        cataloged_at_ms,
        options.parallelism,
    )?;
    // The metadata inventory above already counted every readable source file
    // and byte. Parse workers report those counters for direct file-list calls.
    scan_summary.source_files = 0;
    scan_summary.source_bytes = 0;
    summary.merge_from(scan_summary);
    let parsed_session_count = sessions.len();
    let cached_session_count = cached_sessions.len();
    let mut sessions_to_persist = sessions;
    if stale_session_count > 0 {
        sessions_to_persist.extend(cached_sessions);
    }
    summary.cataloged_sessions = parsed_session_count.saturating_add(cached_session_count);

    let initial_inventory_persisted = if initial_inventory && !sessions_to_persist.is_empty() {
        persist_initial_catalog_sessions_bounded(
            store,
            observation_generation,
            &sessions_to_persist,
        )?
    } else {
        initial_inventory
    };

    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        persist_catalog_sessions_for_publication(
            store,
            observation_generation,
            &sessions_to_persist,
            initial_inventory_persisted,
        )?;
        if stale_session_count > 0 || has_missing_existing_paths {
            store.mark_catalog_source_missing_paths_stale_paced(
                CaptureProvider::Codex,
                &source_root,
                &current_paths,
                cataloged_at_ms,
                observation_generation,
                crate::pace_current_disk_io,
            )?;
        }
        if !store.complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            &source_root,
            observation_generation,
        )? {
            return Err(CaptureError::InventorySuperseded);
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

#[doc(hidden)]
pub fn catalog_codex_session_paths_page(
    paths: Vec<PathBuf>,
    root: impl AsRef<Path>,
    store: &Store,
    inventory_generation: u64,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    if paths.len() > 64 {
        return Err(CaptureError::SystemInvariant(
            "Codex catalog inventory page exceeds its internal path limit",
        ));
    }
    let root = root.as_ref();
    codex_catalog_root_identity(root)?;
    let source_root_path = options.source_root.as_deref().unwrap_or(root);
    let source_root = codex_catalog_root_identity(source_root_path)?.to_owned();
    if !store.catalog_inventory_generation_is_current(
        CaptureProvider::Codex,
        &source_root,
        inventory_generation,
    )? {
        return Err(CaptureError::InventorySuperseded);
    }
    validate_codex_catalog_session_paths(&paths)?;
    let identities = paths
        .iter()
        .map(|path| codex_catalog_session_identity(path).map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
    let existing = store
        .list_catalog_observation_states_for_paths(
            CaptureProvider::Codex,
            &source_root,
            &identities,
        )?
        .into_iter()
        .map(|state| (state.source_path.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let import_revision =
        provider_import_revision(CaptureProvider::Codex, CODEX_SESSION_SOURCE_FORMAT);
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let mut summary = CatalogSummary::default();
    let mut paths_to_parse = Vec::new();
    for (path, source_path) in paths.into_iter().zip(identities) {
        let observation = match crate::observe_ordinary_file(&path) {
            Ok(observation) => observation,
            Err(error) => {
                summary.failed_sessions = summary.failed_sessions.saturating_add(1);
                summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {error}", path.display()),
                });
                continue;
            }
        };
        summary.source_files = summary.source_files.saturating_add(1);
        summary.source_bytes = summary.source_bytes.saturating_add(observation.len());
        let observation_token = observation.token_hex();
        let cached = existing.get(&source_path).is_some_and(|state| {
            !state.is_stale
                && state.source_format == CODEX_SESSION_SOURCE_FORMAT
                && state.import_revision == import_revision
                && state.file_size_bytes == observation.len()
                && state.file_modified_at_ms == system_time_ms(observation.modified_at())
                && state.observation_token.as_deref() == Some(observation_token.as_str())
        });
        if cached {
            summary.cached_sessions = summary.cached_sessions.saturating_add(1);
            summary.cataloged_sessions = summary.cataloged_sessions.saturating_add(1);
        } else {
            paths_to_parse.push(path);
        }
    }

    let (mut parsed_summary, sessions) = catalog_codex_session_paths(
        paths_to_parse,
        &source_root,
        cataloged_at_ms,
        options.parallelism,
    )?;
    parsed_summary.source_files = 0;
    parsed_summary.source_bytes = 0;
    summary.merge_from(parsed_summary);
    summary.cataloged_sessions = summary.cataloged_sessions.saturating_add(sessions.len());
    if !sessions.is_empty() {
        store.begin_immediate_batch()?;
        let persisted =
            persist_catalog_sessions_paced_in_current_batch(store, inventory_generation, &sessions);
        match persisted {
            Ok(()) => store.commit_batch()?,
            Err(error) => {
                let _ = store.rollback_batch();
                return Err(error);
            }
        }
    }
    Ok(summary)
}

pub fn catalog_codex_session_files(
    paths: Vec<PathBuf>,
    source_root: impl AsRef<Path>,
    store: &Store,
    options: CodexSessionCatalogOptions,
) -> Result<CatalogSummary> {
    codex_catalog_root_identity(source_root.as_ref())?;
    let source_root_path = options
        .source_root
        .as_deref()
        .unwrap_or(source_root.as_ref());
    let source_root = codex_catalog_root_identity(source_root_path)?.to_owned();
    validate_codex_catalog_session_paths(&paths)?;
    let observation_generation = match options.observation_generation {
        Some(generation) => generation,
        None => {
            store.allocate_catalog_inventory_generation(CaptureProvider::Codex, &source_root)?
        }
    };
    let initial_inventory =
        prepare_initial_catalog_inventory(store, &source_root, observation_generation)?;
    let cataloged_at_ms = options.cataloged_at.timestamp_millis();
    let (scan_summary, sessions) =
        catalog_codex_session_paths(paths, &source_root, cataloged_at_ms, options.parallelism)?;
    let mut summary = scan_summary;
    summary.cataloged_sessions = sessions.len();
    let initial_inventory_persisted = if initial_inventory && !sessions.is_empty() {
        persist_initial_catalog_sessions_bounded(store, observation_generation, &sessions)?
    } else {
        initial_inventory
    };
    store.begin_immediate_batch()?;
    let persist = (|| -> Result<()> {
        persist_catalog_sessions_for_publication(
            store,
            observation_generation,
            &sessions,
            initial_inventory_persisted,
        )?;
        if !store.complete_catalog_inventory_generation(
            CaptureProvider::Codex,
            &source_root,
            observation_generation,
        )? {
            return Err(CaptureError::InventorySuperseded);
        }
        Ok(())
    })();
    match persist {
        Ok(()) => store.commit_batch()?,
        Err(err) => {
            let _ = store.rollback_batch();
            return Err(err);
        }
    }
    Ok(summary)
}

fn prepare_initial_catalog_inventory(
    store: &Store,
    source_root: &str,
    observation_generation: u64,
) -> Result<bool> {
    if !store.catalog_inventory_generation_is_unpublished(
        CaptureProvider::Codex,
        source_root,
        observation_generation,
    )? {
        return Ok(false);
    }
    loop {
        let Some((deleted, _bytes)) = store.delete_unpublished_catalog_sessions_batch_paced(
            CaptureProvider::Codex,
            source_root,
            observation_generation,
            CATALOG_PERSIST_BATCH_SESSIONS,
            crate::pace_current_disk_io,
        )?
        else {
            return Err(CaptureError::InventorySuperseded);
        };
        if deleted == 0 {
            return Ok(true);
        }
    }
}

fn persist_initial_catalog_sessions_bounded(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
) -> Result<bool> {
    persist_initial_catalog_sessions_bounded_with_observer(
        store,
        observation_generation,
        sessions,
        |_| {},
    )
}

fn persist_catalog_sessions_paced_in_current_batch(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
) -> Result<()> {
    let byte_limit = crate::disk_io_pacing::current_disk_io_burst_bytes()
        .and_then(|bytes| usize::try_from(bytes).ok())
        .map_or(CATALOG_PERSIST_BATCH_BYTES, |bytes| {
            bytes.clamp(1, CATALOG_PERSIST_BATCH_BYTES)
        });
    let mut start = 0;
    while start < sessions.len() {
        let (count, bytes) = catalog_persist_batch(&sessions[start..], byte_limit)?;
        crate::pace_current_disk_io(bytes);
        store.upsert_catalog_sessions(
            observation_generation,
            &sessions[start..start.saturating_add(count)],
        )?;
        start = start.saturating_add(count);
    }
    Ok(())
}

fn persist_catalog_sessions_for_publication(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
    staged: bool,
) -> Result<()> {
    let source_root = sessions
        .first()
        .map(|session| session.source_root.as_str())
        .unwrap_or_default();
    let staged_complete = staged
        && store.catalog_sessions_all_owned_by_source_paced(
            CaptureProvider::Codex,
            source_root,
            sessions,
            crate::pace_current_disk_io,
        )?;
    if !staged_complete && !sessions.is_empty() {
        persist_catalog_sessions_paced_in_current_batch(store, observation_generation, sessions)?;
    }
    Ok(())
}

fn persist_initial_catalog_sessions_bounded_with_observer(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
    mut batch_committed: impl FnMut(usize),
) -> Result<bool> {
    let source_root = sessions
        .first()
        .map(|session| session.source_root.as_str())
        .unwrap_or_default();
    if store.catalog_sessions_have_external_path_owners_paced(
        CaptureProvider::Codex,
        source_root,
        sessions,
        crate::pace_current_disk_io,
    )? {
        return Ok(false);
    }
    let byte_limit = crate::disk_io_pacing::current_disk_io_burst_bytes()
        .and_then(|bytes| usize::try_from(bytes).ok())
        .map_or(CATALOG_PERSIST_BATCH_BYTES, |bytes| {
            bytes.clamp(1, CATALOG_PERSIST_BATCH_BYTES)
        });
    let mut start = 0;
    while start < sessions.len() {
        let (end, bytes) = catalog_persist_batch(&sessions[start..], byte_limit)?;
        crate::pace_current_disk_io(bytes);
        store.begin_immediate_batch()?;
        let persist = (|| -> Result<bool> {
            let source_root = sessions[start].source_root.as_str();
            if !store.catalog_inventory_generation_is_unpublished(
                CaptureProvider::Codex,
                source_root,
                observation_generation,
            )? {
                return Err(CaptureError::InventorySuperseded);
            }
            if store.catalog_sessions_have_external_path_owners_paced(
                CaptureProvider::Codex,
                source_root,
                &sessions[start..start + end],
                crate::pace_current_disk_io,
            )? {
                return Ok(false);
            }
            store.upsert_catalog_sessions(observation_generation, &sessions[start..start + end])?;
            Ok(true)
        })();
        match persist {
            Ok(true) => store.commit_batch()?,
            Ok(false) => {
                store.rollback_batch()?;
                return Ok(false);
            }
            Err(err) => {
                let _ = store.rollback_batch();
                return Err(err);
            }
        }
        start += end;
        batch_committed(start);
    }
    Ok(true)
}

#[cfg(test)]
pub(crate) fn persist_initial_catalog_sessions_bounded_for_test(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
    batch_committed: impl FnMut(usize),
) -> Result<bool> {
    persist_initial_catalog_sessions_bounded_with_observer(
        store,
        observation_generation,
        sessions,
        batch_committed,
    )
}

#[cfg(test)]
pub(crate) fn persist_catalog_sessions_for_publication_for_test(
    store: &Store,
    observation_generation: u64,
    sessions: &[CatalogSession],
    staged: bool,
) -> Result<()> {
    persist_catalog_sessions_for_publication(store, observation_generation, sessions, staged)
}

fn catalog_persist_batch(sessions: &[CatalogSession], byte_limit: usize) -> Result<(usize, u64)> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for session in sessions.iter().take(CATALOG_PERSIST_BATCH_SESSIONS) {
        let session_bytes = catalog_session_persist_bytes(session)?;
        if count > 0 && bytes.saturating_add(session_bytes) > byte_limit as u64 {
            break;
        }
        count += 1;
        bytes = bytes.saturating_add(session_bytes);
        if bytes >= byte_limit as u64 {
            break;
        }
    }
    Ok((count, bytes))
}

fn catalog_session_persist_bytes(session: &CatalogSession) -> Result<u64> {
    let mut bytes = CATALOG_PERSIST_ROW_OVERHEAD_BYTES;
    for value in [
        Some(session.source_format.as_str()),
        Some(session.source_root.as_str()),
        Some(session.source_path.as_str()),
        session.external_session_id.as_deref(),
        session.parent_external_session_id.as_deref(),
        session.role_hint.as_deref(),
        session.external_agent_id.as_deref(),
        session.cwd.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        bytes = bytes.saturating_add(value.len() as u64);
    }
    bytes = bytes.saturating_add(serde_json::to_vec(&session.metadata)?.len() as u64);
    Ok(bytes)
}

#[cfg(test)]
pub(crate) fn catalog_session_persist_bytes_for_test(session: &CatalogSession) -> Result<u64> {
    catalog_session_persist_bytes(session)
}

pub(crate) fn cached_catalog_session_if_unchanged(
    session: Option<&CatalogSession>,
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
    import_revision: u32,
) -> Option<CatalogSession> {
    let session = session?;
    let modified_at_ms = system_time_ms(observation.modified_at());
    let observation_token = observation.token_hex();
    if session.provider == CaptureProvider::Codex
        && session.source_format == CODEX_SESSION_SOURCE_FORMAT
        && session.import_revision == import_revision
        && session.file_size_bytes == observation.len()
        && session.file_modified_at_ms == modified_at_ms
        && session
            .metadata
            .get("file_observation_token_v1")
            .and_then(Value::as_str)
            == Some(observation_token.as_str())
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
}
pub(crate) fn catalog_codex_session_paths(
    paths: Vec<PathBuf>,
    source_root: &str,
    cataloged_at_ms: i64,
    requested_parallelism: Option<usize>,
) -> Result<(CatalogSummary, Vec<CatalogSession>)> {
    validate_codex_catalog_session_paths(&paths)?;
    let parallelism = catalog_parallelism(paths.len(), requested_parallelism);
    let batches = if parallelism <= 1 {
        vec![catalog_codex_session_chunk(
            paths,
            source_root.to_owned(),
            cataloged_at_ms,
        )]
    } else {
        let chunk_size = paths.len().div_ceil(parallelism).max(1);
        let disk_io_pacer = crate::disk_io_pacing::current_disk_io_pacer();
        thread::scope(|scope| -> Result<Vec<CatalogWorkerBatch>> {
            let mut handles = Vec::new();
            for chunk in paths.chunks(chunk_size) {
                let chunk = chunk.to_vec();
                let source_root = source_root.to_owned();
                let disk_io_pacer = disk_io_pacer.clone();
                handles.push(scope.spawn(move || {
                    let _disk_io_pacing =
                        disk_io_pacer.map(crate::disk_io_pacing::install_disk_io_pacer);
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
        summary.merge_from(batch.summary);
        sessions.append(&mut batch.sessions);
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
        let (file, observation) = match open_observed_ordinary_file(&path) {
            Ok(observed) => observed,
            Err(err) => {
                batch.summary.failed_sessions += 1;
                batch.summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {err}", path.display()),
                });
                continue;
            }
        };
        batch.summary.source_files += 1;
        batch.summary.source_bytes = batch.summary.source_bytes.saturating_add(observation.len());
        match catalog_codex_session_file(
            &path,
            file,
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
                batch.summary.sample_failure(ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {err}", path.display()),
                });
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
    let pacing_limit = crate::disk_io_pacing::current_disk_io_pacer()
        .map(|pacer| {
            if pacer.bytes_per_second() <= 8 * 1024 * 1024 {
                QUIET_CATALOG_MAX_PARALLELISM
            } else if pacer.bytes_per_second() <= 32 * 1024 * 1024 {
                INTERACTIVE_CATALOG_MAX_PARALLELISM
            } else {
                32
            }
        })
        .unwrap_or(32);
    requested_parallelism
        .or_else(|| thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .clamp(1, 32)
        .min(pacing_limit)
        .min(path_count)
}
pub(crate) fn catalog_codex_session_file(
    path: &Path,
    file: File,
    source_root: &str,
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
) -> Result<CatalogSession> {
    let source_path = codex_catalog_session_identity(path)?;
    let session_meta = read_codex_session_meta_from_file(file)?;
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
        source_path: source_path.to_owned(),
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
        import_revision: provider_import_revision(
            CaptureProvider::Codex,
            CODEX_SESSION_SOURCE_FORMAT,
        ),
        cataloged_at_ms,
        metadata: json!({
            "originator": payload.and_then(|payload| payload.get("originator")).and_then(Value::as_str),
            "cli_version": payload.and_then(|payload| payload.get("cli_version")).and_then(Value::as_str),
            "model_provider": payload.and_then(|payload| payload.get("model_provider")).and_then(Value::as_str),
            "source_kind": codex_source_kind(&source),
            "source": source,
            "catalog_scope": "session_meta",
            "file_observation_token_v1": observation.token_hex(),
        }),
    })
}

fn codex_catalog_root_identity(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Codex catalog source root is not valid UTF-8",
        })
}

fn codex_catalog_session_identity(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Codex catalog session path is not valid UTF-8",
        })
}

fn validate_codex_catalog_session_paths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        codex_catalog_session_identity(path)?;
    }
    Ok(())
}

fn read_codex_session_meta_from_file(file: File) -> Result<Option<Value>> {
    let mut reader = BufReader::new(crate::disk_io_pacing::PacedReader::new(file));
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
