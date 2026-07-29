//! Provider-local source-backed Hermes adapter.
//!
//! This module deliberately stops at discovery, bounded native projection,
//! source certification, and exact native-row hydration. Publication,
//! replacement/deletion lifecycle, and projection fanout remain shared
//! responsibilities.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::ProviderSourceRoot,
    native_source::NativeSqliteValue,
    provider::{
        native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
        normalization::provider_required_timestamp_seconds,
        sqlite::sqlite_schema_fingerprint,
    },
    provider_sources::{
        discover_provider_sources_for_provider_with_context,
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        DiscoveryContext, DiscoveryIssue, ProviderSource, ProviderSourceStatus,
        SqliteSourceAccessError, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, HERMES_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    hermes_complete_message_with_normalized_hash, hermes_layout_record_digest, hermes_native_event,
    hermes_record_digest,
    layout::{HermesMessageRow, HermesSchema, HermesSessionRow},
    load_hermes_message_values,
    sqlite::{HermesNativeRecord, HermesNativeRow, HermesPhase, HermesRowReader},
    HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

const HERMES_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile";
const HERMES_SESSION_NAMESPACE: &str = "hermes.session";
const HERMES_MESSAGE_NAMESPACE: &str = "hermes.message";
const HERMES_LOGICAL_SESSION_KIND: &str = "hermes-session";
const HERMES_LOGICAL_EVENT_KIND: &str = "hermes-message";
const HERMES_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-db-v1";
const HERMES_SOURCE_REVISION_KIND: &str = "hermes-sqlite-snapshot-v1";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Hermes SQLite source must have an authorized parent and database leaf";
const HERMES_SOURCE_PARSER_REVISION: &str = "hermes-source-backed-v1";
const HERMES_SESSION_RELATION: &str = "sessions";
const HERMES_MESSAGE_RELATION: &str = "messages";
const HERMES_SESSION_METADATA_MAX_CHARS: usize = 8 * 1024;
const HERMES_PARENT_CHAIN_MAX_DEPTH: usize = 256;
const HERMES_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-snapshot-v1\0";
const HERMES_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-session-v1\0";
const HERMES_REJECTION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-rejection-v1\0";

#[derive(Debug, Error)]
pub(crate) enum HermesSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Hermes source-backed source has an invalid profile path: {0:?}")]
    InvalidProfilePath(PathBuf),
    #[error("Hermes source-backed source changed while its snapshot was scanned")]
    SourceChanged,
    #[error("Hermes source-backed source counters overflowed")]
    CountOverflow,
    #[error("Hermes source-backed logical-row digest is malformed")]
    InvalidLogicalDigest,
    #[error("Hermes source-backed locator is not a supported message row")]
    InvalidLocator,
    #[error("Hermes source-backed locator references a stale source snapshot")]
    StaleSourceEvidence,
    #[error("Hermes source-backed locator references a stale logical row")]
    StaleRecordEvidence,
    #[error("Hermes source-backed locator row is missing")]
    MissingRecord,
}

pub(crate) type HermesSourceBackedResult<T> = Result<T, HermesSourceBackedError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HermesSourceSelection {
    DefaultProfile,
    NamedProfile(String),
    Explicit,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceCandidate {
    path: PathBuf,
    source: SourceKey,
    selection: HermesSourceSelection,
    status: ProviderSourceStatus,
}

impl HermesSourceCandidate {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn selection(&self) -> &HermesSourceSelection {
        &self.selection
    }

    pub(crate) fn status(&self) -> ProviderSourceStatus {
        self.status
    }

    pub(crate) fn automatic(source: ProviderSource) -> HermesSourceBackedResult<Self> {
        let selection = automatic_selection(&source.path)?;
        let profile = match &selection {
            HermesSourceSelection::DefaultProfile => "default",
            HermesSourceSelection::NamedProfile(profile) => profile.as_str(),
            HermesSourceSelection::Explicit => {
                return Err(HermesSourceBackedError::InvalidProfilePath(source.path));
            }
        };
        let anchor = SourceAnchor::provider_native(
            HERMES_SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(profile)?,
        )?;
        Ok(Self {
            path: source.path,
            source: hermes_source_key(anchor)?,
            selection,
            status: source.status,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceInventory {
    pub(crate) sources: Vec<HermesSourceCandidate>,
    pub(crate) issues: Vec<DiscoveryIssue>,
}

/// Inventories only the selected ordinary profile or the bounded Gateway
/// multiplex set admitted by the existing Hermes discovery resolver.
pub(crate) fn discover_hermes_source_backed(
    context: &DiscoveryContext,
) -> HermesSourceBackedResult<HermesSourceInventory> {
    let report =
        discover_provider_sources_for_provider_with_context(context, CaptureProvider::Hermes);
    let mut sources = Vec::with_capacity(report.sources.len());
    for source in report.sources {
        if source.source_format == HERMES_SQLITE_SOURCE_FORMAT {
            sources.push(HermesSourceCandidate::automatic(source)?);
        }
    }
    Ok(HermesSourceInventory {
        sources,
        issues: report.issues,
    })
}

/// Admits an explicitly selected Hermes database with caller-owned persistent
/// lineage. This is the only provider-local entry point for inactive profiles.
pub(crate) fn hermes_source_backed_explicit(
    path: impl Into<PathBuf>,
    anchor: SourceAnchor,
) -> HermesSourceBackedResult<HermesSourceCandidate> {
    let path = path.into();
    let status = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            ProviderSourceStatus::Available
        }
        Ok(_) => ProviderSourceStatus::Unknown,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProviderSourceStatus::Missing,
        Err(_) => ProviderSourceStatus::Unknown,
    };
    Ok(HermesSourceCandidate {
        path,
        source: hermes_source_key(anchor)?,
        selection: HermesSourceSelection::Explicit,
        status,
    })
}

fn automatic_selection(path: &Path) -> HermesSourceBackedResult<HermesSourceSelection> {
    let Some(parent) = path.parent() else {
        return Err(HermesSourceBackedError::InvalidProfilePath(
            path.to_path_buf(),
        ));
    };
    if parent.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("profiles")) {
        let profile = parent
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(path.to_path_buf()))?;
        Ok(HermesSourceSelection::NamedProfile(profile.to_owned()))
    } else {
        Ok(HermesSourceSelection::DefaultProfile)
    }
}

fn hermes_source_key(anchor: SourceAnchor) -> HermesSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::Hermes.as_str(),
        HERMES_SQLITE_SOURCE_FORMAT,
        HERMES_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedSession {
    pub(crate) session_id: StableEntityId,
    pub(crate) parent_session_id: Option<StableEntityId>,
    pub(crate) root_session_id: StableEntityId,
    pub(crate) provider_session_id: String,
    pub(crate) provider_parent_session_id: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) source_path: String,
    pub(crate) agent_type: String,
    pub(crate) is_primary: bool,
    pub(crate) started_at_unix_ms: i64,
    pub(crate) ended_at_unix_ms: Option<i64>,
    pub(crate) workspace: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) locator: SourceRecordLocator,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedRejection {
    pub(crate) phase: HermesPhase,
    pub(crate) rowid: i64,
    pub(crate) ordinal: u64,
    pub(crate) reason: String,
}

#[derive(Debug, Clone)]
pub(crate) enum HermesSourceBackedRecord {
    Session(HermesSourceBackedSession),
    Event(LexicalDocument),
    Rejected(HermesSourceBackedRejection),
}

#[derive(Debug, Clone)]
pub(crate) struct HermesSourceBackedPage {
    pub(crate) records: Vec<HermesSourceBackedRecord>,
    pub(crate) owned_bytes: usize,
}

/// Streams bounded provider-local pages and returns authority only after the
/// opening SQLite snapshot has been revalidated unchanged.
pub(crate) fn scan_hermes_source_backed(
    candidate: &HermesSourceCandidate,
    mut emit: impl FnMut(HermesSourceBackedPage) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<CertifiedSource> {
    candidate.source.validate_contract()?;
    let source_path = candidate
        .path
        .to_str()
        .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(candidate.path.clone()))?
        .to_owned();
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&candidate.path)?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let conn = sqlite_snapshot.connection()?;
    let sqlite_user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let schema = HermesSchema::detect(&conn)?;
    let revision =
        hermes_source_revision(&opening_evidence, sqlite_user_version, &schema_fingerprint);
    let revision_digest: [u8; 32] = Sha256::digest(&revision).into();
    let opening = SourceObservation::new(
        candidate.source.clone(),
        HERMES_SOURCE_REVISION_KIND,
        revision,
    )?;

    let mut reader = HermesRowReader::new(conn, &schema)?;
    let mut pending_pages = Vec::new();
    let operation: HermesSourceBackedResult<(ScannedSourceCounts, [u8; 32])> = (|| {
        let mut frontier = super::sqlite::HermesFrontier::initial();
        let mut digest = Sha256::new();
        digest.update(HERMES_SOURCE_DIGEST_DOMAIN);
        let mut counts = ScannedSourceCounts::default();
        let mut page_records = Vec::new();
        let mut page_owned_bytes = 0_usize;
        let mut session_cache: Option<(String, HermesSessionContext)> = None;

        while let Some(native) = reader.next(frontier)? {
            frontier = native.next_frontier;
            counts.complete_records = checked_add(counts.complete_records, 1)?;
            let observed_bytes = u64::try_from(native.observed_bytes)
                .map_err(|_| HermesSourceBackedError::CountOverflow)?;
            counts.certified_bytes = checked_add(counts.certified_bytes, observed_bytes)?;

            let logical_digest = native_record_digest(&native)?;
            digest.update([match native.locator.phase {
                HermesPhase::Sessions => 1,
                HermesPhase::Messages => 2,
            }]);
            digest.update(native.locator.rowid.to_be_bytes());
            digest.update(native.ordinal.to_be_bytes());
            digest.update(observed_bytes.to_be_bytes());
            digest.update(logical_digest);

            let phase = native.locator.phase;
            let rowid = native.locator.rowid;
            let ordinal = native.ordinal;
            let record = project_native_row(
                &conn,
                &schema,
                &candidate.source,
                &source_path,
                revision_digest,
                native,
                logical_digest,
                &mut session_cache,
            )?;
            let (record, owned_bytes) = bound_projected_record(record, phase, rowid, ordinal)?;

            match &record {
                HermesSourceBackedRecord::Session(_) => {
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                }
                HermesSourceBackedRecord::Event(_) => {
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                }
                HermesSourceBackedRecord::Rejected(_) => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                }
            }

            if !page_records.is_empty()
                && (page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                    || page_owned_bytes.saturating_add(owned_bytes)
                        > NATIVE_INGESTION_PAGE_MAX_BYTES)
            {
                pending_pages.push(HermesSourceBackedPage {
                    records: std::mem::take(&mut page_records),
                    owned_bytes: page_owned_bytes,
                });
                page_owned_bytes = 0;
            }
            page_owned_bytes = page_owned_bytes.saturating_add(owned_bytes);
            page_records.push(record);
            if page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS {
                pending_pages.push(HermesSourceBackedPage {
                    records: std::mem::take(&mut page_records),
                    owned_bytes: page_owned_bytes,
                });
                page_owned_bytes = 0;
            }
        }
        if !page_records.is_empty() {
            pending_pages.push(HermesSourceBackedPage {
                records: page_records,
                owned_bytes: page_owned_bytes,
            });
        }
        Ok((counts, digest.finalize().into()))
    })();
    drop(reader);

    let finish = sqlite_snapshot.finish();
    let (counts, content_digest) = operation?;
    let closing_evidence = finish?;
    if closing_evidence != opening_evidence {
        return Err(HermesSourceBackedError::SourceChanged);
    }
    source_root.revalidate()?;
    let closing = opening.clone();
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        HERMES_SOURCE_PARSER_REVISION,
        content_digest,
        counts,
    )?;
    for page in pending_pages {
        emit(page)?;
    }
    Ok(certificate)
}

fn checked_add(left: u64, right: u64) -> HermesSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(HermesSourceBackedError::CountOverflow)
}

fn open_root_authorized_snapshot(
    path: &Path,
) -> HermesSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    path: &Path,
    after_authorize: impl FnOnce(),
) -> HermesSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
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
    let sqlite_authority = retain_sqlite_source_directory_authority(&parent_handle, parent)?;
    let sqlite_snapshot =
        open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_leaf)?;
    after_authorize();
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| HermesSourceBackedError::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((source_root, sqlite_snapshot))
}

fn hermes_source_revision(
    evidence: &SqliteSourceEvidence,
    sqlite_user_version: i64,
    schema_fingerprint: &str,
) -> Vec<u8> {
    format!(
        "hermes-source-backed-snapshot-v1:capture={HERMES_CAPTURE_REVISION};\
         policy={HERMES_POLICY_REVISION};user_version={sqlite_user_version};\
         schema={schema_fingerprint};identity={};length={};revision={}",
        hex_digest(evidence.identity()),
        evidence.length(),
        hex_digest(evidence.revision()),
    )
    .into_bytes()
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[derive(Debug, Clone)]
struct HermesSessionContext {
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    branch: Option<String>,
    agent_type: String,
    is_primary: bool,
    workspace: Option<String>,
    cwd: Option<String>,
}

fn project_native_row(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    source: &SourceKey,
    source_path: &str,
    revision_digest: [u8; 32],
    native: HermesNativeRow,
    logical_digest: [u8; 32],
    session_cache: &mut Option<(String, HermesSessionContext)>,
) -> HermesSourceBackedResult<HermesSourceBackedRecord> {
    let phase = native.locator.phase;
    let rowid = native.locator.rowid;
    let ordinal = native.ordinal;
    match native.record {
        HermesNativeRecord::Session(row) => {
            let context = match load_session_context(conn, schema, source, &row.id) {
                Ok(Some(context)) => context,
                Ok(None) => {
                    return Ok(rejected(
                        phase,
                        rowid,
                        ordinal,
                        format!("Hermes session {} disappeared during projection", row.id),
                    ));
                }
                Err(CaptureError::InvalidPayload(reason)) => {
                    return Ok(rejected(phase, rowid, ordinal, reason));
                }
                Err(error) => return Err(error.into()),
            };
            match project_session(
                source,
                source_path,
                revision_digest,
                rowid,
                row,
                context,
                logical_digest,
            ) {
                Ok(session) => Ok(HermesSourceBackedRecord::Session(session)),
                Err(error) => Ok(rejected(phase, rowid, ordinal, error.to_string())),
            }
        }
        HermesNativeRecord::Message {
            row,
            values: _,
            prepared,
        } => {
            let context = if session_cache
                .as_ref()
                .is_some_and(|(provider_session_id, _)| provider_session_id == &row.session_id)
            {
                session_cache.as_ref().map(|(_, context)| context.clone())
            } else {
                match load_session_context(conn, schema, source, &row.session_id) {
                    Ok(context) => {
                        if let Some(context) = &context {
                            *session_cache = Some((row.session_id.clone(), context.clone()));
                        }
                        context
                    }
                    Err(CaptureError::InvalidPayload(reason)) => {
                        return Ok(rejected(phase, rowid, ordinal, reason));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let Some(context) = context else {
                return Ok(rejected(
                    phase,
                    rowid,
                    ordinal,
                    format!(
                        "Hermes message {} depends on missing session {}",
                        row.id, row.session_id
                    ),
                ));
            };
            match project_message(
                source,
                source_path,
                revision_digest,
                rowid,
                ordinal,
                row,
                prepared,
                logical_digest,
                context,
            ) {
                Ok(document) => Ok(HermesSourceBackedRecord::Event(document)),
                Err(error) => Ok(rejected(phase, rowid, ordinal, error.to_string())),
            }
        }
        HermesNativeRecord::Rejected(reason) => Ok(rejected(phase, rowid, ordinal, reason)),
    }
}

fn rejected(
    phase: HermesPhase,
    rowid: i64,
    ordinal: u64,
    reason: String,
) -> HermesSourceBackedRecord {
    HermesSourceBackedRecord::Rejected(HermesSourceBackedRejection {
        phase,
        rowid,
        ordinal,
        reason,
    })
}

fn project_session(
    source: &SourceKey,
    source_path: &str,
    revision_digest: [u8; 32],
    rowid: i64,
    row: HermesSessionRow,
    context: HermesSessionContext,
    record_digest: [u8; 32],
) -> HermesSourceBackedResult<HermesSourceBackedSession> {
    let started_at =
        provider_required_timestamp_seconds(row.started_at, "Hermes session started_at")?;
    let ended_at = row
        .ended_at
        .map(|value| provider_required_timestamp_seconds(value, "Hermes session ended_at"))
        .transpose()?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: HERMES_SESSION_RELATION.to_owned(),
            primary_key: TypedKey::utf8(&row.id)?,
            row_version: Some(TypedKey::I64(rowid)),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        record_digest,
    )?;
    Ok(HermesSourceBackedSession {
        session_id: context.session_id,
        parent_session_id: context.parent_session_id,
        root_session_id: context.root_session_id,
        provider_session_id: row.id,
        provider_parent_session_id: row.parent_session_id,
        branch: context.branch,
        source_path: source_path.to_owned(),
        agent_type: context.agent_type,
        is_primary: context.is_primary,
        started_at_unix_ms: started_at.timestamp_millis(),
        ended_at_unix_ms: ended_at.map(|value| value.timestamp_millis()),
        workspace: context.workspace,
        cwd: context.cwd,
        locator,
    })
}

fn project_message(
    source: &SourceKey,
    source_path: &str,
    revision_digest: [u8; 32],
    rowid: i64,
    ordinal: u64,
    row: HermesMessageRow,
    prepared: Option<super::HermesPreparedCoreMessage>,
    record_digest: [u8; 32],
    session: HermesSessionContext,
) -> HermesSourceBackedResult<LexicalDocument> {
    let native = match prepared {
        Some(prepared) => prepared.native,
        None => hermes_native_event(&row, ordinal)?,
    };
    let native_item_key = NativeItemKey::composite(
        HERMES_MESSAGE_NAMESPACE,
        vec![TypedKey::utf8(&row.session_id)?, TypedKey::I64(row.id)],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: HERMES_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: HERMES_MESSAGE_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::utf8(&row.session_id)?,
                TypedKey::I64(row.id),
            ])?,
            row_version: Some(TypedKey::I64(rowid)),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        record_digest,
    )?;
    let body = native
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("Hermes event");
    Ok(LexicalDocument {
        event_id,
        session_id: session.session_id,
        parent_session_id: session.parent_session_id,
        root_session_id: session.root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(row.session_id),
        branch: session.branch,
        source_path: Some(source_path.to_owned()),
        agent_type: session.agent_type,
        is_primary: session.is_primary,
        event_sequence: native.provider_event_index,
        occurred_at_unix_ms: Some(native.occurred_at.timestamp_millis()),
        event_type: native.event_type.as_str().to_owned(),
        role: native.role.map(|role| role.as_str().to_owned()),
        body: bounded_text(body, MAX_BODY_PREVIEW_CHARS),
        workspace: session.workspace,
        cwd: session.cwd,
        touched_files: Vec::new(),
    })
}

fn hermes_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> HermesSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        HERMES_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: HERMES_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn load_session_context(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    source: &SourceKey,
    provider_session_id: &str,
) -> Result<Option<HermesSessionContext>, CaptureError> {
    let Some(row) = load_session_row(conn, schema, provider_session_id)? else {
        return Ok(None);
    };
    provider_required_timestamp_seconds(row.started_at, "Hermes session started_at")?;
    row.ended_at
        .map(|value| provider_required_timestamp_seconds(value, "Hermes session ended_at"))
        .transpose()?;
    let session_id = hermes_session_id(source, &row.id)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let parent_session_id = row
        .parent_session_id
        .as_deref()
        .map(|parent| hermes_session_id(source, parent))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let root_provider_session_id =
        root_provider_session_id(conn, schema, &row.id, row.parent_session_id.as_deref())?;
    let root_session_id = hermes_session_id(source, &root_provider_session_id)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let is_primary = row.parent_session_id.is_none();
    Ok(Some(HermesSessionContext {
        session_id,
        parent_session_id,
        root_session_id,
        branch: bounded_optional(row.git_branch.as_deref(), HERMES_SESSION_METADATA_MAX_CHARS),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary,
        workspace: bounded_optional(
            row.git_repo_root.as_deref(),
            HERMES_SESSION_METADATA_MAX_CHARS,
        ),
        cwd: bounded_optional(row.cwd.as_deref(), HERMES_SESSION_METADATA_MAX_CHARS),
    }))
}

fn load_session_row(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    provider_session_id: &str,
) -> Result<Option<HermesSessionRow>, CaptureError> {
    let sql = format!(
        "select {} from sessions s \
         where typeof(s.id) = 'text' and s.id collate binary = ?1 collate binary limit 2",
        schema.sessions().projection()
    );
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query([provider_session_id])?;
    let Some(first) = rows.next()? else {
        return Ok(None);
    };
    let values = schema.sessions().capture_values(first, 0)?;
    let row = super::layout::decode_hermes_session(schema, &values, 0)?;
    if rows.next()?.is_some() {
        return Err(CaptureError::InvalidPayload(format!(
            "Hermes session {} is duplicated",
            provider_session_id
        )));
    }
    Ok(Some(row))
}

fn root_provider_session_id(
    conn: &rusqlite::Connection,
    schema: &HermesSchema,
    provider_session_id: &str,
    direct_parent: Option<&str>,
) -> Result<String, CaptureError> {
    let mut root = provider_session_id.to_owned();
    let mut parent = direct_parent.map(str::to_owned);
    let mut visited = BTreeSet::new();
    visited.insert(root.clone());
    for _ in 0..HERMES_PARENT_CHAIN_MAX_DEPTH {
        let Some(parent_id) = parent.take() else {
            return Ok(root);
        };
        if !visited.insert(parent_id.clone()) {
            return Err(CaptureError::InvalidPayload(format!(
                "Hermes session {} has a cyclic parent chain",
                provider_session_id
            )));
        }
        root.clone_from(&parent_id);
        parent = match load_session_row(conn, schema, &parent_id)? {
            Some(row) => row.parent_session_id,
            None => return Ok(root),
        };
        if parent.is_none() {
            return Ok(root);
        }
    }
    Err(CaptureError::InvalidPayload(format!(
        "Hermes session {} exceeds the {}-level parent bound",
        provider_session_id, HERMES_PARENT_CHAIN_MAX_DEPTH
    )))
}

fn bound_projected_record(
    record: HermesSourceBackedRecord,
    phase: HermesPhase,
    rowid: i64,
    ordinal: u64,
) -> HermesSourceBackedResult<(HermesSourceBackedRecord, usize)> {
    let owned_bytes = projected_owned_bytes(&record)?;
    if owned_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Ok((record, owned_bytes));
    }
    let record = rejected(
        phase,
        rowid,
        ordinal,
        format!(
            "Hermes projected row requires {owned_bytes} bytes and exceeds the {}-byte page limit",
            NATIVE_INGESTION_PAGE_MAX_BYTES
        ),
    );
    let owned_bytes = projected_owned_bytes(&record)?;
    Ok((record, owned_bytes))
}

fn projected_owned_bytes(record: &HermesSourceBackedRecord) -> Result<usize, serde_json::Error> {
    let fixed = 1024_usize;
    match record {
        HermesSourceBackedRecord::Session(session) => Ok(fixed
            .saturating_add(session.provider_session_id.len())
            .saturating_add(
                session
                    .provider_parent_session_id
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )
            .saturating_add(session.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.source_path.len())
            .saturating_add(session.agent_type.len())
            .saturating_add(session.workspace.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.cwd.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(serde_json::to_vec(&session.locator)?.len())),
        HermesSourceBackedRecord::Event(event) => Ok(fixed
            .saturating_add(event.body.len())
            .saturating_add(
                event
                    .provider_session_id
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )
            .saturating_add(event.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(event.source_path.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(event.agent_type.len())
            .saturating_add(event.workspace.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(event.cwd.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(serde_json::to_vec(&event.locator)?.len())),
        HermesSourceBackedRecord::Rejected(rejection) => {
            Ok(fixed.saturating_add(rejection.reason.len()))
        }
    }
}

fn native_record_digest(native: &HermesNativeRow) -> HermesSourceBackedResult<[u8; 32]> {
    match &native.record {
        HermesNativeRecord::Session(row) => Ok(session_record_digest(row)),
        HermesNativeRecord::Message {
            values, prepared, ..
        } => {
            if !values.is_empty() {
                decode_sha256(hermes_layout_record_digest(values).as_str())
            } else if let Some(prepared) = prepared {
                decode_sha256(prepared.record_digest.as_str())
            } else {
                Err(HermesSourceBackedError::InvalidLogicalDigest)
            }
        }
        HermesNativeRecord::Rejected(reason) => {
            let mut digest = Sha256::new();
            digest.update(HERMES_REJECTION_DIGEST_DOMAIN);
            digest.update(reason.as_bytes());
            Ok(digest.finalize().into())
        }
    }
}

fn session_record_digest(row: &HermesSessionRow) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_SESSION_DIGEST_DOMAIN);
    hash_text(&mut digest, &row.id);
    hash_text(&mut digest, &row.source);
    hash_optional_text(&mut digest, row.parent_session_id.as_deref());
    hash_optional_text(&mut digest, row.model.as_deref());
    hash_optional_text(&mut digest, row.model_config.as_deref());
    digest.update(row.started_at.to_bits().to_be_bytes());
    hash_optional_f64(&mut digest, row.ended_at);
    hash_optional_text(&mut digest, row.end_reason.as_deref());
    digest.update(row.message_count.to_be_bytes());
    digest.update(row.tool_call_count.to_be_bytes());
    digest.update(row.input_tokens.to_be_bytes());
    digest.update(row.output_tokens.to_be_bytes());
    digest.update(row.cache_read_tokens.to_be_bytes());
    digest.update(row.cache_write_tokens.to_be_bytes());
    digest.update(row.reasoning_tokens.to_be_bytes());
    hash_optional_text(&mut digest, row.cwd.as_deref());
    hash_optional_text(&mut digest, row.git_branch.as_deref());
    hash_optional_text(&mut digest, row.git_repo_root.as_deref());
    hash_optional_text(&mut digest, row.billing_provider.as_deref());
    hash_optional_text(&mut digest, row.billing_base_url.as_deref());
    hash_optional_text(&mut digest, row.billing_mode.as_deref());
    hash_optional_f64(&mut digest, row.estimated_cost_usd);
    hash_optional_f64(&mut digest, row.actual_cost_usd);
    hash_optional_text(&mut digest, row.title.as_deref());
    digest.update(row.archived.to_be_bytes());
    digest.finalize().into()
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_f64(digest: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn decode_sha256(value: &str) -> HermesSourceBackedResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(HermesSourceBackedError::InvalidLogicalDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn decode_hex_nibble(value: u8) -> HermesSourceBackedResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(HermesSourceBackedError::InvalidLogicalDigest),
    }
}

fn bounded_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn bounded_optional(value: Option<&str>, limit: usize) -> Option<String> {
    value.map(|value| bounded_text(value, limit))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HermesHydratedMessage {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) text: String,
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_hash: String,
    pub(crate) normalized_payload_hash: String,
}

/// Reopens one exact Hermes message through the existing visibility-aware
/// SQLite parser and verifies both snapshot and logical-row evidence.
pub(crate) fn hydrate_hermes_source_backed_message(
    path: &Path,
    locator: &SourceRecordLocator,
) -> HermesSourceBackedResult<HermesHydratedMessage> {
    locator.validate_contract()?;
    if locator.source().provider() != CaptureProvider::Hermes.as_str()
        || locator.source().source_format() != HERMES_SQLITE_SOURCE_FORMAT
        || locator.source().schema_variant() != HERMES_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
    {
        return Err(HermesSourceBackedError::InvalidLocator);
    }
    let (provider_session_id, message_id, rowid) = decode_message_coordinate(locator)?;
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(path)?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let conn = sqlite_snapshot.connection()?;
    let operation = (|| {
        let sqlite_user_version = conn
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(CaptureError::from)?;
        let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
        HermesSchema::detect(&conn)?;
        let revision =
            hermes_source_revision(&opening_evidence, sqlite_user_version, &schema_fingerprint);
        let revision_digest: [u8; 32] = Sha256::digest(&revision).into();
        if locator.certified_source_revision_digest() != Some(&revision_digest) {
            return Err(HermesSourceBackedError::StaleSourceEvidence);
        }
        let values = match load_hermes_message_values(&conn, rowid) {
            Ok(values) => values,
            Err(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => {
                return Err(HermesSourceBackedError::MissingRecord);
            }
            Err(error) => return Err(error.into()),
        };
        let (actual_session_id, provider_event_hash, normalized_payload_hash, text) =
            hermes_complete_message_with_normalized_hash(&conn, &values)?;
        if actual_session_id != provider_session_id
            || provider_event_hash != format!("message:{message_id}")
        {
            return Err(HermesSourceBackedError::StaleRecordEvidence);
        }
        let record_digest = decode_sha256(hermes_record_digest(&values).as_str())?;
        if locator.record_digest() != &record_digest {
            return Err(HermesSourceBackedError::StaleRecordEvidence);
        }
        Ok(HermesHydratedMessage {
            provider_bytes: encode_native_values(&values),
            text,
            provider_session_id: actual_session_id,
            provider_event_hash,
            normalized_payload_hash,
        })
    })();
    let finish = sqlite_snapshot.finish();
    let hydrated = operation?;
    let closing_evidence = finish?;
    if closing_evidence != opening_evidence {
        return Err(HermesSourceBackedError::StaleSourceEvidence);
    }
    source_root.revalidate()?;
    Ok(hydrated)
}

fn decode_message_coordinate(
    locator: &SourceRecordLocator,
) -> HermesSourceBackedResult<(String, i64, i64)> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let [TypedKey::Utf8(provider_session_id), TypedKey::I64(message_id)] = parts.as_slice() else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    let Some(TypedKey::I64(rowid)) = row_version else {
        return Err(HermesSourceBackedError::InvalidLocator);
    };
    if logical_relation != HERMES_MESSAGE_RELATION {
        return Err(HermesSourceBackedError::InvalidLocator);
    }
    Ok((provider_session_id.clone(), *message_id, *rowid))
}

fn encode_native_values(values: &[NativeSqliteValue]) -> Vec<u8> {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&(values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => encoded.push(0),
            NativeSqliteValue::Integer(value) => {
                encoded.push(1);
                encoded.extend_from_slice(&value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                encoded.push(2);
                encoded.extend_from_slice(&value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                encoded.push(3);
                encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
                encoded.extend_from_slice(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                encoded.push(4);
                encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
                encoded.extend_from_slice(value);
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests;
