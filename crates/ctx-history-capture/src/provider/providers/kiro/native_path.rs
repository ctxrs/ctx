use std::{
    collections::BTreeSet,
    convert::Infallible,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, Fidelity, FileTouched, ProviderSourceTrust, Session,
    SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    NativePathSourceRetirementPage, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use rusqlite::{Connection, OptionalExtension};
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
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        },
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity, provider_source_root,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        sqlite::{
            ensure_sqlite_table_columns, sqlite_schema_fingerprint, sqlite_table_columns,
            sqlite_table_exists, SqliteLengthPreflightGuard,
        },
    },
    provider_sources::{
        open_sqlite_source_snapshot, SqliteSourceAccessError, SqliteSourceEvidence,
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, KIRO_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::event::{KiroFileTouch, KiroNativeEvent};
use super::history::{
    kiro_history_entry_events, kiro_provider_session_id, kiro_row_complete_values,
    kiro_session_ended_at, kiro_session_started_at, KiroConversationRow,
};

const KIRO_NATIVE_CURSOR_VERSION: u32 = 2;
const KIRO_NATIVE_PARSER_REVISION: &str = "kiro-sqlite-nativepath-v1";
const KIRO_NATIVE_PUBLICATION_REVISION: &str = "kiro-nativepath-store-v1";
const KIRO_OUTPUT_PARSER_REVISION: &str = "kiro-nativepath-output-v1";
const KIRO_LOCATOR_KIND: &str = "kiro-conversation-row-v1";
const KIRO_LEGACY_POSITION_KIND: &str = "kiro-conversation-keyset-v1";
const KIRO_PAGE_HISTORY_ITEMS: usize = 30;
const KIRO_PAGE_OVERHEAD_UNITS: usize = 1;
const KIRO_PAGE_MAX_UNITS: usize = NATIVE_INGESTION_PAGE_MAX_UNITS;
const KIRO_PAGE_MAX_BYTES: usize = ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES;
const KIRO_PAGE_BASE_BYTES: usize = 2 * 1024;
const KIRO_MAX_REJECTION_DETAILS: usize = 64;
const KIRO_RETIREMENT_PAGE_ENTITIES: usize = 512;
const KIRO_PREFIX_DOMAIN: &[u8] = b"ctx-kiro-nativepath-prefix-v1\0";
const KIRO_PUBLICATION_DOMAIN: &[u8] = b"ctx-kiro-nativepath-publication-v1\0";
const KIRO_RETIREMENT_DOMAIN: &[u8] = b"ctx-kiro-nativepath-retirement-v1\0";

#[path = "native_path_lifecycle.rs"]
mod lifecycle;
#[path = "native_path_model.rs"]
mod model;
#[path = "native_path_output.rs"]
mod output;
#[path = "native_path_publication.rs"]
mod publication;
#[path = "native_path_scan.rs"]
mod scan;
#[path = "source_backed.rs"]
mod source_backed;

use lifecycle::*;
use model::*;
use output::*;
use publication::*;
use scan::*;
#[allow(unused_imports)]
pub(crate) use source_backed::{
    scan_kiro_source_backed_v0, KiroHydratedRecordV0, KiroLocatorResolverV0,
    KiroSourceBackedErrorV0, KiroSourceBackedResultV0, KiroSourceBackedScanV0,
};

pub(super) fn import_kiro_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_path = absolute_kiro_path(path)?;
    let configured_source_root = context
        .source_path
        .clone()
        .map(|path| absolute_kiro_path(&path))
        .transpose()?
        .unwrap_or_else(|| source_path.clone());
    context.source_path = Some(configured_source_root.clone());

    if !source_path.try_exists()? {
        if let Some(sink) = options.import_profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                "kiro_nativepath_source_missing",
                "Kiro output replay source is unavailable",
            ));
        }
        if options.import_profile.is_replay_only() {
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing_kiro_route(&source_path, store, &context);
    }

    let source = KiroSource::acquire(
        &source_path,
        configured_source_root,
        options.inventory_observation_token.as_deref(),
    )?;
    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(store, &source, &context, &options.import_profile);
        return Ok(ProviderImportSummary::default());
    }

    let committed_store = Store::open_read_only(store.path())?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?;
    let start = core_start(stored.as_ref(), &source)?;
    if start.already_terminal {
        let summary = start.summary();
        replay_outputs_or_mark_behind(store, &source, &context, &options.import_profile);
        return Ok(summary);
    }

    let started_in_retirement = start.retirement.is_some();
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut summary = start.summary();
    let operation = (|| {
        if !started_in_retirement {
            let mut scanner = KiroScanner::new(&source, start.frontier, context.imported_at)?;
            while let Some(page) = source.database.read(&source.canonical_path, |connection| {
                scanner.next_page(connection)
            })? {
                let page_summary = publish_core_page(
                    store,
                    &committed_store,
                    &bulk_guard,
                    &source,
                    &context,
                    &options,
                    page,
                )?;
                let changed = page_summary.work_result() == ProviderImportWorkResult::Changed;
                summary.merge_from(page_summary);
                if changed && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                    break;
                }
            }
        }
        if options.capture_work_limit == CaptureWorkLimit::Drain || started_in_retirement {
            summary.work_remaining = false;
            summary.merge_from(retire_pending_generation(
                store,
                &bulk_guard,
                &source,
                &context,
                options.capture_work_limit,
            )?);
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if !summary.work_remaining {
        replay_outputs_or_mark_behind(store, &source, &context, &options.import_profile);
    }
    Ok(summary)
}

struct KiroSource {
    canonical_path: PathBuf,
    configured_source_root: PathBuf,
    database: KiroSqliteDatabase,
    tables: KiroTables,
    locator_identity: String,
    cursor_stream: String,
    source_revision: String,
    user_version: i64,
    schema_fingerprint: String,
}

impl KiroSource {
    fn acquire(
        path: &Path,
        configured_source_root: PathBuf,
        inventory_token: Option<&str>,
    ) -> Result<Self> {
        let canonical_path = absolute_kiro_path(path)?;
        let (database, (user_version, schema_fingerprint, tables)) =
            KiroSqliteDatabase::open(&canonical_path, |connection| {
                let user_version: i64 =
                    connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
                if user_version < 0 {
                    return Err(CaptureError::InvalidPayload(
                        "Kiro SQLite user_version must be nonnegative".to_owned(),
                    ));
                }
                Ok((
                    user_version,
                    sqlite_schema_fingerprint(connection)?,
                    KiroTables::probe(connection)?,
                ))
            })?;
        let locator_identity = provider_path_identity(&canonical_path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::KiroCli,
            KIRO_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let mut revision = format!(
            "kiro-nativepath-source-v2;parser={KIRO_NATIVE_PARSER_REVISION};user_version={user_version};schema={schema_fingerprint};identity={};length={};revision={}",
            hex(database.evidence().identity()),
            database.evidence().length(),
            hex(database.evidence().revision()),
        );
        if let Some(token) = inventory_token {
            let mut digest = Sha256::new();
            digest.update(b"ctx-kiro-inventory-observation-v1\0");
            hash_field(&mut digest, revision.as_bytes());
            hash_field(&mut digest, token.as_bytes());
            revision = format!("kiro-inventory-sha256-v1:{}", hex(&digest.finalize()));
        }
        Ok(Self {
            canonical_path,
            configured_source_root,
            database,
            tables,
            locator_identity,
            cursor_stream,
            source_revision: revision,
            user_version,
            schema_fingerprint,
        })
    }

    fn revalidate(&self) -> Result<()> {
        self.database.revalidate()
    }
}

#[derive(Debug)]
struct KiroSqliteDatabase {
    opened: OpenedProviderSourceFile,
    evidence: SqliteSourceEvidence,
}

impl KiroSqliteDatabase {
    fn open<T>(path: &Path, query: impl FnOnce(&Connection) -> Result<T>) -> Result<(Self, T)> {
        let opened = open_provider_source_file(path)?;
        let snapshot = open_sqlite_source_snapshot(path, opened.file())
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        let evidence = snapshot.evidence().clone();
        let result = snapshot
            .connection()
            .map_err(|error| kiro_sqlite_source_error(path, error))
            .and_then(query);
        let finished = snapshot
            .finish()
            .map_err(|error| kiro_sqlite_source_error(path, error))?;
        opened.revalidate()?;
        if finished != evidence {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok((Self { opened, evidence }, result?))
    }

    fn read<T, E>(
        &self,
        path: &Path,
        query: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<CaptureError>,
    {
        self.revalidate().map_err(E::from)?;
        let snapshot = open_sqlite_source_snapshot(path, self.opened.file())
            .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))?;
        let result = if snapshot.evidence() == &self.evidence {
            snapshot
                .connection()
                .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))
                .and_then(query)
        } else {
            Err(E::from(CaptureError::SourceChangedDuringCapture))
        };
        let finished = snapshot
            .finish()
            .map_err(|error| E::from(kiro_sqlite_source_error(path, error)))?;
        self.revalidate().map_err(E::from)?;
        if finished != self.evidence {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
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

fn kiro_sqlite_source_error(path: &Path, error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => {
            CaptureError::SourceChangedDuringCapture
        }
        error => CaptureError::ProviderSource {
            provider: CaptureProvider::KiroCli.as_str(),
            path: path.to_path_buf(),
            kind: crate::ProviderSourceFailureKind::SourceDatabase,
            detail: error.to_string(),
        },
    }
}

fn absolute_kiro_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

#[derive(Debug, Clone, Copy)]
struct KiroTables {
    v2: bool,
    legacy: bool,
}

impl KiroTables {
    fn probe(connection: &Connection) -> Result<Self> {
        let v2 = sqlite_table_exists(connection, "conversations_v2")?;
        if v2 {
            ensure_kiro_table_columns(
                &sqlite_table_columns(connection, "conversations_v2")?,
                "Kiro conversations_v2",
                &[
                    "key",
                    "conversation_id",
                    "value",
                    "created_at",
                    "updated_at",
                ],
            )?;
        }
        let legacy = sqlite_table_exists(connection, "conversations")?;
        if legacy {
            ensure_kiro_table_columns(
                &sqlite_table_columns(connection, "conversations")?,
                "Kiro conversations",
                &["key", "value"],
            )?;
        }
        if !v2 && !legacy {
            return Err(CaptureError::UnsupportedSchema(
                "Kiro SQLite source has neither conversations_v2 nor conversations".to_owned(),
            ));
        }
        Ok(Self { v2, legacy })
    }

    fn initial_phase(self) -> KiroPhase {
        if self.v2 {
            KiroPhase::V2
        } else {
            KiroPhase::Legacy
        }
    }
}

fn ensure_kiro_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    ensure_sqlite_table_columns(columns, label, required).map_err(|cause| match cause {
        CaptureError::InvalidPayload(reason) => CaptureError::UnsupportedSchema(reason),
        cause => cause,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KiroPhase {
    V2,
    Legacy,
}

impl KiroPhase {
    fn table(self) -> &'static str {
        match self {
            Self::V2 => "conversations_v2",
            Self::Legacy => "conversations",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::V2 => 1,
            Self::Legacy => 2,
        }
    }
}

#[cfg(test)]
#[path = "native_path_tests.rs"]
mod tests;
