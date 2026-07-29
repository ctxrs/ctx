use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
    EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::super::{
    firebender_event_parts, firebender_message_time, firebender_output_evidence,
    FirebenderOutputEvidence,
};
use super::{
    firebender_path_identity, firebender_raw_row_digest, firebender_source_revision,
    scan::build_page, validate_schema, FirebenderFrontier, FirebenderPage, FirebenderRow,
    FirebenderSqliteDatabase, SqliteSourceEvidence, FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES,
};
use crate::{
    native_source::NativeSqliteValue,
    provider::{
        normalization::{provider_policy_event_text, provider_timestamp_millis},
        sqlite::{sqlite_schema_fingerprint, sqlite_table_columns, SqliteLengthPreflightGuard},
    },
    CaptureError, Result as CaptureResult, FIREBENDER_SQLITE_SOURCE_FORMAT,
};

const FIREBENDER_SOURCE_ANCHOR_NAMESPACE: &str = "firebender.explicit-chat-history";
const FIREBENDER_NATIVE_SESSION_NAMESPACE: &str = "firebender.chat-session";
const FIREBENDER_NATIVE_EVENT_NAMESPACE: &str = "firebender.message";
const FIREBENDER_POSITION_KIND: &str = "firebender.messages-json-index";
const FIREBENDER_LOGICAL_SESSION_KIND: &str = "firebender-chat-session";
const FIREBENDER_LOGICAL_EVENT_KIND: &str = "firebender-message";
const FIREBENDER_SOURCE_SCHEMA_VARIANT: &str = "firebender-chat-sessions-v1";
const FIREBENDER_SOURCE_REVISION_KIND: &str = "firebender-sqlite-snapshot-v1";
const FIREBENDER_SOURCE_PARSER_REVISION: &str = "firebender-source-backed-v1";
const FIREBENDER_LOCATOR_RELATION: &str = "chat_sessions.messages_json";
const FIREBENDER_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-firebender-source-revision-digest-v1\0";

#[derive(Debug, Error)]
pub(crate) enum FirebenderSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("Firebender source-backed scan must be drained before certification")]
    ScanNotComplete,
    #[error("Firebender source-backed scan accounting overflowed")]
    CountOverflow,
    #[error("Firebender source-backed locator is malformed")]
    InvalidLocator,
    #[error("Firebender source-backed locator names a different explicit source")]
    LocatorSourceMismatch,
    #[error("Firebender source-backed locator source revision is stale")]
    StaleSourceEvidence,
    #[error("Firebender source-backed chat-session row is missing")]
    MissingSourceRow,
    #[error("Firebender source-backed chat-session row digest is stale")]
    StaleRowEvidence,
    #[error("Firebender source-backed nested message is missing")]
    MissingMessage,
    #[error("Firebender source-backed row exceeds the bounded hydration limit")]
    HydrationTooLarge,
}

pub(crate) type FirebenderSourceBackedResult<T> =
    std::result::Result<T, FirebenderSourceBackedError>;

// Both variants own source authority. Boxing the 1,072-byte replacement scanner
// to match the 496-byte certificate adds indirection without measured benefit.
#[allow(clippy::large_enum_variant)]
pub(crate) enum FirebenderSourceBackedPlan {
    Exact(CertifiedSource),
    Replacement(FirebenderSourceBackedScanner),
}

#[derive(Debug)]
pub(crate) struct FirebenderSourceBackedPage {
    documents: Vec<LexicalDocument>,
    retained_bytes: usize,
}

impl FirebenderSourceBackedPage {
    pub(crate) fn documents(&self) -> &[LexicalDocument] {
        &self.documents
    }

    pub(crate) fn into_documents(self) -> Vec<LexicalDocument> {
        self.documents
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

#[derive(Debug)]
pub(crate) struct FirebenderHydratedSourceRow {
    provider_session_id: String,
    message_index: u64,
    messages_json: Vec<u8>,
}

impl FirebenderHydratedSourceRow {
    pub(crate) fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    pub(crate) fn message_index(&self) -> u64 {
        self.message_index
    }

    pub(crate) fn messages_json(&self) -> &[u8] {
        &self.messages_json
    }
}

pub(crate) struct FirebenderSourceBackedScanner {
    database_path: PathBuf,
    source_path: String,
    workspace: Option<String>,
    source: SourceKey,
    opening: SourceObservation,
    database: FirebenderSqliteDatabase,
    frontier: FirebenderFrontier,
    counts: ScannedSourceCounts,
    drained: bool,
}

pub(crate) fn prepare_firebender_source_backed(
    explicit_path: &Path,
    prior: Option<&CertifiedSource>,
) -> FirebenderSourceBackedResult<FirebenderSourceBackedPlan> {
    let opened = OpenedFirebenderSource::open(explicit_path)?;
    if let Some(prior) = prior.filter(|prior| {
        prior.parser_revision() == FIREBENDER_SOURCE_PARSER_REVISION
            && prior.observation() == &opened.observation
    }) {
        return Ok(FirebenderSourceBackedPlan::Exact(prior.clone()));
    }
    Ok(FirebenderSourceBackedPlan::Replacement(
        FirebenderSourceBackedScanner {
            source_path: opened.database_path.display().to_string(),
            workspace: firebender_workspace(&opened.database_path),
            database_path: opened.database_path,
            source: opened.source,
            opening: opened.observation,
            database: opened.database,
            frontier: FirebenderFrontier::initial(),
            counts: ScannedSourceCounts::default(),
            drained: false,
        },
    ))
}

impl FirebenderSourceBackedScanner {
    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn opening_observation(&self) -> &SourceObservation {
        &self.opening
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> FirebenderSourceBackedResult<Option<FirebenderSourceBackedPage>> {
        if self.drained {
            return Ok(None);
        }
        let page = self
            .database
            .read(&self.database_path, |conn| build_page(conn, &self.frontier))?;
        self.frontier = page.next.clone();
        if page.row.is_none() && page.rejection.is_none() {
            self.drained = page.next.terminal;
            return Ok(None);
        }
        let documents = self.project_page(&page)?;
        self.drained = page.next.terminal;
        Ok(Some(FirebenderSourceBackedPage {
            documents,
            retained_bytes: page.retained_bytes,
        }))
    }

    pub(crate) fn finish(self) -> FirebenderSourceBackedResult<CertifiedSource> {
        if !self.drained || !self.frontier.terminal {
            return Err(FirebenderSourceBackedError::ScanNotComplete);
        }
        self.database.revalidate()?;
        let closing = SourceObservation::new(
            self.source.clone(),
            FIREBENDER_SOURCE_REVISION_KIND,
            self.opening.revision().to_vec(),
        )?;
        Ok(CertifiedSource::certify(
            self.opening,
            closing,
            FIREBENDER_SOURCE_PARSER_REVISION,
            self.frontier.prefix_sha256,
            self.counts,
        )?)
    }

    fn project_page(
        &mut self,
        page: &FirebenderPage,
    ) -> FirebenderSourceBackedResult<Vec<LexicalDocument>> {
        let Some(row) = page.row.as_ref() else {
            if page.rejection.is_some() {
                increment(&mut self.counts.complete_records, 1)?;
                increment(&mut self.counts.rejected_records, 1)?;
                increment(
                    &mut self.counts.certified_bytes,
                    u64::try_from(page.retained_bytes)
                        .map_err(|_| FirebenderSourceBackedError::CountOverflow)?,
                )?;
            }
            return Ok(Vec::new());
        };
        if page.message_start == 0 {
            increment(&mut self.counts.certified_bytes, canonical_row_bytes(row)?)?;
        }
        if page.rejection.is_some() {
            increment(&mut self.counts.complete_records, 1)?;
            increment(&mut self.counts.rejected_records, 1)?;
            return Ok(Vec::new());
        }

        let session_id = firebender_session_id(&self.source, &row.id)?;
        let source_revision_digest = source_revision_digest(&self.opening);
        let row_digest = firebender_raw_row_digest(&row.logical_values());
        let mut documents = Vec::with_capacity(page.message_end.saturating_sub(page.message_start));
        for (relative_index, message) in row.messages[page.message_start..page.message_end]
            .iter()
            .enumerate()
        {
            let message_index = page.message_start.saturating_add(relative_index);
            increment(&mut self.counts.complete_records, 1)?;
            let Some(document) = firebender_document(
                &self.source,
                session_id,
                source_revision_digest,
                &self.source_path,
                self.workspace.as_deref(),
                row,
                message_index,
                message,
                row_digest,
            )?
            else {
                increment(&mut self.counts.ignored_records, 1)?;
                continue;
            };
            increment(&mut self.counts.retained_records, 1)?;
            increment(&mut self.counts.indexed_documents, 1)?;
            documents.push(document);
        }
        Ok(documents)
    }
}

pub(crate) fn hydrate_firebender_source_backed_row(
    explicit_path: &Path,
    locator: &SourceRecordLocator,
) -> FirebenderSourceBackedResult<FirebenderHydratedSourceRow> {
    let opened = OpenedFirebenderSource::open(explicit_path)?;
    if !opened.source.exact_descriptor_eq(locator.source()) {
        return Err(FirebenderSourceBackedError::LocatorSourceMismatch);
    }
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
        || locator.certified_source_revision_digest()
            != Some(&source_revision_digest(&opened.observation))
    {
        return Err(FirebenderSourceBackedError::StaleSourceEvidence);
    }
    let (rowid, expected_session_id, expected_updated_at, message_index) =
        decode_locator_coordinate(locator)?;
    let row = opened
        .database
        .read(&opened.database_path, |conn| load_exact_row(conn, rowid))?
        .ok_or(FirebenderSourceBackedError::MissingSourceRow)?;
    if row.id != expected_session_id || row.updated_at != expected_updated_at {
        return Err(FirebenderSourceBackedError::StaleRowEvidence);
    }
    if &firebender_raw_row_digest(&row.logical_values()) != locator.record_digest() {
        return Err(FirebenderSourceBackedError::StaleRowEvidence);
    }
    let message_index_usize =
        usize::try_from(message_index).map_err(|_| FirebenderSourceBackedError::MissingMessage)?;
    if message_index_usize >= row.messages.len() {
        return Err(FirebenderSourceBackedError::MissingMessage);
    }
    Ok(FirebenderHydratedSourceRow {
        provider_session_id: row.id,
        message_index,
        messages_json: row.messages_json.into_bytes(),
    })
}

struct OpenedFirebenderSource {
    database_path: PathBuf,
    source: SourceKey,
    observation: SourceObservation,
    database: FirebenderSqliteDatabase,
}

impl OpenedFirebenderSource {
    fn open(explicit_path: &Path) -> FirebenderSourceBackedResult<Self> {
        let identity = firebender_path_identity(explicit_path)?;
        let database_path = identity.canonical_database_path;
        let (database, schema_fingerprint) =
            FirebenderSqliteDatabase::open(&database_path, |conn| {
                validate_schema(conn, &database_path)?;
                sqlite_schema_fingerprint(conn)
            })?;
        let source = firebender_source_key(&identity.route_identity)?;
        let observation =
            source_observation(source.clone(), database.evidence(), &schema_fingerprint)?;
        Ok(Self {
            database_path,
            source,
            observation,
            database,
        })
    }

    fn revalidate(&self) -> FirebenderSourceBackedResult<()> {
        self.database.revalidate().map_err(Into::into)
    }
}

fn firebender_source_key(route_identity: &str) -> FirebenderSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        FIREBENDER_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(route_identity)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Firebender.as_str(),
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        FIREBENDER_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn source_observation(
    source: SourceKey,
    evidence: &SqliteSourceEvidence,
    schema_fingerprint: &str,
) -> FirebenderSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source,
        FIREBENDER_SOURCE_REVISION_KIND,
        firebender_source_revision(evidence, schema_fingerprint).into_bytes(),
    )?)
}

fn source_revision_digest(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(FIREBENDER_REVISION_DIGEST_DOMAIN);
    digest.update((observation.revision_kind().len() as u64).to_be_bytes());
    digest.update(observation.revision_kind().as_bytes());
    digest.update((observation.revision().len() as u64).to_be_bytes());
    digest.update(observation.revision());
    digest.finalize().into()
}

fn firebender_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> FirebenderSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        FIREBENDER_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: FIREBENDER_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[allow(clippy::too_many_arguments)]
fn firebender_document(
    source: &SourceKey,
    session_id: StableEntityId,
    source_revision_digest: [u8; 32],
    source_path: &str,
    workspace: Option<&str>,
    row: &FirebenderRow,
    message_index: usize,
    message: &serde_json::Value,
    row_digest: [u8; 32],
) -> FirebenderSourceBackedResult<Option<LexicalDocument>> {
    let message_index_u64 =
        u64::try_from(message_index).map_err(|_| FirebenderSourceBackedError::CountOverflow)?;
    let event = firebender_event_parts(
        &row.id,
        message_index_u64,
        message,
        firebender_message_occurred_at(row, message_index, message),
    );
    let body = if event.event_type == EventType::ToolOutput {
        let evidence = firebender_output_evidence(message);
        if !evidence.failure && !evidence.timeout {
            return Ok(None);
        }
        sparse_output_body(&evidence)
    } else {
        provider_policy_event_text(event.event_type, &event.text, &event.body).text
    };
    let body = if body.is_empty() {
        format!("Firebender {}", event.event_type.as_str())
    } else {
        body
    };
    let native_item_key = message_native_key(message, message_index_u64)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: FIREBENDER_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: FIREBENDER_LOCATOR_RELATION.to_owned(),
            primary_key: TypedKey::I64(row.rowid),
            row_version: Some(TypedKey::composite(vec![
                TypedKey::utf8(&row.id)?,
                TypedKey::I64(row.updated_at),
                TypedKey::U64(message_index_u64),
            ])?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        row_digest,
    )?;
    Ok(Some(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(row.id.clone()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: ctx_history_core::AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: message_index_u64,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: workspace.map(str::to_owned),
        cwd: None,
        touched_files: Vec::new(),
    }))
}

fn firebender_workspace(database_path: &Path) -> Option<String> {
    let firebender_dir = database_path.parent()?;
    if firebender_dir.file_name().and_then(|name| name.to_str()) != Some("firebender") {
        return None;
    }
    let idea_dir = firebender_dir.parent()?;
    if idea_dir.file_name().and_then(|name| name.to_str()) != Some(".idea") {
        return None;
    }
    idea_dir
        .parent()
        .map(|workspace| workspace.display().to_string())
}

fn message_native_key(
    message: &serde_json::Value,
    message_index: u64,
) -> FirebenderSourceBackedResult<NativeItemKey> {
    if let Some(native_id) = message
        .get("id")
        .or_else(|| message.get("tool_call_id"))
        .or_else(|| message.get("toolCallId"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Ok(NativeItemKey::native_id(
            FIREBENDER_NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(native_id)?,
        )?);
    }
    Ok(NativeItemKey::certified_position(
        FIREBENDER_POSITION_KIND,
        TypedKey::U64(message_index),
        PositionStability::StableSlot,
    )?)
}

fn firebender_message_occurred_at(
    row: &FirebenderRow,
    message_index: usize,
    message: &serde_json::Value,
) -> DateTime<Utc> {
    let started_at = provider_timestamp_millis(Some(row.created_at), DateTime::<Utc>::UNIX_EPOCH);
    let offset = i64::try_from(message_index).unwrap_or(i64::MAX);
    firebender_message_time(message, started_at + chrono::Duration::milliseconds(offset))
}

fn sparse_output_body(evidence: &FirebenderOutputEvidence) -> String {
    let outcome = if evidence.timeout {
        "timed out"
    } else {
        "failed"
    };
    evidence.exit_code.map_or_else(
        || format!("Firebender tool output {outcome}"),
        |code| format!("Firebender tool output {outcome} with exit code {code}"),
    )
}

fn canonical_row_bytes(row: &FirebenderRow) -> FirebenderSourceBackedResult<u64> {
    let values = row.logical_values();
    values.iter().try_fold(8_u64, |total, value| {
        let value_bytes = match value {
            NativeSqliteValue::Null => 1,
            NativeSqliteValue::Integer(_) | NativeSqliteValue::RealBits(_) => 9,
            NativeSqliteValue::Text(value) => checked_len(value.len())?.saturating_add(9),
            NativeSqliteValue::Blob(value) => checked_len(value.len())?.saturating_add(9),
        };
        total
            .checked_add(value_bytes)
            .ok_or(FirebenderSourceBackedError::CountOverflow)
    })
}

fn checked_len(value: usize) -> FirebenderSourceBackedResult<u64> {
    u64::try_from(value).map_err(|_| FirebenderSourceBackedError::CountOverflow)
}

fn increment(target: &mut u64, value: u64) -> FirebenderSourceBackedResult<()> {
    *target = target
        .checked_add(value)
        .ok_or(FirebenderSourceBackedError::CountOverflow)?;
    Ok(())
}

fn decode_locator_coordinate(
    locator: &SourceRecordLocator,
) -> FirebenderSourceBackedResult<(i64, String, i64, u64)> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key: TypedKey::I64(rowid),
        row_version: Some(TypedKey::Composite(version)),
    } = locator.coordinate()
    else {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    };
    if logical_relation != FIREBENDER_LOCATOR_RELATION {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    }
    let [TypedKey::Utf8(session_id), TypedKey::I64(updated_at), TypedKey::U64(message_index)] =
        version.as_slice()
    else {
        return Err(FirebenderSourceBackedError::InvalidLocator);
    };
    Ok((*rowid, session_id.clone(), *updated_at, *message_index))
}

fn load_exact_row(conn: &Connection, rowid: i64) -> CaptureResult<Option<FirebenderRow>> {
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    let deleted_filter = if columns.contains("deleted_at") {
        " and deleted_at is null"
    } else {
        ""
    };
    let length_sql = format!(
        "select length(cast(id as blob)) + length(cast(name as blob)) + \
                length(cast(messages_json as blob)) + length(cast(metadata_json as blob)) \
         from chat_sessions where rowid = ?1{deleted_filter}"
    );
    let retained_bytes = {
        let _guard = SqliteLengthPreflightGuard::new(conn);
        conn.query_row(&length_sql, [rowid], |row| row.get::<_, i64>(0))
            .optional()?
    };
    let Some(retained_bytes) = retained_bytes else {
        return Ok(None);
    };
    if retained_bytes < 0
        || usize::try_from(retained_bytes).map_or(true, |bytes| {
            bytes > FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES
        })
    {
        return Err(CaptureError::InvalidPayload(
            FirebenderSourceBackedError::HydrationTooLarge.to_string(),
        ));
    }
    let sql = format!(
        "select id, name, cast(created_at as integer), cast(updated_at as integer), \
                messages_json, metadata_json \
         from chat_sessions where rowid = ?1{deleted_filter}"
    );
    conn.query_row(&sql, params![rowid], |row| {
        let messages_json: String = row.get(4)?;
        let messages =
            serde_json::from_str::<Vec<serde_json::Value>>(&messages_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
        Ok(FirebenderRow {
            rowid,
            id: row.get(0)?,
            name: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
            messages_json,
            metadata_json: row.get(5)?,
            messages,
        })
    })
    .optional()
    .map_err(CaptureError::from)
}
