use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use ctx_history_core::{AgentType, CaptureProvider};
use ctx_history_store::{
    CatalogSession, ImportInventoryCanonicalEffect, Store,
    IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES,
};
use serde_json::{json, Value};

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
const DURABLE_CATALOG_MAX_SOURCE_READ_BYTES: u64 = 16 * 1024 * 1024;
const DURABLE_CATALOG_MAX_SOURCE_READ_BYTES_PER_ROW: u64 = 2 * 1024 * 1024;
const DURABLE_CATALOG_MAX_RETAINED_BYTES: u64 = IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64;
const DURABLE_CATALOG_MAX_SERIALIZED_BYTES: u64 = IMPORT_INVENTORY_CHECKPOINT_MAX_PAGE_BYTES as u64;
const DURABLE_CATALOG_MAX_ELAPSED: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub enum CodexCatalogObservationOutcome {
    Cataloged(Box<CatalogSession>),
    Failed(ProviderImportFailure),
}

#[derive(Debug, Clone)]
pub struct CodexCatalogJournalObservation {
    pub journal: DurableSourceInventoryJournalEntry,
    pub source_files: u64,
    pub source_bytes: u64,
    pub source_read_bytes: u64,
    pub retained_bytes: u64,
    pub serialized_bytes: u64,
    pub outcome: CodexCatalogObservationOutcome,
}

impl CodexCatalogJournalObservation {
    pub fn membership_accounted_bytes(&self) -> u64 {
        self.serialized_bytes
    }

    pub fn canonical_effect(&self) -> Result<ImportInventoryCanonicalEffect<'_>> {
        Ok(match &self.outcome {
            CodexCatalogObservationOutcome::Cataloged(session) => {
                ImportInventoryCanonicalEffect::CatalogUpsert(session.as_ref())
            }
            CodexCatalogObservationOutcome::Failed(_) => {
                ImportInventoryCanonicalEffect::CatalogObservationRejected {
                    source_path: codex_catalog_session_identity(&self.journal.path)?,
                }
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexCatalogObservationStopReason {
    Complete,
    SourceReadBudget,
    RetainedBudget,
    SerializedBudget,
    Elapsed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexCatalogObservationUsage {
    pub rows: u64,
    pub source_read_bytes: u64,
    pub retained_bytes: u64,
    pub serialized_bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct CodexCatalogObservationRequest<'a> {
    pub entries: &'a [DurableSourceInventoryJournalEntry],
    pub after_journal_identity: Option<[u8; 32]>,
    pub source_root: &'a str,
    pub cataloged_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct CodexCatalogObservationPage {
    pub observations: Vec<CodexCatalogJournalObservation>,
    pub summary: CatalogSummary,
    pub next_keyset: Option<[u8; 32]>,
    pub complete: bool,
    pub stop_reason: CodexCatalogObservationStopReason,
    pub usage: CodexCatalogObservationUsage,
}

pub fn observe_codex_catalog_journal_page(
    request: CodexCatalogObservationRequest<'_>,
) -> Result<CodexCatalogObservationPage> {
    if request.entries.len() > DURABLE_CATALOG_OBSERVATION_PAGE {
        return Err(CaptureError::SystemInvariant(
            "Codex durable catalog observation page exceeds its row bound",
        ));
    }
    validate_codex_catalog_session_paths(
        &request
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
    )?;
    let start_index = match request.after_journal_identity {
        Some(keyset) => request
            .entries
            .iter()
            .position(|entry| entry.journal_identity == keyset)
            .map(|index| index.saturating_add(1))
            .ok_or_else(|| {
                CaptureError::SystemInvariant(
                    "Codex durable catalog observation keyset is not in the journal page",
                )
            })?,
        None => 0,
    };
    let started = Instant::now();
    let mut page = CodexCatalogObservationPage {
        observations: Vec::with_capacity(request.entries.len().saturating_sub(start_index)),
        summary: CatalogSummary::default(),
        next_keyset: request.after_journal_identity,
        complete: false,
        stop_reason: CodexCatalogObservationStopReason::Complete,
        usage: CodexCatalogObservationUsage::default(),
    };
    for journal in &request.entries[start_index..] {
        if !page.observations.is_empty() && started.elapsed() >= DURABLE_CATALOG_MAX_ELAPSED {
            page.stop_reason = CodexCatalogObservationStopReason::Elapsed;
            break;
        }
        let remaining_source_bytes =
            DURABLE_CATALOG_MAX_SOURCE_READ_BYTES.saturating_sub(page.usage.source_read_bytes);
        if remaining_source_bytes == 0 {
            page.stop_reason = CodexCatalogObservationStopReason::SourceReadBudget;
            break;
        }
        let (file, observation) = match open_observed_ordinary_file(&journal.path) {
            Ok(observed) => observed,
            Err(error) => {
                let failure = ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {error}", journal.path.display()),
                };
                let outcome = CodexCatalogObservationOutcome::Failed(failure);
                let retained = retain_codex_catalog_observation(
                    &mut page,
                    CodexCatalogJournalObservation {
                        journal: journal.clone(),
                        source_files: 0,
                        source_bytes: 0,
                        source_read_bytes: 0,
                        retained_bytes: 0,
                        serialized_bytes: 0,
                        outcome,
                    },
                    started,
                )?;
                if !retained {
                    break;
                }
                continue;
            }
        };
        let source_bytes = observation.len();
        let read_limit = remaining_source_bytes.min(DURABLE_CATALOG_MAX_SOURCE_READ_BYTES_PER_ROW);
        let parsed = catalog_codex_session_file_with_limit(
            &journal.path,
            file,
            request.source_root,
            &observation,
            request.cataloged_at_ms,
            read_limit,
        );
        let (outcome, source_read_bytes) = match parsed {
            Ok((session, source_read_bytes)) => (
                CodexCatalogObservationOutcome::Cataloged(Box::new(session)),
                source_read_bytes,
            ),
            Err((error, source_read_bytes)) => {
                let failure = ProviderImportFailure {
                    line: 0,
                    error: format!("{}: {error}", journal.path.display()),
                };
                (
                    CodexCatalogObservationOutcome::Failed(failure),
                    source_read_bytes,
                )
            }
        };
        let observation = CodexCatalogJournalObservation {
            journal: journal.clone(),
            source_files: 1,
            source_bytes,
            source_read_bytes,
            retained_bytes: 0,
            serialized_bytes: 0,
            outcome,
        };
        if !retain_codex_catalog_observation(&mut page, observation, started)? {
            break;
        }
    }
    page.complete = start_index.saturating_add(page.observations.len()) == request.entries.len();
    if page.complete {
        page.stop_reason = CodexCatalogObservationStopReason::Complete;
    }
    page.usage.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(page)
}

fn retain_codex_catalog_observation(
    page: &mut CodexCatalogObservationPage,
    mut observation: CodexCatalogJournalObservation,
    started: Instant,
) -> Result<bool> {
    page.usage.source_read_bytes = page
        .usage
        .source_read_bytes
        .saturating_add(observation.source_read_bytes);
    let mut retained_bytes = codex_catalog_observation_retained_bytes(&observation)?;
    let mut serialized_bytes = codex_catalog_observation_serialized_bytes(&observation)?;
    if retained_bytes > DURABLE_CATALOG_MAX_RETAINED_BYTES
        || serialized_bytes > DURABLE_CATALOG_MAX_SERIALIZED_BYTES
    {
        observation.outcome = CodexCatalogObservationOutcome::Failed(ProviderImportFailure {
            line: 0,
            error: "Codex catalog metadata exceeds the bounded observation payload".to_owned(),
        });
        retained_bytes = codex_catalog_observation_retained_bytes(&observation)?;
        serialized_bytes = codex_catalog_observation_serialized_bytes(&observation)?;
        if retained_bytes > DURABLE_CATALOG_MAX_RETAINED_BYTES
            || serialized_bytes > DURABLE_CATALOG_MAX_SERIALIZED_BYTES
        {
            return Err(CaptureError::InvalidPayload(
                "Codex catalog observation identity exceeds its payload bound".to_owned(),
            ));
        }
    }
    if !page.observations.is_empty()
        && page.usage.retained_bytes.saturating_add(retained_bytes)
            > DURABLE_CATALOG_MAX_RETAINED_BYTES
    {
        page.stop_reason = CodexCatalogObservationStopReason::RetainedBudget;
        return Ok(false);
    }
    if !page.observations.is_empty()
        && page.usage.serialized_bytes.saturating_add(serialized_bytes)
            > DURABLE_CATALOG_MAX_SERIALIZED_BYTES
    {
        page.stop_reason = CodexCatalogObservationStopReason::SerializedBudget;
        return Ok(false);
    }
    page.usage.retained_bytes = page.usage.retained_bytes.saturating_add(retained_bytes);
    page.usage.serialized_bytes = page.usage.serialized_bytes.saturating_add(serialized_bytes);
    observation.retained_bytes = retained_bytes;
    observation.serialized_bytes = serialized_bytes;
    let journal_identity = observation.journal.journal_identity;
    page.observations.push(observation);
    finish_codex_catalog_observation(page, journal_identity, started)?;
    Ok(true)
}

fn finish_codex_catalog_observation(
    page: &mut CodexCatalogObservationPage,
    journal_identity: [u8; 32],
    started: Instant,
) -> Result<()> {
    let observation = page
        .observations
        .last()
        .ok_or(CaptureError::SystemInvariant(
            "Codex catalog observation accounting has no retained row",
        ))?;
    page.summary.source_files = page
        .summary
        .source_files
        .saturating_add(observation.source_files);
    page.summary.source_bytes = page
        .summary
        .source_bytes
        .saturating_add(observation.source_bytes);
    match &observation.outcome {
        CodexCatalogObservationOutcome::Cataloged(_) => {
            page.summary.parsed_sessions = page.summary.parsed_sessions.saturating_add(1);
            page.summary.cataloged_sessions = page.summary.cataloged_sessions.saturating_add(1);
        }
        CodexCatalogObservationOutcome::Failed(failure) => {
            page.summary.failed_sessions = page.summary.failed_sessions.saturating_add(1);
            page.summary.sample_failure(failure.clone());
        }
    }
    page.usage.rows = page.usage.rows.saturating_add(1);
    page.usage.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    page.next_keyset = Some(journal_identity);
    Ok(())
}

fn codex_catalog_observation_retained_bytes(
    observation: &CodexCatalogJournalObservation,
) -> Result<u64> {
    let payload_bytes = match &observation.outcome {
        CodexCatalogObservationOutcome::Cataloged(session) => {
            catalog_session_persist_bytes(session)?
        }
        CodexCatalogObservationOutcome::Failed(failure) => {
            u64::try_from(failure.error.len()).unwrap_or(u64::MAX)
        }
    };
    Ok(payload_bytes
        .saturating_add(
            u64::try_from(observation.journal.path.as_os_str().len()).unwrap_or(u64::MAX),
        )
        .saturating_add(512))
}

fn codex_catalog_observation_serialized_bytes(
    observation: &CodexCatalogJournalObservation,
) -> Result<u64> {
    let mut bytes = 512_u64;
    bytes = bytes.saturating_add(
        u64::try_from(serde_json::to_vec(&observation.journal.path.to_string_lossy())?.len())
            .unwrap_or(u64::MAX),
    );
    match &observation.outcome {
        CodexCatalogObservationOutcome::Cataloged(session) => {
            for value in [
                Some(session.provider.as_str()),
                Some(session.source_format.as_str()),
                Some(session.source_root.as_str()),
                Some(session.source_path.as_str()),
                session.external_session_id.as_deref(),
                session.parent_external_session_id.as_deref(),
                Some(session.agent_type.as_str()),
                session.role_hint.as_deref(),
                session.external_agent_id.as_deref(),
                session.cwd.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                bytes = bytes.saturating_add(
                    u64::try_from(serde_json::to_vec(value)?.len()).unwrap_or(u64::MAX),
                );
            }
            bytes = bytes.saturating_add(
                u64::try_from(serde_json::to_vec(&session.metadata)?.len()).unwrap_or(u64::MAX),
            );
        }
        CodexCatalogObservationOutcome::Failed(failure) => {
            bytes = bytes.saturating_add(
                u64::try_from(serde_json::to_vec(&failure.error)?.len()).unwrap_or(u64::MAX),
            );
        }
    }
    Ok(bytes)
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
    catalog_codex_session_file_with_limit(
        path,
        file,
        source_root,
        observation,
        cataloged_at_ms,
        DURABLE_CATALOG_MAX_SOURCE_READ_BYTES,
    )
    .map(|(session, _)| session)
    .map_err(|(error, _)| error)
}

fn catalog_codex_session_file_with_limit(
    path: &Path,
    file: File,
    source_root: &str,
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
    read_limit: u64,
) -> std::result::Result<(CatalogSession, u64), (CaptureError, u64)> {
    let (session_meta, source_read_bytes) = read_codex_session_meta_from_file(file, read_limit)?;
    catalog_codex_session_from_meta(
        path,
        source_root,
        observation,
        cataloged_at_ms,
        session_meta,
    )
    .map(|session| (session, source_read_bytes))
    .map_err(|error| (error, source_read_bytes))
}

fn catalog_codex_session_from_meta(
    path: &Path,
    source_root: &str,
    observation: &OrdinaryFileObservation,
    cataloged_at_ms: i64,
    session_meta: Option<Value>,
) -> Result<CatalogSession> {
    let source_path = codex_catalog_session_identity(path)?;
    let payload = session_meta.as_ref().and_then(|value| value.get("payload"));
    let source = payload.and_then(|payload| payload.get("source"));
    let parent_external_session_id = source.and_then(codex_parent_session_id);
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
            "source_kind": source.and_then(codex_source_kind),
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

fn read_codex_session_meta_from_file(
    file: File,
    read_limit: u64,
) -> std::result::Result<(Option<Value>, u64), (CaptureError, u64)> {
    let mut reader = BufReader::new(BoundedCatalogReader::new(file, read_limit));
    let mut line = Vec::new();
    for _ in 0..32 {
        let line_read = read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)
            .map_err(|error| (error, reader.get_ref().bytes_read()))?;
        match line_read {
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
            return Ok((Some(value), reader.get_ref().bytes_read()));
        }
    }
    if reader.get_ref().exhausted() {
        return Err((
            CaptureError::InvalidPayload(
                "Codex session metadata was not found within the bounded source prefix".to_owned(),
            ),
            reader.get_ref().bytes_read(),
        ));
    }
    Ok((None, reader.get_ref().bytes_read()))
}

struct BoundedCatalogReader {
    file: File,
    remaining: u64,
    bytes_read: u64,
}

impl BoundedCatalogReader {
    fn new(file: File, read_limit: u64) -> Self {
        Self {
            file,
            remaining: read_limit,
            bytes_read: 0,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    fn exhausted(&self) -> bool {
        self.remaining == 0
    }
}

impl Read for BoundedCatalogReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let bounded = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        crate::pace_current_disk_io(u64::try_from(bounded).unwrap_or(u64::MAX));
        let read = self.file.read(&mut buffer[..bounded])?;
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        self.remaining = self.remaining.saturating_sub(read);
        self.bytes_read = self.bytes_read.saturating_add(read);
        usize::try_from(read).map_err(|_| io::Error::other("bounded Codex read overflow"))
    }
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
