use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventType, Fidelity, FileTouched, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    EventSearchBulkGuard, NativePathCursorSetClassification, NativePathCursorTransition,
    NativePathGroupAccounting, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store, StoreError,
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
            provider_source_cursor_stream_for_path, provider_source_event_import_identity,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text, provider_role,
            provider_timestamp_seconds,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    lifecycle::GooseNativePersistedState,
    native_path::{
        GooseNativePage, GooseNativePathReader, GooseNativeProFrontier, GooseNativeProfile,
        GooseNativeSourceSelection,
    },
    normalization::{
        goose_event_payload_hash, goose_timestamp, GooseNativeEvent, GooseNativeEventKind,
        GooseNativeSession,
    },
    position::{GooseNativeScanPhase, GooseNativeScanPosition},
    stream::GooseNativePageLimits,
};

mod publication;

use publication::*;

const GOOSE_NATIVE_CURSOR_VERSION: u32 = 1;
const GOOSE_NATIVE_CURSOR_KIND: &str = "goose_nativepath";
const GOOSE_NATIVE_CURSOR_DOMAIN: &[u8] = b"ctx-goose-nativepath-publication-v1\0";
const GOOSE_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-goose-nativepath-source-revision-v1\0";
const GOOSE_INVENTORY_TOKEN_DOMAIN: &[u8] = b"ctx-goose-nativepath-inventory-token-v1\0";
const GOOSE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const GOOSE_OUTPUT_PARSER_REVISION: &str = "goose-nativepath-output-v1";
const GOOSE_MAX_TOUCHES_PER_EVENT: usize = 32;
const GOOSE_PRODUCTION_PAGE_BYTES: u64 = 6 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GooseNativeCursorWire {
    version: u32,
    kind: String,
    selected_path: PathBuf,
    source_revision: String,
    frontier: GooseNativeScanPosition,
    retained_events: u64,
    rejected_records: u64,
    terminal_state: Option<GooseNativePersistedState>,
}

#[derive(Clone)]
struct GooseKnownRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    wire: GooseNativeCursorWire,
}

struct GoosePublicationContext<'a> {
    machine_id: &'a str,
    source_root: &'a Path,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
}

struct GoosePagePublication<'a> {
    reader: &'a GooseNativePathReader,
    source_revision: &'a str,
    page: &'a GooseNativePage,
    retained_events: u64,
    rejected_records: u64,
    terminal_state: Option<GooseNativePersistedState>,
}

struct ResolvedSession {
    source_id: Uuid,
    session: Session,
}

pub(super) fn import_goose_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let known_routes = known_goose_routes(store, &context.machine_id, path)?;
    let sink = options.import_profile.sink().cloned();

    if !path_exists(path)? {
        if options.import_profile.is_replay_only() {
            return Ok(ProviderImportSummary::default());
        }
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose sessions.db does not exist",
            });
        }
        return retire_missing_goose_route(
            store,
            &context,
            &known_routes[0],
            if source_root.exists() {
                ProviderSourceRouteRetirementReason::SourceMissing
            } else {
                ProviderSourceRouteRetirementReason::RootMissing
            },
        );
    }

    let authority_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let inventory_token = options
        .inventory_observation_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .unwrap_or_else(|| goose_inventory_token(&authority_path));
    let selection = GooseNativeSourceSelection::exact(&authority_path)
        .with_inventory_observation_token(Some(inventory_token));
    let reader = GooseNativePathReader::acquire(selection)?;
    let source_revision = goose_source_revision(&reader);
    // Core publication and Pro replay use independent, fully finished source
    // snapshots. The Core scanner never keeps a query-capable SQLite guard
    // alive merely because a Pro sink is configured.
    let profile = GooseNativeProfile::CoreOnly;

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &reader,
            &source_root,
            &source_revision,
            context.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let publication_context = GoosePublicationContext {
        machine_id: &context.machine_id,
        source_root: &source_root,
        imported_at: context.imported_at,
        history_record_id: options.history_record_id,
    };
    let mut scanner = reader.scanner_with_profile(profile, goose_production_page_limits()?)?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut retained_events = 0_u64;
        let mut rejected_records = 0_u64;
        let mut emitted = false;
        let mut stopped = false;

        while let Some(page) = scanner.next_page()? {
            emitted = true;
            retained_events = retained_events.saturating_add(page.events.len() as u64);
            rejected_records = rejected_records.saturating_add(page.rejections.len() as u64);
            let terminal_state = if page.terminal {
                Some(GooseNativePersistedState::from_summary(
                    &scanner.finish_core()?,
                )?)
            } else {
                None
            };
            let outcome = publish_goose_page(
                store,
                &committed_store,
                &bulk_guard,
                &publication_context,
                GoosePagePublication {
                    reader: &reader,
                    source_revision: &source_revision,
                    page: &page,
                    retained_events,
                    rejected_records,
                    terminal_state,
                },
            )?;
            let changed = outcome.work_result() == ProviderImportWorkResult::Changed;
            summary.merge_from(outcome);
            if changed && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = !page.terminal;
                stopped = true;
                break;
            }
        }

        if !stopped && !emitted {
            let scan_summary = scanner.finish_core()?;
            let terminal_state = GooseNativePersistedState::from_summary(&scan_summary)?;
            summary.merge_from(publish_goose_observation(
                store,
                &bulk_guard,
                &publication_context,
                &reader,
                &source_revision,
                terminal_state,
            )?);
        }
        Ok((summary, stopped))
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let (summary, stopped) = match (operation, finish) {
        (Ok(result), Ok(())) => result,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if !stopped {
        replay_outputs_or_mark_behind(
            &reader,
            &source_root,
            &source_revision,
            context.imported_at,
            sink.as_deref(),
        );
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn record_rejections(page: &GooseNativePage, summary: &mut ProviderImportSummary) {
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(rejection.sqlite_rowid.max(0))
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.reason.clone(),
        });
    }
}

fn skipped_page_summary(page: &GooseNativePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary {
        skipped_sessions: page.sessions.len(),
        skipped_events: page.events.len(),
        skipped: page.sessions.len().saturating_add(page.events.len()),
        ..ProviderImportSummary::default()
    };
    record_rejections(page, &mut summary);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn goose_page_already_committed(
    stored: Option<&SyncCursor>,
    source_revision: &str,
    page: &GooseNativePage,
) -> Result<bool> {
    let Some(stored) = stored else {
        return Ok(false);
    };
    let Some(wire) = decode_goose_cursor(&stored.cursor)? else {
        return Ok(false);
    };
    if wire.source_revision != source_revision {
        return Ok(false);
    }
    Ok(wire.frontier == page.next_frontier
        || wire.frontier.phase == GooseNativeScanPhase::Complete
        || wire.frontier.native_rows_seen > page.next_frontier.native_rows_seen)
}

fn validate_goose_cursor_predecessor(
    stored: Option<&SyncCursor>,
    source_revision: &str,
    expected: GooseNativeScanPosition,
) -> Result<()> {
    let Some(stored) = stored else {
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath page has no committed predecessor".to_owned(),
        ));
    };
    let Some(wire) = decode_goose_cursor(&stored.cursor)? else {
        let _ = CertifiedProviderCursor::decode_if_certified(&stored.cursor)?;
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose released-cursor migration must restart at the initial frontier".to_owned(),
        ));
    };
    if wire.source_revision != source_revision {
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose changed generation did not restart at the initial frontier".to_owned(),
        ));
    }
    if wire.frontier != expected {
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath cursor does not match the next bounded page".to_owned(),
        ));
    }
    Ok(())
}

fn replay_outputs_or_mark_behind(
    reader: &GooseNativePathReader,
    source_root: &Path,
    source_revision: &str,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) =
        replay_goose_outputs(reader, source_root, source_revision, imported_at, sink)
    {
        sink.mark_behind(ProOutputSinkError::new(
            "goose_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_goose_outputs(
    reader: &GooseNativePathReader,
    source_root: &Path,
    source_revision: &str,
    _imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let locator_identity = provider_path_identity(reader.source_observation().source_path())?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Goose.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == GOOSE_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<GooseNativeProFrontier>(&cursor.payload).ok());
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == source_revision
            && progress.parser_revision == GOOSE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.is_some()
    });
    let mut scanner = reader.scanner_with_profile(
        GooseNativeProfile::CoreAndPro,
        goose_production_page_limits()?,
    )?;
    while scanner.next_page()?.is_some() {}
    let _ = scanner.finish_core()?;
    if can_resume {
        scanner.resume_pro_from(progress_frontier.expect("resume frontier is present"))?;
    }
    let mut state = GooseOutputState::new(progress, can_resume, sink.materializer_revision())?;
    while let Some(page) = scanner.next_pro_output_page()? {
        let expected_frontier = goose_safe_output_frontier(page.expected_frontier)?;
        let next_frontier = goose_safe_output_frontier(page.next_frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: source_revision.to_owned(),
            parser_revision: GOOSE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_frontier.clone(),
            observations: page.observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::Goose.as_str(), &locator_identity),
            expected_frontier,
            next_frontier.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: page.accounting.logical_units,
                conservative_serialized_bytes: page.accounting.conservative_serialized_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if let Err(failure) = process_pro_replay_only(replay, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "goose_nativepath_output_replay_page",
                format!("{:?}", failure.output_error),
            ));
            return Ok(());
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_frontier = Some(next_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    let _ = scanner.finish_pro_replay()?;
    Ok(())
}

struct GooseOutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl GooseOutputState {
    fn new(
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let expected_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Goose output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn goose_safe_output_frontier(frontier: GooseNativeProFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        GOOSE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn goose_production_page_limits() -> Result<GooseNativePageLimits> {
    GooseNativePageLimits::new(64, GOOSE_PRODUCTION_PAGE_BYTES)
}

fn known_goose_routes(
    store: &Store,
    machine_id: &str,
    selected: &Path,
) -> Result<Vec<GooseKnownRoute>> {
    let selected = selected
        .canonicalize()
        .unwrap_or_else(|_| selected.to_path_buf());
    let mut routes = BTreeMap::<String, GooseKnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Goose
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT)
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
        if path != selected && path.canonicalize().ok().as_ref() != Some(&selected) {
            continue;
        }
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(wire) = decode_goose_cursor(&current_cursor.cursor)? else {
            continue;
        };
        routes
            .entry(locator_identity.clone())
            .or_insert(GooseKnownRoute {
                locator_identity,
                canonical_source_identity: canonical_source_identity.to_owned(),
                source_revision: source_revision.to_owned(),
                current_cursor,
                wire,
            });
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_goose_route(
    store: &mut Store,
    context: &ProviderAdapterContext,
    route: &GooseKnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let transition = NativePathCursorTransition::new(
            Some(route.current_cursor.cursor.clone()),
            provider_sync_cursor(
                &context.machine_id,
                route.current_cursor.stream.clone(),
                encode_goose_cursor(&route.wire)?,
                context.imported_at,
            ),
        );
        let retirement = ProviderSourceRouteRetirement {
            provider: CaptureProvider::Goose,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: route.locator_identity.clone(),
            cursor_stream: route.current_cursor.stream.clone(),
            expected_canonical_source_identity: route.canonical_source_identity.clone(),
            expected_source_revision: route.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason,
        };
        let publication_id = goose_retirement_publication_id(&retirement);
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

fn decode_goose_cursor(encoded: &str) -> Result<Option<GooseNativeCursorWire>> {
    let provider_cursor = ctx_history_store::decode_native_path_committed_cursor(encoded)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded.to_owned());
    let Ok(wire) = serde_json::from_str::<GooseNativeCursorWire>(&provider_cursor) else {
        return Ok(None);
    };
    if wire.version != GOOSE_NATIVE_CURSOR_VERSION || wire.kind != GOOSE_NATIVE_CURSOR_KIND {
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath cursor has an unsupported version or kind".to_owned(),
        ));
    }
    Ok(Some(wire))
}

fn encode_goose_cursor(wire: &GooseNativeCursorWire) -> Result<String> {
    serde_json::to_string(wire).map_err(CaptureError::from)
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
                CaptureProvider::Goose.as_str(),
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

fn goose_publication_id(
    page_identity: [u8; 32],
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_NATIVE_CURSOR_DOMAIN);
    digest.update(page_identity);
    digest.update(transition.key().stream().as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update(expected.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("goose-nativepath:{:x}", digest.finalize())
}

fn goose_retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-goose-nativepath-retirement-v1\0");
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("goose-nativepath-retirement:{:x}", digest.finalize())
}

fn goose_source_revision(reader: &GooseNativePathReader) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_SOURCE_REVISION_DOMAIN);
    digest.update(reader.source_observation().generation_digest().as_bytes());
    digest.update(reader.schema().capability_digest.as_bytes());
    format!("{:x}", digest.finalize())
}

fn goose_inventory_token(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_INVENTORY_TOKEN_DOMAIN);
    digest.update(path.as_os_str().as_encoded_bytes());
    format!("{:x}", digest.finalize())
}

fn goose_empty_source_id(source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("goose-nativepath-empty:{source_identity}"),
        "source",
    )
}

fn goose_legacy_event_index(event: &GooseNativeEvent) -> u64 {
    let base = event.created_timestamp.unwrap_or(event.sqlite_rowid).max(0) as u64;
    base.saturating_mul(4_096).saturating_add(
        crate::provider::normalization::text_id_index(&event.provider_message_identity, 0) % 4_096,
    )
}

fn path_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
