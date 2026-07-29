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
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator,
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
    schema::{hex_digest, OpenCodeNativeSchema},
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
        DiscoveryContext, DiscoveryReport, SqliteSourceAccessError, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const SOURCE_ANCHOR_KEY: &str = "active-database";
const SOURCE_IDENTITY_VERSION: u32 = 1;
const SOURCE_REVISION_KIND: &str = "provider-sqlite-snapshot-v1";
const PARSER_REVISION: &str = "opencode-family-source-backed-v1";
const LOGICAL_SESSION_KIND: &str = "opencode-family-session";
const LOGICAL_EVENT_KIND: &str = "opencode-family-event";
const NATIVE_SESSION_NAMESPACE: &str = "opencode-family.session-id";
const SOURCE_BACKED_PAGE_ROWS: usize = 64;
const SOURCE_BACKED_MAX_FILE_TOUCHES: usize = 32;
const SQLITE_SOURCE_INVALID_REASON: &str =
    "OpenCode-family history database must be a regular file";

fn source_backed_retained_event_kind(
    effective_type: &str,
    role: &str,
    body: &serde_json::Value,
) -> OpenCodeNativeEventKind {
    if body.get("result_outcome").is_some() {
        if effective_type == "shell" || body.get("command").is_some() {
            return OpenCodeNativeEventKind::CommandOutput;
        }
        return OpenCodeNativeEventKind::ToolOutput;
    }
    if matches!(
        effective_type,
        "tool" | "tool_call" | "tool-call" | "tool_use" | "tooluse"
    ) || json_contains_tool_call(body)
    {
        OpenCodeNativeEventKind::ToolCall
    } else if matches!(effective_type, "reasoning" | "summary") {
        OpenCodeNativeEventKind::Summary
    } else if matches!(role, "user" | "assistant")
        || matches!(effective_type, "user" | "assistant" | "text")
    {
        OpenCodeNativeEventKind::Message
    } else {
        OpenCodeNativeEventKind::Notice
    }
}

fn json_contains_tool_call(body: &serde_json::Value) -> bool {
    body.get("tool_calls").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_call").is_some()
        || body
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(serde_json::Value::as_str),
                        Some("tool" | "tool_use" | "toolCall" | "tool_call")
                    )
                })
            })
}

fn source_backed_retained_searchable_text(
    kind: OpenCodeNativeEventKind,
    effective_type: &str,
    body: &serde_json::Value,
) -> String {
    if let Some(text) = body.get("text").and_then(serde_json::Value::as_str) {
        return text.to_owned();
    }
    if let Some(text) = body.get("summary").and_then(serde_json::Value::as_str) {
        return text.to_owned();
    }
    if kind == OpenCodeNativeEventKind::ToolCall {
        let tool = body
            .get("tool")
            .or_else(|| body.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tool");
        let command = body
            .pointer("/state/input/command")
            .or_else(|| body.pointer("/input/command"))
            .or_else(|| body.get("command"))
            .and_then(serde_json::Value::as_str);
        return command.map_or_else(
            || format!("tool call: {tool}"),
            |command| format!("{tool}\n{command}"),
        );
    }
    if let Some(content) = body.get("content") {
        let text = match content {
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(|value| {
                    value
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            return text;
        }
    }
    format!("OpenCode {effective_type} event")
}

fn source_backed_retained_file_touches(
    kind: OpenCodeNativeEventKind,
    body: &serde_json::Value,
) -> (Vec<OpenCodeNativeFileTouch>, usize) {
    if !matches!(
        kind,
        OpenCodeNativeEventKind::ToolCall | OpenCodeNativeEventKind::Notice
    ) {
        return (Vec::new(), 0);
    }
    let mut paths = BTreeSet::new();
    for pointer in [
        "/path",
        "/file_path",
        "/filePath",
        "/input/path",
        "/input/file_path",
        "/state/input/path",
        "/state/input/file_path",
    ] {
        if let Some(path) = body.pointer(pointer).and_then(serde_json::Value::as_str) {
            if !path.trim().is_empty() {
                paths.insert(path.to_owned());
            }
        }
    }
    if let Some(files) = body.get("files").and_then(serde_json::Value::as_array) {
        for file in files {
            let path = file
                .as_str()
                .or_else(|| file.get("path").and_then(serde_json::Value::as_str));
            if let Some(path) = path.filter(|path| !path.trim().is_empty()) {
                paths.insert(path.to_owned());
            }
        }
    }
    let observed = paths.len();
    let retained = paths
        .into_iter()
        .take(SOURCE_BACKED_MAX_FILE_TOUCHES)
        .map(|path| OpenCodeNativeFileTouch { path })
        .collect();
    (retained, observed)
}

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

    pub(crate) fn exact_resolver(
        self,
        path: impl Into<PathBuf>,
    ) -> OpenCodeSourceBackedExactResolver {
        OpenCodeSourceBackedExactResolver {
            registration: self,
            path: path.into(),
        }
    }

    pub(crate) const fn mutation_policy(self) -> OpenCodeSourceMutationPolicy {
        OpenCodeSourceMutationPolicy::Replace
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenCodeSourceMutationPolicy {
    /// No append frontier is asserted. Any observed mutation replaces the source.
    Replace,
}

#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedScan {
    pub(crate) source: SourceKey,
    pub(crate) certificate: CertifiedSource,
    pub(crate) schema_family: &'static str,
    pub(crate) emitted_pages: u64,
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

#[derive(Debug)]
struct WorkingScan {
    source: SourceKey,
    opening: SourceObservation,
    schema_family: &'static str,
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    emitted_pages: u64,
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
    source_rowid: i64,
    source_data: SqliteSourceValue,
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
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let mut pending_pages = Vec::new();
    let working = {
        let connection = sqlite_snapshot.connection()?;
        register_projection_function(connection, dialect)?;
        let schema = OpenCodeNativeSchema::probe(connection, dialect)?;
        let source = source_key(dialect, schema.family).map_err(source_backed_as_capture)?;
        let opening =
            source_observation(&source, &opening_evidence).map_err(source_backed_as_capture)?;
        let sessions =
            load_sessions(connection, &schema, &source).map_err(source_backed_as_capture)?;
        let (counts, content_digest, emitted_pages) = stream_events(
            connection,
            &schema,
            dialect,
            path,
            &source,
            &opening,
            &sessions,
            &mut |page| {
                pending_pages.push(page);
                Ok(())
            },
        )
        .map_err(source_backed_as_capture)?;
        WorkingScan {
            source,
            opening,
            schema_family: schema.family.label(),
            counts,
            content_digest,
            emitted_pages,
        }
    };
    let closing_evidence = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    let closing = source_observation(&working.source, &closing_evidence)?;
    let certificate = CertifiedSource::certify(
        working.opening,
        closing,
        PARSER_REVISION,
        working.content_digest,
        working.counts,
    )
    .map_err(map_certification_error)?;
    for page in pending_pages {
        emit(page)?;
    }
    Ok(OpenCodeSourceBackedScan {
        source: working.source,
        certificate,
        schema_family: working.schema_family,
        emitted_pages: working.emitted_pages,
    })
}

fn stream_events(
    connection: &Connection,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    path: &Path,
    source: &SourceKey,
    opening: &SourceObservation,
    sessions: &BTreeMap<String, SourceSession>,
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<(ScannedSourceCounts, [u8; 32], u64)> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-source-backed-content-v1\0");
    hash_str(&mut hasher, schema.family.label());
    hash_str(&mut hasher, &schema.capability_digest);
    hash_sessions(&mut hasher, sessions);

    let mut sql = source_backed_event_sql(schema);
    sql.push_str(" order by 3, 4, 5, 6, 2, 1, 12");
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([max_json_bytes])?;
    let revision_digest = source_revision_digest(opening);
    let mut counts = ScannedSourceCounts::default();
    let mut page = Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS);
    let mut emitted_pages = 0_u64;
    let mut session_sequences = HashMap::<String, u64>::new();

    while let Some(row) = rows.next()? {
        let event = decode_source_event_row(row, schema, dialect)?;
        hash_source_event(&mut hasher, &event);
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, event.content_bytes)?;

        let Some(retained) = retained_projection(&event.projection) else {
            if projection_is_rejected(&event.projection_bytes)? {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
            } else {
                counts.ignored_records = checked_add(counts.ignored_records, 1)?;
            }
            continue;
        };
        let session = sessions.get(&event.session_identity).ok_or_else(|| {
            OpenCodeSourceBackedError::MissingSession(event.session_identity.clone())
        })?;
        let document = lexical_document(
            source,
            schema.family,
            revision_digest,
            path,
            session,
            event,
            retained,
            session_sequences
                .entry(session.native_identity.clone())
                .or_default(),
        )?;
        counts.retained_records = checked_add(counts.retained_records, 1)?;
        counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
        page.push(document);
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
    Ok((counts, hasher.finalize().into(), emitted_pages))
}

fn decode_source_event_row(
    row: &Row<'_>,
    _schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
) -> OpenCodeSourceBackedResult<SourceEventRow> {
    let native_identity: String = row.get(0)?;
    let message_identity: String = row.get(1)?;
    let session_identity: String = row.get(2)?;
    let order_tag: i64 = row.get(3)?;
    let order_a: i64 = row.get(4)?;
    let order_b: i64 = row.get(5)?;
    let time_created: i64 = row.get(6)?;
    let time_updated: i64 = row.get(7)?;
    let content_bytes = u64::try_from(row.get::<_, i64>(8)?).map_err(|_| {
        CaptureError::InvalidPayload("OpenCode-family content byte count is negative".to_owned())
    })?;
    let mut projection_bytes: Vec<u8> = row.get(9)?;
    let has_explicit_event_time: i64 = row.get(10)?;
    if has_explicit_event_time == 0 {
        if let Err(error) = provider_required_timestamp_millis(
            time_created,
            dialect.session_message_time_created_field,
        ) {
            projection_bytes = encode_rejection_reason(error.to_string());
        }
    }
    let projection = decode_projection(&projection_bytes)?;
    Ok(SourceEventRow {
        native_order: source_backed_decode_order(
            order_tag,
            &session_identity,
            &message_identity,
            &native_identity,
            order_a,
            order_b,
        )?,
        native_identity,
        message_identity,
        session_identity,
        time_created,
        time_updated,
        content_bytes,
        projection,
        projection_bytes,
        source_rowid: row.get(11)?,
        source_data: SqliteSourceValue::from_ref(row.get_ref(12)?),
    })
}

fn retained_projection(projection: &OpenCodeJsonProjection) -> Option<OpenCodeRetainedJson> {
    match projection {
        OpenCodeJsonProjection::Retained(retained) => Some(retained.clone()),
        OpenCodeJsonProjection::Output(output) => output.diagnostic.clone(),
        OpenCodeJsonProjection::ExcludedOutput
        | OpenCodeJsonProjection::Rejected(_)
        | OpenCodeJsonProjection::RejectedWithReason(_, _) => None,
    }
}

fn projection_is_rejected(bytes: &[u8]) -> OpenCodeSourceBackedResult<bool> {
    Ok(matches!(
        decode_projection(bytes)?,
        OpenCodeJsonProjection::Rejected(_) | OpenCodeJsonProjection::RejectedWithReason(_, _)
    ))
}

#[allow(clippy::too_many_arguments)]
fn lexical_document(
    source: &SourceKey,
    family: OpenCodeNativeSchemaFamily,
    source_revision_digest: [u8; 32],
    source_path: &Path,
    session: &SourceSession,
    event: SourceEventRow,
    retained: OpenCodeRetainedJson,
    next_sequence: &mut u64,
) -> OpenCodeSourceBackedResult<LexicalDocument> {
    let raw_record = event
        .source_data
        .exact_text()
        .ok_or(OpenCodeSourceBackedError::MissingExactText)?;
    let record_digest: [u8; 32] = Sha256::digest(raw_record).into();
    let normalized_time = retained
        .body
        .pointer("/time/created")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(event.time_created);
    let semantic_digest = source_backed_event_digest(
        family,
        &event.native_identity,
        &event.native_order,
        normalized_time,
        event.time_updated,
        &retained,
    )?;
    let native_record_identity = source_backed_native_record_identity(
        family,
        &event.message_identity,
        &event.native_identity,
    );
    let native_item_key = if family == OpenCodeNativeSchemaFamily::MessagePart {
        NativeItemKey::composite(
            family.identity_semantics(),
            vec![
                TypedKey::utf8(event.message_identity.clone())?,
                TypedKey::utf8(event.native_identity.clone())?,
            ],
        )?
    } else {
        NativeItemKey::native_id(
            family.identity_semantics(),
            TypedKey::utf8(native_record_identity)?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let row_version = TypedKey::composite(vec![
        TypedKey::I64(event.time_updated),
        TypedKey::utf8(semantic_digest)?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: family.event_table().to_owned(),
            primary_key: TypedKey::utf8(event.native_identity.clone())?,
            row_version: Some(row_version),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        record_digest,
    )?;

    let kind =
        source_backed_retained_event_kind(&retained.effective_type, &retained.role, &retained.body);
    let searchable =
        source_backed_retained_searchable_text(kind, &retained.effective_type, &retained.body);
    let body = if searchable.is_empty() {
        "OpenCode event".to_owned()
    } else {
        searchable
    };
    let (file_touches, _) = source_backed_retained_file_touches(kind, &retained.body);
    let event_sequence = *next_sequence;
    *next_sequence = checked_add(*next_sequence, 1)?;
    Ok(LexicalDocument {
        event_id,
        session_id: session.session_id,
        parent_session_id: session.parent_session_id,
        root_session_id: session.root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.native_identity.clone()),
        branch: session.branch.clone(),
        source_path: Some(source_path.to_string_lossy().into_owned()),
        agent_type: if session.parent_native_identity.is_some() {
            "subagent".to_owned()
        } else {
            "primary".to_owned()
        },
        is_primary: session.parent_native_identity.is_none(),
        event_sequence,
        occurred_at_unix_ms: Some(normalized_time),
        event_type: event_kind_label(kind).to_owned(),
        role: Some(retained.role),
        body,
        workspace: None,
        cwd: session.directory.clone(),
        touched_files: file_touches.into_iter().map(|touch| touch.path).collect(),
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

fn root_session_identity(
    identity: &str,
    sessions: &BTreeMap<
        String,
        (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >,
) -> String {
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
    source_directory.revalidate()?;
    source_root.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok((source_root, sqlite_snapshot))
}

fn source_observation(
    source: &SourceKey,
    evidence: &SqliteSourceEvidence,
) -> OpenCodeSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        SOURCE_REVISION_KIND,
        format!(
            "identity={};length={};revision={}",
            hex_digest(*evidence.identity()),
            evidence.length(),
            hex_digest(*evidence.revision()),
        )
        .into_bytes(),
    )?)
}

fn source_revision_digest(observation: &SourceObservation) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-source-revision-evidence-v1\0");
    hash_str(&mut hasher, observation.revision_kind());
    hash_bytes(&mut hasher, observation.revision());
    hasher.finalize().into()
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
    hasher.update(event.time_created.to_le_bytes());
    hasher.update(event.time_updated.to_le_bytes());
    hasher.update(event.source_rowid.to_le_bytes());
    hash_bytes(hasher, &event.projection_bytes);
    event.source_data.hash_into(hasher);
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
        request
            .events()
            .iter()
            .map(|event| self.hydrate_event(event))
            .collect()
    }
}

impl OpenCodeSourceBackedExactResolver {
    fn hydrate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> std::result::Result<Vec<u8>, HydrationFailure> {
        locator.validate_contract().map_err(|error| {
            hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
        })?;
        if locator.source().provider() != self.registration.provider().as_str()
            || locator.source().source_format() != self.registration.source_format()
            || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "locator does not belong to this OpenCode-family registration",
            ));
        }
        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&self.path).map_err(temporary_hydration_failure)?;
        let observation = source_observation(locator.source(), sqlite_snapshot.evidence())
            .map_err(|error| {
                hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
            })?;
        if locator.certified_source_revision_digest() != Some(&source_revision_digest(&observation))
        {
            return Err(hydration_failure(
                HydrationFailureKind::StaleSourceEvidence,
                "provider SQLite snapshot no longer matches the certified source revision",
            ));
        }

        let connection = sqlite_snapshot
            .connection()
            .map_err(temporary_hydration_failure)?;
        register_projection_function(connection, self.registration.dialect)
            .map_err(temporary_hydration_failure)?;
        let provider_bytes = hydrate_exact_row(connection, self.registration.dialect, locator)
            .map_err(|failure| {
                CaptureError::InvalidPayload(format!(
                    "{}: {}",
                    hydration_kind_label(failure.kind),
                    failure.detail
                ))
            })
            .map_err(decode_hydration_capture_error)?;
        sqlite_snapshot
            .finish()
            .map_err(temporary_hydration_failure)?;
        source_root
            .revalidate()
            .map_err(temporary_hydration_failure)?;
        Ok(provider_bytes)
    }
}

fn hydrate_exact_row(
    connection: &Connection,
    dialect: &OpenCodeSqliteDialect,
    locator: &SourceRecordLocator,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let schema = OpenCodeNativeSchema::probe(connection, dialect).map_err(|error| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            error.to_string(),
        )
    })?;
    let current_source = source_key(dialect, schema.family).map_err(|error| {
        hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
    })?;
    if !current_source.exact_descriptor_eq(locator.source()) {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "provider SQLite schema family no longer matches the certified source",
        ));
    }
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

    let mut sql = source_backed_event_sql(&schema);
    let alias = if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        "p"
    } else {
        "x"
    };
    sql.push_str(&format!(" where {alias}.id = ?2 order by 12 limit 2"));
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
    let event = decode_source_event_row(row, &schema, dialect).map_err(|error| {
        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
    })?;
    let retained = retained_projection(&event.projection).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row is no longer a retained lexical event",
        )
    })?;
    let provider_bytes = event
        .source_data
        .exact_text()
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::StaleRecordEvidence,
                "provider SQLite row is no longer stored as text",
            )
        })?
        .to_vec();
    let record_digest: [u8; 32] = Sha256::digest(&provider_bytes).into();
    if &record_digest != locator.record_digest() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row digest no longer matches",
        ));
    }
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

fn hydration_kind_label(kind: HydrationFailureKind) -> &'static str {
    match kind {
        HydrationFailureKind::TemporarilyUnavailable => "temporarily_unavailable",
        HydrationFailureKind::ConfirmedDeleted => "confirmed_deleted",
        HydrationFailureKind::StaleSourceEvidence => "stale_source_evidence",
        HydrationFailureKind::StaleRecordEvidence => "stale_record_evidence",
        HydrationFailureKind::MissingRecord => "missing_record",
        HydrationFailureKind::UnsupportedParserRevision => "unsupported_parser_revision",
        HydrationFailureKind::InvalidLocator => "invalid_locator",
    }
}

fn decode_hydration_capture_error(error: CaptureError) -> HydrationFailure {
    let detail = error.to_string();
    for (label, kind) in [
        (
            "stale_source_evidence",
            HydrationFailureKind::StaleSourceEvidence,
        ),
        (
            "stale_record_evidence",
            HydrationFailureKind::StaleRecordEvidence,
        ),
        ("missing_record", HydrationFailureKind::MissingRecord),
        (
            "unsupported_parser_revision",
            HydrationFailureKind::UnsupportedParserRevision,
        ),
        ("invalid_locator", HydrationFailureKind::InvalidLocator),
    ] {
        if detail.contains(label) {
            return hydration_failure(kind, detail);
        }
    }
    temporary_hydration_failure(error)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::{
        ContentSourceResolver, EventHydrationRequest, HydrationFailureKind, NativeRecordCoordinate,
        TypedKey,
    };
    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::*;
    use crate::provider_sources::{DiscoveryPlatform, DiscoveryPlatformDirs, ProviderSourceStatus};

    #[cfg(target_os = "linux")]
    #[test]
    fn source_backed_open_does_not_follow_leaf_swap_after_authorization() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("opencode.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let original = temp.path().join("original.sqlite");
        create_fixture(&path, "expected", 1);
        create_fixture(&attacker, "attacker", 1);

        let result = open_root_authorized_snapshot_with_hook(&path, || {
            fs::rename(&path, &original).unwrap();
            fs::rename(&attacker, &path).unwrap();
        });
        assert!(matches!(
            result,
            Err(OpenCodeSourceBackedError::Capture(
                CaptureError::InvalidProviderTranscriptPath { .. },
            )) | Err(OpenCodeSourceBackedError::SqliteSource(
                SqliteSourceAccessError::SourceChanged,
            ))
        ));
    }

    #[test]
    fn cold_scan_and_exact_row_hydration_cover_all_three_dialects() {
        for registration in opencode_family_source_backed_registrations() {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let path = temp
                .path()
                .join(format!("{}.sqlite", registration.provider().as_str()));
            let expected = create_fixture(&path, registration.provider().as_str(), 2);
            let mut documents = Vec::new();
            let scan = registration
                .scan(&path, &mut |page| {
                    documents.extend(page);
                    Ok(())
                })
                .unwrap();

            assert_eq!(
                registration.mutation_policy(),
                OpenCodeSourceMutationPolicy::Replace
            );
            assert_eq!(scan.certificate.counts().complete_records, 2);
            assert_eq!(scan.certificate.counts().retained_records, 2);
            assert_eq!(scan.certificate.counts().indexed_documents, 2);
            assert!(scan.certificate.frontier().is_none());
            assert_eq!(scan.emitted_pages, 1);
            assert_eq!(scan.schema_family, "session_message_seq");
            assert_eq!(documents.len(), 2);
            let first_row: serde_json::Value = serde_json::from_str(&expected[0]).unwrap();
            let expected_first_body = first_row["text"].as_str().unwrap();
            assert_eq!(documents[0].body, expected_first_body);
            assert!(documents[0].body.ends_with("opencode-tail"));
            assert_eq!(documents[0].provider_session_id.as_deref(), Some("child"));
            let root_session_id = session_id(&scan.source, "root").unwrap();
            assert_eq!(documents[0].parent_session_id, Some(root_session_id));
            assert_eq!(documents[0].root_session_id, root_session_id);
            assert_eq!(documents[0].branch.as_deref(), Some("feature"));
            assert_eq!(
                documents[0].source_path.as_deref(),
                Some(path.to_string_lossy().as_ref())
            );
            assert_eq!(documents[0].agent_type, "subagent");
            assert!(!documents[0].is_primary);
            assert_eq!(documents[0].event_sequence, 0);
            assert_eq!(documents[1].event_sequence, 1);

            let NativeRecordCoordinate::ProviderSqlite {
                logical_relation,
                primary_key,
                row_version,
            } = documents[0].locator.coordinate()
            else {
                panic!("expected provider SQLite locator")
            };
            assert_eq!(logical_relation, "session_message");
            assert_eq!(primary_key, &TypedKey::Utf8("message-0".to_owned()));
            assert!(matches!(row_version, Some(TypedKey::Composite(parts)) if parts.len() == 2));

            let mut replayed = Vec::new();
            let replay = registration
                .scan(&path, &mut |page| {
                    replayed.extend(page);
                    Ok(())
                })
                .unwrap();
            assert_eq!(replay.source.identity(), scan.source.identity());
            assert_eq!(replayed[0].event_id, documents[0].event_id);
            assert_eq!(replayed[0].session_id, documents[0].session_id);

            let request =
                EventHydrationRequest::new(documents[0].event_id, documents[0].locator.clone())
                    .unwrap();
            let resolver = registration.exact_resolver(&path);
            let hydrated = resolver.hydrate_event(&request).unwrap();
            assert_eq!(hydrated.provider_bytes, documents[0].body.as_bytes());

            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "update session_message
                 set data = ?1, time_updated = time_updated + 1
                 where id = 'message-0'",
                [r#"{"role":"user","text":"changed provider row"}"#],
            )
            .unwrap();
            drop(conn);
            let stale = resolver.hydrate_event(&request).unwrap_err();
            assert_eq!(stale.kind, HydrationFailureKind::StaleSourceEvidence);
        }
    }

    #[test]
    fn active_wal_scan_reads_latest_rows_without_persistent_source_writes() {
        let registration = opencode_source_backed_registration();
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("opencode.sqlite");
        create_fixture(&path, "opencode", SOURCE_BACKED_PAGE_ROWS + 1);

        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "wal").unwrap();
        writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        writer
            .execute_batch("pragma wal_checkpoint(truncate)")
            .unwrap();
        let wal_body = r#"{"role":"user","text":"OpenCode active WAL sentinel"}"#;
        writer
            .execute(
                "update session_message set data = ?1, time_updated = time_updated + 1
                 where id = 'message-0'",
                [wal_body],
            )
            .unwrap();
        let before = sqlite_persistent_bytes(&path);

        let mut documents = Vec::new();
        registration
            .scan(&path, &mut |page| {
                assert!(page.len() <= SOURCE_BACKED_PAGE_ROWS);
                documents.extend(page);
                Ok(())
            })
            .unwrap();
        assert!(documents
            .iter()
            .any(|document| document.body.contains("OpenCode active WAL sentinel")));
        assert_eq!(sqlite_persistent_bytes(&path), before);
        drop(writer);
    }

    #[test]
    fn registration_discovery_preserves_winners_and_inactive_exclusions() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let home = temp.path().join("home");
        let xdg = temp.path().join("xdg");
        let cwd = temp.path().join("cwd");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(xdg.join("kilo")).unwrap();
        fs::create_dir_all(xdg.join("mimocode/preview")).unwrap();
        fs::write(xdg.join("kilo/kilo.db"), b"current").unwrap();
        fs::write(xdg.join("kilo/opencode.db"), b"legacy").unwrap();
        fs::write(xdg.join("mimocode/preview/mimocode.db"), b"inactive").unwrap();

        let dirs = DiscoveryPlatformDirs {
            data: Some(xdg.clone()),
            config: None,
            state: None,
            local_data: Some(xdg.clone()),
        };
        let context = DiscoveryContext::new(&home, &cwd, DiscoveryPlatform::Linux, dirs.clone())
            .with_env("XDG_DATA_HOME", &xdg);

        let kilo = kilo_source_backed_registration().discover(&context);
        assert_eq!(kilo.sources.len(), 1);
        assert_eq!(kilo.sources[0].path, xdg.join("kilo/kilo.db"));
        assert_eq!(kilo.sources[0].status, ProviderSourceStatus::Available);

        let mimo = mimocode_source_backed_registration().discover(&context);
        assert_eq!(mimo.sources.len(), 1);
        assert_eq!(mimo.sources[0].path, xdg.join("mimocode/mimocode.db"));
        assert_eq!(mimo.sources[0].status, ProviderSourceStatus::Missing);
        assert!(mimo
            .sources
            .iter()
            .all(|source| !source.path.to_string_lossy().contains("preview")));

        for (registration, env_name) in [
            (opencode_source_backed_registration(), "OPENCODE_DB"),
            (kilo_source_backed_registration(), "KILO_DB"),
            (mimocode_source_backed_registration(), "MIMOCODE_DB"),
        ] {
            let memory = DiscoveryContext::new(&home, &cwd, DiscoveryPlatform::Linux, dirs.clone())
                .with_env("XDG_DATA_HOME", &xdg)
                .with_env(env_name, ":memory:");
            let report = registration.discover(&memory);
            assert!(report.sources.is_empty());
            assert_eq!(report.issues.len(), 1);
        }
    }

    fn create_fixture(path: &Path, provider: &str, rows: usize) -> Vec<String> {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "create table session (
                 id text primary key,
                 parent_id text,
                 title text,
                 directory text,
                 branch text,
                 agent text,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );",
        )
        .unwrap();
        conn.execute(
            "insert into session
             (id, parent_id, title, directory, branch, agent, time_created, time_updated)
             values ('root', null, 'Root', '/workspace/root', 'main', 'primary', 1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "insert into session
             (id, parent_id, title, directory, branch, agent, time_created, time_updated)
             values ('child', 'root', 'Child', '/workspace/child', 'feature',
                     'subagent', 2, 3)",
            [],
        )
        .unwrap();
        let mut expected = Vec::new();
        for sequence in 0..rows {
            let text = if sequence == 0 {
                format!(
                    "{} opencode-tail",
                    format!("{provider} retained ").repeat(400)
                )
            } else {
                format!("{provider} retained message {sequence}")
            };
            let data = json!({
                "role": if sequence % 2 == 0 { "user" } else { "assistant" },
                "text": text
            })
            .to_string();
            conn.execute(
                "insert into session_message
                 (id, session_id, type, seq, time_created, time_updated, data)
                 values (?1, 'child', 'message', ?2, ?3, ?3, ?4)",
                params![
                    format!("message-{sequence}"),
                    i64::try_from(sequence).unwrap(),
                    1_800_000_000_000_i64 + i64::try_from(sequence).unwrap(),
                    data,
                ],
            )
            .unwrap();
            expected.push(data);
        }
        expected
    }

    fn sqlite_persistent_bytes(path: &Path) -> Vec<Vec<u8>> {
        // Stock WAL readers may update volatile SHM reader marks.
        ["", "-wal"]
            .into_iter()
            .map(|suffix| {
                let mut component = path.as_os_str().to_os_string();
                component.push(suffix);
                fs::read(PathBuf::from(component)).unwrap()
            })
            .collect()
    }
}
