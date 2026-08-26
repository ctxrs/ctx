//! Read-only projection of current per-agent OpenClaw SQLite transcripts.
//!
//! One admitted database is one ctx source. Active transcript rows are read
//! only through `session_transcript_active_events`; reset and deleted archive
//! generations are separate generation-qualified native sessions.
//!
//! # JSONL overlap policy
//!
//! Source discovery must select at most one automatic OpenClaw history family
//! for a normalized agent. An admitted `openclaw-agent.sqlite` source wins and
//! suppresses that agent's legacy `sessions/*.jsonl` source. JSONL is a
//! fallback only when no current SQLite database is admitted. The formats have
//! intentionally different source descriptors, so registering both would
//! duplicate provider-native sessions instead of merging them.

use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_timestamp_value;
use ctx_history_capture_runtime::{
    CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
    ReplacementDocumentTree, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CoreRecord, EventIdentityInput,
    NativeItemKey, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchorScope, SourceKey, SourceObservation, TypedKey,
};
pub use ctx_history_openclaw_schema::{
    OPENCLAW_AGENT_SCHEMA_VERSION, OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
};
use ctx_history_provider_runtime::{
    combine_primary_and_cleanup_route_errors, open_provider_sqlite_readonly, source_io,
    CaptureError, ProviderChangedDocumentSink, ProviderRouteControlExpectation,
    ProviderRuntimeBinding, ReadOnlySqliteConnection,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use thiserror::Error;

const DATABASE_LEAF: &str = "openclaw-agent.sqlite";
const DATABASE_PARENT: &str = "agent";
const SOURCE_SCHEMA_VARIANT: &str = "openclaw-agent-schema-v17";
const PARSER_REVISION: &str = "openclaw-agent-sqlite-v3";
const SOURCE_ANCHOR_NAMESPACE: &str = "openclaw.agent";
const ACTIVE_SESSION_NAMESPACE: &str = "openclaw.sqlite.session";
const ARCHIVE_SESSION_NAMESPACE: &str = "openclaw.sqlite.archive-generation";
const LOGICAL_SESSION_KIND: &str = "openclaw-session";
const NATIVE_EVENT_NAMESPACE: &str = "openclaw.sqlite.event";
const LOGICAL_EVENT_KIND: &str = "openclaw-event";
const OBSERVATION_KIND: &str = "openclaw-agent-logical-snapshot-v1";
const FINGERPRINT_DOMAIN: &[u8] = b"ctx-openclaw-agent-sqlite-leaf-v1\0";
const CONTENT_DOMAIN: &[u8] = b"ctx-openclaw-agent-sqlite-content-v1\0";
const MAX_ARCHIVE_DECODED_BYTES: usize = 64 * 1024 * 1024;

/// Automatic discovery policy consumed by the shared OpenClaw resolver.
pub const OPENCLAW_JSONL_SQLITE_OVERLAP_POLICY: &str =
    "per normalized agent, admitted openclaw_agent_sqlite suppresses legacy OpenClaw JSONL";

#[derive(Debug, Error)]
pub enum OpenClawSqliteError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("unsupported OpenClaw transcript archive {session_id}/{generation}: {reason}")]
    UnsupportedArchive {
        session_id: String,
        generation: String,
        reason: String,
    },
    #[error("{primary}; SQLite snapshot finalization also failed: {finalization}")]
    Finalization {
        primary: Box<OpenClawSqliteError>,
        finalization: CaptureError,
    },
}

type Result<T> = std::result::Result<T, OpenClawSqliteError>;

impl From<rusqlite::Error> for OpenClawSqliteError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Capture(CaptureError::Sqlite(error))
    }
}

impl From<serde_json::Error> for OpenClawSqliteError {
    fn from(error: serde_json::Error) -> Self {
        Self::Capture(CaptureError::Json(error))
    }
}

impl From<ProjectionContractError> for OpenClawSqliteError {
    fn from(error: ProjectionContractError) -> Self {
        contract_capture(error)
    }
}

impl From<ctx_history_openclaw_schema::OpenClawSchemaError> for OpenClawSqliteError {
    fn from(error: ctx_history_openclaw_schema::OpenClawSchemaError) -> Self {
        match error {
            ctx_history_openclaw_schema::OpenClawSchemaError::Sqlite(error) => error.into(),
            ctx_history_openclaw_schema::OpenClawSchemaError::Mismatch(detail) => {
                unsupported_schema(detail)
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static BEFORE_TERMINAL_REVALIDATION_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
fn run_before_terminal_revalidation_hook() {
    BEFORE_TERMINAL_REVALIDATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(any(test, feature = "test-support")))]
fn run_before_terminal_revalidation_hook() {}

#[cfg(feature = "test-support")]
pub mod test_support {
    /// Installs a one-shot mutation hook immediately before terminal SQLite
    /// source-family revalidation.
    pub fn set_before_openclaw_sqlite_terminal_revalidation_hook(hook: impl FnOnce() + 'static) {
        super::BEFORE_TERMINAL_REVALIDATION_HOOK.with(|slot| {
            assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
        });
    }
}

pub struct OpenClawSqliteAdapter<B: ProviderRuntimeBinding> {
    data_root: PathBuf,
    path: PathBuf,
    source_scope: SourceAnchorScope,
    binding: PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> OpenClawSqliteAdapter<B> {
    pub fn new(data_root: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self::new_scoped(data_root, path, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(
        data_root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        source_scope: SourceAnchorScope,
    ) -> Self {
        Self {
            data_root: data_root.into(),
            path: path.into(),
            source_scope,
            binding: PhantomData,
        }
    }
}

pub struct OpenClawSqliteLeaf {
    agent_id: String,
    source: SourceKey,
    connection: Mutex<Option<ReadOnlySqliteConnection>>,
}

impl<B: ProviderRuntimeBinding> ReplacementDocumentTree for OpenClawSqliteAdapter<B> {
    type Lifecycle = B::CaptureLifecycleSink;
    type Spool = B::DocumentRecordSpool;
    type RouteControl = ProviderRouteControlExpectation;
    type Leaf = OpenClawSqliteLeaf;
    type TreeAuthority = ();

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::OpenClaw.as_str()
            && source.source_format() == OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let agent_id = path_agent_id(&self.path).map_err(route_error)?;
        let source = source_key_scoped(&agent_id, self.source_scope).map_err(route_error)?;
        let connection = open_database(&self.data_root, &self.path)
            .map_err(|error| route_error(OpenClawSqliteError::Capture(error)))?;
        if let Err(error) = validate_database(&connection, &agent_id) {
            return match finalize_result(connection, Err::<(), _>(error)) {
                Err(error) => Err(route_error(error)),
                Ok(()) => Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "OpenClaw validation failure was lost during SQLite finalization",
                )),
            };
        }
        let fingerprint = source_fingerprint(&source);
        let leaf = OpenClawSqliteLeaf {
            agent_id,
            source,
            connection: Mutex::new(Some(connection)),
        };
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                leaf,
                false,
            )],
            (),
        ))
    }

    fn scan_changed(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ProviderChangedDocumentSink<'_, '_, B>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        sink.begin_source(leaf.source.clone())?;
        let mut slot = leaf.connection.lock().map_err(|_| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "OpenClaw SQLite snapshot mutex was poisoned",
            )
        })?;
        let connection = slot.as_ref().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "OpenClaw SQLite snapshot was already finalized",
            )
        })?;
        let mut sink_error = None;
        let projected = project_database(
            connection,
            &leaf.agent_id,
            &leaf.source,
            MAX_ARCHIVE_DECODED_BYTES,
            &mut |record| {
                if let Err(error) = sink.emit_core_record(record) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(invalid_payload(detail));
                }
                Ok(())
            },
        );
        let scan = match projected {
            Ok(scan) => scan,
            Err(primary) => {
                let finalization = slot.take().map(ReadOnlySqliteConnection::finish);
                drop(slot);
                if let Some(error) = sink_error {
                    return match finalization.transpose() {
                        Ok(_) => Err(error),
                        Err(cleanup) => Err(combine_primary_and_cleanup_route_errors(
                            error,
                            capture_route_error(cleanup),
                        )),
                    };
                }
                let primary = match finalization.transpose() {
                    Ok(_) => primary,
                    Err(finalization) => OpenClawSqliteError::Finalization {
                        primary: Box::new(primary),
                        finalization,
                    },
                };
                return Err(route_error(primary));
            }
        };
        let observation = SourceObservation::new(
            leaf.source.clone(),
            OBSERVATION_KIND,
            scan.content_digest.to_vec(),
        )
        .map_err(contract_error)?;
        Ok(DocumentSourceTerminal {
            source: leaf.source.clone(),
            opening: observation.clone(),
            closing: observation,
            parser_revision: PARSER_REVISION,
            content_digest: scan.content_digest,
            counts: scan.counts,
        })
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let leaf = tree.leaves.first().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "OpenClaw SQLite inventory omitted its only database leaf",
            )
        })?;
        let connection = leaf
            .provider_leaf
            .connection
            .lock()
            .map_err(|_| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "OpenClaw SQLite snapshot mutex was poisoned",
                )
            })?
            .take()
            .ok_or_else(|| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    "OpenClaw SQLite snapshot was finalized before tree revalidation",
                )
            })?;
        run_before_terminal_revalidation_hook();
        connection.finish().map_err(capture_route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

#[derive(Debug)]
struct ProjectionReceipt {
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
}

#[derive(Clone, Copy)]
enum SessionGeneration<'a> {
    Active,
    Archive(&'a str),
}

#[allow(clippy::too_many_arguments)]
fn project_database(
    connection: &Connection,
    agent_id: &str,
    source: &SourceKey,
    archive_limit: usize,
    emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
) -> Result<ProjectionReceipt> {
    validate_active_projection(connection)?;
    let mut digest = Sha256::new();
    digest.update(CONTENT_DOMAIN);
    digest_field(&mut digest, agent_id.as_bytes());
    digest_field(&mut digest, &OPENCLAW_AGENT_SCHEMA_VERSION.to_be_bytes());
    let mut counts = ScannedSourceCounts::default();

    let mut active = connection.prepare(
        r#"SELECT a.session_id, a.active_position, a.event_seq, e.event_json, e.created_at,
                  (SELECT i.event_id FROM transcript_event_identities i
                    WHERE i.session_id = a.session_id AND i.seq = a.event_seq
                    ORDER BY i.event_id LIMIT 1),
                  (SELECT count(*) FROM transcript_event_identities i
                    WHERE i.session_id = a.session_id AND i.seq = a.event_seq)
             FROM session_transcript_active_events a
             JOIN transcript_events e
               ON e.session_id = a.session_id AND e.seq = a.event_seq
            ORDER BY a.session_id, a.active_position"#,
    )?;
    let rows = active.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (session_id, position, seq, event_json, created_at, identity, identity_count) = row?;
        if identity_count != 1 {
            return Err(invalid_payload(format!(
                "OpenClaw active event {session_id}/{seq} has {identity_count} identity rows"
            )));
        }
        let identity = identity.ok_or_else(|| {
            invalid_payload(format!(
                "OpenClaw active event {session_id}/{seq} has no event identity"
            ))
        })?;
        let position = nonnegative_u64(position, "active_position")?;
        let event = parse_event(&event_json, &identity, &session_id)?;
        digest_field(&mut digest, b"active");
        digest_field(&mut digest, session_id.as_bytes());
        digest_field(&mut digest, &position.to_be_bytes());
        digest_field(&mut digest, &seq.to_be_bytes());
        digest_field(&mut digest, identity.as_bytes());
        digest_field(&mut digest, event_json.as_bytes());
        digest_field(&mut digest, &created_at.to_be_bytes());
        emit(project_event(
            source,
            &session_id,
            SessionGeneration::Active,
            position,
            &identity,
            &event,
            created_at,
        )?)?;
        count_retained(&mut counts, event_json.len())?;
    }

    let mut archives = connection.prepare(
        r#"SELECT session_id, generation, reason, encoding, archive_blob, archive_sha256, created_at
             FROM session_transcript_archives
            ORDER BY session_id, generation"#,
    )?;
    let archive_rows = archives.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Vec<u8>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in archive_rows {
        let (session_id, generation, reason, encoding, blob, expected_sha, created_at) = row?;
        if session_id.is_empty()
            || generation.is_empty()
            || !matches!(reason.as_str(), "reset" | "deleted")
        {
            return Err(unsupported_archive(
                &session_id,
                &generation,
                "archive identity or reason violates the current OpenClaw contract",
            ));
        }
        validate_archive_hash(&session_id, &generation, &blob, &expected_sha)?;
        digest_field(&mut digest, b"archive");
        digest_field(&mut digest, session_id.as_bytes());
        digest_field(&mut digest, generation.as_bytes());
        digest_field(&mut digest, reason.as_bytes());
        digest_field(&mut digest, encoding.as_bytes());
        digest_field(&mut digest, &created_at.to_be_bytes());
        digest_field(&mut digest, &blob);
        let decoded = decode_archive(&session_id, &generation, &encoding, &blob, archive_limit)?;
        let mut archive_event_ids = BTreeSet::new();
        for (line_index, line) in decoded.split(|byte| *byte == b'\n').enumerate() {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            if line.len() > ctx_history_providers_jsonl_shared::MAX_PROVIDER_JSONL_LINE_BYTES {
                return Err(unsupported_archive(
                    &session_id,
                    &generation,
                    format!(
                        "JSONL line {} exceeds the bounded line limit",
                        line_index + 1
                    ),
                ));
            }
            let event: serde_json::Value = serde_json::from_slice(line).map_err(|error| {
                unsupported_archive(
                    &session_id,
                    &generation,
                    format!("JSONL line {} is invalid: {error}", line_index + 1),
                )
            })?;
            let identity = event
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    unsupported_archive(
                        &session_id,
                        &generation,
                        format!("JSONL line {} has no native event id", line_index + 1),
                    )
                })?;
            if !archive_event_ids.insert(identity.to_owned()) {
                return Err(unsupported_archive(
                    &session_id,
                    &generation,
                    format!("native event id {identity:?} occurs more than once"),
                ));
            }
            let sequence = u64::try_from(line_index).map_err(|_| {
                unsupported_archive(
                    &session_id,
                    &generation,
                    "archive line index exceeds the supported sequence range",
                )
            })?;
            emit(project_event(
                source,
                &session_id,
                SessionGeneration::Archive(&generation),
                sequence,
                identity,
                &event,
                created_at,
            )?)?;
            count_retained(&mut counts, line.len())?;
        }
        counts.certified_bytes = counts
            .certified_bytes
            .checked_add(u64::try_from(blob.len()).map_err(|_| count_overflow())?)
            .ok_or_else(count_overflow)?;
    }

    Ok(ProjectionReceipt {
        content_digest: digest.finalize().into(),
        counts,
    })
}

fn project_event(
    source: &SourceKey,
    native_session_id: &str,
    generation: SessionGeneration<'_>,
    sequence: u64,
    native_event_id: &str,
    event: &serde_json::Value,
    created_at: i64,
) -> Result<CoreRecord> {
    let (session_key, provider_session_id) = match generation {
        SessionGeneration::Active => (
            NativeSessionKey::native_id(
                ACTIVE_SESSION_NAMESPACE,
                TypedKey::utf8(native_session_id)?,
            )?,
            Some(native_session_id.to_owned()),
        ),
        SessionGeneration::Archive(generation) => (
            NativeSessionKey::composite(
                ARCHIVE_SESSION_NAMESPACE,
                vec![
                    TypedKey::utf8(native_session_id)?,
                    TypedKey::utf8(generation)?,
                ],
            )?,
            None,
        ),
    };
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let item_key =
        NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, TypedKey::utf8(native_event_id)?)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &item_key,
        subrecord_selector: None,
    })?;
    let fallback =
        DateTime::<Utc>::from_timestamp_millis(created_at).unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let occurred_at = provider_timestamp_value(event.get("timestamp"), fallback);
    let fact = ctx_history_providers_jsonl_shared::adapters::normalize_openclaw_event(
        sequence,
        event,
        occurred_at,
    );
    let body = fact.lexical_text;
    // `new_selected` validates its initial body before structured content is
    // attached. Empty lexical events retain only their complete native JSON;
    // the temporary constructor body is never published.
    let constructor_body = if body.is_empty() { "{}" } else { body.as_str() };
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        fact.event_type.as_str(),
        PARSER_REVISION,
        constructor_body,
    )
    .map_err(contract_capture)?;
    record.provider_session_id = provider_session_id;
    record.native_event_id = Some(TypedKey::utf8(native_event_id)?);
    record.occurred_at_unix_ms = Some(fact.occurred_at.timestamp_millis());
    record.role = fact.role.map(|role| role.as_str().to_owned());
    record.agent_scope = None;
    record.content.normalized_body = (!body.is_empty()).then_some(body);
    record.content.structured_content = Some(event.clone());
    record.validate_contract().map_err(contract_capture)?;
    Ok(record)
}

fn validate_database(connection: &Connection, path_agent_id: &str) -> Result<()> {
    ctx_history_openclaw_schema::validate_openclaw_agent_v17(connection, path_agent_id)
        .map_err(Into::into)
}

fn validate_active_projection(connection: &Connection) -> Result<()> {
    let orphaned_active: i64 = connection.query_row(
        r#"SELECT count(*)
             FROM session_transcript_active_events a
             LEFT JOIN transcript_events e
               ON e.session_id = a.session_id AND e.seq = a.event_seq
             LEFT JOIN session_windows w ON w.session_id = a.session_id
            WHERE e.session_id IS NULL OR w.session_id IS NULL"#,
        [],
        |row| row.get(0),
    )?;
    if orphaned_active != 0 {
        return Err(invalid_payload(format!(
            "OpenClaw active transcript projection has {orphaned_active} orphaned rows"
        )));
    }
    let mut statement = connection.prepare(
        r#"SELECT w.session_id, s.indexed_seq, s.needs_rebuild, s.active_event_count,
                  s.active_message_count,
                  (SELECT count(*) FROM session_transcript_active_events a
                    WHERE a.session_id = w.session_id),
                  (SELECT coalesce(max(a.active_position), -1)
                     FROM session_transcript_active_events a
                    WHERE a.session_id = w.session_id),
                  (SELECT count(*) FROM session_transcript_active_events a
                    WHERE a.session_id = w.session_id AND a.message_position IS NOT NULL),
                  (SELECT coalesce(max(a.message_position), -1)
                     FROM session_transcript_active_events a
                    WHERE a.session_id = w.session_id),
                  (SELECT coalesce(max(e.seq), -1) FROM transcript_events e
                    WHERE e.session_id = w.session_id)
             FROM session_windows w
             LEFT JOIN session_transcript_index_state s ON s.session_id = w.session_id
            WHERE s.session_id IS NOT NULL
               OR EXISTS (SELECT 1 FROM transcript_events e WHERE e.session_id = w.session_id)
               OR EXISTS (SELECT 1 FROM session_transcript_active_events a WHERE a.session_id = w.session_id)
            ORDER BY w.session_id"#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
        ))
    })?;
    for row in rows {
        let (
            session_id,
            indexed_seq,
            needs_rebuild,
            declared_events,
            declared_messages,
            events,
            max_event_position,
            messages,
            max_message_position,
            max_seq,
        ) = row?;
        let Some(indexed_seq) = indexed_seq else {
            return Err(invalid_payload(format!(
                "OpenClaw session {session_id:?} has transcript rows but no index state"
            )));
        };
        if needs_rebuild != Some(0)
            || declared_events != Some(events)
            || declared_messages != Some(messages)
            || max_event_position != events - 1
            || max_message_position != messages - 1
            || indexed_seq < max_seq
        {
            return Err(invalid_payload(format!(
                "OpenClaw session {session_id:?} has an incomplete active transcript projection"
            )));
        }
    }
    Ok(())
}

fn parse_event(event_json: &str, identity: &str, session_id: &str) -> Result<serde_json::Value> {
    let event = serde_json::from_str::<serde_json::Value>(event_json)?;
    if !event.is_object() || event.get("id").and_then(serde_json::Value::as_str) != Some(identity) {
        return Err(invalid_payload(format!(
            "OpenClaw active event in session {session_id:?} does not match identity {identity:?}"
        )));
    }
    Ok(event)
}

fn decode_archive(
    session_id: &str,
    generation: &str,
    encoding: &str,
    blob: &[u8],
    limit: usize,
) -> Result<Vec<u8>> {
    if encoding == "identity" {
        if blob.len() > limit {
            return Err(unsupported_archive(
                session_id,
                generation,
                format!("identity archive exceeds {limit} decoded bytes"),
            ));
        }
        return Ok(blob.to_vec());
    }
    if encoding != "zstd" {
        return Err(unsupported_archive(
            session_id,
            generation,
            format!("unknown encoding {encoding:?}"),
        ));
    }
    let decoder = zstd::stream::read::Decoder::new(Cursor::new(blob)).map_err(|error| {
        unsupported_archive(
            session_id,
            generation,
            format!("zstd decoder initialization failed: {error}"),
        )
    })?;
    let maximum = u64::try_from(limit).unwrap_or(u64::MAX);
    let mut decoded = Vec::new();
    decoder
        .take(maximum.saturating_add(1))
        .read_to_end(&mut decoded)
        .map_err(|error| {
            unsupported_archive(
                session_id,
                generation,
                format!("zstd decode failed: {error}"),
            )
        })?;
    if decoded.len() > limit {
        return Err(unsupported_archive(
            session_id,
            generation,
            format!("zstd archive exceeds {limit} decoded bytes"),
        ));
    }
    Ok(decoded)
}

fn validate_archive_hash(
    session_id: &str,
    generation: &str,
    blob: &[u8],
    expected: &str,
) -> Result<()> {
    let digest = Sha256::digest(blob);
    let actual = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        return Err(unsupported_archive(
            session_id,
            generation,
            "archive SHA-256 does not match its metadata",
        ));
    }
    Ok(())
}

fn path_agent_id(path: &Path) -> Result<String> {
    if path.file_name().and_then(|name| name.to_str()) != Some(DATABASE_LEAF)
        || path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(DATABASE_PARENT)
    {
        return Err(invalid_path(
            path,
            "OpenClaw agent database path must end in agent/openclaw-agent.sqlite",
        ));
    }
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|agent_id| !agent_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid_path(
                path,
                "OpenClaw agent database path has no UTF-8 agent owner",
            )
        })
}

fn open_database(
    data_root: &Path,
    path: &Path,
) -> std::result::Result<ReadOnlySqliteConnection, CaptureError> {
    source_io::ensure_provider_path_parents_are_not_symlinks(path)?;
    source_io::ensure_regular_provider_transcript_file(path)?;
    open_provider_sqlite_readonly(data_root, path)
}

#[cfg(test)]
fn source_key(agent_id: &str) -> Result<SourceKey> {
    source_key_scoped(agent_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(agent_id: &str, source_scope: SourceAnchorScope) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::OpenClaw.as_str(),
        OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(agent_id)?,
        source_scope,
    )
    .map_err(contract_capture)
}

fn source_fingerprint(source: &SourceKey) -> DocumentLeafFingerprint {
    let mut digest = Sha256::new();
    digest.update(FINGERPRINT_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.update(PARSER_REVISION.as_bytes());
    DocumentLeafFingerprint::new(digest.finalize().into())
}

fn count_retained(counts: &mut ScannedSourceCounts, bytes: usize) -> Result<()> {
    counts.complete_records = counts
        .complete_records
        .checked_add(1)
        .ok_or_else(count_overflow)?;
    counts.retained_records = counts
        .retained_records
        .checked_add(1)
        .ok_or_else(count_overflow)?;
    counts.indexed_documents = counts
        .indexed_documents
        .checked_add(1)
        .ok_or_else(count_overflow)?;
    counts.certified_bytes = counts
        .certified_bytes
        .checked_add(u64::try_from(bytes).map_err(|_| count_overflow())?)
        .ok_or_else(count_overflow)?;
    Ok(())
}

fn digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn nonnegative_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid_payload(format!("OpenClaw {field} is negative")))
}

fn finalize_result<T>(connection: ReadOnlySqliteConnection, primary: Result<T>) -> Result<T> {
    match (primary, connection.finish()) {
        (Ok(value), Ok(_)) => Ok(value),
        (Err(primary), Ok(_)) => Err(primary),
        (Ok(_), Err(finalization)) => Err(OpenClawSqliteError::Capture(finalization)),
        (Err(primary), Err(finalization)) => Err(OpenClawSqliteError::Finalization {
            primary: Box::new(primary),
            finalization,
        }),
    }
}

fn unsupported_archive(
    session_id: &str,
    generation: &str,
    reason: impl Into<String>,
) -> OpenClawSqliteError {
    OpenClawSqliteError::UnsupportedArchive {
        session_id: session_id.to_owned(),
        generation: generation.to_owned(),
        reason: reason.into(),
    }
}

fn unsupported_schema(detail: impl Into<String>) -> OpenClawSqliteError {
    OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(detail.into()))
}

fn invalid_payload(detail: impl Into<String>) -> OpenClawSqliteError {
    OpenClawSqliteError::Capture(CaptureError::InvalidPayload(detail.into()))
}

fn invalid_path(path: &Path, reason: &'static str) -> OpenClawSqliteError {
    OpenClawSqliteError::Capture(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    })
}

fn count_overflow() -> OpenClawSqliteError {
    OpenClawSqliteError::Capture(CaptureError::SystemInvariant(
        "OpenClaw SQLite source count overflowed",
    ))
}

fn contract_capture(error: impl std::fmt::Display) -> OpenClawSqliteError {
    invalid_payload(error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, error.to_string())
}

fn route_error(error: OpenClawSqliteError) -> SourceBackedRouteError {
    match error {
        error @ OpenClawSqliteError::UnsupportedArchive { .. } => {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unsupported, error.to_string())
        }
        OpenClawSqliteError::Finalization {
            primary,
            finalization,
        } => combine_primary_and_cleanup_route_errors(
            route_error(*primary),
            capture_route_error(finalization),
        ),
        OpenClawSqliteError::Capture(error) => capture_route_error(error),
    }
}

fn capture_route_error(error: CaptureError) -> SourceBackedRouteError {
    let kind = match &error {
        CaptureError::SourceChangedDuringCapture => SourceBackedRouteErrorKind::SourceChanged,
        CaptureError::Io(source) | CaptureError::SystemIo { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            SourceBackedRouteErrorKind::Unavailable
        }
        CaptureError::Io(_) | CaptureError::SystemIo { .. } => {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        CaptureError::SystemInvariant(_) | CaptureError::WorkerPanicked(_) => {
            SourceBackedRouteErrorKind::Internal
        }
        CaptureError::SqliteFinalization { .. } => SourceBackedRouteErrorKind::Internal,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

#[cfg(test)]
mod tests;
