//! Shared source-backed projection for the OpenCode SQLite dialect family.
//!
//! This module owns provider-local discovery, parsing, certification, lexical
//! projection, replacement streaming, and exact-row hydration. Atomic
//! publication remains owned by the shared coordinator.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

#[cfg(test)]
use ctx_history_core::SessionHydrationRequest;
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{limits::Limit, types::ValueRef, Connection, Row};
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
        providers::opencode::OpenCodeSqliteDialect, source_backed::SourceBackedRouteError,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const SOURCE_ANCHOR_KEY: &str = "active-database";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const PARSER_REVISION: &str = "opencode-family-source-backed-v2";
const LOGICAL_SESSION_KIND: &str = "opencode-family-session";
const LOGICAL_EVENT_KIND: &str = "opencode-family-event";
const NATIVE_SESSION_NAMESPACE: &str = "opencode-family.session-id";
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
mod hydration;
mod projection;

pub(crate) use adapter::register as register_source_backed_route;
pub(crate) use hydration::OpenCodeSourceBackedExactResolver;
use projection::{decode_source_event_row, lexical_document, retained_projection};

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

    #[cfg(test)]
    pub(crate) fn scan(
        self,
        path: &Path,
        emit: &mut dyn FnMut(Vec<LexicalDocument>) -> OpenCodeSourceBackedResult<()>,
    ) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
        scan_source(
            crate::test_provider_sqlite_data_root(),
            path,
            self.dialect,
            &mut |output| match output {
                OpenCodeScanOutput::Begin(_) => Ok(()),
                OpenCodeScanOutput::Document(document) => emit(vec![document]),
            },
        )
    }

    pub(crate) fn exact_resolver(
        self,
        data_root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> OpenCodeSourceBackedExactResolver {
        OpenCodeSourceBackedExactResolver::new(self, data_root, path)
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
}

#[derive(Clone, Debug)]
struct OpenCodeLogicalObservation {
    source: SourceKey,
    schema: OpenCodeNativeSchema,
    fingerprint: [u8; 32],
    logical_rows: u64,
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
    Document(LexicalDocument),
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

#[cfg(test)]
fn scan_source(
    data_root: &Path,
    path: &Path,
    dialect: &'static OpenCodeSqliteDialect,
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
    let authorized = open_root_authorized_snapshot_retained(data_root, path)?;
    let observation = observe_logical_source(authorized.sqlite_snapshot.connection()?, dialect)?;
    let scan = scan_pinned_source(
        path,
        dialect,
        &observation,
        authorized.sqlite_snapshot,
        emit,
    )?;
    authorized.source_root.revalidate()?;
    Ok(scan)
}

fn scan_pinned_source(
    path: &Path,
    dialect: &'static OpenCodeSqliteDialect,
    observation: &OpenCodeLogicalObservation,
    sqlite_snapshot: SqliteSourceReadSnapshot,
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<OpenCodeSourceBackedScan> {
    let working = {
        let connection = sqlite_snapshot.connection()?;
        register_projection_function(connection, dialect)?;
        let sessions = load_sessions(connection, &observation.schema, &observation.source)?;
        emit(OpenCodeScanOutput::Begin(observation.source.clone()))?;
        let streamed = stream_logical_rows(
            connection,
            &observation.schema,
            dialect,
            path,
            &observation.source,
            &sessions,
            emit,
        )?;
        let schema_evidence = relevant_schema_evidence(&observation.schema);
        let logical_snapshot = SqliteLogicalSnapshot::new(
            PARSER_REVISION,
            &schema_evidence,
            streamed.content_digest,
            streamed.counts,
        );
        WorkingScan {
            source: observation.source.clone(),
            logical_snapshot,
        }
    };
    sqlite_snapshot.finish()?;
    let certificate = working.logical_snapshot.certify(working.source.clone())?;
    Ok(OpenCodeSourceBackedScan {
        source: working.source,
        certificate,
    })
}

fn observe_logical_source(
    connection: &Connection,
    dialect: &'static OpenCodeSqliteDialect,
) -> OpenCodeSourceBackedResult<OpenCodeLogicalObservation> {
    let schema = OpenCodeNativeSchema::probe(connection, dialect)?;
    let source = source_key(dialect, schema.family)?;
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-logical-leaf-v1\0");
    hasher.update(source.exact_descriptor_digest());
    hash_bytes(&mut hasher, &relevant_schema_evidence(&schema));
    hash_logical_table(connection, &mut hasher, "session")?;
    if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        hash_logical_table(connection, &mut hasher, "message")?;
    }
    let logical_rows = hash_logical_table(connection, &mut hasher, schema.family.event_table())?;
    Ok(OpenCodeLogicalObservation {
        source,
        schema,
        fingerprint: hasher.finalize().into(),
        logical_rows,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StreamedLogicalRows {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
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
    emit: &mut dyn FnMut(OpenCodeScanOutput) -> OpenCodeSourceBackedResult<()>,
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
    let mut session_sequences = HashMap::<String, u64>::new();

    while let Some(row) = rows.next()? {
        let event = decode_source_event_row(row, schema, dialect)?;
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
        emit(OpenCodeScanOutput::Document(document))?;
    }
    Ok(StreamedLogicalRows {
        counts,
        content_digest: hasher.finalize().into(),
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

fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> OpenCodeSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    let authorized = open_root_authorized_snapshot_retained(data_root, path)?;
    Ok((authorized.source_root, authorized.sqlite_snapshot))
}

fn open_root_authorized_snapshot_retained(
    data_root: &Path,
    path: &Path,
) -> OpenCodeSourceBackedResult<OpenCodeAuthorizedSnapshot> {
    open_root_authorized_snapshot_retained_with_hook(data_root, path, || {})
}

#[cfg(test)]
fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> OpenCodeSourceBackedResult<(ProviderSourceRoot, SqliteSourceReadSnapshot)> {
    let authorized =
        open_root_authorized_snapshot_retained_with_hook(data_root, path, after_authorize)?;
    Ok((authorized.source_root, authorized.sqlite_snapshot))
}

fn open_root_authorized_snapshot_retained_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
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
    Ok(OpenCodeAuthorizedSnapshot {
        source_root,
        sqlite_authority,
        sqlite_snapshot,
    })
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

fn hash_logical_table(
    connection: &Connection,
    hasher: &mut Sha256,
    table: &str,
) -> OpenCodeSourceBackedResult<u64> {
    hash_str(hasher, table);
    let quoted = format!("\"{}\"", table.replace('"', "\"\""));
    let mut statement = connection.prepare(&format!("select * from {quoted} order by id"))?;
    let column_count = statement.column_count();
    hasher.update((column_count as u64).to_le_bytes());
    for column in statement.column_names() {
        hash_str(hasher, column);
    }
    let mut rows = statement.query([])?;
    let mut row_count = 0_u64;
    while let Some(row) = rows.next()? {
        row_count = checked_add(row_count, 1)?;
        for column in 0..column_count {
            hash_logical_value(hasher, row.get_ref(column)?);
        }
    }
    hasher.update(row_count.to_le_bytes());
    Ok(row_count)
}

fn hash_logical_value(hasher: &mut Sha256, value: ValueRef<'_>) {
    match value {
        ValueRef::Null => hasher.update([0]),
        ValueRef::Integer(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        ValueRef::Real(value) => {
            hasher.update([2]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        ValueRef::Text(value) => {
            hasher.update([3]);
            hash_bytes(hasher, value);
        }
        ValueRef::Blob(value) => {
            hasher.update([4]);
            hash_bytes(hasher, value);
        }
    }
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

#[cfg(test)]
mod tests;
