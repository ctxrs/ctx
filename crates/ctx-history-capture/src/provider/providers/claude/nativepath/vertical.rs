use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
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
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
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

use super::{
    discover_projects, revalidate_discovered_source, ClaudeEventKind, ClaudeNativeOwnedPage,
    ClaudeNativePage, ClaudeNativePathError, ClaudeNativeProOutputPage, ClaudeNativeProfile,
    ClaudeNativeScanner, ClaudeRetainedRow, ClaudeSessionMetadata, DiscoveredClaudeSession,
    ParseCheckpoint, SessionLayout,
};

const CLAUDE_STORE_CURSOR_VERSION: u32 = 1;
const CLAUDE_OUTPUT_CURSOR_VERSION: u32 = 1;
const CLAUDE_OUTPUT_PARSER_REVISION: &str = "claude-nativepath-output-v5";
const CLAUDE_PUBLICATION_DOMAIN: &[u8] = b"ctx-claude-nativepath-publication-v1\0";
const CLAUDE_RELEASED_CAPTURE_REVISION: u32 = 1;
const CLAUDE_RELEASED_POLICY_REVISION: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeStoreCursor {
    version: u32,
    source_generation: u64,
    source_id: Uuid,
    checkpoint: ParseCheckpoint,
    session: ClaudeSessionMetadata,
    accepted_rows: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedClaudeParserCheckpoint {
    session: Option<ReleasedClaudeSessionCheckpoint>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedClaudeSessionCheckpoint {
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    version: Option<String>,
    git_branch: Option<String>,
}

// Both variants are persisted compatibility shapes; boxing would add allocation
// to every cursor decode for a release-time size-only concern.
#[allow(clippy::large_enum_variant)]
enum ClaudeStoredCursor {
    Native(ClaudeStoreCursor),
    Released(String),
}

#[derive(Clone)]
struct KnownClaudeRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    provider_cursor: String,
}

struct ClaudeOutputState {
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    previous: Option<ParseCheckpoint>,
    source_epoch: u64,
    disposition: ProOutputSourceDisposition,
    expected_source_epoch: Option<u64>,
    expected_cursor: Option<OutputNativeCursor>,
    enabled: bool,
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
    let discovery = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
        Ok(_) => Some(discover_projects(path).map_err(map_native_error)?),
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
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && committed_groups != 0
            {
                summary.work_remaining = true;
                break;
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
    let prior = stored
        .as_ref()
        .map(|cursor| decode_store_cursor(&cursor.cursor))
        .transpose()?
        .and_then(|cursor| match cursor {
            ClaudeStoredCursor::Native(cursor) => Some(cursor),
            ClaudeStoredCursor::Released(_) => None,
        });
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
    let mut summary = ProviderImportSummary::default();
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
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    if matches!(options.import_profile, ImportProfile::CoreAndPro(_)) && !single_scan {
        if let Some(sink) = sink {
            replay_source_outputs(source, source_root, sink);
        }
    }
    Ok(summary)
}

fn next_cursor_state(
    source: &DiscoveredClaudeSession,
    previous: Option<&ClaudeStoreCursor>,
    page: &ClaudeNativePage,
    checkpoint: ParseCheckpoint,
) -> ClaudeStoreCursor {
    let reset = page.expected_frontier.complete_offset == 0;
    let source_generation = if reset {
        previous.map_or(0, |cursor| cursor.source_generation.saturating_add(1))
    } else {
        previous.map_or(0, |cursor| cursor.source_generation)
    };
    let source_id = if reset {
        generation_source_id(source, source_generation, &checkpoint)
    } else {
        previous.map_or_else(
            || generation_source_id(source, source_generation, &checkpoint),
            |cursor| cursor.source_id,
        )
    };
    let accepted_rows = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.accepted_rows)
    }
    .saturating_add(u64::try_from(page.rows.len()).unwrap_or(u64::MAX));
    let accepted_file_touches = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.accepted_file_touches)
    }
    .saturating_add(
        page.rows
            .iter()
            .filter_map(|row| row.tool_call.as_ref())
            .map(|call| u64::try_from(call.file_touches.len()).unwrap_or(u64::MAX))
            .sum::<u64>(),
    );
    let rejected_records = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.rejected_records)
    }
    .saturating_add(page.rejected_records);
    ClaudeStoreCursor {
        version: CLAUDE_STORE_CURSOR_VERSION,
        source_generation,
        source_id,
        checkpoint,
        session: page.session.clone(),
        accepted_rows,
        accepted_file_touches,
        rejected_records,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    options: &ClaudeProjectsImportOptions,
    stream: &str,
    page: &ClaudeNativePage,
    checkpoint: &ParseCheckpoint,
    previous: Option<&ClaudeStoreCursor>,
) -> Result<ProviderImportSummary> {
    revalidate_discovered_source(source).map_err(map_native_error)?;
    let cursor = next_cursor_state(source, previous, page, checkpoint.clone());
    let stored = store.get_sync_cursor(None, &options.machine_id, stream)?;
    let next = provider_sync_cursor(
        &options.machine_id,
        stream.to_owned(),
        encode_store_cursor(&cursor)?,
        options.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = publication_id(source, page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary {
                skipped_events: page.rows.len(),
                skipped: page.rows.len(),
                ..ProviderImportSummary::default()
            };
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let raw_path = source.canonical_path.display().to_string();
    let source_root = source_root.display().to_string();
    let locator_identity = provider_path_identity(&source.canonical_path)?;
    let proposed_source_identity = stable_capture_uuid(
        &format!(
            "claude-nativepath-session:{}",
            source.key.provider_session_id()
        ),
        "provider-source-root",
    )
    .to_string();
    let revision = source_revision(source, options.inventory_observation_token.as_deref());
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Claude,
            source_format: CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned(),
            machine_id: options.machine_id.clone(),
            locator_identity,
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_path.clone()),
            source_revision: revision.clone(),
            observed_at_ms: options.imported_at.timestamp_millis(),
        })?;
    let provider_session_id = source.key.provider_session_id();
    let source_id = cursor.source_id;
    let session_id = provider_session_uuid(CaptureProvider::Claude, &provider_session_id);
    let parent_id = source
        .key
        .parent_provider_session_id()
        .map(|parent| provider_session_uuid(CaptureProvider::Claude, parent));
    let started_at = page
        .session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .unwrap_or(options.imported_at);
    let capture_source = capture_source(
        source,
        source_id,
        &options.machine_id,
        source_root.as_str(),
        &resolution.canonical_source_identity,
        &revision,
        &page.session,
        started_at,
        options.imported_at,
    );
    group.upsert_capture_source(&capture_source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    if let Some(parent_id) = parent_id {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                parent_id,
                source_id,
                source.key.root_session_id.as_str(),
                options,
            ))?;
        }
    }
    let session = canonical_session(
        source,
        source_id,
        session_id,
        parent_id,
        &page.session,
        started_at,
        options,
    );
    let session_existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    let mut summary = ProviderImportSummary::default();
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_id {
        let edge = relationship_edge(source_id, session_id, parent_id, options);
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    for row in &page.rows {
        publish_row(
            committed_store,
            &mut group,
            source_id,
            &session,
            row,
            options,
            &mut summary,
        )?;
    }
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(rejection.source_record_ordinal)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.diagnostic.clone(),
        });
    }
    revalidate_discovered_source(source).map_err(map_native_error)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

// This constructor mirrors the persisted CaptureSource contract; bundling its
// fields would obscure that boundary without reducing caller complexity.
#[allow(clippy::too_many_arguments)]
fn capture_source(
    source: &DiscoveredClaudeSession,
    source_id: Uuid,
    machine_id: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
    metadata: &ClaudeSessionMetadata,
    started_at: DateTime<Utc>,
    imported_at: DateTime<Utc>,
) -> CaptureSource {
    let provider_session_id = source.key.provider_session_id();
    let raw_path = source.canonical_path.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: machine_id.to_owned(),
            process_id: None,
            cwd: metadata.cwd.clone(),
            raw_source_path: Some(raw_path.clone()),
            source_format: Some(CLAUDE_PROJECTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(provider_session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Claude,
                    &source.key.provider_session_id(),
                    CLAUDE_PROJECTS_SOURCE_FORMAT,
                    Some(&raw_path),
                ),
                "imported_at": imported_at,
                "version": metadata.version,
                "git_branch": metadata.git_branch,
            }),
        ),
    }
}

fn canonical_session(
    source: &DiscoveredClaudeSession,
    source_id: Uuid,
    session_id: Uuid,
    parent_id: Option<Uuid>,
    metadata: &ClaudeSessionMetadata,
    started_at: DateTime<Utc>,
    options: &ClaudeProjectsImportOptions,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Claude,
        external_session_id: Some(source.key.provider_session_id()),
        external_agent_id: source.key.agent_id.clone(),
        agent_type: if source.layout == SessionLayout::Primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(
            if source.layout == SessionLayout::Primary {
                "primary"
            } else {
                "subagent"
            }
            .to_owned(),
        ),
        is_primary: source.layout == SessionLayout::Primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.key.provider_session_id(),
                "parent_provider_session_id": source.key.parent_provider_session_id(),
                "root_provider_session_id": source.key.root_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": options.imported_at,
                "version": metadata.version,
                "git_branch": metadata.git_branch,
            }),
        ),
    }
}

fn relationship_placeholder(
    id: Uuid,
    source_id: Uuid,
    external_session_id: &str,
    options: &ClaudeProjectsImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Claude,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: options.imported_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    source_id: Uuid,
    session_id: Uuid,
    parent_id: Uuid,
    options: &ClaudeProjectsImportOptions,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!("claude-nativepath:{session_id}:parent:{parent_id}"),
            "session-edge",
        ),
        from_session_id: session_id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({"source_format": CLAUDE_PROJECTS_SOURCE_FORMAT}),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
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
fn publish_row(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_id: Uuid,
    session: &Session,
    row: &ClaudeRetainedRow,
    options: &ClaudeProjectsImportOptions,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_event_index = if row.identity.source_subrecord_index == 0 {
        row.identity.source_record_ordinal
    } else {
        row.identity
            .source_record_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|index| index.checked_add(row.identity.source_subrecord_index))
            .map(|index| index | (1_u64 << 63))
            .ok_or(CaptureError::SystemInvariant(
                "Claude provider event identity index overflowed",
            ))?
    };
    let event_type = match row.kind {
        ClaudeEventKind::Message => EventType::Message,
        ClaudeEventKind::Summary => EventType::Summary,
        ClaudeEventKind::Notice => EventType::Notice,
        ClaudeEventKind::ToolCall => EventType::ToolCall,
        ClaudeEventKind::ToolOutput => EventType::ToolOutput,
    };
    let role = row
        .role
        .as_deref()
        .map(|role| provider_role(Some(role)))
        .or_else(|| {
            (event_type == EventType::ToolCall || event_type == EventType::ToolOutput)
                .then_some(EventRole::Tool)
        });
    let payload = json!({
        "provider": CaptureProvider::Claude.as_str(),
        "provider_session_id": session.external_session_id,
        "provider_event_index": provider_event_index,
        "native_record_id": row.native_record_id,
        "parent_native_record_id": row.parent_native_record_id,
        "kind": row.kind,
        "body": row.body,
        "body_sha256": row.body_sha256.map(hex),
        "tool_call": row.tool_call,
        "sparse_output": row.sparse_output,
        "artifacts": [],
    });
    let (event_hash, authority) = row.native_record_id.as_ref().map_or_else(
        || {
            Ok::<_, CaptureError>((
                crate::compute_payload_hash(&payload)?,
                ProviderEventHashAuthority::NormalizedPayloadFallback,
            ))
        },
        |native_id| {
            Ok((
                if row.identity.source_subrecord_index == 0 {
                    native_id.clone()
                } else {
                    format!("{native_id}:{}", row.identity.source_subrecord_index)
                },
                ProviderEventHashAuthority::ProviderSupplied,
            ))
        },
    )?;
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Claude,
        provider_session_id,
        source_id,
        provider_event_index,
        provider_event_index,
        &event_hash,
        None,
        Some(row.identity.source_record_ordinal),
        true,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let occurred_at = row
        .occurred_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .unwrap_or(options.imported_at);
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type,
        role,
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_sequence_index": provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": match authority {
                    ProviderEventHashAuthority::ProviderSupplied => "provider_supplied",
                    ProviderEventHashAuthority::NormalizedPayloadFallback => "normalized_payload_fallback",
                },
                "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_record_ordinal": row.identity.source_record_ordinal,
                "source_record_subrecord_index": row.identity.source_subrecord_index,
                "byte_start": row.locator.byte_start,
                "byte_end_exclusive": row.locator.byte_end_exclusive,
                "line_number": row.locator.line_number,
                "imported_at": options.imported_at,
            }),
        ),
    };
    if group.reconcile_provider_event(&event, authority)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    if let Some(call) = &row.tool_call {
        for (touch_index, touch) in call.file_touches.iter().enumerate() {
            let touch_index = u64::try_from(touch_index)
                .map_err(|_| CaptureError::SystemInvariant("Claude file-touch index overflowed"))?;
            // Retain the historical packed identity while it fits. Compound
            // event indices intentionally use the full-width event+touch key.
            let provider_touch_index = provider_event_index
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_index))
                .unwrap_or(touch_index);
            let id = provider_file_touch_import_id(
                committed_store,
                CaptureProvider::Claude,
                provider_session_id,
                source_id,
                Some(provider_event_index),
                provider_touch_index,
                true,
            )?;
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(event.id),
                vcs_workspace_id: None,
                path: touch.path.clone(),
                change_kind: Some(FileChangeKind::Unknown),
                old_path: touch.previous_path.clone(),
                line_count_delta: None,
                confidence: Confidence::Explicit,
                timestamps: timestamps(occurred_at),
                source_id: Some(source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": CaptureProvider::Claude.as_str(),
                        "provider_session_id": provider_session_id,
                        "provider_touch_index": provider_touch_index,
                        "provider_event_index": provider_event_index,
                        "source_event_touch_index": touch_index,
                        "source_record_ordinal": row.identity.source_record_ordinal,
                        "source_record_subrecord_index": row.identity.source_subrecord_index,
                        "source_format": CLAUDE_PROJECTS_SOURCE_FORMAT,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

fn output_state(
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) -> ClaudeOutputState {
    let identity = OutputSourceIdentity {
        provider: CaptureProvider::Claude.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: provider_path_identity(&source.canonical_path)
            .unwrap_or_else(|_| source.canonical_path.display().to_string()),
    };
    let progress = match sink.observe_source(&identity) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return ClaudeOutputState {
                source: identity,
                progress: None,
                previous: None,
                source_epoch: 0,
                disposition: ProOutputSourceDisposition::NewSource,
                expected_source_epoch: None,
                expected_cursor: None,
                enabled: false,
            };
        }
    };
    let previous = progress.as_ref().and_then(|progress| {
        (progress.parser_revision == CLAUDE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision())
        .then_some(progress.cursor.as_ref())
        .flatten()
        .filter(|cursor| cursor.version == CLAUDE_OUTPUT_CURSOR_VERSION)
        .and_then(|cursor| serde_json::from_slice::<ParseCheckpoint>(&cursor.payload).ok())
    });
    let resumable = progress.is_none() || previous.is_some();
    let source_epoch = progress.as_ref().map_or(0, |progress| {
        if resumable {
            progress.source_epoch
        } else {
            progress.source_epoch.saturating_add(1)
        }
    });
    ClaudeOutputState {
        source: identity,
        previous,
        source_epoch,
        disposition: if progress.is_none() {
            ProOutputSourceDisposition::NewSource
        } else if resumable {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
        expected_source_epoch: progress.as_ref().map(|progress| progress.source_epoch),
        expected_cursor: progress
            .as_ref()
            .and_then(|progress| progress.cursor.clone()),
        progress,
        enabled: true,
    }
}

fn output_is_aligned(core: Option<&ClaudeStoreCursor>, output: &ClaudeOutputState) -> bool {
    match (core, output.progress.as_ref(), output.previous.as_ref()) {
        (None, None, None) => true,
        (Some(core), Some(_), Some(output)) => {
            output.pro_revisions_match()
                && output.pro_observation_binding_matches()
                && core.checkpoint.core_frontier() == output.pro_frontier()
                && core.checkpoint.terminal == output.pro_terminal
        }
        _ => false,
    }
}

fn copy_pro_lane(core: &mut ParseCheckpoint, output: &ParseCheckpoint) {
    core.pro_complete_offset = output.pro_complete_offset;
    core.pro_next_raw_ordinal = output.pro_next_raw_ordinal;
    core.pro_complete_record_chain_sha256 = output.pro_complete_record_chain_sha256;
    core.pro_boundary_proof_len = output.pro_boundary_proof_len;
    core.pro_boundary_proof_sha256 = output.pro_boundary_proof_sha256;
    core.pro_native_identity_chain_sha256 = output.pro_native_identity_chain_sha256;
    core.pro_native_identity_records = output.pro_native_identity_records;
    core.pro_appendable_boundary = output.pro_appendable_boundary;
    core.pro_initialized = output.pro_initialized;
    core.pro_terminal = output.pro_terminal;
    core.pro_observed_file_len = output.pro_observed_file_len;
    core.pro_observation_sha256 = output.pro_observation_sha256;
    core.pro_observation_binding_sha256 = output.pro_observation_binding_sha256;
    core.pro_parser_revision = output.pro_parser_revision;
    core.pro_policy_revision = output.pro_policy_revision;
}

fn materialize_output_page(
    source: &DiscoveredClaudeSession,
    sink: &dyn ProOutputSink,
    state: &mut ClaudeOutputState,
    page: ClaudeNativeProOutputPage,
    checkpoint: ParseCheckpoint,
) {
    if !state.enabled {
        return;
    }
    if let (Some(progress), Some(previous)) = (&state.progress, &state.previous) {
        if page.expected_frontier != previous.pro_frontier()
            && state.disposition == ProOutputSourceDisposition::AppendOrResume
        {
            state.source_epoch = progress.source_epoch.saturating_add(1);
            state.disposition = ProOutputSourceDisposition::Rewrite;
        }
    }
    let next_cursor = match serde_json::to_vec(&checkpoint) {
        Ok(payload) => OutputNativeCursor {
            version: CLAUDE_OUTPUT_CURSOR_VERSION,
            payload,
        },
        Err(error) => {
            state.enabled = false;
            sink.mark_behind(ProOutputSinkError::new(
                "claude_nativepath_output_cursor",
                error.to_string(),
            ));
            return;
        }
    };
    let materialization = ProOutputMaterializationPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: source_revision(source, None),
        parser_revision: CLAUDE_OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_cursor: state.expected_cursor.clone(),
        next_safe_cursor: next_cursor.clone(),
        terminal: page.terminal,
        observations: page.outputs,
    };
    match sink.materialize_page(materialization) {
        Ok(result) => {
            state.expected_source_epoch = Some(result.source_epoch);
            state.expected_cursor = Some(result.committed_cursor);
            state.disposition = ProOutputSourceDisposition::AppendOrResume;
            state.previous = Some(checkpoint);
        }
        Err(error) => {
            state.enabled = false;
            sink.mark_behind(error);
        }
    }
}

fn replay_source_outputs(
    source: &DiscoveredClaudeSession,
    source_root: &Path,
    sink: &dyn ProOutputSink,
) {
    let mut state = output_state(source, source_root, sink);
    if !state.enabled {
        return;
    }
    let previous = state.previous.clone();
    let mut scanner = match ClaudeNativeScanner::new(
        source.clone(),
        previous.as_ref(),
        ClaudeNativeProfile::ProReplayOnly,
    ) {
        Ok(scanner) => scanner,
        Err(error) => {
            sink.mark_behind(ProOutputSinkError::new(
                "claude_nativepath_output_replay",
                error.to_string(),
            ));
            return;
        }
    };
    loop {
        let page = match scanner.next_page() {
            Ok(Some(ClaudeNativeOwnedPage::Pro(page))) => page,
            Ok(Some(ClaudeNativeOwnedPage::Core(_))) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "claude_nativepath_output_replay",
                    "Pro replay emitted a Core page",
                ));
                return;
            }
            Ok(None) => break,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "claude_nativepath_output_replay",
                    error.to_string(),
                ));
                return;
            }
        };
        let checkpoint = scanner.checkpoint_at(&page.next_safe_frontier, page.terminal);
        materialize_output_page(source, sink, &mut state, *page, checkpoint);
        if !state.enabled {
            return;
        }
    }
    if let Err(error) = scanner.finish() {
        sink.mark_behind(ProOutputSinkError::new(
            "claude_nativepath_output_replay",
            error.to_string(),
        ));
    }
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

fn encode_store_cursor(cursor: &ClaudeStoreCursor) -> Result<String> {
    Ok(serde_json::to_string(cursor)?)
}

fn decode_store_cursor(cursor: &str) -> Result<ClaudeStoredCursor> {
    let provider = decode_native_path_committed_cursor(cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| cursor.to_owned());
    if let Ok(decoded) = serde_json::from_str::<ClaudeStoreCursor>(&provider) {
        if decoded.version != CLAUDE_STORE_CURSOR_VERSION {
            return Err(CaptureError::InvalidPayload(
                "unsupported Claude NativePath Store cursor".to_owned(),
            ));
        }
        return Ok(ClaudeStoredCursor::Native(decoded));
    }
    validate_released_cursor(&provider)?;
    Ok(ClaudeStoredCursor::Released(provider))
}

fn validate_released_cursor(encoded: &str) -> Result<()> {
    let cursor = CertifiedProviderCursor::decode_if_certified(encoded)?.ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Claude cursor is neither NativePath nor a released certified cursor".to_owned(),
        )
    })?;
    if cursor.parser_revision() != CLAUDE_RELEASED_CAPTURE_REVISION
        || cursor.policy_revision() != CLAUDE_RELEASED_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Claude released cursor has unsupported revisions".to_owned(),
        ));
    }
    crate::released_jsonl_cursor::released_jsonl_position_offset(cursor.native_position())
        .map_err(|_| {
            CaptureError::InvalidPayload("Claude released cursor position is malformed".to_owned())
        })?;
    let checkpoint: ReleasedClaudeParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
    validate_released_checkpoint(&checkpoint)
}

fn validate_released_checkpoint(checkpoint: &ReleasedClaudeParserCheckpoint) -> Result<()> {
    if checkpoint.accepted_captures > checkpoint.next_ordinal
        || checkpoint.accepted_events > checkpoint.accepted_captures
        || checkpoint.rejected_records > checkpoint.next_ordinal
    {
        return Err(CaptureError::InvalidPayload(
            "Claude released cursor checkpoint counters are inconsistent".to_owned(),
        ));
    }
    if let Some(session) = &checkpoint.session {
        if session.native_session_id.trim().is_empty()
            || session.provider_session_id.trim().is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Claude released cursor session identity is empty".to_owned(),
            ));
        }
        let _ = (
            &session.parent_provider_session_id,
            &session.external_agent_id,
            session.is_subagent,
            session.started_at,
            &session.cwd,
            &session.version,
            &session.git_branch,
        );
    }
    let _ = checkpoint.accepted_file_touches;
    Ok(())
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Claude.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn source_revision(source: &DiscoveredClaudeSession, token: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-claude-nativepath-source-revision-v1\0");
    digest.update(source.fingerprint.observation_sha256());
    if let Some(token) = token {
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    }
    format!("claude-nativepath-sha256-v1:{:x}", digest.finalize())
}

fn generation_source_id(
    source: &DiscoveredClaudeSession,
    generation: u64,
    checkpoint: &ParseCheckpoint,
) -> Uuid {
    stable_capture_uuid(
        &serde_json::to_string(&(
            "claude-nativepath-source-generation-v1",
            source.key.provider_session_id(),
            generation,
            checkpoint.canonical_route.as_os_str().as_encoded_bytes(),
            checkpoint.observation_sha256,
            checkpoint.physical_file_id,
        ))
        .expect("Claude source generation identity is serializable"),
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

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs::File,
        io::Write,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use serde_json::json;

    use super::*;
    use crate::{test_support_paths::tempdir, ProOutputPageResult, ProviderImportWorkResult};

    const MACHINE: &str = "claude-nativepath-production-test";
    const SUCCESS_BODY: &str = "CLAUDE_SUCCESS_BODY_MUST_STAY_OUT_OF_CORE";

    #[test]
    fn production_store_lifecycle_is_idempotent_and_retires_disappeared_routes() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".claude/projects");
        let transcript = root.join("-workspace/lifecycle.jsonl");
        write_records(
            &transcript,
            &[
                message("lifecycle", "fresh", "fresh body"),
                success_result("lifecycle", "success-1", SUCCESS_BODY),
            ],
        );
        let store_path = temp.path().join("history.sqlite");
        let mut store = Store::open(&store_path).unwrap();

        let fresh = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(fresh.imported_sessions, 1);
        let session = claude_session(&store);
        let events = store.events_for_session(session.id).unwrap();
        assert_eq!(events.len(), 1);
        assert!(!serde_json::to_string(&events)
            .unwrap()
            .contains(SUCCESS_BODY));
        let routed_event = events[0].id;
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_ok());

        let noop = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(noop.work_result(), ProviderImportWorkResult::NoOp);
        drop(store);
        let mut store = Store::open(&store_path).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::NoOp
        );

        append_record(&transcript, &message("lifecycle", "append", "append body"));
        let append = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(append.imported_events, 1);

        write_records(
            &transcript,
            &[message("lifecycle", "rewrite", "rewritten generation")],
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::Changed
        );

        write_records(
            &transcript,
            &[message("lifecycle", "short", "short generation")],
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::Changed
        );

        let replacement = transcript.with_extension("replacement");
        write_records(
            &replacement,
            &[message(
                "lifecycle",
                "replacement",
                "replacement generation",
            )],
        );
        fs::rename(&replacement, &transcript).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::Changed
        );

        fs::remove_dir_all(&root).unwrap();
        let disappeared = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(disappeared.work_result(), ProviderImportWorkResult::Changed);
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_err());
        assert!(!store.events_for_session(session.id).unwrap().is_empty());
    }

    #[test]
    fn multi_tool_record_publishes_distinct_touch_identities_and_event_links() {
        const PRIVATE_TOOL_INPUT: &str = "CLAUDE_PRIVATE_TOOL_INPUT_MUST_NOT_PERSIST";

        let temp = tempdir().unwrap();
        let root = temp.path().join(".claude/projects");
        let transcript = root.join("-workspace/multi-tool.jsonl");
        write_records(
            &transcript,
            &[json!({
                "sessionId": "multi-tool",
                "type": "assistant",
                "uuid": "multi-tool-record",
                "timestamp": "2026-07-25T12:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "call-a",
                            "name": "Edit",
                            "input": {
                                "path": "src/a.rs",
                                "command": PRIVATE_TOOL_INPUT
                            }
                        },
                        {
                            "type": "tool_use",
                            "id": "call-b",
                            "name": "Write",
                            "input": {
                                "path": "src/b.rs",
                                "command": PRIVATE_TOOL_INPUT
                            }
                        }
                    ]
                }
            })],
        );
        let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

        let summary = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
        let events = store.events_for_session(claude_session(&store).id).unwrap();
        let tool_events = events
            .iter()
            .filter(|event| !event.payload["tool_call"].is_null())
            .collect::<Vec<_>>();
        assert_eq!(tool_events.len(), 2);

        let archive = store.export_archive().unwrap();
        assert_eq!(archive.files_touched.len(), 2);
        assert_eq!(
            archive
                .files_touched
                .iter()
                .map(|touch| touch.id)
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        assert_eq!(
            archive
                .files_touched
                .iter()
                .filter_map(|touch| touch.event_id)
                .collect::<BTreeSet<_>>()
                .len(),
            2
        );
        for (path, call_id) in [("src/a.rs", "call-a"), ("src/b.rs", "call-b")] {
            let event = tool_events
                .iter()
                .find(|event| event.payload["tool_call"]["call_id"] == call_id)
                .unwrap();
            let touch = archive
                .files_touched
                .iter()
                .find(|touch| touch.path == path)
                .unwrap();
            assert_eq!(touch.event_id, Some(event.id));
        }
        assert!(!serde_json::to_string(&archive)
            .unwrap()
            .contains(PRIVATE_TOOL_INPUT));

        let bindings = archive
            .files_touched
            .iter()
            .map(|touch| (touch.id, touch.event_id, touch.path.clone()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::NoOp
        );
        assert_eq!(
            store
                .export_archive()
                .unwrap()
                .files_touched
                .iter()
                .map(|touch| (touch.id, touch.event_id, touch.path.clone()))
                .collect::<BTreeSet<_>>(),
            bindings
        );
    }

    #[test]
    fn core_commits_before_output_failure_and_later_activation_replays_success_body() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".claude/projects");
        let transcript = root.join("-workspace/output.jsonl");
        write_records(
            &transcript,
            &[
                message("output", "message-1", "core message"),
                success_result("output", "success-1", SUCCESS_BODY),
                failure_result("output", "failure-1", "failure private body"),
            ],
        );
        let store_path = temp.path().join("history.sqlite");
        let mut store = Store::open(&store_path).unwrap();
        let failing = Arc::new(RecordingSink::new(store_path.clone(), true));

        let summary = import(
            &root,
            &mut store,
            ImportProfile::CoreAndPro(failing.clone()),
        )
        .unwrap();
        assert_eq!(summary.work_result(), ProviderImportWorkResult::Changed);
        assert!(failing.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(failing.behind.load(Ordering::SeqCst), 1);
        let events = store.events_for_session(claude_session(&store).id).unwrap();
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains(SUCCESS_BODY));
        assert!(!serialized.contains("failure private body"));

        let replay = Arc::new(RecordingSink::new(store_path, false));
        let replay_summary =
            import(&root, &mut store, ImportProfile::CoreAndPro(replay.clone())).unwrap();
        assert_eq!(replay_summary.work_result(), ProviderImportWorkResult::NoOp);
        assert!(replay.pages.load(Ordering::SeqCst) > 0);
        assert!(replay
            .bodies
            .lock()
            .unwrap()
            .iter()
            .any(|body| body.as_slice() == SUCCESS_BODY.as_bytes()));

        let pages_before_append = replay.pages.load(Ordering::SeqCst);
        append_record(
            &transcript,
            &success_result("output", "success-2", "later output"),
        );
        let append = import(&root, &mut store, ImportProfile::CoreAndPro(replay.clone())).unwrap();
        assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
        assert!(replay.pages.load(Ordering::SeqCst) > pages_before_append);
        assert!(replay
            .bodies
            .lock()
            .unwrap()
            .iter()
            .any(|body| body.as_slice() == b"later output"));
    }

    #[test]
    fn corrupt_and_incomplete_input_advance_only_certified_complete_boundaries() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".claude/projects");
        let transcript = root.join("-workspace/incomplete.jsonl");
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let mut file = File::create(&transcript).unwrap();
        writeln!(file, "{}", message("incomplete", "valid", "valid body")).unwrap();
        writeln!(file, "{{malformed").unwrap();
        write!(file, "{}", message("incomplete", "tail", "incomplete tail")).unwrap();
        file.flush().unwrap();
        let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();

        let first = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(first.failed, 1);
        assert_eq!(
            store
                .events_for_session(claude_session(&store).id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::NoOp
        );

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file).unwrap();
        file.flush().unwrap();
        let completed = import(&root, &mut store, ImportProfile::CoreOnly).unwrap();
        assert_eq!(completed.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(
            store
                .events_for_session(claude_session(&store).id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn only_exact_released_cursor_is_reset_then_native_retry_is_idempotent() {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".claude/projects");
        let transcript = root.join("-workspace/migration.jsonl");
        write_records(
            &transcript,
            &[message("migration", "message-1", "migration body")],
        );
        let mut store = Store::open(temp.path().join("history.sqlite")).unwrap();
        import(&root, &mut store, ImportProfile::CoreOnly).unwrap();

        let canonical = fs::canonicalize(&transcript).unwrap();
        let locator = provider_path_identity(&canonical).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Claude,
            CLAUDE_PROJECTS_SOURCE_FORMAT,
            &locator,
        );
        let mut cursor = store
            .get_sync_cursor(None, MACHINE, &stream)
            .unwrap()
            .unwrap();
        cursor.cursor = r#"{"released":"captured-batch-cursor"}"#.to_owned();
        store.upsert_sync_cursor(&cursor).unwrap();
        assert!(import(&root, &mut store, ImportProfile::CoreOnly).is_err());

        cursor.cursor = CertifiedProviderCursor::new(
            "released-claude-source-revision",
            CLAUDE_RELEASED_CAPTURE_REVISION,
            CLAUDE_RELEASED_POLICY_REVISION,
            crate::provider::importer::released_jsonl_initial_position_for_test(),
            crate::provider::importer::BoundedParserCheckpoint::from_serializable(
                &ReleasedClaudeParserCheckpoint {
                    session: None,
                    next_ordinal: 0,
                    accepted_captures: 0,
                    accepted_events: 0,
                    accepted_file_touches: 0,
                    rejected_records: 0,
                },
            )
            .unwrap(),
        )
        .unwrap()
        .encode()
        .unwrap();
        store.upsert_sync_cursor(&cursor).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(matches!(
            decode_store_cursor(
                &store
                    .get_sync_cursor(None, MACHINE, &stream)
                    .unwrap()
                    .unwrap()
                    .cursor
            )
            .unwrap(),
            ClaudeStoredCursor::Native(_)
        ));
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly)
                .unwrap()
                .work_result(),
            ProviderImportWorkResult::NoOp
        );
    }

    struct RecordingSink {
        store_path: PathBuf,
        fail: AtomicBool,
        progress: Mutex<Option<ProOutputProgress>>,
        pages: AtomicUsize,
        behind: AtomicUsize,
        saw_core_before_page: AtomicBool,
        bodies: Mutex<Vec<Vec<u8>>>,
    }

    impl RecordingSink {
        fn new(store_path: PathBuf, fail: bool) -> Self {
            Self {
                store_path,
                fail: AtomicBool::new(fail),
                progress: Mutex::new(None),
                pages: AtomicUsize::new(0),
                behind: AtomicUsize::new(0),
                saw_core_before_page: AtomicBool::new(false),
                bodies: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "claude-nativepath-test-materializer-v1"
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
            if core
                .list_sessions()
                .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                .iter()
                .any(|session| session.provider == CaptureProvider::Claude)
            {
                self.saw_core_before_page.store(true, Ordering::SeqCst);
            }
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(ProOutputSinkError::new(
                    "intentional_test_failure",
                    "intentional output failure",
                ));
            }
            self.pages.fetch_add(1, Ordering::SeqCst);
            self.bodies.lock().unwrap().extend(
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

        fn mark_behind(&self, _error: ProOutputSinkError) {
            self.behind.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn import(
        root: &Path,
        store: &mut Store,
        profile: ImportProfile,
    ) -> Result<ProviderImportSummary> {
        crate::import_claude_projects_jsonl_tree(
            root,
            store,
            ClaudeProjectsImportOptions {
                machine_id: MACHINE.to_owned(),
                source_path: Some(root.to_path_buf()),
                imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
                import_profile: profile,
                ..ClaudeProjectsImportOptions::default()
            },
        )
    }

    fn claude_session(store: &Store) -> Session {
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| {
                session.provider == CaptureProvider::Claude
                    && session.role_hint.as_deref() != Some("relationship_placeholder")
            })
            .unwrap()
    }

    fn write_records(path: &Path, records: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = File::create(path).unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
        file.flush().unwrap();
    }

    fn append_record(path: &Path, record: &Value) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "{record}").unwrap();
        file.flush().unwrap();
    }

    fn message(session: &str, uuid: &str, body: &str) -> Value {
        json!({
            "sessionId": session,
            "type": "user",
            "uuid": uuid,
            "timestamp": "2026-07-25T12:00:00Z",
            "cwd": "/workspace/project",
            "version": "2.1.219",
            "gitBranch": "main",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": body}]
            }
        })
    }

    fn success_result(session: &str, uuid: &str, body: &str) -> Value {
        json!({
            "sessionId": session,
            "type": "user",
            "uuid": uuid,
            "timestamp": "2026-07-25T12:00:01Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-success",
                    "content": body
                }]
            },
            "toolUseResult": {"exitCode": 0}
        })
    }

    fn failure_result(session: &str, uuid: &str, body: &str) -> Value {
        json!({
            "sessionId": session,
            "type": "user",
            "uuid": uuid,
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-failure",
                    "content": body
                }]
            },
            "toolUseResult": {"exitCode": 7}
        })
    }
}
