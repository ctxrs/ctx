mod core;
mod output;

use core::{noop_summary, warp_source_id, WarpCoreStoreSink};
use output::{
    decode_warp_cursor, known_warp_route, replay_outputs_or_mark_behind, retire_known_route,
};

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventType, Fidelity, Session, SessionEdge, SessionEdgeType, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    CanonicalActor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::nativepath::{
    prepare_warp_nativepath_lifecycle, scan_prepared_warp_nativepath, WarpNativeEvent,
    WarpNativeFrontier, WarpNativeHierarchyEdge, WarpNativeMessageIdentity, WarpNativePage,
    WarpNativePersistedState, WarpNativePreparationInputs, WarpNativePreparationOutcome,
    WarpNativeProOutputPage, WarpNativeProOutputPageReceipt, WarpNativeProfile,
    WarpNativeRejection, WarpNativeScanOutcome, WarpNativeSession, WarpNativeSink,
    WarpNativeSourceAuthority, WarpNativeSourceFailure, WarpNativeSourceFailureKind,
    WarpNativeSourceIdentity, WARP_NATIVE_PARSER_REVISION, WARP_NATIVE_POLICY_REVISION,
};
use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentSourceFamily,
    VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
    provider_path_identity, provider_source_cursor_stream_for_path,
    provider_source_event_import_identity, provider_source_identity, provider_sync_metadata,
    timestamps, CertifiedProviderCursor, ProviderEventImportIdentity,
};
use crate::provider::normalization::provider_policy_event_text;
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputNativeCursor, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    ProviderSourceFailureKind, Result, WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_CURSOR_VERSION: u32 = 1;
const WARP_CURSOR_KIND: &str = "warp-nativepath";
const WARP_PUBLICATION_DOMAIN: &[u8] = b"ctx-warp-nativepath-publication-v1\0";
const WARP_RETIREMENT_DOMAIN: &[u8] = b"ctx-warp-nativepath-retirement-v1\0";
const WARP_OUTPUT_FRONTIER_VERSION: u32 = 1;
const WARP_OUTPUT_PARSER_REVISION: &str = "warp-nativepath-output-v1";
const WARP_CONTENT_LOCATOR_KIND: &str = "warp-task-message-v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarpCursorWire {
    version: u32,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<WarpNativePersistedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replacement_prior_source_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    released_migration: Option<ReleasedWarpMigrationState>,
}

struct DecodedWarpCursor {
    state: Option<WarpNativePersistedState>,
    replacement_prior_source_identity: Option<String>,
    released_migration: Option<ReleasedWarpMigrationState>,
    released_source_revision: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedWarpMigrationState {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
}

#[derive(Clone)]
struct WarpPublicationContext {
    machine_id: String,
    raw_source_path: String,
    source_root: String,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    source_revision: String,
    replacement_prior_source_id: Option<Uuid>,
    replacement_prior_source_identity: Option<String>,
    released_migration: Option<ReleasedWarpMigrationState>,
    released_source_ids: BTreeMap<String, Uuid>,
}

#[derive(Clone)]
struct KnownWarpRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    cursor: SyncCursor,
    released_migration: Option<ReleasedWarpMigrationState>,
    released_source_ids: BTreeMap<String, Uuid>,
}

struct KnownWarpRouteQuery<'a> {
    machine_id: &'a str,
    path: &'a Path,
    configured_root: &'a Path,
    path_identity: &'a str,
    cursor_stream: &'a str,
    cursor: Option<&'a SyncCursor>,
    decoded: Option<&'a DecodedWarpCursor>,
}

struct WarpPublicationContextRequest<'a> {
    inputs: &'a WarpNativePreparationInputs,
    adapter: &'a ProviderAdapterContext,
    configured_root: &'a Path,
    cursor_stream: String,
    history_record_id: Option<Uuid>,
    inventory_observation_token: Option<&'a str>,
    replacement_prior_source_identity: Option<String>,
    released_migration: Option<ReleasedWarpMigrationState>,
    released_source_ids: BTreeMap<String, Uuid>,
}

pub(super) fn import_warp_nativepath(
    path: &Path,
    store: &mut Store,
    mut adapter: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if adapter.source_path.is_none() {
        adapter.source_path = Some(path.to_path_buf());
    }
    let configured_root = adapter
        .source_root
        .clone()
        .or_else(|| adapter.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let sink = options.import_profile.sink().cloned();

    let path_identity = provider_path_identity_for_missing(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let current_cursor = store.get_sync_cursor(None, &adapter.machine_id, &cursor_stream)?;
    let decoded_cursor = current_cursor
        .as_ref()
        .map(|cursor| decode_warp_cursor(&cursor.cursor))
        .transpose()?
        .flatten();
    let previous = decoded_cursor
        .as_ref()
        .and_then(|cursor| cursor.state.clone());
    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(path, &adapter, sink.as_deref(), previous.as_ref());
        return Ok(ProviderImportSummary::default());
    }
    let known_route = known_warp_route(
        store,
        KnownWarpRouteQuery {
            machine_id: &adapter.machine_id,
            path,
            configured_root: &configured_root,
            path_identity: &path_identity,
            cursor_stream: &cursor_stream,
            cursor: current_cursor.as_ref(),
            decoded: decoded_cursor.as_ref(),
        },
    )?;

    let prepared = match prepare_warp_nativepath_lifecycle(path, previous.as_slice()) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp {
            persisted_state, ..
        } => {
            replay_outputs_or_mark_behind(path, &adapter, sink.as_deref(), Some(&persisted_state));
            return Ok(noop_summary());
        }
        WarpNativePreparationOutcome::Incomplete(failure) => {
            return Err(preparation_error(failure));
        }
        WarpNativePreparationOutcome::Failed(failure)
            if failure.kind == WarpNativeSourceFailureKind::NotFound =>
        {
            if let Some(route) = known_route {
                let reason = if adapter
                    .source_root
                    .as_deref()
                    .is_some_and(|root| !root.exists())
                {
                    ProviderSourceRouteRetirementReason::RootMissing
                } else {
                    ProviderSourceRouteRetirementReason::SourceMissing
                };
                return retire_known_route(
                    store,
                    &adapter.machine_id,
                    adapter.imported_at,
                    &route,
                    reason,
                );
            }
            return Err(preparation_error(failure));
        }
        WarpNativePreparationOutcome::Failed(failure) => {
            return Err(preparation_error(failure));
        }
    };

    let mut replacement_prior_source_identity = decoded_cursor
        .as_ref()
        .and_then(|cursor| cursor.replacement_prior_source_identity.clone());
    if let (Some(previous), Some(route)) = (previous.as_ref(), known_route.as_ref()) {
        if previous.source_identity != prepared.inputs.source_identity {
            retire_known_route(
                store,
                &adapter.machine_id,
                adapter.imported_at,
                route,
                ProviderSourceRouteRetirementReason::Replaced,
            )?;
            replacement_prior_source_identity = Some(route.canonical_source_identity.clone());
        }
    }
    let context = publication_context(WarpPublicationContextRequest {
        inputs: &prepared.inputs,
        adapter: &adapter,
        configured_root: &configured_root,
        cursor_stream,
        history_record_id: options.history_record_id,
        inventory_observation_token: options.inventory_observation_token.as_deref(),
        replacement_prior_source_identity,
        released_migration: known_route
            .as_ref()
            .and_then(|route| route.released_migration.clone()),
        released_source_ids: known_route
            .as_ref()
            .map(|route| route.released_source_ids.clone())
            .unwrap_or_default(),
    })?;
    let preparation_inputs = prepared.inputs.clone();
    let committed = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = {
        let mut sink = WarpCoreStoreSink {
            store,
            committed: &committed,
            bulk_guard: &bulk_guard,
            context: &context,
            inputs: &preparation_inputs,
            work_limit: options.capture_work_limit,
            pages_committed: 0,
            stopped: false,
            summary: ProviderImportSummary::default(),
        };
        let outcome =
            scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut sink);
        match outcome {
            Ok(WarpNativeScanOutcome::Complete(authority)) if !sink.stopped => {
                sink.publish_terminal(&authority)?;
                Ok((std::mem::take(&mut sink.summary), Some(authority)))
            }
            Ok(WarpNativeScanOutcome::Complete(_)) => {
                sink.summary.work_remaining = true;
                Ok((std::mem::take(&mut sink.summary), None))
            }
            Ok(WarpNativeScanOutcome::Incomplete(_)) => {
                Err(CaptureError::SourceChangedDuringCapture)
            }
            Err(error) => Err(error),
        }
    };
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let (mut summary, authority) = match (operation, finish) {
        (Ok(result), Ok(())) => result,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if let (Some(authority), Some(output_sink)) = (authority.as_ref(), sink.as_deref()) {
        replay_outputs_or_mark_behind(
            path,
            &adapter,
            Some(output_sink),
            Some(&authority.persisted_state),
        );
    }
    if summary.work_result() == ProviderImportWorkResult::NoOp && summary.skipped == 0 {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

fn provider_path_identity_for_missing(path: &Path) -> Result<String> {
    match provider_path_identity(path) {
        Ok(identity) => Ok(identity),
        Err(_) => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            Ok(absolute.display().to_string())
        }
    }
}

fn preparation_error(failure: WarpNativeSourceFailure) -> CaptureError {
    let kind = match failure.kind {
        WarpNativeSourceFailureKind::NotFound => ProviderSourceFailureKind::NotFound,
        WarpNativeSourceFailureKind::Permission => ProviderSourceFailureKind::Permission,
        WarpNativeSourceFailureKind::Locked => ProviderSourceFailureKind::Locked,
        WarpNativeSourceFailureKind::Corrupt => ProviderSourceFailureKind::Corrupt,
        WarpNativeSourceFailureKind::SchemaIncompatible => {
            ProviderSourceFailureKind::SchemaIncompatible
        }
        WarpNativeSourceFailureKind::InvalidSource => ProviderSourceFailureKind::InvalidSource,
        WarpNativeSourceFailureKind::SourceChanged => ProviderSourceFailureKind::SourceChanged,
        WarpNativeSourceFailureKind::SourceDatabase => ProviderSourceFailureKind::SourceDatabase,
        WarpNativeSourceFailureKind::Io => ProviderSourceFailureKind::Io,
    };
    CaptureError::ProviderSource {
        provider: "Warp",
        path: failure.canonical_route,
        kind,
        detail: failure.detail,
    }
}

fn publication_context(
    request: WarpPublicationContextRequest<'_>,
) -> Result<WarpPublicationContext> {
    let WarpPublicationContextRequest {
        inputs,
        adapter,
        configured_root,
        cursor_stream,
        history_record_id,
        inventory_observation_token,
        replacement_prior_source_identity,
        released_migration,
        released_source_ids,
    } = request;
    let generated_source_identity =
        warp_canonical_source_identity(&adapter.machine_id, &inputs.source_identity)?;
    let mut source_revision = format!(
        "warp-nativepath-v1:parser={};policy={};capability={};snapshot={}",
        inputs.parser_revision,
        inputs.policy_revision,
        inputs.capability_digest,
        inputs.snapshot_revision,
    );
    if let Some(token) = inventory_observation_token {
        let mut digest = Sha256::new();
        digest.update(b"ctx-warp-nativepath-inventory-observation-v1\0");
        digest.update((source_revision.len() as u64).to_be_bytes());
        digest.update(source_revision.as_bytes());
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
        source_revision = format!("inventory-observation-sha256-v1:{:x}", digest.finalize());
    }
    let proposed_source_identity = released_migration
        .as_ref()
        .map(|migration| migration.canonical_source_identity.clone())
        .unwrap_or(generated_source_identity);
    let locator_identity = released_migration
        .as_ref()
        .map(|migration| migration.locator_identity.clone())
        .unwrap_or_else(|| format!("{cursor_stream}#{proposed_source_identity}"));
    let released_migration = released_migration.map(|migration| ReleasedWarpMigrationState {
        source_revision: source_revision.clone(),
        ..migration
    });
    Ok(WarpPublicationContext {
        machine_id: adapter.machine_id.clone(),
        raw_source_path: inputs.canonical_route.display().to_string(),
        source_root: configured_root.display().to_string(),
        imported_at: adapter.imported_at,
        history_record_id,
        locator_identity,
        cursor_stream,
        proposed_source_identity,
        source_revision,
        replacement_prior_source_id: replacement_prior_source_identity
            .as_deref()
            .map(warp_source_id),
        replacement_prior_source_identity,
        released_migration,
        released_source_ids,
    })
}

fn warp_canonical_source_identity(
    machine_id: &str,
    source_identity: &WarpNativeSourceIdentity,
) -> Result<String> {
    let source_identity_wire = serde_json::to_string(source_identity)?;
    Ok(format!(
        "warp-nativepath:{}",
        stable_capture_uuid(
            &format!(
                "{machine_id}:{}:{source_identity_wire}",
                WARP_SQLITE_SOURCE_FORMAT
            ),
            "canonical-source"
        )
    ))
}
