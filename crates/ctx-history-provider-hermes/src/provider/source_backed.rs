//! Provider-local source-backed Hermes adapter.
//!
//! This module deliberately stops at discovery, bounded native projection,
//! source certification, and complete direct Core projection. Publication,
//! replacement/deletion lifecycle, and projection fanout remain shared
//! responsibilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CertifiedSource, CoreActivity, CoreRecord,
    CoreRecordError, EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProjectionContractError, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceAnchorScope, SourceKey, SourceObservation,
    StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::ProviderSourceRoot,
    lifecycle::CaptureLifecycleSink,
    native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
    normalization::provider_required_timestamp_seconds,
    provider_sources::{
        retain_sqlite_source_directory_authority, ProviderSource, SqliteArtifactKind,
        SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
        SqliteSourceDirectoryAuthority, SqliteSourceErrorComposition, SqliteSourceEvidence,
        SqliteSourceProgressError, SqliteSourceReadSnapshot,
    },
    source_backed::{
        family::document::{DocumentAppendBase, DocumentLeafFingerprint, ObservedDocumentLeaf},
        sqlite_source_progress, SourceBackedCurrentSourceProgress,
        SourceBackedCurrentSourceProgressStage, SourceBackedReconciliationDemand,
        SourceBackedRouteError, SourceBackedRouteResult,
    },
    source_sqlite::sqlite_schema_fingerprint,
    CaptureError, HERMES_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    hermes_layout_record_digest, hermes_native_event,
    layout::{HermesMessageRow, HermesSchema, HermesSessionRow},
    sqlite::{
        hermes_max_rowid, hermes_message_cursor_page, hermes_message_session_id,
        hermes_session_identity_page, HermesExactMessageSpool, HermesMessageSpoolRange,
        HermesNativeRecord, HermesNativeRow, HermesPhase, HermesRowReader,
    },
    HermesNativeEvent, HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

const HERMES_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile";
const HERMES_SESSION_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile-session";
const HERMES_SESSION_NAMESPACE: &str = "hermes.session";
const HERMES_MESSAGE_NAMESPACE: &str = "hermes.message";
const HERMES_LOGICAL_SESSION_KIND: &str = "hermes-session";
const HERMES_LOGICAL_EVENT_KIND: &str = "hermes-message";
const HERMES_PROFILE_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-db-v1";
const HERMES_SESSION_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-session-v1";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Hermes SQLite source must have an authorized parent and database leaf";
const HERMES_SOURCE_PARSER_REVISION: &str = "hermes-source-backed-v5-optional-admission";
const HERMES_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-session-content-v1\0";
const HERMES_TREE_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-source-inventory-v1\0";
const HERMES_LEAF_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-session-leaf-v1\0";
const HERMES_SESSION_OBSERVATION_DOMAIN: &[u8] = b"ctx-hermes-session-observation-v1\0";
const HERMES_MESSAGE_OBSERVATION_DOMAIN: &[u8] = b"ctx-hermes-message-observation-v1\0";
const HERMES_SESSION_OBSERVATION_KIND: &str = "hermes-session-observation-v1";
const HERMES_INCREMENTAL_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-incremental-session-v1\0";
const HERMES_INCREMENTAL_CONTENT_DOMAIN: &[u8] = b"ctx-hermes-incremental-content-v1\0";
const HERMES_EXACT_INTERVAL_MS: i64 = 60 * 60 * 1_000;
const HERMES_ROUTE_CONTROL_KIND: &str = "hermes-route-control-v1";
const HERMES_ROUTE_CONTROL_VERSION: u32 = 2;
const HERMES_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-session-v1\0";
const HERMES_REJECTION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-rejection-v1\0";

mod contracts;
mod incremental;
mod inventory;
mod projection;
pub(crate) mod replacement;
mod session_scan;

pub(crate) use contracts::*;
use incremental::*;
use inventory::*;
use projection::*;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use projection::{
    inventory_observation_rows, logical_row_traversals, reset_logical_row_traversals,
    session_scan_receipts,
};
#[cfg(feature = "test-support")]
use replacement::{document_base_route_source_visits, reset_document_base_route_source_visits};
use session_scan::*;

#[cfg(feature = "test-support")]
pub(crate) fn reset_base_route_source_visits() {
    reset_document_base_route_source_visits();
}

#[cfg(feature = "test-support")]
pub(crate) fn base_route_source_visits() -> u64 {
    document_base_route_source_visits()
}

#[derive(Clone)]
pub(crate) struct HermesSessionLeaf<L: CaptureLifecycleSink>
where
    L::PinnedAppendBase: Clone,
{
    provider_session_id: String,
    source: SourceKey,
    observation_revision: Vec<u8>,
    incremental: Option<HermesIncrementalLeaf<L>>,
    exact_message_range: Option<HermesMessageSpoolRange>,
}

#[derive(Clone)]
struct HermesIncrementalLeaf<L: CaptureLifecycleSink>
where
    L::PinnedAppendBase: Clone,
{
    base: Option<DocumentAppendBase<L>>,
    session: HermesNativeRow,
    messages: Vec<HermesNativeRow>,
}

struct HermesSessionInventory<L: CaptureLifecycleSink>
where
    L::PinnedAppendBase: Clone,
{
    schema: HermesSchema,
    schema_evidence: Vec<u8>,
    leaves: Vec<ObservedDocumentLeaf<HermesSessionLeaf<L>>>,
    tree_fingerprint: [u8; 32],
    max_session_rowid: i64,
    max_message_rowid: i64,
    reconciliation_demand: SourceBackedReconciliationDemand,
    publication_receipt: Option<HermesRefreshReceipt>,
    message_spool: Option<HermesExactMessageSpool>,
}

trait HermesReconciliationContext<L: CaptureLifecycleSink> {
    fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand;
    fn route_control(&self) -> Option<&[u8]>;
    fn exact_base_source(&self, source: &SourceKey) -> Option<DocumentAppendBase<L>>;
    fn report_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HermesRefreshReceipt {
    kind: String,
    version: u32,
    profile_source_descriptor: [u8; 32],
    database_identity: [u8; 32],
    schema_evidence: [u8; 32],
    session_rowid: i64,
    message_rowid: i64,
    last_successful_exhaustive_at_ms: i64,
    exact_due_at_ms: i64,
    exhaustive_sequence: u64,
    mode: String,
    outcome: String,
}

fn observe_hermes_reconciliation_inventory<L: CaptureLifecycleSink>(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    base_route_control: Option<&[u8]>,
    requested: SourceBackedReconciliationDemand,
    database_identity: [u8; 32],
    now_ms: i64,
    context: &mut dyn HermesReconciliationContext<L>,
) -> HermesSourceBackedResult<HermesSessionInventory<L>>
where
    L::PinnedAppendBase: Clone,
{
    let (schema, schema_evidence) = detect_hermes_schema(conn)?;
    let schema_digest: [u8; 32] = Sha256::digest(&schema_evidence).into();
    let prior = hermes_refresh_receipt(base_route_control);
    let current_session_rowid = hermes_max_rowid(conn, "sessions")?;
    let current_message_rowid = hermes_max_rowid(conn, "messages")?;
    let forced_exhaustive = prior.as_ref().is_none_or(|receipt| {
        receipt.kind != HERMES_ROUTE_CONTROL_KIND
            || receipt.version != HERMES_ROUTE_CONTROL_VERSION
            || receipt.profile_source_descriptor != candidate.source.exact_descriptor_digest()
            || receipt.database_identity != database_identity
            || receipt.schema_evidence != schema_digest
            || current_session_rowid < receipt.session_rowid
            || current_message_rowid < receipt.message_rowid
    });
    let demand = if requested == SourceBackedReconciliationDemand::Exhaustive || forced_exhaustive {
        SourceBackedReconciliationDemand::Exhaustive
    } else {
        SourceBackedReconciliationDemand::Incremental
    };

    let mut inventory = if demand == SourceBackedReconciliationDemand::Exhaustive {
        observe_hermes_session_inventory_with_schema(
            candidate,
            conn,
            schema,
            schema_evidence.clone(),
            &mut |progress| context.report_progress(progress),
        )?
    } else {
        observe_hermes_incremental_inventory(
            candidate,
            conn,
            prior.as_ref().expect("incremental Hermes receipt"),
            current_session_rowid,
            current_message_rowid,
            schema,
            schema_evidence.clone(),
            context,
        )?
    };

    let (last_exact, exact_due, exhaustive_sequence) = match demand {
        SourceBackedReconciliationDemand::Exhaustive => (
            now_ms,
            now_ms.saturating_add(HERMES_EXACT_INTERVAL_MS),
            prior.as_ref().map_or(1, |receipt| {
                if receipt.last_successful_exhaustive_at_ms == now_ms {
                    receipt.exhaustive_sequence
                } else {
                    receipt.exhaustive_sequence.saturating_add(1)
                }
            }),
        ),
        SourceBackedReconciliationDemand::Incremental => {
            let prior = prior.as_ref().expect("incremental Hermes receipt");
            (
                prior.last_successful_exhaustive_at_ms,
                prior.exact_due_at_ms,
                prior.exhaustive_sequence,
            )
        }
    };
    let mode = match (demand, prior.as_ref()) {
        (SourceBackedReconciliationDemand::Incremental, Some(prior))
            if prior.session_rowid == inventory.max_session_rowid
                && prior.message_rowid == inventory.max_message_rowid =>
        {
            prior.mode.clone()
        }
        _ => demand.as_str().to_owned(),
    };
    let receipt = HermesRefreshReceipt {
        kind: HERMES_ROUTE_CONTROL_KIND.to_owned(),
        version: HERMES_ROUTE_CONTROL_VERSION,
        profile_source_descriptor: candidate.source.exact_descriptor_digest(),
        database_identity,
        schema_evidence: schema_digest,
        session_rowid: inventory.max_session_rowid,
        message_rowid: inventory.max_message_rowid,
        last_successful_exhaustive_at_ms: last_exact,
        exact_due_at_ms: exact_due,
        exhaustive_sequence,
        mode,
        outcome: "successful".to_owned(),
    };
    inventory.publication_receipt = Some(receipt);
    Ok(inventory)
}

fn hermes_refresh_receipt(control: Option<&[u8]>) -> Option<HermesRefreshReceipt> {
    control.and_then(|control| serde_json::from_slice(control).ok())
}

fn hermes_now_ms() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX);
    now - now.rem_euclid(60_000)
}

pub fn hermes_route_control_exact_due(control: &[u8], now_ms: i64) -> Option<bool> {
    let value = serde_json::from_slice::<serde_json::Value>(control).ok()?;
    if value.get("kind").and_then(serde_json::Value::as_str) != Some(HERMES_ROUTE_CONTROL_KIND) {
        return None;
    }
    Some(
        serde_json::from_value::<HermesRefreshReceipt>(value).map_or(true, |receipt| {
            receipt.version != HERMES_ROUTE_CONTROL_VERSION
                || receipt.outcome != "successful"
                || receipt.exact_due_at_ms <= now_ms
        }),
    )
}

pub fn hermes_route_control_exact_due_for_profile(
    control: &[u8],
    profile_source_descriptor: [u8; 32],
    now_ms: i64,
) -> Option<bool> {
    let receipt = hermes_refresh_receipt(Some(control))?;
    (receipt.kind == HERMES_ROUTE_CONTROL_KIND
        && receipt.version == HERMES_ROUTE_CONTROL_VERSION
        && receipt.outcome == "successful"
        && receipt.profile_source_descriptor == profile_source_descriptor)
        .then_some(receipt.exact_due_at_ms <= now_ms)
}

pub fn hermes_route_control_database_identity(control: &[u8]) -> Option<[u8; 32]> {
    let receipt = hermes_refresh_receipt(Some(control))?;
    (receipt.kind == HERMES_ROUTE_CONTROL_KIND
        && receipt.version == HERMES_ROUTE_CONTROL_VERSION
        && receipt.outcome == "successful")
        .then_some(receipt.database_identity)
}

#[cfg(test)]
fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(data_root, path, || {}, false, &mut |_| {
        Ok(())
    })
}

fn open_root_authorized_snapshot_with_progress(
    data_root: &Path,
    path: &Path,
    incremental: bool,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(
        data_root,
        path,
        || {},
        incremental,
        report_progress,
    )
}

#[cfg(test)]
fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook_and_progress(
        data_root,
        path,
        after_authorize,
        false,
        &mut |_| Ok(()),
    )
}

fn open_root_authorized_snapshot_with_hook_and_progress(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
    incremental: bool,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let admission_root = ProviderSourceRoot::open(parent)?;
    let admission_directory = admission_root.directory()?;
    let parent_handle = admission_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot = if incremental {
        sqlite_authority.open_incremental_snapshot_with_progress(database_leaf, |progress| {
            report_progress(sqlite_source_progress(progress))
        })
    } else {
        sqlite_authority.open_stable_snapshot_with_progress(database_leaf, |progress| {
            report_progress(sqlite_source_progress(progress))
        })
    }
    .map_err(|error| match error {
        SqliteSourceProgressError::Source(error) => HermesSourceBackedError::from(error),
        SqliteSourceProgressError::Progress(error) => HermesSourceBackedError::from(error),
        SqliteSourceProgressError::ProgressAndFinalization {
            primary,
            finalization,
        } => {
            HermesSourceBackedError::from(primary).compose_sqlite_source_finalization(finalization)
        }
    })?;
    after_authorize();
    let configure = (|| {
        sqlite_snapshot.revalidate()?;
        let connection = sqlite_snapshot.connection()?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| HermesSourceBackedError::CountOverflow)?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|source| {
                sqlite_snapshot.diagnose_provider_query_error(
                    "setting the private Hermes SQLite busy timeout",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        Ok(())
    })();
    if let Err(error) = configure {
        return Err(abort_hermes_snapshot(sqlite_snapshot, error));
    }
    Ok((sqlite_authority, sqlite_snapshot))
}

fn abort_hermes_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: HermesSourceBackedError,
) -> HermesSourceBackedError {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => HermesSourceBackedError::Route(
            crate::source_backed::combine_primary_and_cleanup_route_errors(
                replacement::hermes_route_error(primary),
                replacement::hermes_sqlite_route_error(cleanup),
            ),
        ),
    }
}

#[cfg(test)]
mod tests;
