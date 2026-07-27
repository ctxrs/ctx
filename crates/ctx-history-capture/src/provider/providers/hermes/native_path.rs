//! Production Hermes NativePath ingestion.
//!
//! Hermes owns discovery, SQLite snapshot traversal, cursor/revision semantics,
//! canonical projection, source-route authority, and output replay here. Core
//! commits before the independently durable output lane on every page.

use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, ProviderSourceTrust, Session, SessionEdge, SessionEdgeType,
    SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY},
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_edge_uuid,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        sqlite::{
            open_provider_sqlite_readonly, sqlite_schema_fingerprint, ProviderSqliteSourceSnapshot,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ImportProfile, OutputNativeCursor,
    OutputSourceIdentity, ProOutputProgress, ProOutputSinkError, ProOutputSourceDisposition,
    ProviderAdapterContext, ProviderImportFailure, ProviderImportOptions, ProviderImportSummary,
    ProviderImportWorkResult, Result, HERMES_SQLITE_SOURCE_FORMAT,
};

use super::{
    hermes_decode_content, hermes_native_event, hermes_output_outcome, hermes_pro_output,
    layout::{HermesMessageRow, HermesSessionRow, HermesSqliteValue},
    sqlite::{
        HermesFrontier, HermesNativeRecord, HermesNativeRow, HermesRowReader,
        HERMES_FRONTIER_VERSION,
    },
    HermesNativeEvent, HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

const HERMES_CURSOR_VERSION: u32 = 1;
const HERMES_OUTPUT_PARSER_REVISION: &str = "hermes-output-v1";
const HERMES_PUBLICATION_DOMAIN: &[u8] = b"ctx-hermes-nativepath-publication-v1\0";
const RELEASED_HERMES_POSITION_KIND: &str = "hermes-sqlite-keyset-v1";
const RELEASED_HERMES_CAPTURE_REVISION: u32 = 1;
const RELEASED_HERMES_POLICY_REVISION: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HermesStoreCursor {
    version: u32,
    canonical_source_identity: String,
    locator_identity: String,
    source_revision: String,
    frontier: HermesFrontier,
    terminal: bool,
    generation: u64,
    rejected_records: u64,
    retired: bool,
}

struct CorePlan {
    expected: Option<SyncCursor>,
    cursor: HermesStoreCursor,
    migration: bool,
}

struct OutputPlan {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    scan_frontier: HermesFrontier,
    disposition: ProOutputSourceDisposition,
    enabled: bool,
}

struct HermesPage {
    expected_frontier: HermesFrontier,
    next_frontier: HermesFrontier,
    terminal: bool,
    rows: Vec<HermesNativeRow>,
}

#[derive(Clone)]
struct ResolvedSession {
    source_id: Uuid,
    session: Session,
}

struct PublicationContext<'a> {
    adapter: &'a ProviderAdapterContext,
    options: &'a ProviderImportOptions,
    canonical_path: &'a Path,
    configured_source_root: String,
    locator_identity: &'a str,
    cursor_stream: &'a str,
    source_revision: &'a str,
    source_snapshot: &'a ProviderSqliteSourceSnapshot,
    schema_fingerprint: &'a str,
    sqlite_user_version: i64,
}

pub(super) fn import_hermes_native_path(
    path: &Path,
    store: &mut Store,
    mut adapter: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    adapter.source_path = Some(path.to_path_buf());
    let absolute = absolute_path(path)?;
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            retire_missing_source(store, &absolute, &adapter)?;
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: absolute,
                reason: "Hermes state.db does not exist",
            });
        }
        Err(error) => return Err(CaptureError::Io(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: absolute,
            reason: "Hermes SQLite source must be a regular non-symlink file",
        });
    }
    let canonical_path = canonical_source_path(&absolute)?;
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_snapshot = ProviderSqliteSourceSnapshot::read(
        &canonical_path,
        "Hermes SQLite source must be a regular non-symlink file",
        "Hermes SQLite sidecar must be a regular non-symlink file",
    )?;
    let conn = open_provider_sqlite_readonly(&canonical_path)?;
    conn.execute_batch("BEGIN")?;
    let sqlite_user_version =
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let schema = super::layout::HermesSchema::detect(&conn)?;
    let source_revision = source_revision(
        &source_snapshot,
        &schema_fingerprint,
        options.inventory_observation_token.as_deref(),
    );
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        None,
        Some(&canonical_path.display().to_string()),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath source has no canonical identity",
    ))?;
    let route_observation = ProviderSourceLocatorObservation {
        provider: CaptureProvider::Hermes,
        source_format: HERMES_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: adapter.machine_id.clone(),
        locator_identity: locator_identity.clone(),
        cursor_stream: cursor_stream.clone(),
        proposed_source_identity: proposed_source_identity.clone(),
        raw_source_path: Some(canonical_path.display().to_string()),
        source_revision: source_revision.clone(),
        observed_at_ms: adapter.imported_at.timestamp_millis(),
    };
    // Resolve acquisition identity before constructing either durable lane.
    // Every Core page reconciles this same observation again in its atomic
    // publication group, so a concurrent route change still fails closed.
    let route = store.reconcile_provider_source_locator(&route_observation)?;
    let stored = store.get_sync_cursor(None, &adapter.machine_id, &cursor_stream)?;
    let mut core_plan = core_plan(
        stored,
        &route.canonical_source_identity,
        &locator_identity,
        &source_revision,
    )?;
    core_plan
        .cursor
        .canonical_source_identity
        .clone_from(&route.canonical_source_identity);
    let mut output_plan = output_plan(
        &options.import_profile,
        &adapter.machine_id,
        &core_plan.cursor,
        &source_revision,
    )?;
    if options.import_profile.is_replay_only()
        && (!core_plan.cursor.terminal
            || core_plan.cursor.source_revision != source_revision
            || core_plan.cursor.retired)
    {
        return Err(CaptureError::InvalidPayload(
            "Hermes output replay requires terminal Core at the exact source revision".to_owned(),
        ));
    }

    let replay_only = options.import_profile.is_replay_only();
    let core_noop = core_plan.cursor.terminal
        && core_plan.cursor.source_revision == source_revision
        && !core_plan.cursor.retired
        && !core_plan.migration;
    if core_noop && !output_plan.enabled {
        conn.execute_batch("ROLLBACK")?;
        return Ok(ProviderImportSummary::default());
    }

    let scan_frontier = if replay_only
        || core_noop
        || output_plan.enabled
            && output_plan.scan_frontier.next_ordinal < core_plan.cursor.frontier.next_ordinal
    {
        output_plan.scan_frontier
    } else {
        core_plan.cursor.frontier
    };
    let configured_source_root = adapter
        .source_root_display()
        .unwrap_or_else(|| canonical_path.display().to_string());
    let context = PublicationContext {
        adapter: &adapter,
        options: &options,
        canonical_path: &canonical_path,
        configured_source_root,
        locator_identity: &locator_identity,
        cursor_stream: &cursor_stream,
        source_revision: &source_revision,
        source_snapshot: &source_snapshot,
        schema_fingerprint: &schema_fingerprint,
        sqlite_user_version,
    };
    let committed_store = Store::open_read_only(store.path())?;
    let mut reader = HermesRowReader::new(&conn, &schema)?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut pending = None;
    let mut frontier = scan_frontier;
    let mut summary = ProviderImportSummary::default();
    let mut output_behind = false;
    let mut changed_groups = 0_usize;

    let operation: Result<()> = (|| {
        loop {
            let page = read_page(&mut reader, &mut pending, frontier)?;
            frontier = page.next_frontier;
            let core_prefix = page.next_frontier.next_ordinal
                <= core_plan.cursor.frontier.next_ordinal
                && !core_plan.migration;
            if !replay_only && !core_noop && !core_prefix {
                let changed = publish_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &context,
                    &mut core_plan,
                    &page,
                    &mut summary,
                )?;
                if changed {
                    changed_groups = changed_groups.saturating_add(1);
                }
            }
            if output_plan.enabled
                && !output_behind
                && !publish_output_page(&options.import_profile, &context, &mut output_plan, &page)?
            {
                output_behind = true;
                output_plan.enabled = false;
                summary.record_failure(ProviderImportFailure {
                    line: 0,
                    error: "Hermes Pro output is behind committed Core".to_owned(),
                });
            }
            if !replay_only
                && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining = !page.terminal;
                break;
            }
            if page.terminal {
                break;
            }
        }
        Ok(())
    })();
    drop(reader);
    let snapshot_finish = match source_snapshot.revalidate(&canonical_path) {
        Ok(true) => Ok(()),
        Ok(false) => Err(CaptureError::SourceChangedDuringCapture),
        Err(error) => Err(error),
    };
    let provider_finish = conn.execute_batch("ROLLBACK").map_err(CaptureError::from);
    let search_finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    operation?;
    snapshot_finish?;
    provider_finish?;
    search_finish?;
    if summary.imported != 0 || changed_groups != 0 {
        summary.set_work_result(ProviderImportWorkResult::Changed);
    }
    Ok(summary)
}

fn read_page(
    reader: &mut HermesRowReader<'_>,
    pending: &mut Option<HermesNativeRow>,
    expected_frontier: HermesFrontier,
) -> Result<HermesPage> {
    let mut rows = Vec::new();
    let mut observed_bytes = 0_usize;
    let mut next_frontier = expected_frontier;
    loop {
        let row = match pending.take() {
            Some(row) => Some(row),
            None => reader.next(next_frontier)?,
        };
        let Some(row) = row else {
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: true,
                rows,
            });
        };
        let next_bytes = observed_bytes.saturating_add(row.observed_bytes);
        if !rows.is_empty()
            && (rows.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                || next_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES)
        {
            *pending = Some(row);
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: false,
                rows,
            });
        }
        observed_bytes = next_bytes;
        next_frontier = row.next_frontier;
        rows.push(row);
        if rows.len() == NATIVE_INGESTION_PAGE_MAX_UNITS {
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: false,
                rows,
            });
        }
    }
}

fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    context: &PublicationContext<'_>,
    plan: &mut CorePlan,
    page: &HermesPage,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if page.expected_frontier != plan.cursor.frontier {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath Core frontier is not contiguous".to_owned(),
        ));
    }
    revalidate_source(context)?;
    let rejected = page
        .rows
        .iter()
        .filter(|row| matches!(row.record, HermesNativeRecord::Rejected(_)))
        .count();
    let mut next_cursor = plan.cursor.clone();
    next_cursor.frontier = page.next_frontier;
    next_cursor.terminal = page.terminal;
    next_cursor.retired = false;
    next_cursor.rejected_records = next_cursor
        .rejected_records
        .saturating_add(u64::try_from(rejected).unwrap_or(u64::MAX));
    let next = sync_cursor(context, &next_cursor)?;
    let transition = NativePathCursorTransition::new(
        plan.expected.as_ref().map(|cursor| cursor.cursor.clone()),
        next,
    );
    let publication_id = publication_id(&transition, &next_cursor);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let accounting = NativePathGroupAccounting::new(1, 1, NATIVE_INGESTION_PAGE_MAX_BYTES)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            plan.expected =
                store.get_sync_cursor(None, &context.adapter.machine_id, context.cursor_stream)?;
            plan.cursor = next_cursor;
            plan.migration = false;
            return Ok(false);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        None,
        Some(&context.canonical_path.display().to_string()),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.adapter.machine_id.clone(),
            locator_identity: context.locator_identity.to_owned(),
            cursor_stream: context.cursor_stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(context.canonical_path.display().to_string()),
            source_revision: context.source_revision.to_owned(),
            observed_at_ms: context.adapter.imported_at.timestamp_millis(),
        })?;
    if plan.cursor.canonical_source_identity != resolution.canonical_source_identity {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath source route changed after cursor planning".to_owned(),
        ));
    }
    let mut resolved = BTreeMap::new();
    for row in &page.rows {
        match &row.record {
            HermesNativeRecord::Session(session) => {
                let resolved_session = publish_session(
                    committed_store,
                    &mut group,
                    context,
                    &resolution,
                    session,
                    summary,
                )?;
                resolved.insert(session.id.clone(), resolved_session);
            }
            HermesNativeRecord::Message {
                row: message,
                values,
            } => {
                publish_message(
                    committed_store,
                    &mut group,
                    context,
                    &resolution.canonical_source_identity,
                    &mut resolved,
                    row,
                    message,
                    values,
                    summary,
                )?;
            }
            HermesNativeRecord::Rejected(reason) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(row.ordinal)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: reason.clone(),
                });
            }
        }
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    plan.expected =
        store.get_sync_cursor(None, &context.adapter.machine_id, context.cursor_stream)?;
    plan.cursor = next_cursor;
    plan.migration = false;
    Ok(true)
}

fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &PublicationContext<'_>,
    resolution: &ctx_history_store::ProviderSourceLocatorResolution,
    row: &HermesSessionRow,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSession> {
    let raw_source_path = context.canonical_path.display().to_string();
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &context.adapter.machine_id,
        &resolution.canonical_source_identity,
        &row.id,
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            if resolution.relocated {
                stable_capture_uuid(
                    &serde_json::to_string(&(
                        "provider-relocated-source-v1",
                        CaptureProvider::Hermes.as_str(),
                        HERMES_SQLITE_SOURCE_FORMAT,
                        &resolution.canonical_source_identity,
                        &row.id,
                    ))
                    .expect("Hermes relocated source identity is serializable"),
                    "source",
                )
            } else {
                provider_scoped_source_uuid(
                    CaptureProvider::Hermes,
                    &row.id,
                    HERMES_SQLITE_SOURCE_FORMAT,
                    Some(&raw_source_path),
                )
            }
        });
    let started_at = crate::provider::normalization::provider_required_timestamp_seconds(
        row.started_at,
        "Hermes session started_at",
    )?;
    let ended_at = row
        .ended_at
        .map(|value| {
            crate::provider::normalization::provider_required_timestamp_seconds(
                value,
                "Hermes session ended_at",
            )
        })
        .transpose()?;
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Hermes,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: row.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(HERMES_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.configured_source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(row.id.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": context.configured_source_root,
                "source_revision": context.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Hermes,
                    &row.id,
                    HERMES_SQLITE_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "source_metadata": source_metadata(context),
                "nativepath_publication": HERMES_CURSOR_VERSION,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Hermes,
        &row.id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let parent_id = row
        .parent_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Hermes,
                parent,
                source_id,
                Some(&resolution.canonical_source_identity),
            )
        })
        .transpose()?;
    if let (Some(parent_id), Some(parent_external)) = (parent_id, row.parent_session_id.as_deref())
    {
        group.insert_session_if_absent(&relationship_placeholder(
            context,
            source_id,
            parent_id,
            parent_external,
            &resolution.canonical_source_identity,
        ))?;
    }
    let session = Session {
        id: session_id,
        history_record_id: context.options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Hermes,
        external_session_id: Some(row.id.clone()),
        external_agent_id: Some(row.source.clone()),
        agent_type: if parent_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(row.source.clone()),
        is_primary: parent_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.id,
                "parent_provider_session_id": row.parent_session_id,
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Hermes.as_str(),
                    row.id
                ),
                "metadata": session_metadata(row),
            }),
        ),
    };
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = parent_id {
        let edge_id = if session.id != provider_session_uuid(CaptureProvider::Hermes, &row.id) {
            provider_source_edge_uuid(
                &resolution.canonical_source_identity,
                &row.id,
                "parent_child",
            )
        } else {
            crate::provider::importer::provider_edge_uuid(
                CaptureProvider::Hermes,
                &row.id,
                "parent_child",
            )
        };
        let edge = SessionEdge {
            id: edge_id,
            from_session_id: session.id,
            to_session_id: parent_id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: Confidence::Explicit,
            source_id: Some(source_id),
            timestamps: timestamps(context.adapter.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": row.id,
                    "parent_provider_session_id": row.parent_session_id,
                    "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                    "imported_at": context.adapter.imported_at,
                }),
            ),
        };
        let edge_existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if edge_existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok(ResolvedSession { source_id, session })
}

#[allow(clippy::too_many_arguments)]
fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &PublicationContext<'_>,
    canonical_source_identity: &str,
    resolved: &mut BTreeMap<String, ResolvedSession>,
    native_row: &HermesNativeRow,
    row: &HermesMessageRow,
    values: &[HermesSqliteValue],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let resolved_session = if let Some(resolved) = resolved.get(&row.session_id) {
        resolved.clone()
    } else {
        let source = committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Hermes,
                HERMES_SQLITE_SOURCE_FORMAT,
                &context.adapter.machine_id,
                canonical_source_identity,
                &row.session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Hermes message {} references a session that was not safely imported",
                    row.id
                ))
            })?;
        let session = committed_store
            .session_by_capture_source_and_external_session(
                source.id,
                CaptureProvider::Hermes,
                &row.session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Hermes message {} references a missing canonical session",
                    row.id
                ))
            })?;
        let resolved_session = ResolvedSession {
            source_id: source.id,
            session,
        };
        resolved.insert(row.session_id.clone(), resolved_session.clone());
        resolved
            .get(&row.session_id)
            .cloned()
            .ok_or(CaptureError::SystemInvariant(
                "Hermes resolved session cache lost an inserted row",
            ))?
    };
    let provider_event_index = crate::provider::normalization::provider_nonnegative_i64_to_u64(
        row.id,
        "Hermes message id",
    )?;
    let content = hermes_decode_content(row.content.as_deref());
    let output_outcome = (row.role == "tool").then(|| hermes_output_outcome(row, &content));
    if output_outcome.as_ref().is_some_and(|outcome| {
        !matches!(
            outcome.outcome,
            crate::OutputOutcome::Failure | crate::OutputOutcome::Timeout
        )
    }) {
        return Ok(());
    }
    let mut native = hermes_native_event(row, native_row.ordinal)?;
    let event_hash = ctx_history_core::compute_payload_hash(&native.payload)?;
    if output_outcome.is_none() {
        let complete_text = native.complete_text.clone();
        super::attach_hermes_complete_content(&mut native, &native_row.locator, values, || {
            complete_text
        })?;
    }
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Hermes,
        &row.session_id,
        resolved_session.source_id,
        provider_event_index,
        provider_event_index,
        &event_hash,
        None,
        None,
        resolved_session.session.id
            == provider_session_uuid(CaptureProvider::Hermes, &row.session_id),
    )?;
    let event = hermes_core_event(
        context,
        &row.session_id,
        resolved_session.source_id,
        resolved_session.session.id,
        usize::try_from(native_row.ordinal)
            .unwrap_or(usize::MAX)
            .saturating_add(1),
        &native,
        &event_hash,
        &identity,
    )?;
    if group.reconcile_provider_event(
        &event,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn hermes_core_event(
    context: &PublicationContext<'_>,
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    native: &HermesNativeEvent,
    event_hash: &str,
    identity: &ProviderEventImportIdentity,
) -> Result<Event> {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut provider_metadata = native.metadata.clone();
    let source_record_coordinates = take_hermes_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "verified content locator annotation is malformed".to_owned(),
                )
            })
        })
        .transpose()?;
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": native.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority":
            ProviderEventHashAuthority::NormalizedPayloadFallback.as_str(),
        "cursor": native.cursor,
        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
        "source_trust": ProviderSourceTrust::ProviderNative,
        "fixture_line": line_number,
        "imported_at": context.adapter.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Hermes.as_str(),
            provider_session_id,
            native.provider_event_index,
        ),
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (
        sync_metadata.as_object_mut(),
        verified_content_locators.as_ref(),
    ) {
        metadata.insert(
            VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators.to_metadata_value(),
        );
    }
    Ok(Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: native.event_type,
        role: native.role,
        occurred_at: native.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Hermes.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": native.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": native.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(native.event_type, &native.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    })
}

fn take_hermes_source_record_coordinates(
    metadata: &mut serde_json::Value,
) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

fn publish_output_page(
    profile: &ImportProfile,
    context: &PublicationContext<'_>,
    plan: &mut OutputPlan,
    page: &HermesPage,
) -> Result<bool> {
    let sink = profile.sink().ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath output page has no output sink",
    ))?;
    revalidate_source(context)?;
    if page.next_frontier.next_ordinal <= plan.scan_frontier.next_ordinal {
        return Ok(true);
    }
    let output_page = (|| {
        if page.expected_frontier.next_ordinal < plan.scan_frontier.next_ordinal {
            return Err(CaptureError::InvalidPayload(
                "Hermes output cursor is not a certified page boundary".to_owned(),
            ));
        }
        let observations = page
            .rows
            .iter()
            .filter_map(|native_row| match &native_row.record {
                HermesNativeRecord::Message { row, .. } if row.role == "tool" => {
                    Some(hermes_pro_output(row, native_row))
                }
                HermesNativeRecord::Session(_)
                | HermesNativeRecord::Message { .. }
                | HermesNativeRecord::Rejected(_) => None,
            })
            .collect::<Result<Vec<_>>>()?;
        let expected = safe_frontier(page.expected_frontier)?;
        let next = safe_frontier(page.next_frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: plan.source.clone(),
            source_epoch: plan.source_epoch,
            observed_revision: context.source_revision.to_owned(),
            parser_revision: HERMES_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: plan.disposition,
            expected_prior_source_epoch: plan.expected_source_epoch,
            expected_prior_frontier: plan.expected_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::Hermes.as_str(),
                plan.source.source_id.clone(),
            ),
            expected,
            next.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: output.observations.len(),
                conservative_serialized_bytes: NATIVE_INGESTION_PAGE_MAX_BYTES,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok::<_, CaptureError>((replay, next))
    })();
    let (replay, next) = match output_page {
        Ok(page) => page,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "hermes_output_page",
                "Hermes Pro output page is invalid",
            ));
            return Ok(false);
        }
    };
    if process_pro_replay_only(replay, sink.as_ref()).is_err() {
        // The bounded output coordinator already marked only this sink behind.
        // Keep the committed Core result and retry this exact frontier later.
        return Ok(false);
    }
    plan.expected_source_epoch = Some(plan.source_epoch);
    plan.expected_frontier = Some(next);
    plan.scan_frontier = page.next_frontier;
    plan.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn core_plan(
    stored: Option<SyncCursor>,
    proposed_source_identity: &str,
    locator_identity: &str,
    source_revision: &str,
) -> Result<CorePlan> {
    let Some(stored) = stored else {
        return Ok(CorePlan {
            expected: None,
            cursor: HermesStoreCursor {
                version: HERMES_CURSOR_VERSION,
                canonical_source_identity: proposed_source_identity.to_owned(),
                locator_identity: locator_identity.to_owned(),
                source_revision: source_revision.to_owned(),
                frontier: HermesFrontier::initial(),
                terminal: false,
                generation: 0,
                rejected_records: 0,
                retired: false,
            },
            migration: false,
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let mut cursor: HermesStoreCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "Hermes NativePath committed cursor payload is malformed".to_owned(),
                )
            })?;
        validate_cursor(&cursor)?;
        let same_source = cursor.source_revision == source_revision && !cursor.retired;
        if !same_source {
            cursor.frontier = HermesFrontier::initial();
            cursor.terminal = false;
            cursor.retired = false;
            cursor.generation = cursor.generation.checked_add(1).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Hermes NativePath cursor generation overflowed".to_owned(),
                )
            })?;
            cursor.rejected_records = 0;
            cursor.source_revision = source_revision.to_owned();
        }
        cursor.locator_identity = locator_identity.to_owned();
        return Ok(CorePlan {
            expected: Some(stored),
            cursor,
            migration: false,
        });
    }
    let legacy =
        CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Hermes cursor is neither NativePath nor a released migration cursor".to_owned(),
            )
        })?;
    validate_released_cursor(&legacy)?;
    Ok(CorePlan {
        expected: Some(stored),
        cursor: HermesStoreCursor {
            version: HERMES_CURSOR_VERSION,
            canonical_source_identity: proposed_source_identity.to_owned(),
            locator_identity: locator_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            frontier: HermesFrontier::initial(),
            terminal: false,
            generation: 1,
            rejected_records: legacy.rejected_records(),
            retired: false,
        },
        migration: true,
    })
}

fn validate_released_cursor(cursor: &CertifiedProviderCursor) -> Result<()> {
    let position = cursor.native_position();
    let valid_position = position.value() == [0]
        || (position.value().len() == 17 && matches!(position.value()[0], 1 | 2));
    let _: () = cursor.parser_checkpoint().deserialize()?;
    if cursor.parser_revision() != RELEASED_HERMES_CAPTURE_REVISION
        || cursor.policy_revision() != RELEASED_HERMES_POLICY_REVISION
        || position.kind() != RELEASED_HERMES_POSITION_KIND
        || !valid_position
    {
        return Err(CaptureError::InvalidPayload(
            "Hermes cursor is not the released SQLite cursor shape".to_owned(),
        ));
    }
    Ok(())
}

fn validate_cursor(cursor: &HermesStoreCursor) -> Result<()> {
    if cursor.version != HERMES_CURSOR_VERSION
        || cursor.canonical_source_identity.is_empty()
        || cursor.locator_identity.is_empty()
        || cursor.source_revision.is_empty()
        || (cursor.retired && !cursor.terminal)
    {
        return Err(CaptureError::InvalidPayload(
            "Hermes NativePath cursor authority is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn output_plan(
    profile: &ImportProfile,
    machine_id: &str,
    core: &HermesStoreCursor,
    source_revision: &str,
) -> Result<OutputPlan> {
    let Some(sink) = profile.sink() else {
        return Ok(OutputPlan {
            source: OutputSourceIdentity {
                provider: CaptureProvider::Hermes.as_str().to_owned(),
                namespace_id: machine_id.to_owned(),
                source_id: core.canonical_source_identity.clone(),
            },
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            scan_frontier: HermesFrontier::initial(),
            disposition: ProOutputSourceDisposition::NewSource,
            enabled: false,
        });
    };
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Hermes.as_str().to_owned(),
        namespace_id: machine_id.to_owned(),
        source_id: core.canonical_source_identity.clone(),
    };
    let progress = sink.observe_source(&source).map_err(|error| {
        CaptureError::InvalidPayload(format!("Hermes output progress failed: {error}"))
    })?;
    output_plan_from_progress(
        source,
        progress,
        core,
        source_revision,
        sink.materializer_revision(),
    )
}

fn output_plan_from_progress(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    core: &HermesStoreCursor,
    source_revision: &str,
    materializer_revision: &str,
) -> Result<OutputPlan> {
    let Some(progress) = progress else {
        return Ok(OutputPlan {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            scan_frontier: HermesFrontier::initial(),
            disposition: ProOutputSourceDisposition::NewSource,
            enabled: true,
        });
    };
    let progress_frontier = progress.cursor.as_ref().map(output_frontier).transpose()?;
    if progress.observed_revision == source_revision
        && progress.parser_revision == HERMES_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
    {
        let frontier = progress_frontier.ok_or_else(|| {
            CaptureError::InvalidPayload("Hermes output progress has no native cursor".to_owned())
        })?;
        if frontier.next_ordinal > core.frontier.next_ordinal
            || (frontier.next_ordinal == core.frontier.next_ordinal && frontier != core.frontier)
            || (progress.terminal && !core.terminal)
        {
            return Err(CaptureError::InvalidPayload(
                "Hermes output progress is ahead of certified Core".to_owned(),
            ));
        }
        return Ok(OutputPlan {
            source,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_frontier: progress
                .cursor
                .as_ref()
                .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
                .transpose()
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            scan_frontier: frontier,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            enabled: !(progress.terminal && core.terminal && frontier == core.frontier),
        });
    }
    Ok(OutputPlan {
        source,
        source_epoch: progress.source_epoch.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("Hermes output source epoch overflowed".to_owned())
        })?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier: progress
            .cursor
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        scan_frontier: HermesFrontier::initial(),
        disposition: ProOutputSourceDisposition::Rewrite,
        enabled: true,
    })
}

fn output_frontier(cursor: &OutputNativeCursor) -> Result<HermesFrontier> {
    if cursor.version != HERMES_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Hermes output cursor version is unsupported".to_owned(),
        ));
    }
    HermesFrontier::decode(&cursor.payload)
}

fn safe_frontier(frontier: HermesFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(HERMES_FRONTIER_VERSION, frontier.encode())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn sync_cursor(context: &PublicationContext<'_>, cursor: &HermesStoreCursor) -> Result<SyncCursor> {
    Ok(SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Hermes.as_str(),
                context.adapter.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.adapter.machine_id.clone(),
        stream: context.cursor_stream.to_owned(),
        cursor: serde_json::to_string(cursor)?,
        last_synced_at: Some(context.adapter.imported_at),
        timestamps: timestamps(context.adapter.imported_at),
    })
}

fn publication_id(transition: &NativePathCursorTransition, cursor: &HermesStoreCursor) -> String {
    let mut digest = Sha256::new();
    digest.update(HERMES_PUBLICATION_DOMAIN);
    digest.update(transition.key().stream().as_bytes());
    digest.update(cursor.version.to_be_bytes());
    digest.update((cursor.canonical_source_identity.len() as u64).to_be_bytes());
    digest.update(cursor.canonical_source_identity.as_bytes());
    digest.update((cursor.locator_identity.len() as u64).to_be_bytes());
    digest.update(cursor.locator_identity.as_bytes());
    digest.update(cursor.generation.to_be_bytes());
    digest.update(cursor.frontier.encode());
    digest.update([u8::from(cursor.terminal)]);
    digest.update(cursor.rejected_records.to_be_bytes());
    digest.update([u8::from(cursor.retired)]);
    digest.update(cursor.source_revision.as_bytes());
    format!("hermes-nativepath-v1:{:x}", digest.finalize())
}

fn source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
    inventory_token: Option<&str>,
) -> String {
    let revision = format!(
        "hermes-nativepath-snapshot-v1:capture={HERMES_CAPTURE_REVISION};policy={HERMES_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    );
    let Some(token) = inventory_token else {
        return revision;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-hermes-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
}

fn source_metadata(context: &PublicationContext<'_>) -> Value {
    json!({
        "adapter": HERMES_SQLITE_SOURCE_FORMAT,
        "sqlite_user_version": context.sqlite_user_version,
        "schema_fingerprint": context.schema_fingerprint,
        "upstream_schema_version_at_research": 17,
        "capture_policy": "provider_owned_nativepath_v1",
    })
}

fn revalidate_source(context: &PublicationContext<'_>) -> Result<()> {
    if context.source_snapshot.revalidate(context.canonical_path)? {
        Ok(())
    } else {
        Err(CaptureError::SourceChangedDuringCapture)
    }
}

fn session_metadata(row: &HermesSessionRow) -> Value {
    json!({
        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
        "source": row.source,
        "title": row.title,
        "model": row.model,
        "model_config": row.model_config.as_deref().map(
            crate::provider::normalization::provider_json_text
        ),
        "end_reason": row.end_reason,
        "message_count": row.message_count,
        "tool_call_count": row.tool_call_count,
        "tokens": {
            "input": row.input_tokens,
            "output": row.output_tokens,
            "cache_read": row.cache_read_tokens,
            "cache_write": row.cache_write_tokens,
            "reasoning": row.reasoning_tokens,
        },
        "git": {
            "branch": row.git_branch,
            "repo_root": row.git_repo_root,
        },
        "billing": {
            "provider": row.billing_provider,
            "base_url": row.billing_base_url,
            "mode": row.billing_mode,
            "estimated_cost_usd": row.estimated_cost_usd,
            "actual_cost_usd": row.actual_cost_usd,
        },
        "archived": row.archived != 0,
    })
}

fn relationship_placeholder(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Hermes,
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
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
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

fn retire_missing_source(
    store: &mut Store,
    path: &Path,
    adapter: &ProviderAdapterContext,
) -> Result<()> {
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &stream)? else {
        return Ok(());
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => return Ok(()),
    };
    let mut cursor: HermesStoreCursor = serde_json::from_str(committed.provider_cursor())?;
    validate_cursor(&cursor)?;
    if cursor.retired {
        return Ok(());
    }
    cursor.retired = true;
    cursor.terminal = true;
    let next = SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Hermes.as_str(),
                adapter.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: adapter.machine_id.clone(),
        stream: stream.clone(),
        cursor: serde_json::to_string(&cursor)?,
        last_synced_at: Some(adapter.imported_at),
        timestamps: timestamps(adapter.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor.clone()), next);
    let publication_id = publication_id(&transition, &cursor);
    let guard = store.begin_event_search_bulk_mode()?;
    let operation: Result<()> = (|| {
        let admission = store.admit_event_search_bulk_group(&guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(1, 1, 0)?,
        )?;
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {}
            NativePathCursorSetClassification::AllExpected => {
                group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                    provider: CaptureProvider::Hermes,
                    source_format: HERMES_SQLITE_SOURCE_FORMAT.to_owned(),
                    machine_id: adapter.machine_id.clone(),
                    locator_identity: cursor.locator_identity.clone(),
                    cursor_stream: stream,
                    expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
                    expected_source_revision: cursor.source_revision.clone(),
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
                })?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
            }
        }
        group.commit()?;
        Ok(())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&guard)
        .map_err(CaptureError::from);
    operation?;
    finish
}

fn absolute_path(path: &Path) -> Result<std::path::PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn canonical_source_path(path: &Path) -> Result<std::path::PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Hermes SQLite source has no parent directory",
        })?;
    let file_name =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Hermes SQLite source has no file name",
            })?;
    Ok(fs::canonicalize(parent)?.join(file_name))
}
