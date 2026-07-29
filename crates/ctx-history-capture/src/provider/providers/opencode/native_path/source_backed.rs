//! Shared source-backed projection for the OpenCode SQLite dialect family.
//!
//! This module deliberately stops at provider-local discovery, parsing,
//! certification, lexical projection, and exact-row hydration. Publication and
//! lifecycle policy remain owned by the shared coordinator.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{limits::Limit, params, types::ValueRef, Connection, Row};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    json::{
        decode_projection, encode_rejection_reason, register_projection_function,
        OpenCodeJsonProjection, OpenCodeRetainedJson,
    },
    model::{OpenCodeNativeEventKind, OpenCodeNativeFileTouch, OpenCodeNativeSchemaFamily},
    query::{
        source_backed_decode_order, source_backed_event_digest, source_backed_event_sql,
        source_backed_native_record_identity,
    },
    schema::OpenCodeNativeSchema,
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        normalization::provider_required_timestamp_millis,
        providers::opencode::OpenCodeSqliteDialect,
    },
    provider_sources::{
        discover_provider_sources_for_provider_with_context,
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        DiscoveryContext, DiscoveryReport, SqliteLogicalSnapshot, SqliteSourceAccessError,
        SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const SOURCE_ANCHOR_KEY: &str = "active-database";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "opencode-family-source-backed-v2";
const LOGICAL_SESSION_KIND: &str = "opencode-family-session";
const LOGICAL_EVENT_KIND: &str = "opencode-family-event";
const NATIVE_SESSION_NAMESPACE: &str = "opencode-family.session-id";
const SOURCE_BACKED_PAGE_ROWS: usize = 64;
const SOURCE_BACKED_MAX_FILE_TOUCHES: usize = 32;
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
    Resolver(#[from] SourceResolverContractError),
    #[error("OpenCode-family source-backed counter overflow")]
    CountOverflow,
    #[error("OpenCode-family source-backed event references an unprojected session {0:?}")]
    MissingSession(String),
    #[error("OpenCode-family retained row is not backed by an exact text value")]
    MissingExactText,
}

pub(crate) type OpenCodeSourceBackedResult<T> = Result<T, OpenCodeSourceBackedError>;

mod projection;

use projection::{
    decode_source_event_row, lexical_document, retained_projection,
    source_backed_retained_event_kind, source_backed_retained_searchable_text,
};

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

    pub(crate) const fn source_format(self) -> &'static str {
        self.dialect.source_format
    }

    // Retain provider-scoped discovery on the registration boundary even while
    // the shared registry supplies the selected source directly.
    #[allow(dead_code)]
    pub(crate) fn discover(self, context: &DiscoveryContext) -> DiscoveryReport {
        discover_provider_sources_for_provider_with_context(context, self.provider())
    }

    pub(crate) fn scan(
        self,
        path: &Path,
        emit: &mut dyn FnMut(Vec<LexicalDocument>) -> OpenCodeSourceBackedResult<()>,
    ) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
        scan_source(path, self.dialect, emit)
    }

    /// Cheap commit-time fence over acquisition evidence only.
    ///
    /// The evidence is transient runtime state and is never part of the
    /// logical certificate or locator revision.
    #[allow(dead_code, reason = "narrow seam for the pending coordinator hookup")]
    pub(crate) fn terminal_fence(
        self,
        path: &Path,
        expected: &OpenCodeSourceTerminalFence,
    ) -> OpenCodeSourceBackedResult<bool> {
        if expected.provider != self.provider() {
            return Ok(false);
        }
        let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(path)?;
        let current = sqlite_snapshot.finish()?;
        source_root.revalidate()?;
        Ok(current == expected.physical_evidence)
    }

    pub(crate) fn exact_resolver(
        self,
        path: impl Into<PathBuf>,
    ) -> OpenCodeSourceBackedExactResolver {
        OpenCodeSourceBackedExactResolver {
            registration: self,
            path: path.into(),
        }
    }

    // No append frontier is asserted: a matching logical snapshot is unchanged
    // and every other accepted snapshot is a full replacement.
    #[allow(dead_code)]
    pub(crate) const fn mutation_policy(self) -> OpenCodeSourceMutationPolicy {
        OpenCodeSourceMutationPolicy::UnchangedOrReplace
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

// This family aggregate is retained for cross-provider conformance checks.
#[allow(dead_code)]
pub(crate) const fn opencode_family_source_backed_registrations(
) -> [OpenCodeSourceBackedRegistration; 3] {
    [
        opencode_source_backed_registration(),
        kilo_source_backed_registration(),
        mimocode_source_backed_registration(),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum OpenCodeSourceMutationPolicy {
    UnchangedOrReplace,
}

#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedScan {
    pub(crate) source: SourceKey,
    pub(crate) certificate: CertifiedSource,
    #[allow(dead_code, reason = "narrow seam for the pending coordinator hookup")]
    pub(crate) terminal_fence: OpenCodeSourceTerminalFence,
    // Schema and page-count evidence remain attached to release scan receipts.
    #[allow(dead_code)]
    pub(crate) schema_family: &'static str,
    #[allow(dead_code)]
    pub(crate) emitted_pages: u64,
    #[allow(dead_code)]
    pub(crate) row_decode_passes: u64,
    #[allow(dead_code)]
    pub(crate) decoded_rows: u64,
    #[allow(dead_code)]
    pub(crate) peak_buffered_rows: u64,
}

#[derive(Clone, Debug)]
#[allow(dead_code, reason = "narrow seam for the pending coordinator hookup")]
pub(crate) struct OpenCodeSourceTerminalFence {
    provider: CaptureProvider,
    physical_evidence: SqliteSourceEvidence,
}

#[derive(Clone, Debug)]
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

type RawSession = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug)]
struct WorkingScan {
    source: SourceKey,
    logical_snapshot: SqliteLogicalSnapshot,
    schema_family: &'static str,
    emitted_pages: u64,
    row_decode_passes: u64,
    decoded_rows: u64,
    peak_buffered_rows: u64,
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
    projection_bytes: Vec<u8>,
    source_data: SqliteSourceValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProjectionDisposition {
    Retained,
    Rejected,
    Ignored,
}

#[derive(Debug)]
enum SqliteSourceValue {
    Null,
    Integer(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl SqliteSourceValue {
    fn from_ref(value: ValueRef<'_>) -> Self {
        match value {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(value) => Self::Integer(value),
            ValueRef::Real(value) => Self::Real(value.to_bits()),
            ValueRef::Text(value) => Self::Text(value.to_vec()),
            ValueRef::Blob(value) => Self::Blob(value.to_vec()),
        }
    }

    fn hash_into(&self, hasher: &mut Sha256) {
        match self {
            Self::Null => hasher.update([0]),
            Self::Integer(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            Self::Real(bits) => {
                hasher.update([2]);
                hasher.update(bits.to_le_bytes());
            }
            Self::Text(value) => {
                hasher.update([3]);
                hash_bytes(hasher, value);
            }
            Self::Blob(value) => {
                hasher.update([4]);
                hash_bytes(hasher, value);
            }
        }
    }

    fn exact_text(&self) -> Option<&[u8]> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

fn scan_source(
    path: &Path,
    dialect: &'static OpenCodeSqliteDialect,
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(path)?;
    let working = {
        let connection = sqlite_snapshot.connection()?;
        register_projection_function(connection, dialect)?;
        let schema = OpenCodeNativeSchema::probe(connection, dialect)?;
        let source = source_key(dialect, schema.family).map_err(source_backed_as_capture)?;
        let sessions = load_sessions(connection, &schema, &source)?;
        let streamed =
            stream_logical_rows(connection, &schema, dialect, path, &source, &sessions, emit)?;
        let schema_evidence = relevant_schema_evidence(&schema);
        let logical_snapshot = SqliteLogicalSnapshot::new(
            PARSER_REVISION,
            &schema_evidence,
            streamed.content_digest,
            streamed.counts,
        );
        WorkingScan {
            source,
            logical_snapshot,
            schema_family: schema.family.label(),
            emitted_pages: streamed.emitted_pages,
            row_decode_passes: 1,
            decoded_rows: streamed.decoded_rows,
            peak_buffered_rows: streamed.peak_buffered_rows,
        }
    };
    let physical_evidence = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    let certificate = working
        .logical_snapshot
        .certify(working.source.clone())
        .map_err(map_certification_error)?;
    Ok(OpenCodeSourceBackedScan {
        source: working.source,
        certificate,
        terminal_fence: OpenCodeSourceTerminalFence {
            provider: dialect.provider,
            physical_evidence,
        },
        schema_family: working.schema_family,
        emitted_pages: working.emitted_pages,
        row_decode_passes: working.row_decode_passes,
        decoded_rows: working.decoded_rows,
        peak_buffered_rows: working.peak_buffered_rows,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamedLogicalRows {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    emitted_pages: u64,
    decoded_rows: u64,
    peak_buffered_rows: u64,
}

// These seven inputs keep one ordered SQL decode pass explicit; a one-use
// context bundle would not add reuse.
#[allow(clippy::too_many_arguments)]
fn stream_logical_rows(
    connection: &Connection,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source: &SourceKey,
    sessions: &BTreeMap<String, SourceSession>,
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<StreamedLogicalRows> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-logical-content-v2\0");
    hash_sessions(&mut hasher, sessions);
    let mut sql = source_backed_event_sql(schema);
    sql.push_str(" order by 3, 4, 5, 6, 2, 1");
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([max_json_bytes])?;
    let mut counts = ScannedSourceCounts::default();
    let mut emitted_pages = 0_u64;
    let mut decoded_rows = 0_u64;
    let mut peak_buffered_rows = 0_u64;
    let mut page = Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS);
    let mut session_sequences = HashMap::<String, u64>::new();

    while let Some(row) = rows.next()? {
        let event = decode_source_event_row(row, schema, dialect)?;
        decoded_rows = checked_add(decoded_rows, 1)?;
        hash_source_event(&mut hasher, &event);
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, event.content_bytes)?;
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
            continue;
        };
        let session = sessions.get(&event.session_identity).ok_or_else(|| {
            OpenCodeSourceBackedError::MissingSession(event.session_identity.clone())
        })?;
        let document = lexical_document(
            source,
            schema.family,
            path,
            session,
            event,
            retained,
            session_sequences
                .entry(session.native_identity.clone())
                .or_default(),
        )?;
        page.push(document);
        peak_buffered_rows = peak_buffered_rows
            .max(u64::try_from(page.len()).map_err(|_| OpenCodeSourceBackedError::CountOverflow)?);
        if page.len() == SOURCE_BACKED_PAGE_ROWS {
            emit(std::mem::take(&mut page))?;
            page = Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS);
            emitted_pages = checked_add(emitted_pages, 1)?;
        }
    }
    if !page.is_empty() {
        emit(page)?;
        emitted_pages = checked_add(emitted_pages, 1)?;
    }
    Ok(StreamedLogicalRows {
        counts,
        content_digest: hasher.finalize().into(),
        emitted_pages,
        decoded_rows,
        peak_buffered_rows,
    })
}

fn load_sessions(
    connection: &Connection,
    schema: &OpenCodeNativeSchema,
    source: &SourceKey,
) -> OpenCodeSourceBackedResult<BTreeMap<String, SourceSession>> {
    let parent = optional_session_text(&schema.session_columns, "parent_id");
    let directory = optional_session_text(&schema.session_columns, "directory");
    let branch = optional_session_text(&schema.session_columns, "branch");
    let agent = optional_session_text(&schema.session_columns, "agent");
    let sql = format!(
        "select cast(id as text), {parent}, {directory}, {branch}, {agent}
         from session order by cast(id as text)"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            nonempty(row.get::<_, String>(1)?),
            nonempty(row.get::<_, String>(2)?),
            nonempty(row.get::<_, String>(3)?),
            nonempty(row.get::<_, String>(4)?),
        ))
    })?;
    let raw = rows
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .map(|(identity, parent, directory, branch, agent)| {
            (
                identity.clone(),
                (identity, parent, directory, branch, agent),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut sessions = BTreeMap::new();
    for (identity, (_, parent, directory, branch, agent)) in &raw {
        let root_native_identity = root_session_identity(identity, &raw);
        let derived_session_id = session_id(source, identity)?;
        let parent_session_id = parent
            .as_deref()
            .map(|identity| session_id(source, identity))
            .transpose()?;
        let root_session_id = session_id(source, &root_native_identity)?;
        sessions.insert(
            identity.clone(),
            SourceSession {
                native_identity: identity.clone(),
                parent_native_identity: parent.clone(),
                root_native_identity,
                session_id: derived_session_id,
                parent_session_id,
                root_session_id,
                directory: directory.clone(),
                branch: branch.clone(),
                agent_identity: agent.clone(),
            },
        );
    }
    Ok(sessions)
}

fn root_session_identity(identity: &str, sessions: &BTreeMap<String, RawSession>) -> String {
    let mut root = identity.to_owned();
    let mut visited = HashSet::from([identity.to_owned()]);
    for _ in 0..64 {
        let Some(parent) = sessions
            .get(&root)
            .and_then(|(_, parent, _, _, _)| parent.as_deref())
        else {
            break;
        };
        if !sessions.contains_key(parent) || !visited.insert(parent.to_owned()) {
            break;
        }
        root = parent.to_owned();
    }
    root
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

fn open_root_authorized_snapshot(
    path: &Path,
) -> OpenCodeSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    path: &Path,
    after_authorize: impl FnOnce(),
) -> OpenCodeSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
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
    sqlite_snapshot.revalidate()?;
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok((source_root, sqlite_snapshot))
}

fn relevant_schema_evidence(schema: &OpenCodeNativeSchema) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-relevant-schema-v1\0");
    hash_str(&mut hasher, schema.family.label());
    hasher.update(schema.user_version.to_le_bytes());
    hasher.update([u8::from(schema.event_has_type)]);
    for column in ["parent_id", "directory", "branch", "agent"] {
        hash_str(&mut hasher, column);
        hasher.update([u8::from(schema.session_columns.contains(column))]);
    }
    hasher.finalize().to_vec()
}

#[derive(Debug)]
struct HydrationColumnCapability {
    declared_type: String,
    primary_key_ordinal: i64,
}

/// Selects and structurally validates the current schema family without
/// scanning provider rows. Exact hydration validates only the addressed row;
/// the generation scan remains responsible for corpus-wide admission.
fn probe_hydration_schema(
    connection: &Connection,
    family: OpenCodeNativeSchemaFamily,
) -> OpenCodeSourceBackedResult<OpenCodeNativeSchema> {
    let session_columns = hydration_table_capabilities(connection, "session")?;
    validate_hydration_identity_column(&session_columns, "session", "id")?;
    validate_hydration_column(&session_columns, "session", "time_created", "INTEGER")?;
    validate_hydration_column(&session_columns, "session", "time_updated", "INTEGER")?;
    let event_columns = hydration_table_capabilities(connection, family.event_table())?;
    let event_has_type = event_columns.contains_key("type");
    validate_hydration_identity_column(&event_columns, family.event_table(), "id")?;
    validate_hydration_column(&event_columns, family.event_table(), "session_id", "TEXT")?;
    validate_hydration_column(
        &event_columns,
        family.event_table(),
        "time_created",
        "INTEGER",
    )?;
    validate_hydration_column(
        &event_columns,
        family.event_table(),
        "time_updated",
        "INTEGER",
    )?;
    validate_hydration_column(&event_columns, family.event_table(), "data", "TEXT")?;
    if family == OpenCodeNativeSchemaFamily::SessionMessageSeq {
        validate_hydration_column(&event_columns, "session_message", "seq", "INTEGER")?;
    }
    if family == OpenCodeNativeSchemaFamily::MessagePart {
        validate_hydration_column(&event_columns, "part", "message_id", "TEXT")?;
        let message_columns = hydration_table_capabilities(connection, "message")?;
        validate_hydration_identity_column(&message_columns, "message", "id")?;
        validate_hydration_column(&message_columns, "message", "session_id", "TEXT")?;
        validate_hydration_column(&message_columns, "message", "time_created", "INTEGER")?;
        validate_hydration_column(&message_columns, "message", "time_updated", "INTEGER")?;
        validate_hydration_column(&message_columns, "message", "data", "TEXT")?;
    }

    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_version = connection.pragma_query_value(None, "schema_version", |row| row.get(0))?;
    Ok(OpenCodeNativeSchema {
        family,
        capability_digest: String::new(),
        user_version,
        schema_version,
        session_columns: session_columns.keys().cloned().collect(),
        event_has_type,
    })
}

fn locator_schema_family(
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

fn hydration_table_capabilities(
    connection: &Connection,
    table: &str,
) -> OpenCodeSourceBackedResult<BTreeMap<String, HydrationColumnCapability>> {
    let sql = format!("pragma table_info(\"{}\")", table.replace('"', "\"\""));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            HydrationColumnCapability {
                declared_type: row.get::<_, String>(2)?.trim().to_ascii_uppercase(),
                primary_key_ordinal: row.get(5)?,
            },
        ))
    })?;
    rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(OpenCodeSourceBackedError::from)
}

fn validate_hydration_identity_column(
    columns: &BTreeMap<String, HydrationColumnCapability>,
    table: &str,
    column: &str,
) -> OpenCodeSourceBackedResult<()> {
    let capability = validate_hydration_column(columns, table, column, "TEXT")?;
    if capability.primary_key_ordinal != 1 {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} is no longer the primary identity"
        ))
        .into());
    }
    Ok(())
}

fn validate_hydration_column<'a>(
    columns: &'a BTreeMap<String, HydrationColumnCapability>,
    table: &str,
    column: &str,
    declared_type: &str,
) -> OpenCodeSourceBackedResult<&'a HydrationColumnCapability> {
    let capability = columns.get(column).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} is missing required column {column}"
        ))
    })?;
    if capability.declared_type != declared_type {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} changed type from {declared_type}"
        ))
        .into());
    }
    Ok(capability)
}

fn hash_sessions(hasher: &mut Sha256, sessions: &BTreeMap<String, SourceSession>) {
    for session in sessions.values() {
        hasher.update(b"session\0");
        hash_str(hasher, &session.native_identity);
        hash_optional_str(hasher, session.parent_native_identity.as_deref());
        hash_str(hasher, &session.root_native_identity);
        hash_optional_str(hasher, session.directory.as_deref());
        hash_optional_str(hasher, session.branch.as_deref());
        hash_optional_str(hasher, session.agent_identity.as_deref());
    }
}

fn hash_source_event(hasher: &mut Sha256, event: &SourceEventRow) {
    hasher.update(b"event\0");
    hash_str(hasher, &event.native_identity);
    hash_str(hasher, &event.message_identity);
    hash_str(hasher, &event.session_identity);
    hash_native_order(hasher, &event.native_order);
    hasher.update(event.time_created.to_le_bytes());
    hasher.update(event.time_updated.to_le_bytes());
    hasher.update(event.content_bytes.to_le_bytes());
    hasher.update([match projection_disposition(&event.projection) {
        ProjectionDisposition::Retained => 1,
        ProjectionDisposition::Rejected => 2,
        ProjectionDisposition::Ignored => 3,
    }]);
    hash_bytes(hasher, &event.projection_bytes);
    event.source_data.hash_into(hasher);
}

fn source_event_row_digest(event: &SourceEventRow) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-logical-row-v1\0");
    hash_source_event(&mut hasher, event);
    hasher.finalize().into()
}

fn hash_native_order(hasher: &mut Sha256, order: &super::model::OpenCodeNativeOrder) {
    match order {
        super::model::OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        super::model::OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        super::model::OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

fn projection_disposition(projection: &OpenCodeJsonProjection) -> ProjectionDisposition {
    match projection {
        OpenCodeJsonProjection::Retained(_) => ProjectionDisposition::Retained,
        OpenCodeJsonProjection::Output(output) if output.diagnostic.is_some() => {
            ProjectionDisposition::Retained
        }
        OpenCodeJsonProjection::Rejected(_) | OpenCodeJsonProjection::RejectedWithReason(_, _) => {
            ProjectionDisposition::Rejected
        }
        OpenCodeJsonProjection::Output(_) | OpenCodeJsonProjection::ExcludedOutput => {
            ProjectionDisposition::Ignored
        }
    }
}

fn hash_optional_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn hash_str(hasher: &mut Sha256, value: &str) {
    hash_bytes(hasher, value.as_bytes());
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
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

fn source_backed_as_capture(error: OpenCodeSourceBackedError) -> CaptureError {
    match error {
        OpenCodeSourceBackedError::Capture(error) => error,
        OpenCodeSourceBackedError::Sqlite(error) => CaptureError::Sqlite(error),
        OpenCodeSourceBackedError::SqliteSource(error) => {
            CaptureError::InvalidPayload(error.to_string())
        }
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn map_certification_error(error: ProjectionContractError) -> OpenCodeSourceBackedError {
    if error == ProjectionContractError::SourceRevisionChanged {
        OpenCodeSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture)
    } else {
        OpenCodeSourceBackedError::Projection(error)
    }
}

/// Exact-row resolver bound to one already-discovered provider database.
#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedExactResolver {
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
}

impl ContentSourceResolver for OpenCodeSourceBackedExactResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let provider_bytes = self.hydrate_locator(request.locator())?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes,
        })
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let locators = request
            .events()
            .iter()
            .map(EventHydrationRequest::locator)
            .collect::<Vec<_>>();
        let provider_bytes = self.hydrate_locators(&locators)?;
        Ok(request
            .events()
            .iter()
            .zip(provider_bytes)
            .map(|(event, provider_bytes)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes,
            })
            .collect())
    }
}

impl OpenCodeSourceBackedExactResolver {
    fn hydrate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> std::result::Result<Vec<u8>, HydrationFailure> {
        self.hydrate_locators(&[locator])?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "OpenCode-family hydration request contained no locator",
                )
            })
    }

    fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> std::result::Result<Vec<Vec<u8>>, HydrationFailure> {
        if locators.is_empty() {
            return Ok(Vec::new());
        }
        for locator in locators {
            self.validate_locator(locator)?;
        }
        let family = locator_schema_family(self.registration.dialect, locators[0].source())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "locator has an unsupported OpenCode-family schema descriptor",
                )
            })?;
        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&self.path).map_err(temporary_hydration_failure)?;
        let resolved = (|| {
            let connection = sqlite_snapshot
                .connection()
                .map_err(temporary_hydration_failure)?;
            register_projection_function(connection, self.registration.dialect)
                .map_err(temporary_hydration_failure)?;
            let schema = probe_hydration_schema(connection, family).map_err(|error| {
                hydration_failure(
                    HydrationFailureKind::UnsupportedParserRevision,
                    error.to_string(),
                )
            })?;
            let current_source =
                source_key(self.registration.dialect, schema.family).map_err(|error| {
                    hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
                })?;
            if locators
                .iter()
                .any(|locator| !current_source.exact_descriptor_eq(locator.source()))
            {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "provider SQLite schema family no longer matches the certified source",
                ));
            }
            locators
                .iter()
                .map(|locator| {
                    hydrate_exact_row(connection, self.registration.dialect, &schema, locator)
                })
                .collect()
        })();
        let snapshot_finish = sqlite_snapshot.finish();
        let root_finish = source_root.revalidate();
        snapshot_finish.map_err(temporary_hydration_failure)?;
        root_finish.map_err(temporary_hydration_failure)?;
        resolved
    }

    fn validate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> std::result::Result<(), HydrationFailure> {
        locator.validate_contract().map_err(|error| {
            hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
        })?;
        if locator.source().provider() != self.registration.provider().as_str()
            || locator.source().source_format() != self.registration.source_format()
            || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
            || locator.certified_source_revision_digest().is_some()
            || locator_schema_family(self.registration.dialect, locator.source()).is_none()
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "locator does not belong to this OpenCode-family registration",
            ));
        }
        Ok(())
    }
}

fn hydrate_exact_row(
    connection: &Connection,
    dialect: &OpenCodeSqliteDialect,
    schema: &OpenCodeNativeSchema,
    locator: &SourceRecordLocator,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "locator is not a provider SQLite coordinate",
        ));
    };
    let TypedKey::Utf8(native_identity) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "OpenCode-family primary key is not typed UTF-8",
        ));
    };
    if logical_relation != schema.family.event_table() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "OpenCode-family logical relation does not match the selected schema",
        ));
    }

    let mut sql = source_backed_event_sql(schema);
    let alias = if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        "p"
    } else {
        "x"
    };
    sql.push_str(&format!(" where {alias}.id = ?2 limit 2"));
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "provider SQLite value limit is unrepresentable",
        )
    })?;
    let mut statement = connection
        .prepare(&sql)
        .map_err(temporary_hydration_failure)?;
    let mut rows = statement
        .query(params![max_json_bytes, native_identity])
        .map_err(temporary_hydration_failure)?;
    let row = rows
        .next()
        .map_err(temporary_hydration_failure)?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "provider SQLite row no longer exists",
            )
        })?;
    let event = decode_source_event_row(row, schema, dialect).map_err(|error| {
        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
    })?;
    if &event.native_identity != native_identity {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite native row key no longer matches",
        ));
    }
    let record_digest = source_event_row_digest(&event);
    if &record_digest != locator.record_digest() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite exact row digest no longer matches",
        ));
    }
    let retained = retained_projection(&event.projection).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row is no longer a retained lexical event",
        )
    })?;
    event.source_data.exact_text().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row is no longer stored as text",
        )
    })?;
    let normalized_time = retained
        .body
        .pointer("/time/created")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(event.time_created);
    let semantic_digest = source_backed_event_digest(
        schema.family,
        &event.native_identity,
        &event.native_order,
        normalized_time,
        event.time_updated,
        &retained,
    )
    .map_err(|error| {
        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
    })?;
    let expected_version = TypedKey::composite(vec![
        TypedKey::I64(event.time_updated),
        TypedKey::utf8(semantic_digest).map_err(|error| {
            hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
        })?,
    ])
    .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string()))?;
    if row_version.as_ref() != Some(&expected_version) {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite typed row version no longer matches",
        ));
    }
    let kind =
        source_backed_retained_event_kind(&retained.effective_type, &retained.role, &retained.body);
    let display_text =
        source_backed_retained_searchable_text(kind, &retained.effective_type, &retained.body);
    if display_text.is_empty() {
        Ok(b"OpenCode event".to_vec())
    } else {
        Ok(display_text.into_bytes())
    }
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn temporary_hydration_failure(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(
        HydrationFailureKind::TemporarilyUnavailable,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests;
