use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event, EventRole,
    EventType, Fidelity, FileTouched, Run, RunStatus, RunType, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps,
        },
        native_ingestion::{NativeIngestionPage, NativePublicationPage, NativeSourceIdentity},
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ClineTaskJsonImportOptions, ImportProfile,
    OutputOutcome, ProviderImportSummary, ProviderImportWorkResult, Result,
    RooTaskJsonImportOptions,
};

use super::store_adapter::{
    ClineNativePageAdapter, ClineNativePageAdapterError, ClineNativeStoreCursor,
};
use super::{
    ClineArrayCheckpoint, ClineCatalogCompletion, ClineCatalogIndex, ClineCertifiedPage,
    ClineComponent, ClineComponentObservation, ClineDiscovery, ClineEventComponent, ClineEventKind,
    ClineEventRole, ClineEventRow, ClineLiveTaskObservation, ClineMetadataCheckpoint,
    ClineNativePathError, ClineNativeProfile, ClineNativeReader, ClineObservedFileState,
    ClineSessionRow, ClineTaskCheckpoint, ClineTaskIdentity, ClineTaskIdentityOrigin,
    TaskJsonNativeDialect,
};

const CLINE_TASK_CURSOR_VERSION: u32 = 1;
const TASK_JSON_GENERATION_EVENT_STRIDE: u64 = 1 << 48;

#[derive(Debug, Error)]
pub(crate) enum ClineNativeVerticalError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Store(#[from] ctx_history_store::StoreError),
    #[error(transparent)]
    Adapter(#[from] ClineNativePageAdapterError),
    #[error(transparent)]
    Source(#[from] ClineNativePathError),
    #[error("Cline NativePath page source identity does not match its certified payload")]
    SourceIdentityMismatch,
    #[error("Cline NativePath component cursor is malformed or inconsistent")]
    CorruptCursor,
    #[error("Cline NativePath component generation is exhausted")]
    GenerationExhausted,
    #[error("Cline NativePath page has retained events but no certified task session")]
    MissingSession,
    #[error("Cline NativePath event index exceeds the canonical packed-index range")]
    EventIndexOverflow,
    #[error("Cline NativePath source changed before Core publication")]
    SourceChanged,
}

struct ClineFreshPublicationContext<'a> {
    options: &'a TaskJsonNativeImportOptions,
    configured_source_root: &'a Path,
    dialect: TaskJsonNativeDialect,
}

struct TaskJsonNativeImportOptions {
    machine_id: String,
    source_path: Option<PathBuf>,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    capture_work_limit: CaptureWorkLimit,
    import_profile: ImportProfile,
}

impl From<ClineTaskJsonImportOptions> for TaskJsonNativeImportOptions {
    fn from(options: ClineTaskJsonImportOptions) -> Self {
        Self {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            import_profile: options.import_profile,
        }
    }
}

impl From<RooTaskJsonImportOptions> for TaskJsonNativeImportOptions {
    fn from(options: RooTaskJsonImportOptions) -> Self {
        Self {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            import_profile: options.import_profile,
        }
    }
}

struct ResolvedClineSource {
    source_id: Uuid,
    session: Session,
}

// Publication classification carries the full atomic cursor transition without heap indirection.
#[allow(clippy::large_enum_variant)]
enum ComponentCursorPlan {
    AlreadyCommitted,
    Publish {
        transition: NativePathCursorTransition,
        generation: u64,
        rejected_records: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClinePersistedObservation {
    component: u8,
    path: PathBuf,
    stamp_token: Option<String>,
    missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineArrayCheckpointWire {
    component: u8,
    observation: ClinePersistedObservation,
    certified_revision_sha256: [u8; 32],
    complete_bytes: u64,
    observed_items: u64,
    retained_rows: u64,
    final_frontier: super::ClinePageFrontier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineSessionRowWire {
    identity: String,
    identity_origin: u8,
    title: Option<String>,
    workspace_directory: Option<String>,
    created_at: Option<String>,
    last_modified: Option<String>,
    model_id: Option<String>,
    model_provider: Option<String>,
    tokens_input: Option<u64>,
    tokens_output: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineMetadataCheckpointWire {
    observation: ClinePersistedObservation,
    content_sha256: Option<[u8; 32]>,
    session: ClineSessionRowWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineTaskCheckpointWire {
    version: u32,
    canonical_task_path: PathBuf,
    api_history: Option<ClineArrayCheckpointWire>,
    ui_messages: Option<ClineArrayCheckpointWire>,
    fallback_history: Option<ClineArrayCheckpointWire>,
    task_metadata: ClineMetadataCheckpointWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineRootManifestWire {
    version: u32,
    tasks_root: PathBuf,
    source_root: String,
    task_names: Vec<String>,
}

pub(crate) fn import_cline_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: ClineTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_task_json_nativepath_history(path, store, options.into(), TaskJsonNativeDialect::CLINE)
}

pub(crate) fn import_roo_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_task_json_nativepath_history(path, store, options.into(), TaskJsonNativeDialect::ROO)
}

fn import_task_json_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
) -> Result<ProviderImportSummary> {
    let configured_source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let discovery = if dialect == TaskJsonNativeDialect::CLINE {
        super::discover_cline_root(path)
    } else {
        super::discover_roo_root(path)
    }
    .map_err(map_source_error)?;
    let committed_store = Store::open_read_only(store.path())?;
    let tasks_root = discovery.root_authority().tasks_root().to_path_buf();
    let prior_manifest =
        load_cline_root_manifest(&committed_store, &options.machine_id, &tasks_root, dialect)?;
    let current_task_names = cline_task_names(&discovery)?;
    if current_task_names.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: if dialect == TaskJsonNativeDialect::CLINE {
                "Cline task history root contains no task directories"
            } else {
                "Roo Code task history root contains no task directories"
            },
        });
    }
    let mut previous = Vec::new();
    if !matches!(&options.import_profile, ImportProfile::ProReplayOnly(_)) {
        for route in discovery.task_routes() {
            if let Some(checkpoint) =
                load_cline_task_checkpoint(&committed_store, &options.machine_id, route)?
            {
                previous.push(checkpoint);
            }
        }
        if let Some(manifest) = &prior_manifest {
            let current = current_task_names.iter().collect::<BTreeSet<_>>();
            for task_name in &manifest.task_names {
                if current.contains(task_name) {
                    continue;
                }
                let task_path = manifest.tasks_root.join(task_name);
                let checkpoint = load_cline_task_checkpoint_by_path(
                    &committed_store,
                    &options.machine_id,
                    &task_path,
                    dialect,
                )
                .map_err(map_vertical_error)?
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Cline root manifest references a missing task checkpoint".to_owned(),
                    )
                })?;
                previous.push(checkpoint);
            }
        }
    }
    let native_profile = match &options.import_profile {
        ImportProfile::CoreOnly => ClineNativeProfile::CoreOnly,
        ImportProfile::CoreAndPro(_) | ImportProfile::ProReplayOnly(_) => {
            ClineNativeProfile::CoreAndPro
        }
    };
    let replay_only = matches!(&options.import_profile, ImportProfile::ProReplayOnly(_));
    let mut reader = ClineNativeReader::new(discovery, &previous, native_profile);
    let mut adapter = ClineNativePageAdapter::new(dialect.provider, &options.import_profile);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut summary = ProviderImportSummary::default();
    let operation = (|| {
        let mut changed_groups = 0_usize;
        while let Some(page) = reader.next_page().map_err(map_source_error)? {
            let adapted = adapter
                .adapt(page)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            if replay_only {
                verify_cline_core_page_committed(store, &options, dialect, adapted.core)?;
            } else {
                let core = publish_task_json_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &configured_source_root,
                    &options,
                    dialect,
                    adapted.core,
                )?;
                if core.work_result() == ProviderImportWorkResult::Changed {
                    changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(core);
            }
            if let Some(output) = adapted.output {
                let sink = options
                    .import_profile
                    .sink()
                    .ok_or(CaptureError::SystemInvariant(
                        "task JSON NativePath output page has no output sink",
                    ))?;
                let _ = crate::provider::native_ingestion::process_pro_replay_only(
                    output,
                    sink.as_ref(),
                );
            }
            if !replay_only
                && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }
        let completion = reader.finish_catalog().map_err(map_source_error)?;
        record_catalog_failures(&completion, &mut summary);
        if !replay_only {
            for checkpoint in &completion.live_checkpoints {
                summary.merge_from(publish_task_json_task_checkpoint(
                    store,
                    &bulk_guard,
                    &options,
                    dialect,
                    checkpoint,
                )?);
            }
            let retirement_source_root = prior_manifest.as_ref().map_or_else(
                || configured_source_root.display().to_string(),
                |manifest| manifest.source_root.clone(),
            );
            for missing_path in &completion.missing_task_paths {
                let checkpoint = previous
                    .iter()
                    .find(|checkpoint| &checkpoint.canonical_task_path == missing_path)
                    .ok_or(CaptureError::SystemInvariant(
                        "Cline catalog retirement lost its prior task checkpoint",
                    ))?;
                summary.merge_from(retire_cline_task_routes(
                    store,
                    &bulk_guard,
                    &options,
                    dialect,
                    &retirement_source_root,
                    checkpoint,
                )?);
            }
            summary.merge_from(publish_cline_root_manifest(
                store,
                &bulk_guard,
                &options,
                dialect,
                &tasks_root,
                &configured_source_root,
                &current_task_names,
            )?);
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn verify_cline_core_page_committed(
    store: &Store,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> Result<()> {
    let (source_identity, page) = publication_page.into_parts();
    validate_source_identity(dialect, &source_identity, &page).map_err(map_vertical_error)?;
    revalidate_page_source(&page).map_err(map_vertical_error)?;
    let stream = component_cursor_stream(dialect, &page.core.source.canonical_path)
        .map_err(map_vertical_error)?;
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "{} output replay requires committed NativePath Core",
                dialect.display_name
            ))
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = ClineNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let source_revision = revision(&page.core.source_revision.revision_sha256);
    if prior.version != ClineNativeStoreCursor::VERSION
        || prior.provider != dialect.provider.as_str()
        || prior.source_identity != page.core.source.stable_id.as_ref()
        || prior.source_revision != source_revision
        || prior.frontier.next_native_index < page.core.next_safe_frontier.next_native_index
        || (prior.frontier.next_native_index == page.core.next_safe_frontier.next_native_index
            && prior.frontier != page.core.next_safe_frontier)
    {
        return Err(CaptureError::InvalidPayload(format!(
            "{} output replay source no longer matches committed Core authority",
            dialect.display_name
        )));
    }
    Ok(())
}

fn record_catalog_failures(
    completion: &ClineCatalogCompletion,
    summary: &mut ProviderImportSummary,
) {
    let rejection = match &completion.root_index {
        ClineCatalogIndex::Incomplete(rejection)
        | ClineCatalogIndex::Malformed(rejection)
        | ClineCatalogIndex::Unavailable(rejection) => Some(rejection),
        ClineCatalogIndex::Missing | ClineCatalogIndex::Parsed { .. } => None,
    };
    if let Some(rejection) = rejection {
        summary.record_failure(crate::ProviderImportFailure {
            line: 0,
            error: format!("{}: {}", rejection.path.display(), rejection.message),
        });
    }
    for failure in completion
        .component_outcomes
        .iter()
        .filter_map(|outcome| outcome.failure.as_ref())
    {
        summary.record_failure(crate::ProviderImportFailure {
            line: 0,
            error: format!("{}: {}", failure.path.display(), failure.message),
        });
    }
    for checkpoint in &completion.live_checkpoints {
        let retained_rows = [
            checkpoint.api_history.as_ref(),
            checkpoint.ui_messages.as_ref(),
            checkpoint.fallback_history.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(|component| component.retained_rows)
        .sum::<u64>();
        let has_component_failure = completion.component_outcomes.iter().any(|outcome| {
            outcome.failure.is_some() && outcome.path.starts_with(&checkpoint.canonical_task_path)
        });
        if retained_rows == 0 && !has_component_failure {
            summary.record_failure(crate::ProviderImportFailure {
                line: 0,
                error: format!(
                    "{}: provider source contained no real conversation message",
                    checkpoint.canonical_task_path.display()
                ),
            });
        }
    }
}

fn cline_task_names(discovery: &ClineDiscovery) -> std::result::Result<Vec<String>, CaptureError> {
    let mut names = discovery
        .task_routes()
        .iter()
        .map(|task| {
            task.canonical_task_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or(CaptureError::SystemInvariant(
                    "Cline task route has no UTF-8 direct-child name",
                ))
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    Ok(names)
}

fn load_cline_root_manifest(
    store: &Store,
    machine_id: &str,
    tasks_root: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<Option<ClineRootManifestWire>> {
    let stream = root_cursor_stream(dialect, tasks_root).map_err(map_vertical_error)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: ClineRootManifestWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if manifest.version != CLINE_TASK_CURSOR_VERSION || manifest.tasks_root != tasks_root {
        return Err(CaptureError::InvalidPayload(
            "Cline NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

fn publish_cline_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    tasks_root: &Path,
    configured_source_root: &Path,
    task_names: &[String],
) -> Result<ProviderImportSummary> {
    let stream = root_cursor_stream(dialect, tasks_root).map_err(map_vertical_error)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let wire = ClineRootManifestWire {
        version: CLINE_TASK_CURSOR_VERSION,
        tasks_root: tasks_root.to_path_buf(),
        source_root: configured_source_root.display().to_string(),
        task_names: task_names.to_vec(),
    };
    let encoded = serde_json::to_string(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if encoded.len() > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Cline NativePath root manifest exceeds the bounded Store page".to_owned(),
        ));
    }
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = root_manifest_publication_id(dialect, &wire, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len())?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn retire_cline_task_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    source_root: &str,
    checkpoint: &ClineTaskCheckpoint,
) -> Result<ProviderImportSummary> {
    let task_path = &checkpoint.canonical_task_path;
    let stream = task_cursor_stream(dialect, task_path).map_err(map_vertical_error)?;
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "task JSON route retirement requires its committed task cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let locator_identity = provider_path_identity(task_path)?;
    let raw_source_path = task_path.display().to_string();
    let canonical_source_identity = provider_source_identity(
        dialect.provider,
        dialect.source_format,
        Some(source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cline route retirement has no canonical source identity",
    ))?;
    let retirement = ProviderSourceRouteRetirement {
        provider: dialect.provider,
        source_format: dialect.source_format.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: canonical_source_identity,
        expected_source_revision: task_route_revision(dialect, checkpoint.identity.as_str()),
        retired_at_ms: options.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    let publication_id = retirement_publication_id(dialect, &retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream,
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let accounting = NativePathGroupAccounting::new(0, 1, 0)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

fn root_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.root_cursor_stream_format,
        &identity,
    ))
}

fn root_manifest_publication_id(
    dialect: TaskJsonNativeDialect,
    wire: &ClineRootManifestWire,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.root_publication_domain);
    digest.update(wire.tasks_root.as_os_str().as_encoded_bytes());
    digest.update(wire.source_root.as_bytes());
    for task_name in &wire.task_names {
        digest.update((task_name.len() as u64).to_le_bytes());
        digest.update(task_name.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.root_publication_prefix,
        hex(&digest.finalize())
    )
}

fn retirement_publication_id(
    dialect: TaskJsonNativeDialect,
    retirement: &ProviderSourceRouteRetirement,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.retirement_publication_domain);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!(
        "{}{}",
        dialect.retirement_publication_prefix,
        hex(&digest.finalize())
    )
}

fn publish_task_json_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> Result<ProviderImportSummary> {
    publish_cline_core_page_inner(
        store,
        committed_store,
        bulk_guard,
        &ClineFreshPublicationContext {
            options,
            configured_source_root,
            dialect,
        },
        publication_page,
    )
    .map_err(map_vertical_error)
}

fn publish_cline_core_page_inner(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ClineFreshPublicationContext<'_>,
    publication_page: NativePublicationPage<ClineCertifiedPage>,
) -> std::result::Result<ProviderImportSummary, ClineNativeVerticalError> {
    let (source_identity, page) = publication_page.into_parts();
    validate_source_identity(context.dialect, &source_identity, &page)?;
    revalidate_page_source(&page)?;

    let stream = component_cursor_stream(context.dialect, &page.core.source.canonical_path)?;
    let stored = store.get_sync_cursor(None, &context.options.machine_id, &stream)?;
    let plan = classify_component_cursor(stored.as_ref(), context, &stream, &page)?;
    let ComponentCursorPlan::Publish {
        transition,
        generation,
        rejected_records,
    } = plan
    else {
        let mut summary = ProviderImportSummary::default();
        summary.skipped_events = page.core.core.events.len();
        summary.skipped = summary.skipped.saturating_add(summary.skipped_events);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    };
    let next = component_sync_cursor(context, &stream, &page, generation, rejected_records)?;
    let transition =
        NativePathCursorTransition::new(transition.expected_cursor().map(str::to_owned), next);
    let publication_id = page_publication_id(context.dialect, &source_identity, &page, &transition);
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_fresh_source(
        committed_store,
        &mut group,
        context,
        &page.core,
        &mut summary,
    )?;
    publish_page_events(
        committed_store,
        &mut group,
        context,
        &resolved,
        generation,
        &page.core.core.events,
        &mut summary,
    )?;
    for rejection in &page.core.core.rejections {
        summary.record_failure(crate::ProviderImportFailure {
            line: usize::try_from(rejection.native_index)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.detail.to_string(),
        });
    }

    revalidate_page_source(&page)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn classify_component_cursor(
    stored: Option<&SyncCursor>,
    context: &ClineFreshPublicationContext<'_>,
    stream: &str,
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<ComponentCursorPlan, ClineNativeVerticalError> {
    let page_revision = revision(&page.core.source_revision.revision_sha256);
    let Some(stored) = stored else {
        if page.core.expected_frontier.next_native_index != 0 {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                None,
                component_sync_cursor(context, stream, page, 0, 0)?,
            ),
            generation: 0,
            rejected_records: 0,
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    let prior = ClineNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    if prior.version != ClineNativeStoreCursor::VERSION
        || prior.provider != context.dialect.provider.as_str()
        || prior.source_identity != page.core.source.stable_id.as_ref()
    {
        return Err(ClineNativeVerticalError::CorruptCursor);
    }
    if prior.source_revision == page_revision {
        if prior.frontier == page.core.next_safe_frontier
            || prior.frontier.next_native_index > page.core.next_safe_frontier.next_native_index
        {
            return Ok(ComponentCursorPlan::AlreadyCommitted);
        }
        if prior.frontier != page.core.expected_frontier {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                Some(stored.cursor.clone()),
                component_sync_cursor(
                    context,
                    stream,
                    page,
                    prior.generation,
                    prior.rejected_records,
                )?,
            ),
            generation: prior.generation,
            rejected_records: prior.rejected_records,
        });
    }
    if prior.frontier == page.core.expected_frontier
        && page.core.expected_frontier.next_native_index != 0
    {
        return Ok(ComponentCursorPlan::Publish {
            transition: NativePathCursorTransition::new(
                Some(stored.cursor.clone()),
                component_sync_cursor(
                    context,
                    stream,
                    page,
                    prior.generation,
                    prior.rejected_records,
                )?,
            ),
            generation: prior.generation,
            rejected_records: prior.rejected_records,
        });
    }
    if page.core.expected_frontier.next_native_index != 0 {
        return Err(ClineNativeVerticalError::CorruptCursor);
    }
    let generation = prior
        .generation
        .checked_add(1)
        .ok_or(ClineNativeVerticalError::GenerationExhausted)?;
    Ok(ComponentCursorPlan::Publish {
        transition: NativePathCursorTransition::new(
            Some(stored.cursor.clone()),
            component_sync_cursor(context, stream, page, generation, 0)?,
        ),
        generation,
        rejected_records: 0,
    })
}

fn validate_source_identity(
    dialect: TaskJsonNativeDialect,
    source_identity: &NativeSourceIdentity,
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<(), ClineNativeVerticalError> {
    if source_identity.provider() != dialect.provider.as_str()
        || source_identity.source_identity() != page.core.source.stable_id.as_ref()
        || page.core.source.provider != dialect.provider.as_str()
    {
        return Err(ClineNativeVerticalError::SourceIdentityMismatch);
    }
    Ok(())
}

fn revalidate_page_source(
    page: &NativeIngestionPage<ClineCertifiedPage>,
) -> std::result::Result<(), ClineNativeVerticalError> {
    if !super::revalidate_cline_component_source(
        &page.core.source.canonical_path,
        page.core.source.component,
        &page.core.source_revision.observed_stamp_token,
    )? {
        return Err(ClineNativeVerticalError::SourceChanged);
    }
    Ok(())
}

fn resolve_fresh_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ClineFreshPublicationContext<'_>,
    page: &ClineCertifiedPage,
    summary: &mut ProviderImportSummary,
) -> std::result::Result<ResolvedClineSource, ClineNativeVerticalError> {
    let session_fact = page.core.session.as_ref().or_else(|| {
        page.core
            .terminal_metadata_checkpoint
            .as_deref()
            .map(|checkpoint| &checkpoint.session)
    });
    let task_path = page
        .source
        .canonical_path
        .parent()
        .ok_or(CaptureError::SystemInvariant(
            "Cline component path has no task directory",
        ))?;
    let raw_source_path = task_path.display().to_string();
    let source_root = context.configured_source_root.display().to_string();
    let task_id = page.source.task.as_str();
    let locator_identity = provider_path_identity(task_path)?;
    let proposed_source_identity = provider_source_identity(
        context.dialect.provider,
        context.dialect.source_format,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Cline NativePath source has no canonical identity",
    ))?;
    let route_stream = task_cursor_stream(context.dialect, task_path)?;
    let source_revision = task_route_revision(context.dialect, task_id);
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: context.dialect.provider,
            source_format: context.dialect.source_format.to_owned(),
            machine_id: context.options.machine_id.clone(),
            locator_identity,
            cursor_stream: route_stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.clone(),
            observed_at_ms: context.options.imported_at.timestamp_millis(),
        })?;
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        context.dialect.provider,
        context.dialect.source_format,
        &context.options.machine_id,
        &resolution.canonical_source_identity,
        task_id,
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                context.dialect.provider,
                task_id,
                context.dialect.source_format,
                Some(&raw_source_path),
            )
        });
    if let Some(session_fact) = session_fact {
        group.upsert_capture_source(&cline_capture_source(
            context,
            session_fact.identity.as_str(),
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
            session_fact.workspace_directory.as_deref(),
            parse_timestamp(
                session_fact.created_at.as_deref(),
                context.options.imported_at,
            ),
        ))?;
    } else {
        let source = existing_source.ok_or(ClineNativeVerticalError::MissingSession)?;
        group.upsert_capture_source(&source)?;
    }
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session = if let Some(session_fact) = session_fact {
        let session = cline_session(
            committed_store,
            context,
            session_fact,
            source_id,
            &resolution.canonical_source_identity,
        )?;
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        session
    } else {
        committed_store
            .session_by_capture_source_and_external_session(
                source_id,
                context.dialect.provider,
                task_id,
            )?
            .ok_or(ClineNativeVerticalError::MissingSession)?
    };
    Ok(ResolvedClineSource { source_id, session })
}

#[allow(clippy::too_many_arguments)]
fn cline_capture_source(
    context: &ClineFreshPublicationContext<'_>,
    task_id: &str,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
    source_revision: &str,
    cwd: Option<&str>,
    started_at: DateTime<Utc>,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.options.machine_id.clone(),
            process_id: None,
            cwd: cwd.map(str::to_owned),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(task_id.to_owned()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": task_id,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.options.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    context.dialect.provider,
                    task_id,
                    context.dialect.source_format,
                    Some(raw_source_path),
                ),
                "nativepath_publication": context.dialect.publication_revision,
            }),
        ),
    }
}

fn cline_session(
    committed_store: &Store,
    context: &ClineFreshPublicationContext<'_>,
    fact: &super::ClineSessionRow,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> std::result::Result<Session, ClineNativeVerticalError> {
    let id = provider_import_session_uuid(
        committed_store,
        context.dialect.provider,
        fact.identity.as_str(),
        source_id,
        Some(canonical_source_identity),
    )?;
    let started_at = parse_timestamp(fact.created_at.as_deref(), context.options.imported_at);
    let ended_at = fact
        .last_modified
        .as_deref()
        .and_then(crate::common::time::parse_rfc3339_utc);
    Ok(Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(fact.identity.as_str().to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.identity.as_str(),
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.options.imported_at,
                "session_idempotency_key":
                    format!(
                        "provider-session:{}:{}",
                        context.dialect.provider.as_str(),
                        fact.identity.as_str()
                    ),
                "metadata": {
                    "title": fact.title,
                    "workspace_directory": fact.workspace_directory,
                    "created_at": fact.created_at,
                    "last_modified": fact.last_modified,
                    "model_id": fact.model_id,
                    "model_provider": fact.model_provider,
                    "tokens_input": fact.tokens_input,
                    "tokens_output": fact.tokens_output,
                    "nativepath_publication": context.dialect.publication_revision,
                },
            }),
        ),
    })
}

fn publish_page_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ClineFreshPublicationContext<'_>,
    resolved: &ResolvedClineSource,
    generation: u64,
    events: &[ClineEventRow],
    summary: &mut ProviderImportSummary,
) -> std::result::Result<(), ClineNativeVerticalError> {
    for event in events {
        let native_provider_event_index = packed_event_index(event)?;
        let provider_event_index = generation
            .checked_mul(TASK_JSON_GENERATION_EVENT_STRIDE)
            .and_then(|base| base.checked_add(native_provider_event_index))
            .ok_or(ClineNativeVerticalError::EventIndexOverflow)?;
        let event_hash = hex(&event.content_hash);
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            context.dialect.provider,
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            provider_event_index,
            provider_event_index,
            &event_hash,
            None,
            Some(event.native_order.item_index),
            resolved.session.id
                == crate::provider::importer::provider_session_uuid(
                    context.dialect.provider,
                    resolved
                        .session
                        .external_session_id
                        .as_deref()
                        .unwrap_or_default(),
                ),
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
                .unwrap_or(identity.dedupe_key);
        let occurred_at = event
            .occurred_at_millis
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .unwrap_or(resolved.session.started_at);
        let run = task_json_command_run(
            context,
            resolved,
            event,
            provider_event_index,
            &event_hash,
            occurred_at,
            identity.run_source_id,
        )?;
        if let Some(run) = &run {
            group.upsert_run(run)?;
        }
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: context.options.history_record_id,
            session_id: Some(resolved.session.id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: event_type(event.kind),
            role: Some(event_role(event.role)),
            occurred_at,
            capture_source_id: Some(resolved.source_id),
            payload: json!({
                "provider": context.dialect.provider.as_str(),
                "provider_session_id": resolved.session.external_session_id,
                "provider_event_index": provider_event_index,
                "native_provider_event_index": native_provider_event_index,
                "source_generation": generation,
                "provider_event_hash": event_hash,
                "native_component": component_name(event.native_order.component),
                "native_item_index": event.native_order.item_index,
                "native_sub_index": event.native_order.sub_index,
                "body": event.body,
                "preview": event.preview,
                "result_outcome": event.sparse_output.as_ref().map(|output| {
                    format!("{:?}", output.outcome).to_lowercase()
                }),
                "exit_code": event.sparse_output.as_ref().and_then(|output| output.exit_code),
                "duration_ms": event.sparse_output.as_ref().and_then(|output| output.duration_ms),
                "output_bytes": event.sparse_output.as_ref().map(|output| output.output_bytes),
                "output_preview": event.sparse_output.as_ref().and_then(|output| output.preview.clone()),
                "call_id": event.sparse_output.as_ref().and_then(|output| output.call_id.clone()),
                "tool_call": event.tool_call.as_ref().map(|tool| json!({
                    "call_id": tool.call_id,
                    "name": tool.name,
                })),
                "result": event.sparse_output.as_ref().map(|output| json!({
                    "outcome": format!("{:?}", output.outcome).to_lowercase(),
                    "exit_code": output.exit_code,
                    "duration_ms": output.duration_ms,
                    "output_bytes": output.output_bytes,
                    "preview": output.preview,
                    "call_id": output.call_id,
                })),
                "artifacts": [],
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "native_provider_event_index": native_provider_event_index,
                    "source_generation": generation,
                    "provider_event_hash": event_hash,
                    "provider_event_hash_authority": "provider_supplied",
                    "source_format": context.dialect.source_format,
                    "source_trust": "provider_native",
                    "source_record_ordinal": event.native_order.item_index,
                    "source_record_subrecord_index": event.native_order.sub_index,
                    "native_component": component_name(event.native_order.component),
                }),
            ),
        };
        if group
            .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?
        {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        for (touch_ordinal, touch) in event.file_touches.iter().enumerate() {
            let touch_ordinal = u64::try_from(touch_ordinal)
                .map_err(|_| ClineNativeVerticalError::EventIndexOverflow)?;
            let provider_touch_index = native_provider_event_index
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_ordinal))
                .ok_or(ClineNativeVerticalError::EventIndexOverflow)?;
            let provider_session_id = resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default();
            let id = provider_file_touch_import_id(
                committed_store,
                context.dialect.provider,
                provider_session_id,
                resolved.source_id,
                Some(provider_event_index),
                provider_touch_index,
                resolved.session.id
                    == crate::provider::importer::provider_session_uuid(
                        context.dialect.provider,
                        provider_session_id,
                    ),
            )?;
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: context.options.history_record_id,
                run_id: run.as_ref().map(|run| run.id),
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: touch.path.to_string(),
                change_kind: touch.change_kind,
                old_path: touch.old_path.as_deref().map(str::to_owned),
                line_count_delta: None,
                confidence: touch.confidence,
                timestamps: timestamps(occurred_at),
                source_id: Some(resolved.source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": context.dialect.provider.as_str(),
                        "provider_session_id": provider_session_id,
                        "provider_touch_index": provider_touch_index,
                        "provider_event_index": provider_event_index,
                        "native_provider_event_index": native_provider_event_index,
                        "source_generation": generation,
                        "source_format": context.dialect.source_format,
                        "session_id": resolved.session.id,
                        "metadata": touch.metadata,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn task_json_command_run(
    context: &ClineFreshPublicationContext<'_>,
    resolved: &ResolvedClineSource,
    event: &ClineEventRow,
    provider_event_index: u64,
    event_hash: &str,
    occurred_at: DateTime<Utc>,
    run_source_id: Option<Uuid>,
) -> std::result::Result<Option<Run>, ClineNativeVerticalError> {
    if event.kind != ClineEventKind::CommandOutput {
        return Ok(None);
    }
    let diagnostic = event
        .sparse_output
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "task-JSON command output has no sparse diagnostic",
        ))?;
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    let run_key = diagnostic.call_id.as_deref().unwrap_or(event_hash);
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    context.dialect.provider.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{run_key}"),
                "run",
            )
        },
    );
    let started_at = match diagnostic.duration_ms {
        Some(duration_ms) => {
            let duration_ms = i64::try_from(duration_ms).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            occurred_at.checked_sub_signed(duration).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms moves command start before representable time: {duration_ms}"
                ))
            })?
        }
        None => occurred_at,
    };
    Ok(Some(Run {
        id,
        history_record_id: context.options.history_record_id,
        session_id: Some(resolved.session.id),
        run_type: RunType::Command,
        status: match diagnostic.outcome {
            OutputOutcome::Success => RunStatus::Succeeded,
            OutputOutcome::Failure => RunStatus::Failed,
            OutputOutcome::Timeout => RunStatus::Cancelled,
            OutputOutcome::Unknown => RunStatus::Partial,
        },
        started_at,
        ended_at: Some(occurred_at),
        exit_code: diagnostic.exit_code,
        cwd: None,
        command_preview: None,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": diagnostic.call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn component_sync_cursor(
    context: &ClineFreshPublicationContext<'_>,
    stream: &str,
    page: &NativeIngestionPage<ClineCertifiedPage>,
    generation: u64,
    prior_rejected_records: u64,
) -> std::result::Result<SyncCursor, ClineNativeVerticalError> {
    let rejected_records = prior_rejected_records
        .saturating_add(u64::try_from(page.core.core.rejections.len()).unwrap_or(u64::MAX));
    let cursor = ClineNativeStoreCursor {
        version: ClineNativeStoreCursor::VERSION,
        provider: context.dialect.provider.as_str().to_owned(),
        source_identity: page.core.source.stable_id.to_string(),
        source_revision: revision(&page.core.source_revision.revision_sha256),
        frontier: page.core.next_safe_frontier.clone(),
        terminal: page.terminal,
        generation,
        rejected_records,
    }
    .encode()
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.options.machine_id.clone(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(context.options.imported_at),
        timestamps: timestamps(context.options.imported_at),
    })
}

fn component_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.component_cursor_stream_format,
        &identity,
    ))
}

fn load_cline_task_checkpoint(
    store: &Store,
    machine_id: &str,
    task: &ClineLiveTaskObservation,
) -> Result<Option<ClineTaskCheckpoint>> {
    let stream =
        task_cursor_stream(task.dialect, &task.canonical_task_path).map_err(map_vertical_error)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let wire: ClineTaskCheckpointWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    wire.into_checkpoint(Some(task))
        .map(Some)
        .map_err(map_vertical_error)
}

fn load_cline_task_checkpoint_by_path(
    store: &Store,
    machine_id: &str,
    task_path: &Path,
    dialect: TaskJsonNativeDialect,
) -> std::result::Result<Option<ClineTaskCheckpoint>, ClineNativeVerticalError> {
    let stream = task_cursor_stream(dialect, task_path)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    let wire: ClineTaskCheckpointWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
    wire.into_checkpoint(None).map(Some)
}

fn publish_task_json_task_checkpoint(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
) -> Result<ProviderImportSummary> {
    publish_task_json_task_checkpoint_inner(store, bulk_guard, options, dialect, checkpoint)
        .map_err(map_vertical_error)
}

fn publish_task_json_task_checkpoint_inner(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &TaskJsonNativeImportOptions,
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
) -> std::result::Result<ProviderImportSummary, ClineNativeVerticalError> {
    let stream = task_cursor_stream(dialect, &checkpoint.canonical_task_path)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let wire = ClineTaskCheckpointWire::from_checkpoint(checkpoint);
    let encoded = serde_json::to_string(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = &stored {
        let committed = decode_native_path_committed_cursor(&stored.cursor)
            .map_err(|_| ClineNativeVerticalError::CorruptCursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = task_checkpoint_publication_id(dialect, checkpoint, &transition);
    let retained_bytes = serde_json::to_vec(&wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
        .len();
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn task_cursor_stream(
    dialect: TaskJsonNativeDialect,
    path: &Path,
) -> std::result::Result<String, ClineNativeVerticalError> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.task_cursor_stream_format,
        &identity,
    ))
}

fn task_checkpoint_publication_id(
    dialect: TaskJsonNativeDialect,
    checkpoint: &ClineTaskCheckpoint,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.task_publication_domain);
    digest.update(
        checkpoint
            .canonical_task_path
            .as_os_str()
            .as_encoded_bytes(),
    );
    digest.update(checkpoint.identity.as_str().as_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.task_publication_prefix,
        hex(&digest.finalize())
    )
}

impl ClineTaskCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineTaskCheckpoint) -> Self {
        Self {
            version: CLINE_TASK_CURSOR_VERSION,
            canonical_task_path: checkpoint.canonical_task_path.clone(),
            api_history: checkpoint
                .api_history
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            ui_messages: checkpoint
                .ui_messages
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            fallback_history: checkpoint
                .fallback_history
                .as_ref()
                .map(ClineArrayCheckpointWire::from_checkpoint),
            task_metadata: ClineMetadataCheckpointWire::from_checkpoint(&checkpoint.task_metadata),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineTaskCheckpoint, ClineNativeVerticalError> {
        if self.version != CLINE_TASK_CURSOR_VERSION
            || live.is_some_and(|live| self.canonical_task_path != live.canonical_task_path)
        {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        let task_metadata = self.task_metadata.into_checkpoint(live)?;
        Ok(ClineTaskCheckpoint {
            identity: task_metadata.session.identity.clone(),
            canonical_task_path: self.canonical_task_path,
            api_history: self
                .api_history
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            ui_messages: self
                .ui_messages
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            fallback_history: self
                .fallback_history
                .map(|wire| wire.into_checkpoint(live))
                .transpose()?,
            task_metadata,
        })
    }
}

impl ClineArrayCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineArrayCheckpoint) -> Self {
        Self {
            component: checkpoint.component as u8,
            observation: ClinePersistedObservation::from_observation(&checkpoint.observation),
            certified_revision_sha256: checkpoint.certified_revision_sha256,
            complete_bytes: checkpoint.complete_bytes,
            observed_items: checkpoint.observed_items,
            retained_rows: checkpoint.retained_rows,
            final_frontier: checkpoint.final_frontier.clone(),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineArrayCheckpoint, ClineNativeVerticalError> {
        let component = event_component(self.component)?;
        if self.observation.component != component.source_component() as u8 {
            return Err(ClineNativeVerticalError::CorruptCursor);
        }
        Ok(ClineArrayCheckpoint {
            component,
            observation: self.observation.into_observation(live)?,
            certified_revision_sha256: self.certified_revision_sha256,
            complete_bytes: self.complete_bytes,
            observed_items: self.observed_items,
            retained_rows: self.retained_rows,
            final_frontier: self.final_frontier,
        })
    }
}

impl ClineMetadataCheckpointWire {
    fn from_checkpoint(checkpoint: &ClineMetadataCheckpoint) -> Self {
        Self {
            observation: ClinePersistedObservation::from_observation(&checkpoint.observation),
            content_sha256: checkpoint.content_sha256,
            session: ClineSessionRowWire::from_session(&checkpoint.session),
        }
    }

    fn into_checkpoint(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineMetadataCheckpoint, ClineNativeVerticalError> {
        Ok(ClineMetadataCheckpoint {
            observation: self.observation.into_observation(live)?,
            content_sha256: self.content_sha256,
            session: self.session.into_session()?,
        })
    }
}

impl ClinePersistedObservation {
    fn from_observation(observation: &ClineComponentObservation) -> Self {
        Self {
            component: observation.component as u8,
            path: observation.path.clone(),
            stamp_token: observation.stamp().map(super::ClineFileStamp::token),
            missing: observation.is_missing(),
        }
    }

    fn into_observation(
        self,
        live: Option<&ClineLiveTaskObservation>,
    ) -> std::result::Result<ClineComponentObservation, ClineNativeVerticalError> {
        let component = component(self.component)?;
        if let Some(live) = live {
            let current = live.component(component);
            if current.path != self.path {
                return Err(ClineNativeVerticalError::CorruptCursor);
            }
            let current_token = current.stamp().map(super::ClineFileStamp::token);
            if (self.missing && current.is_missing())
                || (!self.missing
                    && self.stamp_token.is_some()
                    && self.stamp_token == current_token)
            {
                return Ok(current.clone());
            }
        }
        if self.missing {
            return Ok(ClineComponentObservation {
                component,
                path: self.path,
                state: ClineObservedFileState::Missing,
            });
        }
        Ok(ClineComponentObservation {
            component,
            path: self.path,
            state: ClineObservedFileState::Unavailable(
                "persisted prior Cline component observation".into(),
            ),
        })
    }
}

impl ClineSessionRowWire {
    fn from_session(session: &ClineSessionRow) -> Self {
        Self {
            identity: session.identity.as_str().to_owned(),
            identity_origin: match session.identity_origin {
                ClineTaskIdentityOrigin::TaskMetadata => 0,
                ClineTaskIdentityOrigin::DirectoryNameDegraded => 1,
            },
            title: session.title.as_deref().map(str::to_owned),
            workspace_directory: session.workspace_directory.as_deref().map(str::to_owned),
            created_at: session.created_at.as_deref().map(str::to_owned),
            last_modified: session.last_modified.as_deref().map(str::to_owned),
            model_id: session.model_id.as_deref().map(str::to_owned),
            model_provider: session.model_provider.as_deref().map(str::to_owned),
            tokens_input: session.tokens_input,
            tokens_output: session.tokens_output,
        }
    }

    fn into_session(self) -> std::result::Result<ClineSessionRow, ClineNativeVerticalError> {
        let identity_origin = match self.identity_origin {
            0 => ClineTaskIdentityOrigin::TaskMetadata,
            1 => ClineTaskIdentityOrigin::DirectoryNameDegraded,
            _ => return Err(ClineNativeVerticalError::CorruptCursor),
        };
        Ok(ClineSessionRow::new(
            ClineTaskIdentity::new(self.identity),
            identity_origin,
            self.title.map(String::into_boxed_str),
            self.workspace_directory.map(String::into_boxed_str),
            self.created_at.map(String::into_boxed_str),
            self.last_modified.map(String::into_boxed_str),
            self.model_id.map(String::into_boxed_str),
            self.model_provider.map(String::into_boxed_str),
            self.tokens_input,
            self.tokens_output,
        ))
    }
}

fn component(value: u8) -> std::result::Result<ClineComponent, ClineNativeVerticalError> {
    match value {
        value if value == ClineComponent::ApiHistory as u8 => Ok(ClineComponent::ApiHistory),
        value if value == ClineComponent::UiMessages as u8 => Ok(ClineComponent::UiMessages),
        value if value == ClineComponent::TaskMetadata as u8 => Ok(ClineComponent::TaskMetadata),
        value if value == ClineComponent::RootIndex as u8 => Ok(ClineComponent::RootIndex),
        value if value == ClineComponent::FallbackHistory as u8 => {
            Ok(ClineComponent::FallbackHistory)
        }
        value if value == ClineComponent::HistoryItem as u8 => Ok(ClineComponent::HistoryItem),
        value if value == ClineComponent::TaskIndex as u8 => Ok(ClineComponent::TaskIndex),
        _ => Err(ClineNativeVerticalError::CorruptCursor),
    }
}

fn event_component(
    value: u8,
) -> std::result::Result<ClineEventComponent, ClineNativeVerticalError> {
    match value {
        value if value == ClineEventComponent::ApiHistory as u8 => {
            Ok(ClineEventComponent::ApiHistory)
        }
        value if value == ClineEventComponent::UiMessages as u8 => {
            Ok(ClineEventComponent::UiMessages)
        }
        value if value == ClineEventComponent::FallbackHistory as u8 => {
            Ok(ClineEventComponent::FallbackHistory)
        }
        _ => Err(ClineNativeVerticalError::CorruptCursor),
    }
}

fn page_publication_id(
    dialect: TaskJsonNativeDialect,
    source: &NativeSourceIdentity,
    page: &NativeIngestionPage<ClineCertifiedPage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(dialect.page_publication_domain);
    digest.update(source.provider().as_bytes());
    digest.update(source.source_identity().as_bytes());
    digest.update(page.core.identity.as_bytes());
    digest.update(page.expected_frontier.version.to_le_bytes());
    digest.update(&page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    digest.update(&page.next_safe_frontier.bytes);
    digest.update([u8::from(page.terminal)]);
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "{}{}",
        dialect.page_publication_prefix,
        hex(&digest.finalize())
    )
}

fn packed_event_index(event: &ClineEventRow) -> std::result::Result<u64, ClineNativeVerticalError> {
    let component = u64::from(event.native_order.component as u8);
    let sub_index = u64::from(event.native_order.sub_index);
    let item_index = event.native_order.item_index;
    if item_index >= (TASK_JSON_GENERATION_EVENT_STRIDE >> 18) || sub_index > 0xffff {
        return Err(ClineNativeVerticalError::EventIndexOverflow);
    }
    Ok((component << 16) | (item_index << 18) | sub_index)
}

fn event_type(kind: ClineEventKind) -> EventType {
    match kind {
        ClineEventKind::Message => EventType::Message,
        ClineEventKind::Summary => EventType::Summary,
        ClineEventKind::ToolCall => EventType::ToolCall,
        ClineEventKind::ToolOutput => EventType::ToolOutput,
        ClineEventKind::CommandOutput => EventType::CommandOutput,
        ClineEventKind::Notice => EventType::Notice,
    }
}

fn event_role(role: ClineEventRole) -> EventRole {
    match role {
        ClineEventRole::User => EventRole::User,
        ClineEventRole::Assistant => EventRole::Assistant,
        ClineEventRole::System => EventRole::System,
        ClineEventRole::Unknown => EventRole::Unknown,
    }
}

fn component_name(component: ClineEventComponent) -> &'static str {
    match component {
        ClineEventComponent::ApiHistory => "api_history",
        ClineEventComponent::UiMessages => "ui_messages",
        ClineEventComponent::FallbackHistory => "fallback_history",
    }
}

fn parse_timestamp(value: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    value
        .and_then(crate::common::time::parse_rfc3339_utc)
        .unwrap_or(fallback)
}

fn revision(hash: &[u8; 32]) -> String {
    format!("sha256:{}", hex(hash))
}

fn task_route_revision(dialect: TaskJsonNativeDialect, task_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-task-json-route-revision-v1\0");
    digest.update(dialect.provider.as_str().as_bytes());
    digest.update(dialect.source_format.as_bytes());
    digest.update((task_id.len() as u64).to_le_bytes());
    digest.update(task_id.as_bytes());
    revision(&digest.finalize().into())
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn map_vertical_error(error: ClineNativeVerticalError) -> CaptureError {
    match error {
        ClineNativeVerticalError::Capture(error) => error,
        ClineNativeVerticalError::Store(error) => CaptureError::Store(error),
        ClineNativeVerticalError::Adapter(error) => CaptureError::InvalidPayload(error.to_string()),
        ClineNativeVerticalError::Source(error) => CaptureError::InvalidPayload(error.to_string()),
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

fn map_source_error(error: ClineNativePathError) -> CaptureError {
    match error {
        ClineNativePathError::SourceChanged { .. } => CaptureError::SourceChangedDuringCapture,
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}
