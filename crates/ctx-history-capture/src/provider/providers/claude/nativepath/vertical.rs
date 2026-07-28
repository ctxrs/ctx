#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::sync::{atomic::AtomicUsize, Arc};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session,
    SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    NativePathPublicationGroup, NativePathRetainedSourceEntities, NativePathSourceEntityFrontier,
    NativePathSourceEntityKind, NativePathSourceGenerationKey, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_GROUP_PAGES, NATIVE_PATH_MAX_MUTATION_UNITS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    complete_content::{
        attach_verified_content_locator, verified_content_address_supported,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole,
    },
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_path_identity, provider_scoped_source_identity_key, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_sync_metadata, timestamps,
            CertifiedProviderCursor,
        },
        normalization::provider_role,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ClaudeProjectsImportOptions,
    ImportProfile, OutputNativeCursor, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputProgress, ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition,
    ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult, Result,
    CLAUDE_PROJECTS_SOURCE_FORMAT,
};

use super::super::complete_content::claude_nativepath_message_hash_payload;
use super::{
    discover_projects, revalidate_discovered_source, ClaudeEventKind, ClaudeNativeOwnedPage,
    ClaudeNativePage, ClaudeNativePathError, ClaudeNativeProOutputPage, ClaudeNativeProfile,
    ClaudeNativeScanner, ClaudeRetainedRow, ClaudeSessionMetadata, DiscoveredClaudeSession,
    ParseCheckpoint, SessionLayout,
};

mod cursor;
mod entities;
mod output;
mod preparation;
mod publication;

use cursor::*;
use entities::*;
use output::*;
use preparation::*;
use publication::*;

#[cfg(test)]
mod tests;

const CLAUDE_STORE_CURSOR_VERSION: u32 = 1;
const CLAUDE_OUTPUT_CURSOR_VERSION: u32 = 1;
const CLAUDE_OUTPUT_PARSER_REVISION: &str = "claude-nativepath-output-v5";
const CLAUDE_PUBLICATION_DOMAIN: &[u8] = b"ctx-claude-nativepath-publication-v1\0";
const CLAUDE_GROUP_PUBLICATION_DOMAIN: &[u8] = b"ctx-claude-nativepath-group-publication-v1\0";
const CLAUDE_GROUP_MAX_SOURCES: usize = 64;
const CLAUDE_CORE_PREPARATION_MAX_WORKERS: usize = 16;
const CLAUDE_CORE_PREPARATION_QUEUE_MAX_SOURCES: usize = CLAUDE_GROUP_MAX_SOURCES;
// Leave half of the Store's exact 8 MiB bind budget for source, route, cursor,
// and SQLite row encodings that are not part of the retained page certificate.
const CLAUDE_GROUP_MAX_RETAINED_PAGE_BYTES: usize = 4 * 1024 * 1024;
const CLAUDE_RETIREMENT_UNITS_PER_PAGE: usize = 512;
const CLAUDE_RETIREMENT_ACCOUNTING_BYTES: usize = 256 * 1024;
const CLAUDE_RELEASED_CAPTURE_REVISION: u32 = 1;
const CLAUDE_RELEASED_POLICY_REVISION: u32 = 6;

#[derive(Clone)]
struct KnownClaudeRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    provider_cursor: String,
}

pub(crate) fn import_claude_nativepath_projects(
    path: &Path,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let known = known_routes(store, &options.machine_id, &configured_source_root)?;
    let discovery = match discover_projects(path) {
        Ok(discovery) => Some(discovery),
        Err(ClaudeNativePathError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => return Err(map_native_error(error)),
    };

    if options.import_profile.is_replay_only() {
        if let Some(sink) = options.import_profile.sink() {
            if let Some(discovery) = discovery.as_ref() {
                for source in &discovery.sessions {
                    replay_source_outputs(source, &configured_source_root, sink.as_ref());
                }
            } else {
                sink.mark_behind(ProOutputSinkError::new(
                    "claude_nativepath_output_root_missing",
                    "Claude projects root is unavailable for output replay",
                ));
            }
        }
        return Ok(ProviderImportSummary::default());
    }

    let Some(discovery) = discovery else {
        if known.is_empty() {
            return invalid_root(path);
        }
        return retire_routes(
            store,
            &options.machine_id,
            options.imported_at,
            &known,
            &BTreeSet::new(),
            ProviderSourceRouteRetirementReason::RootMissing,
        );
    };
    if discovery.sessions.is_empty() {
        if known.is_empty() {
            return invalid_root(path);
        }
        return retire_routes(
            store,
            &options.machine_id,
            options.imported_at,
            &known,
            &BTreeSet::new(),
            ProviderSourceRouteRetirementReason::SourceMissing,
        );
    }
    let authoritative_inventory = discovery.root.is_dir();

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut committed_groups = 0_usize;
        if matches!(options.import_profile, ImportProfile::CoreOnly) && authoritative_inventory {
            summary.merge_from(import_core_sources_grouped(
                store,
                &committed_store,
                &bulk_guard,
                &discovery.sessions,
                &configured_source_root,
                &options,
                &mut committed_groups,
            )?);
        } else {
            for source in &discovery.sessions {
                let source_summary = import_source(
                    store,
                    &committed_store,
                    &bulk_guard,
                    source,
                    &configured_source_root,
                    &options,
                    &mut committed_groups,
                )?;
                summary.merge_from(source_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && committed_groups != 0
                {
                    summary.work_remaining = true;
                    break;
                }
            }
        }
        if !summary.work_remaining && authoritative_inventory {
            discovery.revalidate_inventory().map_err(map_native_error)?;
            let live = discovery
                .sessions
                .iter()
                .map(|source| source.canonical_path.clone())
                .collect::<BTreeSet<_>>();
            summary.merge_from(retire_routes_with_guard(
                store,
                &bulk_guard,
                &options.machine_id,
                options.imported_at,
                &known,
                &live,
                ProviderSourceRouteRetirementReason::SourceMissing,
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

#[allow(clippy::too_many_arguments)]
fn import_source(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    committed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(&source.canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Claude,
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    let mut prior = stored
        .as_ref()
        .map(|cursor| decode_store_cursor(&cursor.cursor))
        .transpose()?
        .and_then(|cursor| match cursor {
            ClaudeStoredCursor::Native(cursor) => Some(cursor),
            ClaudeStoredCursor::Released(_) => None,
        });
    let mut summary = ProviderImportSummary::default();
    let mut expected_cursor = stored.as_ref().map(|cursor| cursor.cursor.clone());
    while let Some(ClaudeGenerationPhase::Retiring { after }) =
        prior.as_ref().map(|cursor| cursor.generation_phase.clone())
    {
        let cursor = prior.as_ref().ok_or(CaptureError::SystemInvariant(
            "Claude retirement cursor disappeared",
        ))?;
        let (next, changed) = publish_claude_retirement_page(
            store,
            bulk_guard,
            source,
            options,
            &locator_identity,
            &stream,
            cursor,
            after.as_ref(),
            expected_cursor.clone(),
        )?;
        if changed {
            *committed_groups = committed_groups.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        prior = Some(next);
        expected_cursor = store
            .get_sync_cursor(None, &options.machine_id, &stream)?
            .map(|cursor| cursor.cursor);
        if changed && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = prior.as_ref().is_some_and(|cursor| {
                !matches!(cursor.generation_phase, ClaudeGenerationPhase::Live)
            });
            return Ok(summary);
        }
    }
    let sink = options.import_profile.sink().map(|sink| sink.as_ref());
    let mut output = sink.map(|sink| output_state(source, source_root, sink));
    let single_scan = matches!(options.import_profile, ImportProfile::CoreAndPro(_))
        && output
            .as_ref()
            .is_some_and(|output| output_is_aligned(prior.as_ref(), output));
    let profile = if single_scan {
        ClaudeNativeProfile::CoreAndPro
    } else {
        ClaudeNativeProfile::CoreOnly
    };
    let scanner_previous = prior.as_ref().map(|cursor| {
        let mut checkpoint = cursor.checkpoint.clone();
        if single_scan {
            if let Some(output_checkpoint) =
                output.as_ref().and_then(|output| output.previous.as_ref())
            {
                copy_pro_lane(&mut checkpoint, output_checkpoint);
            }
        }
        checkpoint
    });
    let mut scanner = ClaudeNativeScanner::new(source.clone(), scanner_previous.as_ref(), profile)
        .map_err(map_native_error)?;
    let mut cumulative = prior.clone();
    let mut pending_output: Option<(Box<ClaudeNativeProOutputPage>, ParseCheckpoint)> = None;
    let mut emitted_core = false;

    while let Some(owned) = scanner.next_page().map_err(map_native_error)? {
        match owned {
            ClaudeNativeOwnedPage::Pro(page) => {
                let checkpoint = scanner.checkpoint_at(&page.next_safe_frontier, page.terminal);
                if pending_output.replace((page, checkpoint)).is_some() {
                    return Err(CaptureError::SystemInvariant(
                        "Claude NativePath retained more than one paired output page",
                    ));
                }
            }
            ClaudeNativeOwnedPage::Core(page) => {
                emitted_core = true;
                let checkpoint = scanner.checkpoint_at(&page.next_safe_frontier, page.terminal);
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    source,
                    source_root,
                    options,
                    &stream,
                    page.as_ref(),
                    &checkpoint,
                    cumulative.as_ref(),
                )?;
                *committed_groups = committed_groups.saturating_add(1);
                cumulative = Some(next_cursor_state(
                    source,
                    cumulative.as_ref(),
                    page.as_ref(),
                    checkpoint,
                    &source_revision(source, options.inventory_observation_token.as_deref()),
                ));
                summary.merge_from(page_summary);

                if let Some((output_page, output_checkpoint)) = pending_output.take() {
                    if output_page.expected_frontier != page.expected_frontier
                        || output_page.next_safe_frontier != page.next_safe_frontier
                    {
                        return Err(CaptureError::SystemInvariant(
                            "Claude paired Core and output frontiers diverged",
                        ));
                    }
                    if let (Some(sink), Some(state)) = (sink, output.as_mut()) {
                        materialize_output_page(
                            source,
                            sink,
                            state,
                            *output_page,
                            output_checkpoint,
                        );
                    }
                }
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }
    }
    if pending_output.is_some() {
        return Err(CaptureError::SystemInvariant(
            "Claude output page had no matching Core page",
        ));
    }
    let finished = scanner.finish().map_err(map_native_error)?;
    if !finished.source_certified {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if !emitted_core {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
        if summary.work_result() != ProviderImportWorkResult::Changed {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    if matches!(options.import_profile, ImportProfile::CoreAndPro(_)) && !single_scan {
        if let Some(sink) = sink {
            replay_source_outputs(source, source_root, sink);
        }
    }
    Ok(summary)
}

fn known_routes(store: &Store, machine_id: &str, root: &Path) -> Result<Vec<KnownClaudeRoute>> {
    let root = root.display().to_string();
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Claude
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(CLAUDE_PROJECTS_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(root.as_str())
        {
            continue;
        }
        let (Some(raw_path), Some(canonical_source_identity), Some(source_revision)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let stored_cursor = decode_store_cursor(&current_cursor.cursor)?;
        let provider_cursor = match &stored_cursor {
            ClaudeStoredCursor::Native(_) => {
                decode_native_path_committed_cursor(&current_cursor.cursor)?
                    .provider_cursor()
                    .to_owned()
            }
            ClaudeStoredCursor::Released(provider_cursor) => provider_cursor.clone(),
        };
        if matches!(
            &stored_cursor,
            ClaudeStoredCursor::Native(cursor) if cursor.source_id != source.id
        ) {
            continue;
        }
        let route = KnownClaudeRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Claude persisted duplicate current routes",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known: &[KnownClaudeRoute],
    live: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let guard = store.begin_event_search_bulk_mode()?;
    let operation =
        retire_routes_with_guard(store, &guard, machine_id, retired_at, known, live, reason);
    let finish = store
        .finish_event_search_bulk_mode(&guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn retire_routes_with_guard(
    store: &Store,
    guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known: &[KnownClaudeRoute],
    live: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known.iter().filter(|route| !live.contains(&route.path)) {
        let transition = NativePathCursorTransition::new(
            Some(route.current_cursor.cursor.clone()),
            provider_sync_cursor(
                machine_id,
                route.current_cursor.stream.clone(),
                route.provider_cursor.clone(),
                retired_at,
            ),
        );
        let retirement = ProviderSourceRouteRetirement {
            provider: CaptureProvider::Claude,
            source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
            machine_id: machine_id.to_owned(),
            locator_identity: route.locator_identity.clone(),
            cursor_stream: route.current_cursor.stream.clone(),
            expected_canonical_source_identity: route.canonical_source_identity.clone(),
            expected_source_revision: route.source_revision.clone(),
            retired_at_ms: retired_at.timestamp_millis(),
            reason,
        };
        let admission = store.admit_event_search_bulk_group(guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let publication_id = retirement_publication_id(&retirement);
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
        if changed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(summary)
}

fn claude_generation_key(
    options: &ClaudeProjectsImportOptions,
    canonical_source_identity: &str,
    locator_identity: &str,
    stream: &str,
    cursor: &ClaudeStoreCursor,
) -> Result<NativePathSourceGenerationKey> {
    let source_revision =
        cursor
            .generation_source_revision
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "Claude source generation has no revision",
            ))?;
    Ok(NativePathSourceGenerationKey {
        provider: CaptureProvider::Claude,
        source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        locator_identity: locator_identity.to_owned(),
        cursor_stream: stream.to_owned(),
        generation_id: format!(
            "claude-nativepath-v1:{}:{source_revision}",
            cursor.source_generation
        ),
        source_revision,
    })
}

fn dedupe_retained(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.session_edge_ids.sort_unstable();
    retained.session_edge_ids.dedup();
    retained.run_ids.sort_unstable();
    retained.run_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
    retained.file_touch_ids.sort_unstable();
    retained.file_touch_ids.dedup();
}

fn stable_native_event_index(native_record_id: &str, subrecord_index: u64) -> u64 {
    let mut digest = Sha256::new();
    digest.update(b"ctx-claude-nativepath-native-event-index-v1\0");
    digest.update(native_record_id.as_bytes());
    digest.update([0]);
    digest.update(subrecord_index.to_be_bytes());
    let digest = digest.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix)
}

fn stable_route_source_id(source: &DiscoveredClaudeSession) -> Uuid {
    let mut material = Vec::new();
    material.extend_from_slice(b"claude-nativepath-stable-source-v1\0");
    material.extend_from_slice(source.key.provider_session_id().as_bytes());
    material.push(0);
    material.extend_from_slice(source.canonical_path.as_os_str().as_encoded_bytes());
    stable_capture_uuid(
        &format!(
            "claude-nativepath-stable-source-v1:{:x}",
            Sha256::digest(material)
        ),
        "source",
    )
}

fn publication_id(
    source: &DiscoveredClaudeSession,
    page: &ClaudeNativePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CLAUDE_PUBLICATION_DOMAIN);
    digest.update(source.canonical_path.as_os_str().as_encoded_bytes());
    digest.update(page.expected_frontier.complete_offset.to_be_bytes());
    digest.update(page.next_safe_frontier.complete_offset.to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("claude-nativepath-v1:{:x}", digest.finalize())
}

fn generation_retirement_publication_id(
    source: &DiscoveredClaudeSession,
    cursor: &ClaudeStoreCursor,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-claude-nativepath-retirement-v1\0");
    digest.update(source.canonical_path.as_os_str().as_encoded_bytes());
    digest.update(cursor.source_generation.to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("claude-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn group_publication_id(sources: &[PreparedClaudeCoreSource]) -> String {
    let mut digest = Sha256::new();
    digest.update(CLAUDE_GROUP_PUBLICATION_DOMAIN);
    digest.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        let path = source.source.canonical_path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update(source.page.expected_frontier.complete_offset.to_be_bytes());
        digest.update(source.page.next_safe_frontier.complete_offset.to_be_bytes());
        digest.update(source.transition.next().cursor.as_bytes());
    }
    format!("claude-nativepath-group-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-claude-nativepath-retirement-v1\0");
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("claude-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn invalid_root(path: &Path) -> Result<ProviderImportSummary> {
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Claude projects root contains no supported JSONL sessions",
    })
}

fn map_native_error(error: ClaudeNativePathError) -> CaptureError {
    match error {
        ClaudeNativePathError::Io { source, .. } => CaptureError::Io(source),
        ClaudeNativePathError::StaleDiscovery { .. }
        | ClaudeNativePathError::SourceChanged { .. }
        | ClaudeNativePathError::InventoryChanged { .. } => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
