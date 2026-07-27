//! Production Hermes NativePath ingestion.
//!
//! Hermes owns discovery, SQLite snapshot traversal, cursor/revision semantics,
//! canonical projection, source-route authority, and output replay here. Core
//! commits before the independently durable output lane on every page.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    mem::size_of,
    path::Path,
};

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
    hermes_decode_content, hermes_output_outcome, hermes_pro_output,
    layout::{HermesMessageRow, HermesSessionRow, HermesSqliteValue},
    sqlite::{
        HermesFrontier, HermesNativeRecord, HermesNativeRow, HermesRowReader,
        HERMES_FRONTIER_VERSION,
    },
    HermesNativeEvent, HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

mod cursor;
mod lifecycle;
mod publication;

use cursor::*;
use lifecycle::*;
use publication::*;

#[cfg(test)]
pub(super) use cursor::install_before_cursor_publication_revalidation_hook;

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
    terminal: bool,
    enabled: bool,
    initially_behind: bool,
}

struct HermesPage {
    expected_frontier: HermesFrontier,
    next_frontier: HermesFrontier,
    terminal: bool,
    core_owned_bytes: usize,
    output_owned_bytes: usize,
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
    // Plan acquisition identity without changing Store authority. Every Core
    // page reconciles this exact observation in its atomic publication group.
    let route = store.plan_provider_source_locator(&route_observation)?;
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
    if core_noop && !output_plan.enabled && !output_plan.initially_behind {
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
    let mut output_behind = output_plan.initially_behind;
    if output_behind {
        summary.record_failure(ProviderImportFailure {
            line: 0,
            error: "Hermes Pro output is behind committed Core".to_owned(),
        });
    }
    let mut changed_groups = 0_usize;

    let operation: Result<()> = (|| {
        loop {
            let output_fixed_owned_bytes = output_plan
                .enabled
                .then(|| {
                    output_page_fixed_owned_bytes(&options.import_profile, &context, &output_plan)
                })
                .transpose()?;
            let mut page = read_page(
                &mut reader,
                &mut pending,
                frontier,
                output_fixed_owned_bytes,
            )?;
            frontier = page.next_frontier;
            let core_prefix = page.next_frontier.next_ordinal
                <= core_plan.cursor.frontier.next_ordinal
                && !core_plan.migration;
            let terminal_transition = page.terminal
                && !core_plan.cursor.terminal
                && page.expected_frontier == core_plan.cursor.frontier
                && page.next_frontier == core_plan.cursor.frontier;
            if !replay_only && !core_noop && (!core_prefix || terminal_transition) {
                let changed = publish_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &context,
                    &mut core_plan,
                    &mut page,
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
    output_fixed_owned_bytes: Option<usize>,
) -> Result<HermesPage> {
    let mut rows = Vec::new();
    let mut core_owned_bytes = 0_usize;
    let mut output_owned_bytes = output_fixed_owned_bytes.unwrap_or(0);
    let mut next_frontier = expected_frontier;
    loop {
        let mut row = match pending.take() {
            Some(row) => Some(row),
            None => reader.next(next_frontier)?,
        };
        let Some(mut row) = row.take() else {
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: true,
                core_owned_bytes,
                output_owned_bytes,
                rows,
            });
        };
        let mut row_output_owned_bytes = output_fixed_owned_bytes
            .map(|_| output_observation_owned_bytes(&row))
            .transpose()?
            .unwrap_or(0);
        let mut next_core_owned_bytes = core_owned_bytes.saturating_add(row.observed_bytes);
        let mut next_output_owned_bytes = output_owned_bytes.saturating_add(row_output_owned_bytes);
        if rows.is_empty()
            && (next_core_owned_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES
                || next_output_owned_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES)
        {
            let rejected_bytes = next_core_owned_bytes.max(next_output_owned_bytes);
            row.replace_with_rejection(format!(
                "Hermes {:?} row {} requires {rejected_bytes} owned page bytes and exceeds the {}-byte NativePath page limit",
                row.locator.phase,
                row.locator.rowid,
                NATIVE_INGESTION_PAGE_MAX_BYTES
            ));
            row_output_owned_bytes = 0;
            next_core_owned_bytes = core_owned_bytes.saturating_add(row.observed_bytes);
            next_output_owned_bytes = output_owned_bytes.saturating_add(row_output_owned_bytes);
        }
        if !rows.is_empty()
            && (rows.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                || next_core_owned_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES
                || next_output_owned_bytes > NATIVE_INGESTION_PAGE_MAX_BYTES)
        {
            *pending = Some(row);
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: false,
                core_owned_bytes,
                output_owned_bytes,
                rows,
            });
        }
        core_owned_bytes = next_core_owned_bytes;
        output_owned_bytes = next_output_owned_bytes;
        next_frontier = row.next_frontier;
        rows.push(row);
        if rows.len() == NATIVE_INGESTION_PAGE_MAX_UNITS {
            return Ok(HermesPage {
                expected_frontier,
                next_frontier,
                terminal: false,
                core_owned_bytes,
                output_owned_bytes,
                rows,
            });
        }
    }
}

#[derive(Default)]
struct HermesOwnedByteCounter {
    bytes: usize,
}

impl HermesOwnedByteCounter {
    fn add_fixed(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    fn add_bytes(&mut self, bytes: &[u8]) {
        self.add_fixed(size_of::<u64>());
        self.add_fixed(bytes.len());
    }

    fn add_string(&mut self, value: &str) {
        self.add_bytes(value.as_bytes());
    }

    fn add_optional_string(&mut self, value: Option<&str>) {
        self.add_fixed(size_of::<u8>());
        if let Some(value) = value {
            self.add_string(value);
        }
    }

    fn add_optional_fixed(&mut self, present: bool, bytes: usize) {
        self.add_fixed(size_of::<u8>());
        if present {
            self.add_fixed(bytes);
        }
    }

    fn add_frontier(&mut self, frontier: &NativeSafeFrontier) {
        self.add_fixed(size_of::<u32>());
        self.add_bytes(&frontier.bytes);
    }

    fn add_optional_frontier(&mut self, frontier: Option<&NativeSafeFrontier>) {
        self.add_fixed(size_of::<u8>());
        if let Some(frontier) = frontier {
            self.add_frontier(frontier);
        }
    }
}

fn output_page_fixed_owned_bytes(
    profile: &ImportProfile,
    context: &PublicationContext<'_>,
    plan: &OutputPlan,
) -> Result<usize> {
    let sink = profile.sink().ok_or(CaptureError::SystemInvariant(
        "Hermes NativePath output accounting has no output sink",
    ))?;
    let next = safe_frontier(HermesFrontier::initial())?;
    let mut counter = HermesOwnedByteCounter::default();
    counter.add_fixed(32);
    counter.add_frontier(&next);
    counter.add_fixed(size_of::<u8>());
    counter.add_fixed(size_of::<u64>());
    counter.add_string(&plan.source.provider);
    counter.add_string(&plan.source.namespace_id);
    counter.add_string(&plan.source.source_id);
    counter.add_fixed(size_of::<u64>());
    counter.add_string(context.source_revision);
    counter.add_string(HERMES_OUTPUT_PARSER_REVISION);
    counter.add_string(sink.materializer_revision());
    counter.add_fixed(size_of::<u8>());
    counter.add_optional_fixed(plan.expected_source_epoch.is_some(), size_of::<u64>());
    counter.add_optional_frontier(plan.expected_frontier.as_ref());
    counter.add_fixed(size_of::<u64>());
    Ok(counter.bytes)
}

fn output_observation_owned_bytes(row: &HermesNativeRow) -> Result<usize> {
    let HermesNativeRecord::Message { row: message, .. } = &row.record else {
        return Ok(0);
    };
    if message.role != "tool" {
        return Ok(0);
    }
    let observation = hermes_pro_output(message, row)?;
    let mut counter = HermesOwnedByteCounter::default();
    counter.add_fixed(size_of::<u8>());
    counter.add_string(&observation.coordinate.unit_key);
    counter.add_fixed(size_of::<u64>());
    counter.add_optional_string(observation.coordinate.native_record_id.as_deref());
    counter.add_optional_fixed(
        observation.coordinate.source_record_ordinal.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(
        observation
            .coordinate
            .source_record_subrecord_index
            .is_some(),
        size_of::<u32>(),
    );
    counter.add_optional_fixed(
        observation.coordinate.byte_start.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(
        observation.coordinate.byte_end_exclusive.is_some(),
        size_of::<u64>(),
    );
    counter.add_optional_fixed(observation.occurred_at_unix_ms.is_some(), size_of::<i64>());
    counter.add_string(&observation.associations.direct_session_id);
    counter.add_string(&observation.associations.root_session_id);
    counter.add_optional_string(observation.associations.parent_session_id.as_deref());
    counter.add_optional_string(observation.associations.provider_session_id.as_deref());
    counter.add_optional_string(observation.associations.agent_id.as_deref());
    counter.add_fixed(size_of::<u8>());
    if let Some(repository) = &observation.associations.repository {
        counter.add_string(&repository.repository_id);
        counter.add_optional_string(repository.checkout_id.as_deref());
        counter.add_optional_string(repository.worktree_id.as_deref());
        counter.add_optional_string(repository.object_format.as_deref());
    }
    counter.add_optional_string(observation.call_id.as_deref());
    counter.add_fixed(size_of::<u8>());
    if let Some(command) = &observation.command {
        counter.add_string(&command.tool_name);
        counter.add_string(&command.command);
        counter.add_optional_string(command.working_directory.as_deref());
    }
    counter.add_fixed(size_of::<u8>());
    counter.add_optional_fixed(observation.outcome.exit_code.is_some(), size_of::<i32>());
    counter.add_optional_fixed(observation.outcome.duration_ms.is_some(), size_of::<u64>());
    counter.add_fixed(size_of::<u32>());
    counter.add_string(&observation.locator.kind);
    counter.add_bytes(&observation.locator.payload);
    counter.add_bytes(&observation.content);
    Ok(counter.bytes)
}

fn localize_dependent_messages(
    committed_store: &Store,
    context: &PublicationContext<'_>,
    plan: &CorePlan,
    page: &mut HermesPage,
) -> Result<()> {
    let page_sessions = page
        .rows
        .iter()
        .filter_map(|row| match &row.record {
            HermesNativeRecord::Session(session) => Some(session.id.clone()),
            HermesNativeRecord::Message { .. } | HermesNativeRecord::Rejected(_) => None,
        })
        .collect::<BTreeSet<_>>();
    let dependencies = page
        .rows
        .iter()
        .filter_map(|row| match &row.record {
            HermesNativeRecord::Message { row, .. } => Some((row.session_id.clone(), row.id)),
            HermesNativeRecord::Session(_) | HermesNativeRecord::Rejected(_) => None,
        })
        .collect::<Vec<_>>();
    let mut available = BTreeMap::new();
    for (provider_session_id, _) in &dependencies {
        if available.contains_key(provider_session_id) {
            continue;
        }
        if page_sessions.contains(provider_session_id) {
            available.insert(provider_session_id.clone(), true);
            continue;
        }
        let source = committed_store.capture_source_by_canonical_identity_session(
            CaptureProvider::Hermes,
            HERMES_SQLITE_SOURCE_FORMAT,
            &context.adapter.machine_id,
            &plan.cursor.canonical_source_identity,
            provider_session_id,
        )?;
        let present = match source {
            Some(source) => committed_store
                .session_by_capture_source_and_external_session(
                    source.id,
                    CaptureProvider::Hermes,
                    provider_session_id,
                )?
                .is_some(),
            None => false,
        };
        available.insert(provider_session_id.clone(), present);
    }
    for row in &mut page.rows {
        let rejection = match &row.record {
            HermesNativeRecord::Message { row, .. }
                if !available.get(&row.session_id).copied().unwrap_or(false) =>
            {
                Some(format!(
                    "Hermes message {} depends on malformed or missing session {}",
                    row.id, row.session_id
                ))
            }
            HermesNativeRecord::Session(_)
            | HermesNativeRecord::Message { .. }
            | HermesNativeRecord::Rejected(_) => None,
        };
        if let Some(reason) = rejection {
            row.replace_with_rejection(reason);
        }
    }
    Ok(())
}
