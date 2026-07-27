//! Provider-owned NativePath Store publication for the OpenCode SQLite family.
//!
//! The reader owns discovery, snapshotting, parsing, source mutation evidence,
//! and independent output replay. This module is the narrow typed Store leaf;
//! it deliberately does not expose a generic provider record envelope.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence, Event,
    EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor,
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
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_edge_uuid,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputNativeCursor, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    classify_opencode_native_lifecycle, OpenCodeNativeEvent, OpenCodeNativeEventKind,
    OpenCodeNativeFrontier, OpenCodeNativeGenerationChange, OpenCodeNativePage,
    OpenCodeNativePageLimits, OpenCodeNativePathReader, OpenCodeNativePersistedState,
    OpenCodeNativePhysicalSourceIdentity, OpenCodeNativePriorGeneration, OpenCodeNativeProFrontier,
    OpenCodeNativeProfile, OpenCodeNativePublicationMode, OpenCodeNativeScanPhase,
    OpenCodeNativeScanSummary, OpenCodeNativeSession, OpenCodeNativeSourceSelection,
};
use crate::provider::providers::opencode::OpenCodeSqliteDialect;

const OPENCODE_NATIVE_STORE_CURSOR_VERSION: u32 = 1;
const OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION: u32 = 1;
const OPENCODE_NATIVE_OUTPUT_PARSER_REVISION: &str = "opencode-family-nativepath-output-v1";
const OPENCODE_NATIVE_SOURCE_REVISION_DOMAIN: &[u8] =
    b"ctx-opencode-family-nativepath-source-revision-v1\0";
const OPENCODE_NATIVE_PUBLICATION_DOMAIN: &[u8] =
    b"ctx-opencode-family-nativepath-publication-v1\0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeNativeStoreCursor {
    version: u32,
    provider: String,
    source_format: String,
    selected_path: PathBuf,
    cursor_path_identity: String,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    generation: u64,
    rejected_records: u64,
    frontier: OpenCodeNativeFrontier,
    route_retired: bool,
    completed_state: Option<OpenCodeNativePersistedState>,
    pending_state: OpenCodeNativePersistedState,
}

// The decoded native cursor stays inline so cursor-state handling remains allocation-free.
#[allow(clippy::large_enum_variant)]
enum StoredCursor {
    None,
    Native {
        stored: SyncCursor,
        cursor: OpenCodeNativeStoreCursor,
    },
    Released {
        stored: SyncCursor,
    },
}

struct OpenCodePublicationContext<'a> {
    dialect: &'a OpenCodeSqliteDialect,
    adapter: &'a ProviderAdapterContext,
    options: &'a ProviderImportOptions,
    selected_path: &'a Path,
    raw_source_path: String,
    source_root: String,
    cursor_path_identity: String,
    locator_identity: String,
    cursor_stream: String,
    source_revision: String,
    canonical_source_identity: String,
    generation: u64,
    replacement: bool,
    current_state: OpenCodeNativePersistedState,
}

pub(in crate::provider::providers::opencode) fn import_opencode_nativepath(
    path: &Path,
    store: &mut Store,
    mut adapter: ProviderAdapterContext,
    options: ProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    adapter.source_path = Some(path.to_path_buf());
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason:
                    "OpenCode-family SQLite source component must be a regular non-symlink file",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return import_missing_source(path, store, &adapter, &options, dialect);
        }
        Err(error) => return Err(error.into()),
    }

    let selected_path = fs::canonicalize(path)?;
    let cursor_path_identity = provider_path_identity(&selected_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.source_format,
        &cursor_path_identity,
    );
    let stored = load_store_cursor(store, &adapter.machine_id, &cursor_stream, dialect)?;
    let prior_completed = match &stored {
        StoredCursor::Native { cursor, .. } if !cursor.route_retired => {
            cursor.completed_state.clone()
        }
        StoredCursor::None | StoredCursor::Released { .. } | StoredCursor::Native { .. } => None,
    };

    let selection = OpenCodeNativeSourceSelection::exact(&selected_path)
        .with_inventory_observation_token(options.inventory_observation_token.clone());
    let reader = OpenCodeNativePathReader::acquire(selection)?;
    let current_summary = scan_current_summary(&reader, prior_completed.as_ref())?;
    let current_state = current_summary.persisted_state();
    let source_revision = source_revision(dialect, &current_summary);
    let physical_locator_identity = sqlite_locator_identity(
        &cursor_path_identity,
        &current_summary.physical_source_identity,
    )?;
    let previous = prior_completed
        .clone()
        .map(OpenCodeNativePriorGeneration::from_persisted)
        .into_iter()
        .collect::<Vec<_>>();
    let plan = classify_opencode_native_lifecycle(&previous, &current_summary)?;

    let raw_source_path = selected_path.display().to_string();
    let source_root = adapter
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let default_source_identity = provider_source_identity(
        dialect.provider,
        dialect.source_format,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenCode NativePath source has no canonical identity",
    ))?;
    let replacement = matches!(
        plan.change,
        OpenCodeNativeGenerationChange::Rewrite
            | OpenCodeNativeGenerationChange::Rewind
            | OpenCodeNativeGenerationChange::RewriteAndRewind
            | OpenCodeNativeGenerationChange::Replacement
    );
    let locator_identity = if replacement {
        sqlite_generation_locator_identity(&physical_locator_identity, &source_revision)
    } else if let StoredCursor::Native { cursor, .. } = &stored {
        if !cursor.route_retired
            && cursor.pending_state.physical_source_identity
                == current_summary.physical_source_identity
        {
            cursor.locator_identity.clone()
        } else {
            physical_locator_identity
        }
    } else {
        physical_locator_identity
    };
    let relocated_source_identity = prior_relocation_identity(
        store,
        dialect,
        &adapter.machine_id,
        &source_revision,
        &raw_source_path,
    )?;
    let canonical_source_identity = match &stored {
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.provider == dialect.provider.as_str()
                && cursor.source_format == dialect.source_format =>
        {
            cursor.canonical_source_identity.clone()
        }
        _ if relocated_source_identity.is_some() => {
            relocated_source_identity.expect("guarded OpenCode relocation identity is present")
        }
        _ => default_source_identity,
    };
    let generation = next_generation(&stored, &current_state, replacement)?;
    let context = OpenCodePublicationContext {
        dialect,
        adapter: &adapter,
        options: &options,
        selected_path: &selected_path,
        raw_source_path,
        source_root,
        cursor_path_identity,
        locator_identity,
        cursor_stream,
        source_revision,
        canonical_source_identity,
        generation,
        replacement,
        current_state,
    };

    if options.import_profile.is_replay_only() {
        verify_committed_core(&stored, &context)?;
        replay_outputs_or_mark_behind(&context, options.import_profile.sink().map(AsRef::as_ref));
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = if plan.publication == OpenCodeNativePublicationMode::ObservationOnly
        && stored_is_terminal_for(&stored, &context.current_state)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        summary
    } else {
        publish_core(store, &reader, &stored, &context)?
    };
    replay_outputs_or_mark_behind(&context, options.import_profile.sink().map(AsRef::as_ref));
    if summary.work_result() == ProviderImportWorkResult::NoOp && summary.failed == 0 {
        summary.skipped_sessions =
            usize::try_from(current_summary.metrics.native_sessions).unwrap_or(usize::MAX);
        summary.skipped = summary.skipped.saturating_add(summary.skipped_sessions);
    }
    Ok(summary)
}

fn scan_current_summary(
    reader: &OpenCodeNativePathReader,
    prior: Option<&OpenCodeNativePersistedState>,
) -> Result<OpenCodeNativeScanSummary> {
    let mut scanner = match prior {
        Some(prior) if prior.is_supported() => reader.scanner_with_profile_and_prior(
            OpenCodeNativeProfile::CoreOnly,
            OpenCodeNativePageLimits::default(),
            prior,
        )?,
        _ => reader.scanner(OpenCodeNativePageLimits::default())?,
    };
    while scanner.next_page()?.is_some() {}
    scanner.finish()
}

fn load_store_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
    dialect: &OpenCodeSqliteDialect,
) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor: OpenCodeNativeStoreCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "{} NativePath cursor is malformed: {error}",
                dialect.display_name
            ))
        })?;
        validate_store_cursor(&cursor, dialect, machine_id, stream)?;
        return Ok(StoredCursor::Native { stored, cursor });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredCursor::Released { stored }),
        None => Err(CaptureError::InvalidPayload(format!(
            "{} cursor is neither NativePath nor a released migration cursor",
            dialect.display_name
        ))),
    }
}

fn validate_store_cursor(
    cursor: &OpenCodeNativeStoreCursor,
    dialect: &OpenCodeSqliteDialect,
    machine_id: &str,
    stream: &str,
) -> Result<()> {
    if cursor.version != OPENCODE_NATIVE_STORE_CURSOR_VERSION
        || cursor.provider != dialect.provider.as_str()
        || cursor.source_format != dialect.source_format
        || cursor.selected_path.as_os_str().is_empty()
        || cursor.cursor_path_identity.is_empty()
        || cursor.locator_identity.is_empty()
        || cursor.canonical_source_identity.is_empty()
        || cursor.source_revision.is_empty()
        || cursor.pending_state.selected_path != cursor.selected_path
        || cursor
            .completed_state
            .as_ref()
            .is_some_and(|state| !state.is_supported())
        || provider_source_cursor_stream_for_path(
            dialect.provider,
            dialect.source_format,
            &cursor.cursor_path_identity,
        ) != stream
        || machine_id.is_empty()
    {
        return Err(CaptureError::InvalidPayload(format!(
            "{} NativePath cursor is inconsistent",
            dialect.display_name
        )));
    }
    Ok(())
}

fn next_generation(
    stored: &StoredCursor,
    current: &OpenCodeNativePersistedState,
    replacement: bool,
) -> Result<u64> {
    let StoredCursor::Native { cursor, .. } = stored else {
        return Ok(0);
    };
    if !replacement && !cursor.route_retired && same_generation(&cursor.pending_state, current) {
        return Ok(cursor.generation);
    }
    cursor
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode NativePath generation overflowed",
        ))
}

fn same_generation(
    left: &OpenCodeNativePersistedState,
    right: &OpenCodeNativePersistedState,
) -> bool {
    left.source_generation_digest == right.source_generation_digest
        && left.capability_digest == right.capability_digest
        && left.semantic_digest == right.semantic_digest
        && left.schema_family == right.schema_family
        && left.parser_revision == right.parser_revision
        && left.policy_revision == right.policy_revision
}

fn stored_is_terminal_for(stored: &StoredCursor, current: &OpenCodeNativePersistedState) -> bool {
    matches!(
        stored,
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.frontier.phase == OpenCodeNativeScanPhase::Complete
                && cursor.completed_state.as_ref().is_some_and(|state| same_generation(state, current))
    )
}

fn verify_committed_core(
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> Result<()> {
    if !stored_is_terminal_for(stored, &context.current_state) {
        return Err(CaptureError::InvalidPayload(format!(
            "{} output replay requires exact committed NativePath Core",
            context.dialect.display_name
        )));
    }
    Ok(())
}

fn publish_core(
    store: &mut Store,
    reader: &OpenCodeNativePathReader,
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let mut scanner = reader.scanner(OpenCodeNativePageLimits::default())?;
    let resume_frontier = resumable_frontier(stored, context);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut expected_cursor = stored_sync_cursor(stored).cloned();
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        while let Some(page) = scanner.next_page()? {
            if frontier_at_or_before(page.next_frontier, resume_frontier) {
                expected_cursor = stored_sync_cursor(stored).cloned();
                continue;
            }
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                reader,
                expected_cursor.as_ref(),
                context,
                page,
            )?;
            if page_summary.work_result() == ProviderImportWorkResult::Changed {
                changed_groups = changed_groups.saturating_add(1);
            }
            summary.merge_from(page_summary);
            expected_cursor =
                store.get_sync_cursor(None, &context.adapter.machine_id, &context.cursor_stream)?;
            if context.options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }
        let finished = scanner.finish()?;
        if !same_generation(&finished.persisted_state(), &context.current_state) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if summary.imported == 0 && summary.skipped == 0 && summary.failed == 0 {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
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

fn resumable_frontier(
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> OpenCodeNativeFrontier {
    match stored {
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.generation == context.generation
                && same_generation(&cursor.pending_state, &context.current_state) =>
        {
            cursor.frontier
        }
        _ => OpenCodeNativeFrontier {
            phase: OpenCodeNativeScanPhase::Sessions,
            scan_ordinal: 0,
        },
    }
}

fn frontier_at_or_before(
    candidate: OpenCodeNativeFrontier,
    committed: OpenCodeNativeFrontier,
) -> bool {
    frontier_key(candidate) <= frontier_key(committed)
}

fn frontier_key(frontier: OpenCodeNativeFrontier) -> (u8, u64) {
    (
        match frontier.phase {
            OpenCodeNativeScanPhase::Sessions => 0,
            OpenCodeNativeScanPhase::Events => 1,
            OpenCodeNativeScanPhase::Complete => 2,
        },
        frontier.scan_ordinal,
    )
}

fn stored_sync_cursor(stored: &StoredCursor) -> Option<&SyncCursor> {
    match stored {
        StoredCursor::Native { stored, .. } | StoredCursor::Released { stored } => Some(stored),
        StoredCursor::None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    reader: &OpenCodeNativePathReader,
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
    page: OpenCodeNativePage,
) -> Result<ProviderImportSummary> {
    if page.source_authority.selected_path() != context.selected_path {
        return Err(CaptureError::SystemInvariant(
            "OpenCode NativePath page escaped its selected source",
        ));
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let rejected_in_page = u64::try_from(page.rejections.len()).map_err(|_| {
        CaptureError::SystemInvariant("OpenCode NativePath rejection count exceeds u64")
    })?;
    let rejected_before = expected_cursor
        .and_then(|stored| decode_current_cursor(&stored.cursor).ok())
        .map_or(0, |cursor| cursor.rejected_records);
    let rejected_records =
        rejected_before
            .checked_add(rejected_in_page)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode NativePath rejection count overflowed",
            ))?;
    let next_wire = next_store_cursor(context, &page, rejected_records);
    let next_cursor = provider_sync_cursor(
        context,
        serde_json::to_string(&next_wire)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    );
    let transition = NativePathCursorTransition::new(
        expected_cursor.map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = page_publication_id(context, &page, &transition);
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
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

    if context.replacement
        && page.expected_frontier.phase == OpenCodeNativeScanPhase::Sessions
        && page.expected_frontier.scan_ordinal == 0
    {
        if let Some(retirement) = replacement_retirement(expected_cursor, context)? {
            group.retire_provider_source_route(&retirement)?;
        }
    }
    let locator = ProviderSourceLocatorObservation {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        locator_identity: context.locator_identity.clone(),
        cursor_stream: context.cursor_stream.clone(),
        proposed_source_identity: context.canonical_source_identity.clone(),
        raw_source_path: Some(context.raw_source_path.clone()),
        source_revision: context.source_revision.clone(),
        observed_at_ms: context.adapter.imported_at.timestamp_millis(),
    };
    let resolution = group.reconcile_provider_source_locator(&locator)?;
    if resolution.canonical_source_identity != context.canonical_source_identity {
        return Err(CaptureError::InvalidPayload(format!(
            "{} source route resolved to an unexpected logical source",
            context.dialect.display_name
        )));
    }
    let route_binding = resolution.route_binding();
    let mut summary = ProviderImportSummary::default();
    let page_session_ids = page
        .sessions
        .iter()
        .map(|session| session.native_identity.as_str())
        .collect::<BTreeSet<_>>();
    for session in &page.sessions {
        publish_session(
            committed_store,
            &mut group,
            context,
            &route_binding,
            resolution.relocated,
            &page_session_ids,
            session,
            &mut summary,
        )?;
    }
    for event in &page.events {
        publish_event(
            reader,
            committed_store,
            &mut group,
            context,
            &route_binding,
            resolution.relocated,
            event,
            &mut summary,
        )?;
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(
                rejection
                    .native_order
                    .as_ref()
                    .map_or(page.position.native_events_seen, |_| {
                        page.next_frontier.scan_ordinal
                    }),
            )
            .unwrap_or(usize::MAX)
            .saturating_add(1),
            error: format!("{}: {}", rejection.kind.label(), rejection.reason),
        });
    }
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn next_store_cursor(
    context: &OpenCodePublicationContext<'_>,
    page: &OpenCodeNativePage,
    rejected_records: u64,
) -> OpenCodeNativeStoreCursor {
    OpenCodeNativeStoreCursor {
        version: OPENCODE_NATIVE_STORE_CURSOR_VERSION,
        provider: context.dialect.provider.as_str().to_owned(),
        source_format: context.dialect.source_format.to_owned(),
        selected_path: context.selected_path.to_path_buf(),
        cursor_path_identity: context.cursor_path_identity.clone(),
        locator_identity: context.locator_identity.clone(),
        canonical_source_identity: context.canonical_source_identity.clone(),
        source_revision: context.source_revision.clone(),
        generation: context.generation,
        rejected_records,
        frontier: page.next_frontier,
        route_retired: false,
        completed_state: page.terminal.then(|| context.current_state.clone()),
        pending_state: context.current_state.clone(),
    }
}

fn provider_sync_cursor(context: &OpenCodePublicationContext<'_>, cursor: String) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                context.dialect.provider.as_str(),
                context.adapter.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.adapter.machine_id.clone(),
        stream: context.cursor_stream.clone(),
        cursor,
        last_synced_at: Some(context.adapter.imported_at),
        timestamps: timestamps(context.adapter.imported_at),
    }
}

fn page_publication_id(
    context: &OpenCodePublicationContext<'_>,
    page: &OpenCodeNativePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_PUBLICATION_DOMAIN);
    hash_field(&mut digest, context.dialect.provider.as_str().as_bytes());
    hash_field(&mut digest, context.dialect.source_format.as_bytes());
    hash_field(&mut digest, context.canonical_source_identity.as_bytes());
    digest.update(context.generation.to_le_bytes());
    digest.update(page.identity.0);
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    format!("opencode-nativepath-v1:{:x}", digest.finalize())
}

fn replacement_retirement(
    expected_cursor: Option<&SyncCursor>,
    context: &OpenCodePublicationContext<'_>,
) -> Result<Option<ProviderSourceRouteRetirement>> {
    if !context.replacement {
        return Ok(None);
    }
    let Some(expected_cursor) = expected_cursor else {
        return Ok(None);
    };
    let Ok(prior) = decode_current_cursor(&expected_cursor.cursor) else {
        return Ok(None);
    };
    if prior.route_retired || prior.locator_identity == context.locator_identity {
        return Ok(None);
    }
    Ok(Some(ProviderSourceRouteRetirement {
        provider: context.dialect.provider,
        source_format: context.dialect.source_format.to_owned(),
        machine_id: context.adapter.machine_id.clone(),
        locator_identity: prior.locator_identity,
        cursor_stream: context.cursor_stream.clone(),
        expected_canonical_source_identity: prior.canonical_source_identity,
        expected_source_revision: prior.source_revision,
        retired_at_ms: context.adapter.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::Replaced,
    }))
}

fn decode_current_cursor(encoded: &str) -> Result<OpenCodeNativeStoreCursor> {
    let committed = decode_native_path_committed_cursor(encoded)?;
    serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &OpenCodePublicationContext<'_>,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    relocated: bool,
    page_session_ids: &BTreeSet<&str>,
    native: &OpenCodeNativeSession,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let source_id =
        source_id_for_session(committed_store, context, &native.native_identity, relocated)?;
    let source = capture_source(context, native, source_id)?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;
    let session_id = provider_import_session_uuid(
        committed_store,
        context.dialect.provider,
        &native.native_identity,
        source_id,
        Some(&context.canonical_source_identity),
    )?;
    let parent_session_id = native
        .parent_identity
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                context.dialect.provider,
                parent,
                source_id,
                Some(&context.canonical_source_identity),
            )
        })
        .transpose()?;
    let root_session_id = (native.root_identity != native.native_identity)
        .then(|| {
            provider_import_session_uuid(
                committed_store,
                context.dialect.provider,
                &native.root_identity,
                source_id,
                Some(&context.canonical_source_identity),
            )
        })
        .transpose()?
        .or(parent_session_id);
    for (related_id, external_id) in [
        parent_session_id.zip(native.parent_identity.as_deref()),
        root_session_id.zip(Some(native.root_identity.as_str())),
    ]
    .into_iter()
    .flatten()
    {
        if related_id != session_id
            && committed_store.get_session(related_id).is_err()
            && !page_session_ids.contains(external_id)
        {
            group.upsert_session(&relationship_placeholder(
                context,
                source_id,
                related_id,
                external_id,
            ))?;
        }
    }
    let session = canonical_session(
        context,
        native,
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
    )?;
    let existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_session_id {
        let edge = relationship_edge(context, source_id, &session, parent_id);
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok(())
}

fn source_id_for_session(
    committed_store: &Store,
    context: &OpenCodePublicationContext<'_>,
    provider_session_id: &str,
    relocated: bool,
) -> Result<Uuid> {
    if context
        .locator_identity
        .starts_with("opencode-family-generation-locator-v1:")
    {
        return Ok(stable_capture_uuid(
            &serde_json::to_string(&(
                "opencode-nativepath-source-generation-v1",
                context.dialect.provider.as_str(),
                context.dialect.source_format,
                &context.canonical_source_identity,
                provider_session_id,
                &context.locator_identity,
            ))?,
            "source",
        ));
    }
    if let Some(existing) = committed_store.capture_source_by_canonical_identity_session(
        context.dialect.provider,
        context.dialect.source_format,
        &context.adapter.machine_id,
        &context.canonical_source_identity,
        provider_session_id,
    )? {
        return Ok(existing.id);
    }
    if relocated || context.replacement {
        return Ok(stable_capture_uuid(
            &serde_json::to_string(&(
                "opencode-nativepath-canonical-source-v1",
                context.dialect.provider.as_str(),
                context.dialect.source_format,
                &context.canonical_source_identity,
                provider_session_id,
            ))?,
            "source",
        ));
    }
    Ok(provider_scoped_source_uuid(
        context.dialect.provider,
        provider_session_id,
        context.dialect.source_format,
        Some(&context.raw_source_path),
    ))
}

fn capture_source(
    context: &OpenCodePublicationContext<'_>,
    native: &OpenCodeNativeSession,
    source_id: Uuid,
) -> Result<CaptureSource> {
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: native.directory.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: Some(native.native_identity.clone()),
        },
        started_at: timestamp(
            native.time_created,
            context.dialect.session_time_created_field,
        )?,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.native_identity,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    context.dialect.provider,
                    &native.native_identity,
                    context.dialect.source_format,
                    Some(&context.raw_source_path),
                ),
            }),
        ),
    })
}

fn canonical_session(
    context: &OpenCodePublicationContext<'_>,
    native: &OpenCodeNativeSession,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> Result<Session> {
    let is_subagent = parent_session_id.is_some();
    Ok(Session {
        id: session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(native.native_identity.clone()),
        external_agent_id: native.agent_identity.clone(),
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: native
            .agent_identity
            .clone()
            .or_else(|| Some(if is_subagent { "subagent" } else { "primary" }.to_owned())),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: timestamp(
            native.time_created,
            context.dialect.session_time_created_field,
        )?,
        ended_at: None,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.native_identity,
                "parent_provider_session_id": native.parent_identity,
                "root_provider_session_id": native.root_identity,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "metadata": {
                    "title": native.title,
                    "directory": native.directory,
                    "model": native.model_identity,
                    "agent": native.agent_identity,
                    "time_updated": native.time_updated,
                },
            }),
        ),
    })
}

fn relationship_placeholder(
    context: &OpenCodePublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: context.dialect.provider,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.adapter.imported_at,
        ended_at: None,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": context.dialect.source_format,
                "source_identity": context.canonical_source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    context: &OpenCodePublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: provider_source_edge_uuid(
            &context.canonical_source_identity,
            session.external_session_id.as_deref().unwrap_or_default(),
            "parent_child",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": context.dialect.source_format,
                "imported_at": context.adapter.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> ctx_history_store::CanonicalActor {
    ctx_history_store::CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    reader: &OpenCodeNativePathReader,
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &OpenCodePublicationContext<'_>,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    relocated: bool,
    native: &OpenCodeNativeEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let source_id = source_id_for_session(
        committed_store,
        context,
        &native.session_identity,
        relocated,
    )?;
    let session_id = provider_import_session_uuid(
        committed_store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        Some(&context.canonical_source_identity),
    )?;
    let session = committed_store.get_session(session_id).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "{} event references missing committed session {}",
            context.dialect.display_name, native.session_identity
        ))
    })?;
    let source = event_capture_source(context, &session, source_id)?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, route_binding)?;

    let event_type = event_type(native.kind);
    let occurred_at = timestamp(
        native.time_created,
        context.dialect.event_time_created_field,
    )?;
    let retained = provider_policy_event_text(event_type, &native.searchable_text, &native.body);
    let result_evidence = native
        .body
        .get("result_evidence")
        .cloned()
        .unwrap_or_else(|| {
            provider_result_identifier_evidence(event_type, &native.searchable_text, &native.body)
        });
    let result_outcome = native
        .body
        .get("result_outcome")
        .cloned()
        .unwrap_or_else(|| provider_result_outcome_evidence(event_type, &native.body));
    let mut payload = json!({
        "entry_type": event_kind_label(native.kind),
        "message_id": native.message_identity,
        "text": retained.text,
        "text_retention": retained.retention.as_json(),
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "body": provider_capped_json(
            &provider_policy_body(event_type, &native.body),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    });
    if let Some(object) = payload.as_object_mut() {
        for key in ["output_preview", "exit_code", "timed_out", "duration_ms"] {
            if let Some(value) = native.body.get(key) {
                object.insert(key.to_owned(), value.clone());
            }
        }
    }
    payload = crate::provider::importer::compact_provider_result_payload(event_type, &payload);
    let verified_content_locators = if event_type == EventType::Message
        && payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            == Some(true)
    {
        let (locator, values, complete_text) =
            reader.complete_message_record(native, context.dialect)?;
        let mut metadata = json!({});
        crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
            context.dialect.provider,
            context.dialect.source_format,
            &native.content_digest,
            &payload,
            &mut metadata,
            &locator,
            &values,
            || complete_text,
        )?;
        metadata
            .get(crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
            .cloned()
    } else {
        None
    };
    let legacy_hash = legacy_provider_event_hash(context, native);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        context.dialect.provider,
        &native.session_identity,
        source_id,
        native.provider_event_index,
        native.provider_event_index,
        &legacy_hash,
        None,
        Some(native.provider_event_index),
        session_id
            == crate::provider::importer::provider_session_uuid(
                context.dialect.provider,
                &native.session_identity,
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &native.content_digest,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_metadata = json!({
        "provider_session_id": native.session_identity,
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": native.content_digest,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": event_cursor(native),
        "source_format": context.dialect.source_format,
        "source_trust": "provider_native",
        "imported_at": context.adapter.imported_at,
        "source_record_ordinal": native.provider_event_index,
        "source_record_subrecord_index": 0,
        "native_record_id": legacy_hash,
        "metadata": {
            "message_id": native.message_identity,
            "time_created": native.time_created,
            "time_updated": native.time_updated,
            "native_locator_kind": native.locator.kind,
            "native_locator_version": native.locator.version,
        },
    });
    if let (Some(object), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        object.insert(
            crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators,
        );
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type,
        role: Some(event_role(&native.role)),
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for (touch_index, touch) in native.file_touches.iter().enumerate() {
        let touch_index = u64::try_from(touch_index)
            .ok()
            .and_then(|touch| {
                native
                    .provider_event_index
                    .checked_mul(u64::from(u16::MAX) + 1)
                    .and_then(|base| base.checked_add(touch))
            })
            .ok_or(CaptureError::InvalidPayload(
                "OpenCode NativePath file-touch identity overflowed".to_owned(),
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            context.dialect.provider,
            &native.session_identity,
            source_id,
            Some(native.provider_event_index),
            touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    context.dialect.provider,
                    &native.session_identity,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.options.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: Some(FileChangeKind::Modified),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": context.dialect.provider.as_str(),
                    "provider_session_id": native.session_identity,
                    "provider_touch_index": touch_index,
                    "provider_event_index": native.provider_event_index,
                    "source_format": context.dialect.source_format,
                    "session_id": session_id,
                }),
            ),
        })?;
    }
    Ok(())
}

fn event_capture_source(
    context: &OpenCodePublicationContext<'_>,
    session: &Session,
    source_id: Uuid,
) -> Result<CaptureSource> {
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.dialect.provider,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(context.dialect.source_format.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: session.external_session_id.clone(),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": context.dialect.source_format,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
            }),
        ),
    })
}

fn event_type(kind: OpenCodeNativeEventKind) -> EventType {
    match kind {
        OpenCodeNativeEventKind::Message => EventType::Message,
        OpenCodeNativeEventKind::Summary => EventType::Summary,
        OpenCodeNativeEventKind::Notice => EventType::Notice,
        OpenCodeNativeEventKind::ToolCall => EventType::ToolCall,
        OpenCodeNativeEventKind::ToolOutput => EventType::ToolOutput,
        OpenCodeNativeEventKind::CommandOutput => EventType::CommandOutput,
    }
}

fn event_kind_label(kind: OpenCodeNativeEventKind) -> &'static str {
    match kind {
        OpenCodeNativeEventKind::Message => "message",
        OpenCodeNativeEventKind::Summary => "summary",
        OpenCodeNativeEventKind::Notice => "notice",
        OpenCodeNativeEventKind::ToolCall => "tool_call",
        OpenCodeNativeEventKind::ToolOutput => "tool_output",
        OpenCodeNativeEventKind::CommandOutput => "command_output",
    }
}

fn event_role(role: &str) -> EventRole {
    provider_role(Some(role))
}

fn legacy_provider_event_hash(
    context: &OpenCodePublicationContext<'_>,
    event: &OpenCodeNativeEvent,
) -> String {
    if context.current_state.schema_family == super::OpenCodeNativeSchemaFamily::MessagePart {
        format!("{}:{}", event.message_identity, event.native_identity)
    } else {
        event.native_identity.clone()
    }
}

fn event_cursor(event: &OpenCodeNativeEvent) -> String {
    match &event.native_order {
        super::OpenCodeNativeOrder::ExplicitSequence { sequence, .. } => {
            format!("session_message:{}:seq:{sequence}", event.session_identity)
        }
        super::OpenCodeNativeOrder::SynthesizedSequence { .. } => {
            format!(
                "session_message:{}:{}",
                event.session_identity, event.native_identity
            )
        }
        super::OpenCodeNativeOrder::MessagePart { part_id, .. } => {
            format!("message:{}:part:{part_id}", event.message_identity)
        }
    }
}

fn timestamp(value: i64, field: &str) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("{field} is outside the supported timestamp range"))
    })
}

fn replay_outputs_or_mark_behind(
    context: &OpenCodePublicationContext<'_>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(context, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "opencode_family_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    context: &OpenCodePublicationContext<'_>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let reader = OpenCodeNativePathReader::acquire(
        OpenCodeNativeSourceSelection::exact(context.selected_path)
            .with_inventory_observation_token(context.options.inventory_observation_token.clone()),
    )?;
    let mut verification = reader.scanner_with_profile_and_prior(
        OpenCodeNativeProfile::CoreOnly,
        OpenCodeNativePageLimits::default(),
        &context.current_state,
    )?;
    while verification.next_page()?.is_some() {}
    let verified = verification.finish()?;
    if !same_generation(&verified.persisted_state(), &context.current_state) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let source = output_source_identity(context);
    let progress = sink.observe_source(&source).map_err(output_sink_error)?;
    let output_plan = output_replay_plan(context, sink, progress.as_ref())?;
    if output_plan.already_terminal {
        return Ok(());
    }
    let mut scanner = reader.scanner_with_profile_and_prior(
        OpenCodeNativeProfile::CoreAndPro,
        OpenCodeNativePageLimits::default(),
        &context.current_state,
    )?;
    scanner.resume_pro_from(output_plan.frontier)?;
    let mut expected_prior_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.clone());
    while let Some(mut page) = scanner.next_pro_output_page()? {
        if !page.rejections.is_empty() {
            return Err(CaptureError::InvalidPayload(format!(
                "{} output replay rejected {} malformed output records",
                context.dialect.display_name,
                page.rejections.len()
            )));
        }
        for observation in &mut page.observations {
            observation.coordinate.unit_key = output_unit_key(context, observation);
        }
        let next_safe_cursor = encode_output_frontier(page.next_frontier)?;
        let materialized = sink
            .materialize_page(ProOutputMaterializationPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: output_plan.source_epoch,
                observed_revision: context.source_revision.clone(),
                parser_revision: OPENCODE_NATIVE_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: output_plan.disposition,
                expected_prior_source_epoch: output_plan.expected_prior_source_epoch,
                expected_prior_cursor: expected_prior_cursor.clone(),
                next_safe_cursor: next_safe_cursor.clone(),
                terminal: page.terminal,
                observations: page.observations,
            })
            .map_err(output_sink_error)?;
        if materialized.source_epoch != output_plan.source_epoch
            || materialized.committed_cursor != next_safe_cursor
        {
            return Err(CaptureError::InvalidPayload(format!(
                "{} output sink acknowledged the wrong NativePath frontier",
                context.dialect.display_name
            )));
        }
        expected_prior_cursor = Some(next_safe_cursor);
    }
    let finished = scanner.finish_pro_replay()?;
    if !finished.complete
        || finished.source_generation_digest != context.current_state.source_generation_digest
        || finished.capability_digest != context.current_state.capability_digest
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

struct OutputReplayPlan {
    source_epoch: u64,
    disposition: ProOutputSourceDisposition,
    expected_prior_source_epoch: Option<u64>,
    frontier: OpenCodeNativeProFrontier,
    already_terminal: bool,
}

fn output_replay_plan(
    context: &OpenCodePublicationContext<'_>,
    sink: &dyn ProOutputSink,
    progress: Option<&ProOutputProgress>,
) -> Result<OutputReplayPlan> {
    let Some(progress) = progress else {
        return Ok(OutputReplayPlan {
            source_epoch: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            expected_prior_source_epoch: None,
            frontier: OpenCodeNativeProFrontier::default(),
            already_terminal: false,
        });
    };
    let same_revision = progress.observed_revision == context.source_revision
        && progress.parser_revision == OPENCODE_NATIVE_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision();
    if same_revision {
        let frontier = progress
            .cursor
            .as_ref()
            .map(decode_output_frontier)
            .transpose()?
            .unwrap_or_default();
        return Ok(OutputReplayPlan {
            source_epoch: progress.source_epoch,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            expected_prior_source_epoch: Some(progress.source_epoch),
            frontier,
            already_terminal: progress.terminal && frontier.terminal,
        });
    }
    Ok(OutputReplayPlan {
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode NativePath output epoch overflowed",
            ))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        expected_prior_source_epoch: Some(progress.source_epoch),
        frontier: OpenCodeNativeProFrontier::default(),
        already_terminal: false,
    })
}

fn output_source_identity(context: &OpenCodePublicationContext<'_>) -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: context.dialect.provider.as_str().to_owned(),
        namespace_id: context.cursor_stream.clone(),
        source_id: format!("opencode-sqlite:{}", context.cursor_path_identity),
    }
}

fn output_unit_key(
    context: &OpenCodePublicationContext<'_>,
    observation: &crate::ProOutputObservation,
) -> String {
    let session = &observation.associations.direct_session_id;
    let native = observation
        .coordinate
        .native_record_id
        .as_deref()
        .unwrap_or("unknown-native-record");
    match observation.coordinate.source_record_subrecord_index {
        Some(0) | None => format!(
            "{}:{session}:{native}:output",
            context.dialect.source_format
        ),
        Some(index) => format!(
            "{}:{session}:{native}:output:subrecord:{index}",
            context.dialect.source_format
        ),
    }
}

fn encode_output_frontier(frontier: OpenCodeNativeProFrontier) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION,
        payload: serde_json::to_vec(&frontier)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    })
}

fn decode_output_frontier(cursor: &OutputNativeCursor) -> Result<OpenCodeNativeProFrontier> {
    if cursor.version != OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "OpenCode NativePath output cursor has an unsupported version".to_owned(),
        ));
    }
    serde_json::from_slice(&cursor.payload)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_sink_error(error: ProOutputSinkError) -> CaptureError {
    CaptureError::InvalidPayload(format!("OpenCode-family output sink failed: {error}"))
}

fn import_missing_source(
    path: &Path,
    store: &mut Store,
    adapter: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    let known = known_route_for_path(path, store, adapter, dialect)?;
    let Some(known) = known else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenCode-family SQLite history database does not exist",
        });
    };
    if options.import_profile.is_replay_only() {
        if let Some(sink) = options.import_profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                "opencode_family_source_missing",
                format!("{} source is unavailable", dialect.display_name),
            ));
        }
        return Ok(ProviderImportSummary::default());
    }
    if known.cursor.route_retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let mut next = known.cursor.clone();
    next.route_retired = true;
    let next_sync = SyncCursor {
        id: known.stored.id,
        team_id: known.stored.team_id.clone(),
        device_id: known.stored.device_id.clone(),
        stream: known.stored.stream.clone(),
        cursor: serde_json::to_string(&next)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        last_synced_at: Some(adapter.imported_at),
        timestamps: timestamps(adapter.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(known.stored.cursor.clone()), next_sync);
    let retirement = ProviderSourceRouteRetirement {
        provider: dialect.provider,
        source_format: dialect.source_format.to_owned(),
        machine_id: adapter.machine_id.clone(),
        locator_identity: known.cursor.locator_identity.clone(),
        cursor_stream: known.stored.stream.clone(),
        expected_canonical_source_identity: known.cursor.canonical_source_identity.clone(),
        expected_source_revision: known.cursor.source_revision.clone(),
        retired_at_ms: adapter.imported_at.timestamp_millis(),
        reason: if adapter
            .source_root
            .as_ref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_PUBLICATION_DOMAIN);
    digest.update(b"retire\0");
    hash_field(&mut digest, known.cursor.locator_identity.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    let publication_id = format!("opencode-nativepath-retire-v1:{:x}", digest.finalize());
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllExpected => {
                    let disposition = group.retire_provider_source_route(&retirement)?;
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    matches!(
                        disposition,
                        ProviderSourceRouteRetirementDisposition::Retired
                    )
                }
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        });
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

struct KnownRoute {
    stored: SyncCursor,
    cursor: OpenCodeNativeStoreCursor,
}

fn known_route_for_path(
    path: &Path,
    store: &Store,
    adapter: &ProviderAdapterContext,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<KnownRoute>> {
    let requested = absolute_lexical_path(path)?;
    let requested_text = requested.display().to_string();
    let direct_path_identity = provider_path_identity(&requested)?;
    let direct_stream = provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.source_format,
        &direct_path_identity,
    );
    if let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &direct_stream)? {
        if let Ok(cursor) = decode_current_cursor(&stored.cursor) {
            if cursor.provider == dialect.provider.as_str()
                && cursor.source_format == dialect.source_format
                && cursor.selected_path == requested
            {
                return Ok(Some(KnownRoute { stored, cursor }));
            }
        }
    }
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != dialect.provider
            || source.descriptor.machine_id != adapter.machine_id
            || source.descriptor.source_format.as_deref() != Some(dialect.source_format)
            || source.descriptor.raw_source_path.as_deref() != Some(requested_text.as_str())
        {
            continue;
        }
        let stream = direct_stream.clone();
        let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &stream)? else {
            continue;
        };
        let Ok(cursor) = decode_current_cursor(&stored.cursor) else {
            continue;
        };
        if cursor.selected_path != requested {
            continue;
        }
        routes.insert(stream, KnownRoute { stored, cursor });
    }
    match routes.len() {
        0 => Ok(None),
        1 => Ok(routes.into_values().next()),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode NativePath found duplicate current routes",
        )),
    }
}

fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn source_revision(dialect: &OpenCodeSqliteDialect, summary: &OpenCodeNativeScanSummary) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_SOURCE_REVISION_DOMAIN);
    hash_field(&mut digest, dialect.provider.as_str().as_bytes());
    hash_field(&mut digest, dialect.source_format.as_bytes());
    hash_field(&mut digest, summary.source_generation_digest.as_bytes());
    hash_field(&mut digest, summary.capability_digest.as_bytes());
    hash_field(&mut digest, summary.semantic_digest.as_bytes());
    hash_field(&mut digest, summary.schema_family.label().as_bytes());
    format!("opencode-family-nativepath-v1:{:x}", digest.finalize())
}

fn prior_relocation_identity(
    store: &Store,
    dialect: &OpenCodeSqliteDialect,
    machine_id: &str,
    source_revision: &str,
    current_path: &str,
) -> Result<Option<String>> {
    let mut candidates = BTreeSet::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != dialect.provider
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(dialect.source_format)
            || source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str)
                != Some(source_revision)
        {
            continue;
        }
        let Some(prior_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        if prior_path == current_path || Path::new(prior_path).exists() {
            continue;
        }
        if let Some(identity) = source.descriptor.source_identity.as_deref() {
            candidates.insert(identity.to_owned());
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "{} relocation matches multiple canonical sources",
            dialect.display_name
        ))),
    }
}

fn sqlite_locator_identity(
    path_identity: &str,
    physical: &OpenCodeNativePhysicalSourceIdentity,
) -> Result<String> {
    let encoded =
        serde_json::to_string(&("opencode-family-sqlite-locator-v1", path_identity, physical))?;
    Ok(encoded)
}

fn sqlite_generation_locator_identity(
    physical_locator_identity: &str,
    source_revision: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-opencode-family-nativepath-generation-locator-v1\0");
    hash_field(&mut digest, physical_locator_identity.as_bytes());
    hash_field(&mut digest, source_revision.as_bytes());
    format!(
        "opencode-family-generation-locator-v1:{:x}",
        digest.finalize()
    )
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    };

    use chrono::{TimeZone, Utc};
    use ctx_history_store::Store;

    use super::{import_opencode_nativepath, ProviderAdapterContext, ProviderImportWorkResult};
    use crate::provider::providers::opencode::native_path::tests::{
        create_family_database, insert_part_event, insert_row_event, insert_session,
    };
    use crate::provider::providers::opencode::native_path::OpenCodeNativeSchemaFamily;
    use crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
    use crate::{
        ImportProfile, OutputSourceIdentity, ProOutputMaterializationPage, ProOutputPageResult,
        ProOutputProgress, ProOutputSink, ProOutputSinkError, ProviderImportOptions,
    };

    fn context() -> ProviderAdapterContext {
        ProviderAdapterContext {
            machine_id: "opencode-nativepath-test".to_owned(),
            source_path: None,
            source_root: None,
            imported_at: Utc.timestamp_millis_opt(1_785_024_000_000).unwrap(),
        }
    }

    #[test]
    fn opencode_nativepath_vertical_core_is_restart_safe_and_appends() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("opencode.db");
        let conn =
            create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
        insert_session(&conn, "session-a", None, 1_785_024_000_000);
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            "message-a",
            "session-a",
            "user",
            1,
            1_785_024_000_001,
            r#"{"role":"user","text":"first"}"#,
        );
        drop(conn);

        let mut store = Store::open(temp.path().join("store.db")).unwrap();
        let first = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(first.imported_sessions, 1);
        assert_eq!(first.imported_events, 1);
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        let events = store.events_for_session(sessions[0].id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].payload["text"], "first");

        let second = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(second.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 1);

        let conn = rusqlite::Connection::open(&source_path).unwrap();
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            "message-b",
            "session-a",
            "assistant",
            2,
            1_785_024_000_002,
            r#"{"role":"assistant","text":"second"}"#,
        );
        drop(conn);
        let appended = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(appended.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(appended.imported_events, 1);
        assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 2);

        let conn = rusqlite::Connection::open(&source_path).unwrap();
        conn.execute(
            "update session_message
             set data = '{\"role\":\"user\",\"text\":\"rewritten\"}'
             where id = 'message-a'",
            [],
        )
        .unwrap();
        drop(conn);
        let rewritten = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(rewritten.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(rewritten.imported_events, 2);
        let events = store.events_for_session(sessions[0].id).unwrap();
        assert_eq!(events.len(), 4);
        assert!(events
            .iter()
            .any(|event| event.payload["text"] == "rewritten"));

        let conn = rusqlite::Connection::open(&source_path).unwrap();
        insert_row_event(
            &conn,
            OpenCodeNativeSchemaFamily::SessionMessageSeq,
            "message-c",
            "session-a",
            "assistant",
            3,
            1_785_024_000_003,
            r#"{"role":"assistant","text":"after rewrite"}"#,
        );
        drop(conn);
        let after_rewrite = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            ProviderImportOptions::default(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(
            after_rewrite.work_result(),
            ProviderImportWorkResult::Changed
        );
        assert_eq!(after_rewrite.imported_events, 1);
        assert_eq!(store.events_for_session(sessions[0].id).unwrap().len(), 5);
    }

    #[test]
    fn opencode_nativepath_vertical_retires_a_deleted_exact_route() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("opencode.db");
        let conn =
            create_family_database(&source_path, OpenCodeNativeSchemaFamily::SessionMessageSeq);
        insert_session(&conn, "session-a", None, 1_785_024_000_000);
        drop(conn);
        let mut store = Store::open(temp.path().join("store.db")).unwrap();
        let options = ProviderImportOptions {
            inventory_observation_token: Some("exact-root-scan-1".to_owned()),
            ..ProviderImportOptions::default()
        };
        import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            options.clone(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        std::fs::remove_file(&source_path).unwrap();

        let retired = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            options.clone(),
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(retired.work_result(), ProviderImportWorkResult::Changed);
        let repeated = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            options,
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(repeated.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn opencode_nativepath_vertical_commits_core_before_independent_output_replay() {
        const OUTPUT_SENTINEL: &str = "OPENCODE_NATIVEPATH_OUTPUT_SENTINEL";

        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("opencode.db");
        let store_path = temp.path().join("store.db");
        let conn = create_family_database(&source_path, OpenCodeNativeSchemaFamily::MessagePart);
        insert_session(&conn, "session-a", None, 1_785_024_000_000);
        insert_part_event(
            &conn,
            "message-a",
            "part-a",
            "session-a",
            "assistant",
            "text",
            1_785_024_000_001,
            r#"{"type":"text","text":"safe core text"}"#,
        );
        insert_part_event(
            &conn,
            "message-output",
            "part-output",
            "session-a",
            "assistant",
            "tool_result",
            1_785_024_000_002,
            &serde_json::json!({
                "type": "tool_result",
                "state": {"status": "completed", "output": OUTPUT_SENTINEL}
            })
            .to_string(),
        );
        drop(conn);

        let mut store = Store::open(&store_path).unwrap();
        let failing = Arc::new(FailingSink::default());
        let options = ProviderImportOptions {
            import_profile: ImportProfile::CoreAndPro(failing.clone()),
            ..ProviderImportOptions::default()
        };
        let summary = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            options,
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
        assert!(failing.behind.load(Ordering::SeqCst));
        let session = store.list_sessions().unwrap().pop().unwrap();
        let core_debug = format!("{:?}", store.events_for_session(session.id).unwrap());
        assert!(!core_debug.contains(OUTPUT_SENTINEL));

        let sink = Arc::new(RecordingSink::new(store_path));
        let replay_options = ProviderImportOptions {
            import_profile: ImportProfile::ProReplayOnly(sink.clone()),
            ..ProviderImportOptions::default()
        };
        let replay = import_opencode_nativepath(
            &source_path,
            &mut store,
            context(),
            replay_options,
            &OPENCODE_SQLITE_DIALECT,
        )
        .unwrap();
        assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
        assert!(sink.saw_committed_core.load(Ordering::SeqCst));
        assert!(sink.pages.load(Ordering::SeqCst) > 0);
        assert_eq!(sink.outputs.load(Ordering::SeqCst), 1);
        assert_eq!(
            sink.contents.lock().unwrap().as_slice(),
            [OUTPUT_SENTINEL.as_bytes()]
        );
        assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
    }

    struct RecordingSink {
        store_path: std::path::PathBuf,
        progress: Mutex<Option<ProOutputProgress>>,
        contents: Mutex<Vec<Vec<u8>>>,
        pages: AtomicUsize,
        outputs: AtomicUsize,
        saw_committed_core: AtomicBool,
    }

    impl RecordingSink {
        fn new(store_path: std::path::PathBuf) -> Self {
            Self {
                store_path,
                progress: Mutex::new(None),
                contents: Mutex::new(Vec::new()),
                pages: AtomicUsize::new(0),
                outputs: AtomicUsize::new(0),
                saw_committed_core: AtomicBool::new(false),
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "opencode-nativepath-test-materializer-v1"
        }

        fn observe_source(
            &self,
            _source: &OutputSourceIdentity,
        ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
            Ok(self.progress.lock().unwrap().clone())
        }

        fn materialize_page(
            &self,
            page: ProOutputMaterializationPage,
        ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
            let core = Store::open_read_only(&self.store_path)
                .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
            if !core
                .list_sessions()
                .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                .is_empty()
            {
                self.saw_committed_core.store(true, Ordering::SeqCst);
            }
            self.pages.fetch_add(1, Ordering::SeqCst);
            self.outputs
                .fetch_add(page.observations.len(), Ordering::SeqCst);
            self.contents.lock().unwrap().extend(
                page.observations
                    .iter()
                    .map(|observation| observation.content.clone()),
            );
            let committed_cursor = page.next_safe_cursor.clone();
            *self.progress.lock().unwrap() = Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            Ok(ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor,
                accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
                materialized_facts: 0,
                replayed: false,
            })
        }
    }

    #[derive(Default)]
    struct FailingSink {
        behind: AtomicBool,
    }

    impl ProOutputSink for FailingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "opencode-nativepath-failing-materializer-v1"
        }

        fn observe_source(
            &self,
            _source: &OutputSourceIdentity,
        ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
            Err(ProOutputSinkError::new("test_failure", "expected failure"))
        }

        fn materialize_page(
            &self,
            _page: ProOutputMaterializationPage,
        ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
            unreachable!("observe_source fails before materialization")
        }

        fn mark_behind(&self, _error: ProOutputSinkError) {
            self.behind.store(true, Ordering::SeqCst);
        }
    }
}
