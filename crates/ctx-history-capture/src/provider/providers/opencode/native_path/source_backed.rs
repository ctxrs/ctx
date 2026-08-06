//! Shared source-backed projection for the OpenCode SQLite dialect family.
//!
//! This module owns provider-local discovery, parsing, certification, lexical
//! projection, replacement streaming, and complete direct Core records. Atomic
//! publication remains owned by the shared coordinator.

use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    RepositoryAbstention, RepositoryAbstentionReason, RepositoryEvidenceKind, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
    CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
};
use rusqlite::{limits::Limit, Connection, Row, Statement};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    json::{OpenCodeJsonProjection, OpenCodeRetainedJson},
    model::{
        OpenCodeNativeEventKind, OpenCodeNativeFileTouch, OpenCodeNativeRejectionKind,
        OpenCodeNativeSchemaFamily,
    },
    query::{source_backed_decode_order, source_backed_native_record_identity},
    schema::OpenCodeNativeSchema,
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        normalization::provider_required_timestamp_millis,
        providers::opencode::OpenCodeSqliteDialect,
        source_backed::{
            SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
            SourceBackedRouteError, SourceBackedRouteResult,
        },
    },
    provider_sources::{
        retain_sqlite_source_directory_authority, SqliteArtifactKind, SqliteCleanupStatus,
        SqliteFailurePhase, SqliteLogicalSnapshot, SqliteSourceAccessError,
        SqliteSourceDirectoryAuthority, SqliteSourceProgressError, SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const SOURCE_ANCHOR_KEY: &str = "active-database";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "opencode-family-source-backed-v8-session-origin";
const LOGICAL_SESSION_KIND: &str = "opencode-family-session";
const LOGICAL_EVENT_KIND: &str = "opencode-family-event";
const NATIVE_SESSION_NAMESPACE: &str = "opencode-family.session-id";
const SOURCE_BACKED_MAX_FILE_TOUCHES: usize = 32;
const SOURCE_BACKED_MAX_NATIVE_IDENTITY_BYTES: usize = 4 * 1024;
const LOGICAL_SCAN_PROGRESS_ROW_CADENCE: u64 = 4_096;
const SQLITE_SOURCE_INVALID_REASON: &str =
    "OpenCode-family history database must be a regular file";

#[derive(Debug, Error)]
pub(crate) enum OpenCodeSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Route(#[from] SourceBackedRouteError),
    #[error("OpenCode-family source-backed counter overflow")]
    CountOverflow,
    #[error("OpenCode-family source-backed event references an unprojected session {0:?}")]
    MissingSession(String),
    #[error("OpenCode-family retained row is not backed by an exact text value")]
    MissingExactText,
}

pub(crate) type OpenCodeSourceBackedResult<T> = Result<T, OpenCodeSourceBackedError>;

mod adapter;
mod fingerprint;
mod invocation;
mod ordering;
mod projection;
mod value;

pub(crate) use adapter::register as register_source_backed_route;
use fingerprint::*;
use invocation::*;
use ordering::{
    initialize_ordering_scratch, stream_fallback_ordered_events, stream_ordered_session_identities,
    OPENCODE_FALLBACK_SCRATCH_MAX_BYTES,
};
use projection::{core_record, decode_source_event_row, retained_projection};
use value::SqliteSourceValue;

/// Provider-local hook consumed later by the shared registration layer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenCodeSourceBackedRegistration {
    dialect: &'static OpenCodeSqliteDialect,
}

impl OpenCodeSourceBackedRegistration {
    pub(crate) const fn new(dialect: &'static OpenCodeSqliteDialect) -> Self {
        Self { dialect }
    }

    pub(crate) const fn provider(self) -> CaptureProvider {
        self.dialect.provider
    }

    fn owns_source(self, source: &SourceKey) -> bool {
        schema_family_for_source(self.dialect, source).is_some()
    }
}

pub(crate) const fn opencode_source_backed_registration() -> OpenCodeSourceBackedRegistration {
    OpenCodeSourceBackedRegistration::new(&super::super::OPENCODE_SQLITE_DIALECT)
}

pub(crate) const fn kilo_source_backed_registration() -> OpenCodeSourceBackedRegistration {
    OpenCodeSourceBackedRegistration::new(&super::super::KILO_SQLITE_DIALECT)
}

pub(crate) const fn mimocode_source_backed_registration() -> OpenCodeSourceBackedRegistration {
    OpenCodeSourceBackedRegistration::new(&super::super::MIMOCODE_SQLITE_DIALECT)
}

pub(crate) const fn opencode_family_source_backed_registrations(
) -> [OpenCodeSourceBackedRegistration; 3] {
    [
        opencode_source_backed_registration(),
        kilo_source_backed_registration(),
        mimocode_source_backed_registration(),
    ]
}

#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedScan {
    pub(crate) source: SourceKey,
    pub(crate) certificate: CertifiedSource,
    bounds: OpenCodeScanBounds,
}

#[derive(Debug)]
struct SourceSession {
    native_identity: String,
    parent_native_identity: Option<String>,
    root_native_identity: String,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    directory: Option<String>,
    branch: Option<String>,
    agent_identity: Option<String>,
}

#[derive(Debug)]
struct RawSession {
    parent_native_identity: Option<String>,
    directory: Option<String>,
    branch: Option<String>,
    agent_identity: Option<String>,
}

struct SessionScanState {
    content_hasher: Sha256,
    session_rows_scanned: u64,
    max_buffered_session_metadata: u64,
    max_session_ancestry_depth: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OpenCodeScanBounds {
    session_rows_scanned: u64,
    session_metadata_loads: u64,
    max_buffered_session_metadata: u64,
    max_session_ancestry_depth: u64,
    fallback_payload_hydrations: u64,
    max_buffered_payload_rows: u64,
    fallback_disk_sort: bool,
    fallback_sort_rows: u64,
    fallback_scratch_bytes: u64,
    ordering_data_statements: u64,
    ordering_sort_key_batches: u64,
    ordering_hydration_batches: u64,
    max_sort_key_batch_rows: u64,
    max_buffered_payload_bytes: u64,
}

#[derive(Debug)]
struct WorkingScan {
    source: SourceKey,
    logical_snapshot: SqliteLogicalSnapshot,
    bounds: OpenCodeScanBounds,
}

#[derive(Clone, Debug)]
struct OpenCodeLogicalObservation {
    source: SourceKey,
    schema: OpenCodeNativeSchema,
}

#[derive(Debug)]
struct OpenCodeAuthorizedSnapshot {
    source_root: ProviderSourceRoot,
    sqlite_authority: SqliteSourceDirectoryAuthority,
    sqlite_snapshot: SqliteSourceReadSnapshot,
}

// Documents intentionally move through this short-lived scanner enum by value.
// Boxing them would add an allocation to every indexed event on the hot path.
#[allow(clippy::large_enum_variant)]
enum OpenCodeScanOutput {
    Begin(SourceKey),
    CompletedBytes(u64),
    Document(CoreRecord),
    Progress(SourceBackedCurrentSourceProgress),
}

#[derive(Debug)]
struct SourceEventRow {
    native_identity: String,
    message_identity: String,
    session_identity: String,
    native_order: super::model::OpenCodeNativeOrder,
    time_created: i64,
    time_updated: i64,
    content_bytes: u64,
    projection: OpenCodeJsonProjection,
    source_data: SqliteSourceValue,
    parent_source_data: SqliteSourceValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionDisposition {
    Retained,
    Rejected,
    Ignored,
}

fn scan_pinned_source(
    path: &Path,
    dialect: &'static OpenCodeSqliteDialect,
    observation: &OpenCodeLogicalObservation,
    sqlite_snapshot: SqliteSourceReadSnapshot,
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
    scan_pinned_source_with_scratch_limit(
        path,
        dialect,
        observation,
        sqlite_snapshot,
        OPENCODE_FALLBACK_SCRATCH_MAX_BYTES,
        emit,
    )
}

fn scan_pinned_source_with_scratch_limit(
    path: &Path,
    dialect: &'static OpenCodeSqliteDialect,
    observation: &OpenCodeLogicalObservation,
    sqlite_snapshot: SqliteSourceReadSnapshot,
    scratch_limit: u64,
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
    let working = (|| {
        let connection = sqlite_snapshot.connection()?;
        let streamed = sqlite_snapshot
            .with_private_scratch_database(
                "opencode-order-",
                scratch_limit,
                |scratch, scratch_path| {
                    initialize_ordering_scratch(scratch)?;
                    let session_scan = scan_session_evidence(
                        connection,
                        scratch,
                        &observation.schema,
                        &observation.source,
                    )?;
                    emit(OpenCodeScanOutput::Begin(observation.source.clone()))?;
                    stream_logical_rows(
                        connection,
                        &observation.schema,
                        dialect,
                        path,
                        &observation.source,
                        session_scan,
                        scratch,
                        scratch_path,
                        emit,
                    )
                },
            )
            .map_err(|error| {
                diagnose_provider_query_error(error, SqliteFailurePhase::Projection)
            })?;
        let schema_evidence = relevant_schema_evidence(&observation.schema);
        let logical_snapshot = SqliteLogicalSnapshot::new(
            PARSER_REVISION,
            &schema_evidence,
            streamed.content_digest,
            streamed.counts,
        );
        Ok(WorkingScan {
            source: observation.source.clone(),
            logical_snapshot,
            bounds: streamed.bounds,
        })
    })();
    let working = match working {
        Ok(working) => working,
        Err(error) => return Err(adapter::abort_opencode_snapshot(sqlite_snapshot, error)),
    };
    sqlite_snapshot.finish()?;
    let certificate = working.logical_snapshot.certify(working.source.clone())?;
    Ok(OpenCodeSourceBackedScan {
        source: working.source,
        certificate,
        bounds: working.bounds,
    })
}

#[cfg(test)]
fn observe_logical_source(
    connection: &Connection,
    dialect: &'static OpenCodeSqliteDialect,
) -> OpenCodeSourceBackedResult<OpenCodeLogicalObservation> {
    observe_logical_source_with_progress(connection, dialect, &mut |_| Ok(()))
}

fn observe_logical_source_with_progress(
    connection: &Connection,
    dialect: &'static OpenCodeSqliteDialect,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeLogicalObservation> {
    report_progress(opencode_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    let schema = OpenCodeNativeSchema::probe(connection, dialect)
        .map_err(OpenCodeSourceBackedError::from)
        .map_err(|error| diagnose_provider_query_error(error, SqliteFailurePhase::Schema))?;
    let source = source_key(dialect, schema.family)?;
    report_progress(opencode_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalFingerprint,
        0,
        0,
    ))?;
    Ok(OpenCodeLogicalObservation { source, schema })
}

fn diagnose_provider_query_error(
    error: OpenCodeSourceBackedError,
    phase: SqliteFailurePhase,
) -> OpenCodeSourceBackedError {
    match error {
        OpenCodeSourceBackedError::Sqlite(source)
        | OpenCodeSourceBackedError::Capture(CaptureError::Sqlite(source)) => {
            SqliteSourceAccessError::Sqlite {
                operation: match phase {
                    SqliteFailurePhase::Schema => "probing the OpenCode SQLite schema",
                    _ => "projecting the OpenCode SQLite snapshot",
                },
                source,
            }
            .with_diagnostic(
                phase,
                SqliteArtifactKind::PrivateBackup,
                0,
                0,
                SqliteCleanupStatus::NotRequired,
            )
            .into()
        }
        error => error,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamedLogicalRows {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    bounds: OpenCodeScanBounds,
}

// These inputs keep one ordered decode pass explicit; a one-use context bundle
// would not add reuse.
#[allow(clippy::too_many_arguments)]
fn stream_logical_rows(
    connection: &Connection,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source: &SourceKey,
    session_scan: SessionScanState,
    scratch: &Connection,
    scratch_path: &Path,
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<StreamedLogicalRows> {
    let session_by_id_sql = format!("{} where id = ?1", session_source_sql(schema));
    let mut session_by_id = connection.prepare(&session_by_id_sql)?;
    let mut parent_by_id = connection.prepare(&session_parent_sql(schema))?;
    let SessionScanState {
        mut content_hasher,
        session_rows_scanned,
        max_buffered_session_metadata,
        mut max_session_ancestry_depth,
    } = session_scan;
    let mut counts = ScannedSourceCounts::default();
    let mut current_session = None::<SourceSession>;
    let mut next_session_sequence = 0_u64;
    let mut previous_explicit_order = None::<(String, i64)>;
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let mut session_metadata_loads = 0_u64;
    emit(OpenCodeScanOutput::Progress(opencode_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalScan,
        0,
        0,
    )))?;
    let fallback_stats = {
        let mut consume_event = |event: SourceEventRow| -> OpenCodeSourceBackedResult<()> {
            if let super::model::OpenCodeNativeOrder::ExplicitSequence {
                session_id,
                sequence,
                ..
            } = &event.native_order
            {
                if previous_explicit_order.as_ref().is_some_and(
                    |(previous_session, previous_sequence)| {
                        previous_session == session_id && previous_sequence == sequence
                    },
                ) {
                    return Err(CaptureError::InvalidPayload(
                        "OpenCode NativePath explicit session_message sequence is not unique"
                            .to_owned(),
                    )
                    .into());
                }
                previous_explicit_order = Some((session_id.clone(), *sequence));
            }
            hash_source_event(&mut content_hasher, &event);
            counts.complete_records = checked_add(counts.complete_records, 1)?;
            counts.certified_bytes = checked_add(counts.certified_bytes, event.content_bytes)?;
            if counts.complete_records % LOGICAL_SCAN_PROGRESS_ROW_CADENCE == 0 {
                emit(OpenCodeScanOutput::Progress(opencode_logical_progress(
                    SourceBackedCurrentSourceProgressStage::LogicalScan,
                    counts.complete_records,
                    counts.certified_bytes,
                )))?;
            }
            emit(OpenCodeScanOutput::CompletedBytes(event.content_bytes))?;
            let disposition = projection_disposition(&event.projection);
            let retained = retained_projection(&event.projection);
            match disposition {
                ProjectionDisposition::Retained => {
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                }
                ProjectionDisposition::Rejected => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                }
                ProjectionDisposition::Ignored => {
                    counts.ignored_records = checked_add(counts.ignored_records, 1)?;
                }
            }
            let Some(retained) = retained else {
                return Ok(());
            };
            if current_session
                .as_ref()
                .map(|session| session.native_identity.as_str())
                != Some(event.session_identity.as_str())
            {
                if current_session.as_ref().is_some_and(|previous| {
                    previous.native_identity.as_bytes() >= event.session_identity.as_bytes()
                }) {
                    return Err(OpenCodeSourceBackedError::Capture(
                        CaptureError::SystemInvariant(
                            "OpenCode source-backed rows are not ordered by session identity",
                        ),
                    ));
                }
                let (session, ancestry_depth) = load_source_session(
                    &mut session_by_id,
                    &mut parent_by_id,
                    source,
                    &event.session_identity,
                )?;
                session_metadata_loads = checked_add(session_metadata_loads, 1)?;
                max_session_ancestry_depth = max_session_ancestry_depth.max(ancestry_depth);
                current_session = Some(session);
                next_session_sequence = 0;
            }
            let session = current_session.as_ref().ok_or_else(|| {
                OpenCodeSourceBackedError::MissingSession(event.session_identity.clone())
            })?;
            let document = core_record(
                source,
                schema.family,
                path,
                session,
                event,
                retained,
                &mut next_session_sequence,
                &mut repository_attributor,
            )?;
            emit(OpenCodeScanOutput::Document(document))
        };
        stream_fallback_ordered_events(
            connection,
            scratch,
            scratch_path,
            schema,
            dialect,
            &mut consume_event,
        )?
    };
    emit(OpenCodeScanOutput::Progress(opencode_logical_progress(
        SourceBackedCurrentSourceProgressStage::LogicalScan,
        counts.complete_records,
        counts.certified_bytes,
    )))?;
    let fallback_payload_hydrations = fallback_stats.rows;
    Ok(StreamedLogicalRows {
        counts,
        content_digest: content_hasher.finalize().into(),
        bounds: OpenCodeScanBounds {
            session_rows_scanned,
            session_metadata_loads,
            max_buffered_session_metadata: max_buffered_session_metadata
                .max(u64::from(current_session.is_some())),
            max_session_ancestry_depth,
            fallback_payload_hydrations,
            max_buffered_payload_rows: fallback_stats.max_hydration_batch_rows,
            fallback_disk_sort: true,
            fallback_sort_rows: fallback_stats.rows,
            fallback_scratch_bytes: fallback_stats.scratch_bytes,
            ordering_data_statements: fallback_stats.data_statements,
            ordering_sort_key_batches: fallback_stats.sort_key_batches,
            ordering_hydration_batches: fallback_stats.hydration_batches,
            max_sort_key_batch_rows: fallback_stats.max_sort_key_batch_rows,
            max_buffered_payload_bytes: fallback_stats.max_hydration_batch_bytes,
        },
    })
}

fn scan_session_evidence(
    connection: &Connection,
    scratch: &Connection,
    schema: &OpenCodeNativeSchema,
    source: &SourceKey,
) -> OpenCodeSourceBackedResult<SessionScanState> {
    let mut content_hasher = Sha256::new();
    content_hasher.update(b"ctx-opencode-family-logical-content-v3\0");
    let session_by_id_sql = format!("{} where id = ?1", session_source_sql(schema));
    let mut session_by_id = connection.prepare(&session_by_id_sql)?;
    let mut parent_by_id = connection.prepare(&session_parent_sql(schema))?;
    let mut session_rows_scanned = 0_u64;
    let mut max_session_ancestry_depth = 0_u64;
    stream_ordered_session_identities(connection, scratch, &mut |identity| {
        let mut rows = session_by_id.query([identity])?;
        let Some(row) = rows.next()? else {
            return Err(OpenCodeSourceBackedError::MissingSession(
                identity.to_owned(),
            ));
        };
        let (actual_identity, raw) = decode_session_row(row)?;
        if actual_identity != identity {
            return Err(OpenCodeSourceBackedError::MissingSession(
                identity.to_owned(),
            ));
        }
        drop(rows);
        let (session, ancestry_depth) =
            source_session(&mut parent_by_id, source, actual_identity, raw)?;
        hash_session(&mut content_hasher, &session);
        session_rows_scanned = checked_add(session_rows_scanned, 1)?;
        max_session_ancestry_depth = max_session_ancestry_depth.max(ancestry_depth);
        Ok(())
    })?;
    Ok(SessionScanState {
        content_hasher,
        session_rows_scanned,
        max_buffered_session_metadata: u64::from(session_rows_scanned != 0),
        max_session_ancestry_depth,
    })
}

fn session_source_sql(schema: &OpenCodeNativeSchema) -> String {
    let parent = optional_session_text(&schema.session_columns, "parent_id");
    let directory = optional_session_text(&schema.session_columns, "directory");
    let branch = optional_session_text(&schema.session_columns, "branch");
    let agent = optional_session_text(&schema.session_columns, "agent");
    let parent_invalid = if schema.session_columns.contains("parent_id") {
        format!(
            "(parent_id is not null and (
                 typeof(parent_id) <> 'text'
                 or octet_length(parent_id) > {SOURCE_BACKED_MAX_NATIVE_IDENTITY_BYTES}
             ))"
        )
    } else {
        "0".to_owned()
    };
    format!(
        "select cast(id as text), {parent}, {directory}, {branch}, {agent},
                case when typeof(id) <> 'text' or trim(id) = ''
                           or octet_length(id) > {SOURCE_BACKED_MAX_NATIVE_IDENTITY_BYTES}
                           or typeof(time_created) <> 'integer'
                           or typeof(time_updated) <> 'integer'
                           or {parent_invalid}
                     then 1 else 0 end
         from session"
    )
}

fn session_parent_sql(schema: &OpenCodeNativeSchema) -> String {
    let parent = optional_session_text(&schema.session_columns, "parent_id");
    format!("select cast(id as text), {parent} from session where id = ?1")
}

fn decode_session_row(row: &Row<'_>) -> OpenCodeSourceBackedResult<(String, RawSession)> {
    if row.get::<_, i64>(5)? != 0 {
        return Err(OpenCodeSourceBackedError::Capture(
            CaptureError::InvalidPayload(
                "OpenCode NativePath session identity/order rows are unsafe".to_owned(),
            ),
        ));
    }
    Ok((
        row.get(0)?,
        RawSession {
            parent_native_identity: nonempty(row.get(1)?),
            directory: nonempty(row.get(2)?),
            branch: nonempty(row.get(3)?),
            agent_identity: nonempty(row.get(4)?),
        },
    ))
}

fn load_source_session(
    session_by_id: &mut Statement<'_>,
    parent_by_id: &mut Statement<'_>,
    source: &SourceKey,
    requested_identity: &str,
) -> OpenCodeSourceBackedResult<(SourceSession, u64)> {
    let mut rows = session_by_id.query([requested_identity])?;
    let Some(row) = rows.next()? else {
        return Err(OpenCodeSourceBackedError::MissingSession(
            requested_identity.to_owned(),
        ));
    };
    let (identity, raw) = decode_session_row(row)?;
    if identity != requested_identity {
        return Err(OpenCodeSourceBackedError::MissingSession(
            requested_identity.to_owned(),
        ));
    }
    source_session(parent_by_id, source, identity, raw)
}

fn source_session(
    parent_by_id: &mut Statement<'_>,
    source: &SourceKey,
    identity: String,
    raw: RawSession,
) -> OpenCodeSourceBackedResult<(SourceSession, u64)> {
    let (root_native_identity, ancestry_depth) = root_session_identity(
        parent_by_id,
        &identity,
        raw.parent_native_identity.as_deref(),
    )?;
    let derived_session_id = session_id(source, &identity)?;
    let parent_session_id = raw
        .parent_native_identity
        .as_deref()
        .map(|parent| session_id(source, parent))
        .transpose()?;
    let root_session_id = session_id(source, &root_native_identity)?;
    Ok((
        SourceSession {
            native_identity: identity,
            parent_native_identity: raw.parent_native_identity,
            root_native_identity,
            session_id: derived_session_id,
            parent_session_id,
            root_session_id,
            directory: raw.directory,
            branch: raw.branch,
            agent_identity: raw.agent_identity,
        },
        ancestry_depth,
    ))
}

fn root_session_identity(
    parent_by_id: &mut Statement<'_>,
    identity: &str,
    initial_parent: Option<&str>,
) -> OpenCodeSourceBackedResult<(String, u64)> {
    let mut root = identity.to_owned();
    let mut visited = HashSet::from([identity.to_owned()]);
    let mut parent = initial_parent.map(str::to_owned);
    let mut ancestry_depth = 0_u64;
    for _ in 0..64 {
        let Some(candidate) = parent.take() else {
            break;
        };
        let Some((actual_identity, parent_identity)) =
            parent_session_row(parent_by_id, &candidate)?
        else {
            break;
        };
        if actual_identity != candidate || !visited.insert(candidate.clone()) {
            break;
        }
        root = candidate;
        parent = parent_identity;
        ancestry_depth = checked_add(ancestry_depth, 1)?;
    }
    Ok((root, ancestry_depth))
}

fn parent_session_row(
    statement: &mut Statement<'_>,
    identity: &str,
) -> OpenCodeSourceBackedResult<Option<(String, Option<String>)>> {
    let mut rows = statement.query([identity])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some((row.get(0)?, nonempty(row.get(1)?))))
}

fn session_id(
    source: &SourceKey,
    native_identity: &str,
) -> OpenCodeSourceBackedResult<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(NATIVE_SESSION_NAMESPACE, TypedKey::utf8(native_identity)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn source_key(
    dialect: &OpenCodeSqliteDialect,
    family: OpenCodeNativeSchemaFamily,
) -> OpenCodeSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        format!("{}.sqlite-authority", dialect.provider.as_str()),
        TypedKey::utf8(SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive(
        dialect.provider.as_str(),
        dialect.source_format,
        format!("opencode-family-{}-v1", family.label()),
        SOURCE_IDENTITY_VERSION,
        anchor,
    )?)
}

fn schema_family_for_source(
    dialect: &OpenCodeSqliteDialect,
    source: &SourceKey,
) -> Option<OpenCodeNativeSchemaFamily> {
    [
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
        OpenCodeNativeSchemaFamily::MessagePart,
    ]
    .into_iter()
    .find(|family| {
        source_key(dialect, *family).is_ok_and(|candidate| candidate.exact_descriptor_eq(source))
    })
}

#[cfg(test)]
fn open_root_authorized_snapshot_retained(
    data_root: &Path,
    path: &Path,
) -> OpenCodeSourceBackedResult<OpenCodeAuthorizedSnapshot> {
    open_root_authorized_snapshot_retained_with_hook_and_progress(
        data_root,
        path,
        || {},
        &mut |_| Ok(()),
    )
}

fn open_root_authorized_snapshot_retained_with_progress(
    data_root: &Path,
    path: &Path,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeAuthorizedSnapshot> {
    open_root_authorized_snapshot_retained_with_hook_and_progress(
        data_root,
        path,
        || {},
        report_progress,
    )
}

#[cfg(test)]
fn open_root_authorized_snapshot_retained_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> OpenCodeSourceBackedResult<OpenCodeAuthorizedSnapshot> {
    open_root_authorized_snapshot_retained_with_hook_and_progress(
        data_root,
        path,
        after_authorize,
        &mut |_| Ok(()),
    )
}

fn open_root_authorized_snapshot_retained_with_hook_and_progress(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeAuthorizedSnapshot> {
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
    let source_root = ProviderSourceRoot::open(parent)?;
    let source_directory = source_root.directory()?;
    let parent_handle = source_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot = sqlite_authority
        .open_logical_online_backup_snapshot_with_progress(database_leaf, report_progress)
        .map_err(|error| match error {
            SqliteSourceProgressError::Source(error) => OpenCodeSourceBackedError::from(error),
            SqliteSourceProgressError::Progress(error) => OpenCodeSourceBackedError::from(error),
        })?;
    after_authorize();
    let configure = (|| {
        sqlite_snapshot.revalidate()?;
        source_root.revalidate_same_object()?;
        let connection = sqlite_snapshot.connection()?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|source| {
                sqlite_snapshot.diagnose_provider_query_error(
                    "setting the private OpenCode SQLite busy timeout",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        // Every corpus-sized ordering/index is externalized below. Disable
        // SQLite's query-time automatic indexes so an accepted no-index schema
        // cannot silently rebuild source-cardinality state in temp_store=MEMORY.
        connection
            .pragma_update(None, "automatic_index", "OFF")
            .map_err(|source| {
                sqlite_snapshot.diagnose_provider_query_error(
                    "disabling private OpenCode SQLite automatic indexes",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        let automatic_index: i64 = connection
            .pragma_query_value(None, "automatic_index", |row| row.get(0))
            .map_err(|source| {
                sqlite_snapshot.diagnose_provider_query_error(
                    "verifying private OpenCode SQLite automatic-index state",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        if automatic_index != 0 {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "OpenCode source automatic-index suppression was not enforced".to_owned(),
            }
            .into());
        }
        Ok(())
    })();
    if let Err(error) = configure {
        return Err(adapter::abort_opencode_snapshot(sqlite_snapshot, error));
    }
    Ok(OpenCodeAuthorizedSnapshot {
        source_root,
        sqlite_authority,
        sqlite_snapshot,
    })
}

fn opencode_logical_progress(
    stage: SourceBackedCurrentSourceProgressStage,
    rows_scanned: u64,
    certified_bytes: u64,
) -> SourceBackedCurrentSourceProgress {
    let mut progress = SourceBackedCurrentSourceProgress::new(stage);
    progress.logical_rows_scanned = Some(rows_scanned);
    progress.logical_certified_bytes = Some(certified_bytes);
    progress
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

fn optional_session_text(columns: &BTreeSet<String>, column: &str) -> String {
    if columns.contains(column) {
        format!("case when typeof({column}) = 'text' then cast({column} as text) else '' end")
    } else {
        "''".to_owned()
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn checked_add(left: u64, right: u64) -> OpenCodeSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(OpenCodeSourceBackedError::CountOverflow)
}

#[cfg(test)]
mod tests;
