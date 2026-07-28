use std::{
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventType, Fidelity, ProviderSourceTrust, Session,
    SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    native_source::{NativeLocator, NativeSqliteValue},
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{provider_capped_json, provider_json_text, provider_timestamp_millis},
        sqlite::{
            ensure_sqlite_table_columns, sqlite_schema_fingerprint, sqlite_table_columns,
            sqlite_table_exists, SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_sqlite_source_snapshot, SqliteSourceAccessError, SqliteSourceEvidence,
    },
    CaptureError, CaptureWorkLimit, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    FIREBENDER_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use crate::summaries::MAX_RETAINED_PROVIDER_FAILURES;

use super::{
    firebender_chat_history_db_path, firebender_message_text, firebender_message_time,
    firebender_native_event, firebender_output_evidence, firebender_result_content,
    FirebenderNativeEvent, FIREBENDER_LOCATOR_KIND,
};

mod core;
mod lifecycle;
mod output;
mod scan;
mod source_backed;
#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use source_backed::{
    hydrate_firebender_source_backed_row, prepare_firebender_source_backed,
    FirebenderHydratedSourceRow, FirebenderSourceBackedError, FirebenderSourceBackedPage,
    FirebenderSourceBackedPlan, FirebenderSourceBackedResult, FirebenderSourceBackedScanner,
};

use self::{
    core::import_core,
    lifecycle::{
        firebender_path_identity, firebender_source_revision, publication_id,
        retire_missing_firebender_source, validate_schema,
    },
    output::replay_output,
    scan::{
        build_page, decode_core_cursor_for_migration, next_generation,
        require_complete_matching_core,
    },
};

const FIREBENDER_NATIVE_CURSOR_VERSION: u32 = 1;
const FIREBENDER_NATIVE_FRONTIER_VERSION: u32 = 1;
const FIREBENDER_NATIVE_PARSER_REVISION: u32 = 1;
const FIREBENDER_NATIVE_POLICY_REVISION: u32 = 1;
const FIREBENDER_OUTPUT_PARSER_REVISION: &str = "firebender-native-output-v1";
const FIREBENDER_MAX_MESSAGES_PER_CORE_PAGE: usize = 60;
const FIREBENDER_MAX_OUTPUTS_PER_PAGE: usize = NATIVE_INGESTION_PAGE_MAX_UNITS;
const FIREBENDER_MAX_FAILURE_BYTES: usize = 4 * 1024;
const FIREBENDER_PAGE_OVERHEAD_BYTES: usize = 4 * 1024;
const FIREBENDER_INITIAL_PREFIX_DOMAIN: &[u8] = b"ctx-firebender-native-prefix-v1\0";
const FIREBENDER_PUBLICATION_DOMAIN: &[u8] = b"ctx-firebender-native-publication-v1\0";
const FIREBENDER_RETIREMENT_DOMAIN: &[u8] = b"ctx-firebender-native-retirement-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirebenderFrontier {
    version: u32,
    row_ordinal: u64,
    updated_at: i64,
    rowid: i64,
    next_message_index: u64,
    prefix_sha256: [u8; 32],
    terminal: bool,
}

impl FirebenderFrontier {
    fn initial() -> Self {
        Self {
            version: FIREBENDER_NATIVE_FRONTIER_VERSION,
            row_ordinal: 0,
            updated_at: 0,
            rowid: 0,
            next_message_index: 0,
            prefix_sha256: Sha256::digest(FIREBENDER_INITIAL_PREFIX_DOMAIN).into(),
            terminal: false,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.version != FIREBENDER_NATIVE_FRONTIER_VERSION
            || (self.terminal && self.next_message_index != 0)
        {
            return Err(CaptureError::InvalidPayload(
                "Firebender NativePath cursor frontier is malformed".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirebenderNativeCursor {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    route_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
    generation: u64,
    rejected_records: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    #[serde(default)]
    frontier_accepted_sessions: u64,
    #[serde(default)]
    frontier_accepted_events: u64,
    #[serde(default)]
    failures: Vec<ProviderImportFailure>,
    #[serde(default)]
    scan_terminal: bool,
    frontier: FirebenderFrontier,
}

impl FirebenderNativeCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        cursor.frontier.validate()?;
        if cursor.version != FIREBENDER_NATIVE_CURSOR_VERSION
            || cursor.parser_revision != FIREBENDER_NATIVE_PARSER_REVISION
            || cursor.policy_revision != FIREBENDER_NATIVE_POLICY_REVISION
            || cursor.route_identity.is_empty()
            || cursor.canonical_source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.schema_fingerprint.is_empty()
            || cursor.failures.len() > MAX_RETAINED_PROVIDER_FAILURES
            || cursor
                .failures
                .iter()
                .any(|failure| failure.error.len() > FIREBENDER_MAX_FAILURE_BYTES)
            || cursor.rejected_records < cursor.failures.len() as u64
        {
            return Err(CaptureError::InvalidPayload(
                "Firebender NativePath cursor is unsupported or incomplete".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug)]
struct FirebenderSourceAuthority {
    database: FirebenderSqliteDatabase,
    configured_source_root: PathBuf,
    database_path: PathBuf,
    canonical_database_path: PathBuf,
    route_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
}

#[derive(Debug)]
struct FirebenderSqliteDatabase {
    opened: OpenedProviderSourceFile,
    evidence: SqliteSourceEvidence,
}

impl FirebenderSqliteDatabase {
    fn open<T>(path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<(Self, T)> {
        let opened = open_provider_source_file(path)?;
        let snapshot = open_sqlite_source_snapshot(path, opened.file())
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| firebender_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        opened.revalidate()?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok((Self { opened, evidence }, result?))
    }

    fn read<T>(&self, path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.revalidate()?;
        let snapshot = open_sqlite_source_snapshot(path, self.opened.file())
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| firebender_sqlite_source_error(path, error))
                .and_then(query)
        } else {
            Err(CaptureError::SourceChangedDuringCapture)
        };
        let finished = snapshot
            .finish()
            .map_err(|error| firebender_sqlite_source_error(path, error))?;
        self.revalidate()?;
        if finished != self.evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        result
    }

    fn revalidate(&self) -> Result<()> {
        self.opened.revalidate()
    }

    fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }
}

fn firebender_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::Firebender.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

#[derive(Debug)]
struct FirebenderPathIdentity {
    database_path: PathBuf,
    canonical_database_path: PathBuf,
    route_identity: String,
    cursor_stream: String,
}

#[derive(Debug)]
struct FirebenderRow {
    rowid: i64,
    row_ordinal: u64,
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    messages_json: String,
    metadata_json: String,
    messages: Vec<Value>,
}

impl FirebenderRow {
    fn logical_values(&self) -> Vec<NativeSqliteValue> {
        vec![
            NativeSqliteValue::Text(self.id.clone()),
            NativeSqliteValue::Text(self.name.clone()),
            NativeSqliteValue::Integer(self.created_at),
            NativeSqliteValue::Integer(self.updated_at),
            NativeSqliteValue::Text(self.messages_json.clone()),
            NativeSqliteValue::Text(self.metadata_json.clone()),
        ]
    }
}

#[derive(Debug)]
struct FirebenderPage {
    expected: FirebenderFrontier,
    next: FirebenderFrontier,
    row: Option<FirebenderRow>,
    message_start: usize,
    message_end: usize,
    rejection: Option<String>,
    retained_bytes: usize,
}

#[derive(Debug)]
struct FirebenderRowCandidate {
    rowid: i64,
    updated_at: i64,
    id_bytes: i64,
    name_bytes: i64,
    messages_bytes: i64,
    metadata_bytes: i64,
}

impl FirebenderRowCandidate {
    fn retained_bytes(&self) -> Result<usize> {
        [
            self.id_bytes,
            self.name_bytes,
            self.messages_bytes,
            self.metadata_bytes,
        ]
        .into_iter()
        .try_fold(FIREBENDER_PAGE_OVERHEAD_BYTES, |total, value| {
            let value = usize::try_from(value).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Firebender SQLite text length must be nonnegative".to_owned(),
                )
            })?;
            total
                .checked_add(value)
                .ok_or(CaptureError::SystemInvariant(
                    "Firebender NativePath retained byte count overflowed",
                ))
        })
    }
}

pub(crate) fn import_firebender_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let path_identity = firebender_path_identity(path)?;
    let source_metadata = match fs::symlink_metadata(&path_identity.database_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return retire_missing_firebender_source(
                path,
                &path_identity,
                error,
                store,
                &context,
                &import_options,
            );
        }
        Err(error) => return Err(CaptureError::Io(error)),
    };
    if source_metadata.file_type().is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path_identity.database_path,
            reason: "Firebender SQLite source must be a regular non-symlink file",
        });
    }

    let canonical_database_path = path_identity.canonical_database_path;
    let database_path = canonical_database_path.clone();
    let (database, schema_fingerprint) = FirebenderSqliteDatabase::open(&database_path, |conn| {
        validate_schema(conn, &database_path)?;
        sqlite_schema_fingerprint(conn)
    })?;
    let source_revision = firebender_source_revision(database.evidence(), &schema_fingerprint);
    let configured_source_root = context
        .source_root
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let route_identity = path_identity.route_identity;
    let cursor_stream = path_identity.cursor_stream;
    let raw_source_path = canonical_database_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Firebender NativePath source has no canonical identity",
    ))?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)?;
    let prior = decode_core_cursor_for_migration(stored.as_ref())?;
    let generation = next_generation(prior.as_ref(), &route_identity, &source_revision)?;
    let canonical_source_identity = prior
        .as_ref()
        .map(|cursor| cursor.canonical_source_identity.clone())
        .unwrap_or_else(|| proposed_source_identity.clone());
    let mut authority = FirebenderSourceAuthority {
        database,
        configured_source_root,
        database_path,
        canonical_database_path,
        route_identity,
        cursor_stream,
        proposed_source_identity,
        canonical_source_identity,
        source_revision,
        schema_fingerprint,
    };

    let replay_only = import_options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    if !replay_only {
        summary = import_core(
            store,
            &mut authority,
            &context,
            &import_options,
            prior,
            generation,
        )?;
    } else {
        require_complete_matching_core(store, &authority, &context)?;
    }

    if summary.work_remaining {
        return Ok(summary);
    }
    if let Some(sink) = import_options.import_profile.sink() {
        if replay_output(&authority, sink.as_ref())? {
            summary.record_failure(ProviderImportFailure {
                line: 0,
                error: "Firebender Pro output is behind committed Core".to_owned(),
            });
        }
    }
    Ok(summary)
}
