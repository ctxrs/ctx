use std::collections::BTreeSet;

use ctx_history_core::{
    EventRole, FileChangeKind, ProjectionContractError, SourceKey, SourceResolverContractError,
    StableEntityId, StableEntityKind,
};
use rusqlite::{params, Connection};

use super::{
    hex,
    manifest::{ManifestSource, ValidatedManifest},
    sqlite_i64, sqlite_u64, RelationalEventMetadata, RelationalFileTouchMetadata,
    RelationalProjectionError, RelationalProjectionRecord, RelationalSessionMetadata,
    RelationalSourceMetadata, Result,
};

const MAX_METADATA_TEXT_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024;

pub(super) fn materialize_records<I>(
    conn: &Connection,
    expected: BTreeSet<String>,
    manifest: &ValidatedManifest,
    records: I,
) -> Result<()>
where
    I: Iterator<Item = RelationalProjectionRecord>,
{
    let mut current: Option<OpenSource> = None;
    let mut received = BTreeSet::new();
    for record in records {
        match record {
            RelationalProjectionRecord::BeginSource(metadata) => {
                if current.is_some() {
                    return stream_order("a source began before the prior source ended");
                }
                let source_id = metadata.source.identity().as_uuid().to_string();
                if !expected.contains(&source_id) {
                    return stream_order(format!(
                        "source {source_id} is not required by this projection update"
                    ));
                }
                if !received.insert(source_id.clone()) {
                    return stream_order(format!("source {source_id} appeared more than once"));
                }
                let source = manifest.sources.get(&source_id).ok_or_else(|| {
                    RelationalProjectionError::InvalidRecord(format!(
                        "source {source_id} is absent from the manifest"
                    ))
                })?;
                metadata
                    .source
                    .validate_exact_descriptor(source.certificate.observation().source())
                    .map_err(contract_record_error)?;
                validate_source_metadata(&metadata)?;
                insert_source(conn, &metadata, source)?;
                current = Some(OpenSource {
                    source_id,
                    source: metadata.source,
                    expected_events: source.certificate.counts().indexed_documents,
                    received_events: 0,
                });
            }
            RelationalProjectionRecord::Session(session) => {
                let open = current_source(&current)?;
                validate_session(&session, &open.source)?;
                insert_session(conn, &open.source_id, &session)?;
            }
            RelationalProjectionRecord::Event(event) => {
                let open = current_source_mut(&mut current)?;
                validate_event(&event, &open.source)?;
                insert_event(conn, &open.source_id, &event)?;
                open.received_events = open.received_events.checked_add(1).ok_or(
                    RelationalProjectionError::CountOverflow("source event count"),
                )?;
            }
            RelationalProjectionRecord::FileTouch(file) => {
                let open = current_source(&current)?;
                validate_file_touch(&file, &open.source)?;
                insert_file_touch(conn, &open.source_id, &file)?;
            }
            RelationalProjectionRecord::EndSource { source_id } => {
                let open = current.take().ok_or_else(|| {
                    RelationalProjectionError::InvalidStreamOrder(
                        "a source ended while no source was active".to_owned(),
                    )
                })?;
                if open.source.identity().as_uuid() != source_id {
                    return stream_order(
                        "the end-source identity does not match the active source",
                    );
                }
                if open.received_events != open.expected_events {
                    return Err(RelationalProjectionError::SourceEventCountMismatch {
                        source_id: open.source_id,
                        expected: open.expected_events,
                        received: open.received_events,
                    });
                }
            }
        }
    }
    if current.is_some() {
        return stream_order("the final source did not emit EndSource");
    }
    if received != expected {
        return Err(RelationalProjectionError::SourceSetMismatch {
            expected: expected.into_iter().collect(),
            received: received.into_iter().collect(),
        });
    }
    Ok(())
}

struct OpenSource {
    source_id: String,
    source: SourceKey,
    expected_events: u64,
    received_events: u64,
}

pub(super) struct ProjectionCounts {
    pub(super) sources: i64,
    pub(super) sessions: i64,
    pub(super) events: i64,
    pub(super) file_touches: i64,
}

fn insert_source(
    conn: &Connection,
    metadata: &RelationalSourceMetadata,
    manifest: &ManifestSource,
) -> Result<()> {
    let certificate = &manifest.certificate;
    let source = certificate.observation().source();
    conn.execute(
        "INSERT INTO source_backed_sources (
            source_id, source_identity, source_descriptor_json, certificate_json,
            certificate_digest, provider, source_format, source_root, source_path, cwd,
            revision_kind, parser_revision, certified_bytes, content_digest_hex,
            indexed_event_count
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
         )",
        params![
            source.identity().as_uuid().to_string(),
            source
                .identity()
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            serde_json::to_vec(source)?,
            manifest.certificate_json,
            manifest.certificate_digest.as_slice(),
            source.provider(),
            source.source_format(),
            metadata.source_root,
            metadata.source_path,
            metadata.cwd,
            certificate.observation().revision_kind(),
            certificate.parser_revision(),
            sqlite_i64(certificate.counts().certified_bytes, "certified bytes")?,
            hex(certificate.content_digest()),
            sqlite_i64(
                certificate.counts().indexed_documents,
                "source indexed documents"
            )?,
        ],
    )?;
    Ok(())
}

fn insert_session(
    conn: &Connection,
    source_id: &str,
    session: &RelationalSessionMetadata,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_backed_sessions (
            ctx_session_id, session_identity, source_id, parent_ctx_session_id,
            parent_session_identity, root_ctx_session_id, root_session_identity,
            provider_session_id, external_agent_id, agent_type, role_hint, is_primary,
            branch, workspace, cwd, source_path, status, fidelity, started_at_ms,
            ended_at_ms
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18, ?19, ?20
         )",
        params![
            session.session_id.as_uuid().to_string(),
            session
                .session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            source_id,
            session.parent_session_id.map(|id| id.as_uuid().to_string()),
            session
                .parent_session_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            session.root_session_id.as_uuid().to_string(),
            session
                .root_session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            session.provider_session_id,
            session.external_agent_id,
            session.agent_type.as_str(),
            session.role_hint,
            i64::from(session.is_primary),
            session.branch,
            session.workspace,
            session.cwd,
            session.source_path,
            session.status.as_str(),
            session.fidelity.as_str(),
            session.started_at_unix_ms,
            session.ended_at_unix_ms,
        ],
    )?;
    Ok(())
}

fn insert_event(conn: &Connection, source_id: &str, event: &RelationalEventMetadata) -> Result<()> {
    conn.execute(
        "INSERT INTO source_backed_events (
            ctx_event_id, event_identity, source_id, ctx_session_id, session_identity, event_seq,
            event_type, role, occurred_at_ms, fidelity, native_locator_json, record_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            event.event_id.as_uuid().to_string(),
            event
                .event_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            source_id,
            event.session_id.as_uuid().to_string(),
            event
                .session_id
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            sqlite_i64(event.event_sequence, "event sequence")?,
            event.event_type.as_str(),
            event.role.map(EventRole::as_str),
            event.occurred_at_unix_ms,
            event.fidelity.as_str(),
            serde_json::to_vec(&event.locator)?,
            event.locator.record_digest().as_slice(),
        ],
    )?;
    Ok(())
}

fn insert_file_touch(
    conn: &Connection,
    source_id: &str,
    file: &RelationalFileTouchMetadata,
) -> Result<()> {
    conn.execute(
        "INSERT INTO source_backed_files_touched (
            ctx_file_touch_id, source_id, ctx_event_id, event_identity, ctx_session_id,
            session_identity, path, old_path, change_kind, line_count_delta,
            confidence, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            file.file_touch_id.to_string(),
            source_id,
            file.event_id.map(|id| id.as_uuid().to_string()),
            file.event_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            file.session_id.map(|id| id.as_uuid().to_string()),
            file.session_id
                .map(StableEntityId::encode_canonical)
                .transpose()
                .map_err(contract_record_error)?
                .map(|identity| identity.to_vec()),
            file.path,
            file.old_path,
            file.change_kind.map(FileChangeKind::as_str),
            file.line_count_delta,
            file.confidence.as_str(),
            file.created_at_unix_ms,
            file.updated_at_unix_ms,
        ],
    )?;
    Ok(())
}

fn validate_source_metadata(metadata: &RelationalSourceMetadata) -> Result<()> {
    metadata
        .source
        .validate_contract()
        .map_err(contract_record_error)?;
    validate_optional_text(
        "source_root",
        metadata.source_root.as_deref(),
        MAX_PATH_BYTES,
    )?;
    validate_optional_text(
        "source_path",
        metadata.source_path.as_deref(),
        MAX_PATH_BYTES,
    )?;
    validate_optional_text("cwd", metadata.cwd.as_deref(), MAX_PATH_BYTES)
}

fn validate_session(session: &RelationalSessionMetadata, source: &SourceKey) -> Result<()> {
    validate_entity(session.session_id, StableEntityKind::Session, source)?;
    for relation in [session.parent_session_id, Some(session.root_session_id)]
        .into_iter()
        .flatten()
    {
        relation
            .validate_contract()
            .map_err(contract_record_error)?;
        if relation.entity_kind() != StableEntityKind::Session {
            return invalid_record("session relationship has a non-session identity");
        }
    }
    validate_optional_text(
        "provider_session_id",
        session.provider_session_id.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text(
        "external_agent_id",
        session.external_agent_id.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text(
        "role_hint",
        session.role_hint.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text("branch", session.branch.as_deref(), MAX_METADATA_TEXT_BYTES)?;
    validate_optional_text(
        "workspace",
        session.workspace.as_deref(),
        MAX_METADATA_TEXT_BYTES,
    )?;
    validate_optional_text("cwd", session.cwd.as_deref(), MAX_PATH_BYTES)?;
    validate_optional_text(
        "source_path",
        session.source_path.as_deref(),
        MAX_PATH_BYTES,
    )
}

fn validate_event(event: &RelationalEventMetadata, source: &SourceKey) -> Result<()> {
    validate_entity(event.event_id, StableEntityKind::Event, source)?;
    validate_entity(event.session_id, StableEntityKind::Session, source)?;
    event
        .locator
        .validate_contract()
        .map_err(resolver_record_error)?;
    if !event.locator.source().exact_descriptor_eq(source) {
        return invalid_record("event locator does not match the active source");
    }
    Ok(())
}

fn validate_file_touch(file: &RelationalFileTouchMetadata, source: &SourceKey) -> Result<()> {
    validate_text("path", &file.path, MAX_PATH_BYTES)?;
    validate_optional_text("old_path", file.old_path.as_deref(), MAX_PATH_BYTES)?;
    if let Some(event_id) = file.event_id {
        validate_entity(event_id, StableEntityKind::Event, source)?;
    }
    if let Some(session_id) = file.session_id {
        validate_entity(session_id, StableEntityKind::Session, source)?;
    }
    Ok(())
}

fn validate_entity(id: StableEntityId, kind: StableEntityKind, source: &SourceKey) -> Result<()> {
    id.validate_contract().map_err(contract_record_error)?;
    if id.entity_kind() != kind {
        return invalid_record("stable identity has the wrong entity kind");
    }
    if id.source_digest() != source.identity().digest()
        || id.source_descriptor_digest() != source.exact_descriptor_digest()
    {
        return invalid_record("stable identity does not belong to the active source");
    }
    Ok(())
}

pub(super) fn validate_projected_generation(
    conn: &Connection,
    manifest: &ValidatedManifest,
) -> Result<()> {
    let counts = projection_counts(conn)?;
    let projected_events = sqlite_u64(counts.events, "event_count")?;
    if projected_events != manifest.indexed_documents {
        return Err(RelationalProjectionError::GenerationEventCountMismatch {
            expected: manifest.indexed_documents,
            projected: projected_events,
        });
    }
    let source_count = sqlite_u64(counts.sources, "source_count")?;
    if source_count != manifest.sources.len() as u64 {
        return invalid_record("projected source count does not match the manifest");
    }
    let dangling_relationships: i64 = conn.query_row(
        "SELECT COUNT(*) FROM source_backed_sessions child
         WHERE (child.parent_ctx_session_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM source_backed_sessions parent
                    WHERE parent.ctx_session_id = child.parent_ctx_session_id
                      AND parent.session_identity = child.parent_session_identity
                ))
            OR (child.root_ctx_session_id IS NOT NULL
                AND NOT EXISTS (
                    SELECT 1 FROM source_backed_sessions root
                    WHERE root.ctx_session_id = child.root_ctx_session_id
                      AND root.session_identity = child.root_session_identity
                ))",
        [],
        |row| row.get(0),
    )?;
    if dangling_relationships != 0 {
        return invalid_record("session relationships reference absent sessions");
    }
    let dangling_event_or_file_relations: i64 = conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM source_backed_events event
             WHERE NOT EXISTS (
                 SELECT 1 FROM source_backed_sessions session
                 WHERE session.ctx_session_id = event.ctx_session_id
                   AND session.session_identity = event.session_identity
             ))
          + (SELECT COUNT(*) FROM source_backed_files_touched file
             WHERE file.ctx_event_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM source_backed_events event
                   WHERE event.ctx_event_id = file.ctx_event_id
                     AND event.event_identity = file.event_identity
               ))
          + (SELECT COUNT(*) FROM source_backed_files_touched file
             WHERE file.ctx_session_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM source_backed_sessions session
                   WHERE session.ctx_session_id = file.ctx_session_id
                     AND session.session_identity = file.session_identity
               ))",
        [],
        |row| row.get(0),
    )?;
    if dangling_event_or_file_relations != 0 {
        return invalid_record("event or file relationships have mismatched stable identities");
    }
    Ok(())
}

pub(super) fn projection_counts(conn: &Connection) -> Result<ProjectionCounts> {
    conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM source_backed_sources),
            (SELECT COUNT(*) FROM source_backed_sessions),
            (SELECT COUNT(*) FROM source_backed_events),
            (SELECT COUNT(*) FROM source_backed_files_touched)",
        [],
        |row| {
            Ok(ProjectionCounts {
                sources: row.get(0)?,
                sessions: row.get(1)?,
                events: row.get(2)?,
                file_touches: row.get(3)?,
            })
        },
    )
    .map_err(RelationalProjectionError::from)
}

fn current_source(current: &Option<OpenSource>) -> Result<&OpenSource> {
    current.as_ref().ok_or_else(|| {
        RelationalProjectionError::InvalidStreamOrder(
            "a relational record appeared outside a source scope".to_owned(),
        )
    })
}

fn current_source_mut(current: &mut Option<OpenSource>) -> Result<&mut OpenSource> {
    current.as_mut().ok_or_else(|| {
        RelationalProjectionError::InvalidStreamOrder(
            "a relational record appeared outside a source scope".to_owned(),
        )
    })
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum {
        return invalid_record(format!("{field} is empty or exceeds {maximum} bytes"));
    }
    Ok(())
}

fn validate_optional_text(field: &'static str, value: Option<&str>, maximum: usize) -> Result<()> {
    if let Some(value) = value {
        validate_text(field, value, maximum)?;
    }
    Ok(())
}

fn contract_record_error(error: ProjectionContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn resolver_record_error(error: SourceResolverContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn invalid_record<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidRecord(detail.into()))
}

fn stream_order<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidStreamOrder(detail.into()))
}
