use std::collections::BTreeSet;

use ctx_history_core::{
    CoreContentPolicyStatus, CoreRecord, ProjectionContractError, SourceKey, StableEntityId,
};
use rusqlite::{params, Connection, Statement};
use serde::Serialize;

use super::{
    manifest::ValidatedGeneration, sqlite_i64, sqlite_u64, sqlite_u64_ordered_text,
    RelationalProjectionError, RelationalProjectionRecord, RelationalSourceMetadata, Result,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ProjectionCounts {
    pub(super) sources: i64,
    pub(super) sessions: i64,
    pub(super) events: i64,
    pub(super) repository_bindings: i64,
    pub(super) file_observations: i64,
    pub(super) vcs_observations: i64,
}

pub(super) fn materialize_records<I>(
    conn: &Connection,
    expected: BTreeSet<String>,
    generation: &ValidatedGeneration,
    records: I,
) -> Result<()>
where
    I: Iterator<Item = Result<RelationalProjectionRecord>>,
{
    conn.execute_batch("PRAGMA cache_size = -65536; PRAGMA temp_store = MEMORY;")?;
    let mut statements = MaterializationStatements::prepare(conn)?;
    let mut current: Option<OpenSource> = None;
    let mut received = BTreeSet::new();

    for record in records {
        match record? {
            RelationalProjectionRecord::BeginSource(metadata) => {
                let metadata = *metadata;
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
                let expected_source = generation.sources.get(&source_id).ok_or_else(|| {
                    RelationalProjectionError::InvalidRecord(format!(
                        "source {source_id} is absent from the pinned Core generation"
                    ))
                })?;
                metadata
                    .source
                    .validate_exact_descriptor(&expected_source.source)
                    .map_err(contract_record_error)?;
                if metadata.revision_digest != expected_source.revision_digest
                    || metadata.parser_revision != expected_source.parser_revision
                    || metadata.indexed_event_count != expected_source.indexed_event_count
                    || metadata.health != expected_source.health
                {
                    return invalid_record("source metadata does not match the pinned generation");
                }
                statements.insert_source(&metadata)?;
                current = Some(OpenSource {
                    source_id,
                    source: metadata.source,
                    expected_events: metadata.indexed_event_count,
                    received_events: 0,
                });
            }
            RelationalProjectionRecord::CoreRecord(record) => {
                let open = current.as_mut().ok_or_else(|| {
                    RelationalProjectionError::InvalidStreamOrder(
                        "a Core record appeared outside a source scope".to_owned(),
                    )
                })?;
                record.validate_contract().map_err(core_record_error)?;
                record
                    .source
                    .validate_exact_descriptor(&open.source)
                    .map_err(contract_record_error)?;
                statements.insert_core_record(&open.source_id, &record)?;
                open.received_events = open.received_events.checked_add(1).ok_or(
                    RelationalProjectionError::CountOverflow("source event count"),
                )?;
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

struct MaterializationStatements<'conn> {
    source: Statement<'conn>,
    session: Statement<'conn>,
    event: Statement<'conn>,
    repository: Statement<'conn>,
    alias: Statement<'conn>,
    evidence: Statement<'conn>,
    abstention: Statement<'conn>,
    file: Statement<'conn>,
    vcs: Statement<'conn>,
    vcs_parent: Statement<'conn>,
}

impl<'conn> MaterializationStatements<'conn> {
    fn prepare(conn: &'conn Connection) -> Result<Self> {
        Ok(Self {
            source: conn.prepare(
                "INSERT INTO core_sources (
                    source_id, source_identity, provider, source_format, schema_variant,
                    provider_identity_version, parser_revision, revision_digest,
                    indexed_event_count, health
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?,
            session: conn.prepare(
                "INSERT INTO core_sessions (
                    ctx_session_id, session_identity, source_id, parent_ctx_session_id,
                    parent_session_identity, root_ctx_session_id, root_session_identity,
                    provider_session_id, agent_type, is_primary, branch, workspace, cwd,
                    first_event_seq, started_at_ms, ended_at_ms, health
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                    ?14, ?15, ?15, 'ready'
                 )
                 ON CONFLICT(ctx_session_id) DO UPDATE SET
                    parent_ctx_session_id = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.parent_ctx_session_id ELSE core_sessions.parent_ctx_session_id END,
                    parent_session_identity = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.parent_session_identity ELSE core_sessions.parent_session_identity END,
                    root_ctx_session_id = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.root_ctx_session_id ELSE core_sessions.root_ctx_session_id END,
                    root_session_identity = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.root_session_identity ELSE core_sessions.root_session_identity END,
                    provider_session_id = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.provider_session_id ELSE core_sessions.provider_session_id END,
                    agent_type = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.agent_type ELSE core_sessions.agent_type END,
                    is_primary = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.is_primary ELSE core_sessions.is_primary END,
                    branch = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.branch ELSE core_sessions.branch END,
                    workspace = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.workspace ELSE core_sessions.workspace END,
                    cwd = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.cwd ELSE core_sessions.cwd END,
                    first_event_seq = MIN(core_sessions.first_event_seq, excluded.first_event_seq),
                    started_at_ms = CASE
                        WHEN core_sessions.started_at_ms IS NULL THEN excluded.started_at_ms
                        WHEN excluded.started_at_ms IS NULL THEN core_sessions.started_at_ms
                        ELSE MIN(core_sessions.started_at_ms, excluded.started_at_ms) END,
                    ended_at_ms = CASE
                        WHEN core_sessions.ended_at_ms IS NULL THEN excluded.ended_at_ms
                        WHEN excluded.ended_at_ms IS NULL THEN core_sessions.ended_at_ms
                        ELSE MAX(core_sessions.ended_at_ms, excluded.ended_at_ms) END
                 WHERE core_sessions.session_identity = excluded.session_identity
                   AND core_sessions.source_id = excluded.source_id",
            )?,
            event: conn.prepare(
                "INSERT INTO core_events (
                    ctx_event_id, event_identity, source_id, ctx_session_id, session_identity,
                    native_event_id_json, event_seq, event_type, role, occurred_at_ms,
                    parser_revision, normalization_revision, content_policy_revision,
                    content_policy_status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?,
            repository: conn.prepare(
                "INSERT INTO core_event_repositories (
                    ctx_event_id, binding_id, source_id, ctx_session_id,
                    logical_repository_id, checkout_id, worktree_id, git_object_format,
                    association_policy_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?,
            alias: conn.prepare(
                "INSERT INTO core_repository_aliases (
                    ctx_event_id, binding_id, ordinal, kind, host, namespace, name, remote_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?,
            evidence: conn.prepare(
                "INSERT INTO core_repository_evidence (
                    ctx_event_id, binding_id, ordinal, kind, confidence
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
            abstention: conn.prepare(
                "INSERT INTO core_repository_abstentions (
                    ctx_event_id, ordinal, source_id, ctx_session_id, evidence_kind,
                    reason, association_policy_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            file: conn.prepare(
                "INSERT INTO core_file_observations (
                    ctx_event_id, binding_id, ordinal, source_id, ctx_session_id,
                    relative_path, prior_relative_path, observation_kind, observed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?,
            vcs: conn.prepare(
                "INSERT INTO core_vcs_observations (
                    ctx_event_id, binding_id, ordinal, source_id, ctx_session_id,
                    observation_kind, object_format, object_id, reference_name,
                    relative_path, observed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?,
            vcs_parent: conn.prepare(
                "INSERT INTO core_vcs_parent_objects (
                    ctx_event_id, observation_ordinal, parent_ordinal, object_format, object_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
        })
    }

    fn insert_source(&mut self, metadata: &RelationalSourceMetadata) -> Result<()> {
        let source = &metadata.source;
        self.source.execute(params![
            source.identity().as_uuid().to_string(),
            source
                .identity()
                .encode_canonical()
                .map_err(contract_record_error)?
                .as_slice(),
            source.provider(),
            source.source_format(),
            source.schema_variant(),
            i64::from(source.provider_identity_version()),
            metadata.parser_revision,
            metadata.revision_digest.as_slice(),
            sqlite_i64(metadata.indexed_event_count, "source indexed events")?,
            metadata.health.as_str(),
        ])?;
        Ok(())
    }

    fn insert_core_record(&mut self, source_id: &str, record: &CoreRecord) -> Result<()> {
        self.insert_session(source_id, record)?;
        self.insert_event(source_id, record)?;
        self.insert_repository_metadata(source_id, record)
    }

    fn insert_session(&mut self, source_id: &str, record: &CoreRecord) -> Result<()> {
        let changed = self.session.execute(params![
            record.session_id.as_uuid().to_string(),
            identity_bytes(record.session_id)?,
            source_id,
            record.parent_session_id.map(|id| id.as_uuid().to_string()),
            record.parent_session_id.map(identity_bytes).transpose()?,
            record.root_session_id.as_uuid().to_string(),
            identity_bytes(record.root_session_id)?,
            record.provider_session_id,
            record.agent_type,
            i64::from(record.is_primary),
            record.branch,
            record.workspace,
            record.cwd,
            sqlite_u64_ordered_text(record.event_sequence),
            record.occurred_at_unix_ms,
        ])?;
        if changed == 0 {
            return invalid_record("session UUID collides with a different Core identity");
        }
        Ok(())
    }

    fn insert_event(&mut self, source_id: &str, record: &CoreRecord) -> Result<()> {
        let native_event_id = record
            .native_event_id
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.event.execute(params![
            record.event_id.as_uuid().to_string(),
            identity_bytes(record.event_id)?,
            source_id,
            record.session_id.as_uuid().to_string(),
            identity_bytes(record.session_id)?,
            native_event_id,
            sqlite_u64_ordered_text(record.event_sequence),
            record.event_type,
            record.role,
            record.occurred_at_unix_ms,
            record.parser_revision,
            i64::from(record.normalization_revision),
            i64::from(record.content.policy_revision),
            content_policy_status(&record.content.policy_status),
        ])?;
        Ok(())
    }

    fn insert_repository_metadata(&mut self, source_id: &str, record: &CoreRecord) -> Result<()> {
        let event_id = record.event_id.as_uuid().to_string();
        let session_id = record.session_id.as_uuid().to_string();
        for binding in &record.repository_bindings {
            self.repository.execute(params![
                event_id,
                binding.binding_id,
                source_id,
                session_id,
                binding.logical_repository_id,
                binding.checkout_id,
                binding.worktree_id,
                binding
                    .git_object_format
                    .as_ref()
                    .map(enum_text)
                    .transpose()?,
                i64::from(binding.association_policy_revision),
            ])?;
            for (ordinal, alias) in binding.aliases.iter().enumerate() {
                self.alias.execute(params![
                    event_id,
                    binding.binding_id,
                    sqlite_i64(ordinal as u64, "repository alias ordinal")?,
                    enum_text(&alias.kind)?,
                    alias.host,
                    alias.namespace.join("/"),
                    alias.name,
                    alias.remote_name,
                ])?;
            }
            for (ordinal, evidence) in binding.evidence.iter().enumerate() {
                self.evidence.execute(params![
                    event_id,
                    binding.binding_id,
                    sqlite_i64(ordinal as u64, "repository evidence ordinal")?,
                    enum_text(&evidence.kind)?,
                    enum_text(&evidence.confidence)?,
                ])?;
            }
        }
        for (ordinal, abstention) in record.repository_abstentions.iter().enumerate() {
            self.abstention.execute(params![
                event_id,
                sqlite_i64(ordinal as u64, "repository abstention ordinal")?,
                source_id,
                session_id,
                enum_text(&abstention.evidence_kind)?,
                enum_text(&abstention.reason)?,
                i64::from(abstention.association_policy_revision),
            ])?;
        }
        for (ordinal, observation) in record.repository_file_observations.iter().enumerate() {
            self.file.execute(params![
                event_id,
                observation.repository_binding_id,
                sqlite_i64(ordinal as u64, "file observation ordinal")?,
                source_id,
                session_id,
                observation.relative_path,
                observation.prior_relative_path,
                enum_text(&observation.kind)?,
                record.occurred_at_unix_ms,
            ])?;
        }
        for (ordinal, observation) in record.repository_vcs_observations.iter().enumerate() {
            self.vcs.execute(params![
                event_id,
                observation.repository_binding_id,
                sqlite_i64(ordinal as u64, "VCS observation ordinal")?,
                source_id,
                session_id,
                enum_text(&observation.kind)?,
                observation
                    .object_id
                    .as_ref()
                    .map(|id| enum_text(&id.format))
                    .transpose()?,
                observation.object_id.as_ref().map(|id| id.hex.as_str()),
                observation.reference,
                observation.relative_path,
                record.occurred_at_unix_ms,
            ])?;
            for (parent_ordinal, parent) in observation.parent_object_ids.iter().enumerate() {
                self.vcs_parent.execute(params![
                    event_id,
                    sqlite_i64(ordinal as u64, "VCS observation ordinal")?,
                    sqlite_i64(parent_ordinal as u64, "VCS parent ordinal")?,
                    enum_text(&parent.format)?,
                    parent.hex,
                ])?;
            }
        }
        Ok(())
    }
}

pub(super) fn projection_counts(conn: &Connection) -> Result<ProjectionCounts> {
    conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM core_sources),
            (SELECT COUNT(*) FROM core_sessions),
            (SELECT COUNT(*) FROM core_events),
            (SELECT COUNT(*) FROM core_event_repositories),
            (SELECT COUNT(*) FROM core_file_observations),
            (SELECT COUNT(*) FROM core_vcs_observations)",
        [],
        |row| {
            Ok(ProjectionCounts {
                sources: row.get(0)?,
                sessions: row.get(1)?,
                events: row.get(2)?,
                repository_bindings: row.get(3)?,
                file_observations: row.get(4)?,
                vcs_observations: row.get(5)?,
            })
        },
    )
    .map_err(Into::into)
}

pub(super) fn validate_projected_generation(
    conn: &Connection,
    generation: &ValidatedGeneration,
) -> Result<ProjectionCounts> {
    let counts = projection_counts(conn)?;
    if sqlite_u64(counts.events, "event count")? != generation.indexed_documents {
        return Err(RelationalProjectionError::GenerationEventCountMismatch {
            expected: generation.indexed_documents,
            projected: sqlite_u64(counts.events, "event count")?,
        });
    }
    if sqlite_u64(counts.sources, "source count")? != generation.sources.len() as u64 {
        return invalid_record("projected source count does not match the Core generation");
    }
    let invalid_sources: i64 = conn.query_row(
        "SELECT COUNT(*) FROM core_sources source
         WHERE source.indexed_event_count != (
             SELECT COUNT(*) FROM core_events event WHERE event.source_id = source.source_id
         )",
        [],
        |row| row.get(0),
    )?;
    let invalid_relationships: i64 = conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM core_events event
             WHERE NOT EXISTS (
                 SELECT 1 FROM core_sessions session
                 WHERE session.ctx_session_id = event.ctx_session_id
                   AND session.session_identity = event.session_identity
             ))
          + (SELECT COUNT(*) FROM core_sessions child
             WHERE child.parent_ctx_session_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM core_sessions parent
                   WHERE parent.ctx_session_id = child.parent_ctx_session_id
                     AND parent.session_identity = child.parent_session_identity
               ))
          + (SELECT COUNT(*) FROM core_sessions child
             WHERE NOT EXISTS (
                 SELECT 1 FROM core_sessions root
                 WHERE root.ctx_session_id = child.root_ctx_session_id
                   AND root.session_identity = child.root_session_identity
             ))",
        [],
        |row| row.get(0),
    )?;
    if invalid_sources != 0 || invalid_relationships != 0 {
        return invalid_record("projected Core identities or counts are incoherent");
    }
    Ok(counts)
}

fn identity_bytes(identity: StableEntityId) -> Result<Vec<u8>> {
    identity
        .encode_canonical()
        .map(|bytes| bytes.to_vec())
        .map_err(contract_record_error)
}

fn content_policy_status(status: &CoreContentPolicyStatus) -> &'static str {
    match status {
        CoreContentPolicyStatus::Selected => "selected",
        CoreContentPolicyStatus::Redacted { .. } => "redacted",
        CoreContentPolicyStatus::Omitted { .. } => "omitted",
    }
}

fn enum_text(value: &impl Serialize) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(value) => Ok(value),
        _ => invalid_record("Core enum did not serialize as text"),
    }
}

fn contract_record_error(error: ProjectionContractError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn core_record_error(error: ctx_history_core::CoreRecordError) -> RelationalProjectionError {
    RelationalProjectionError::InvalidRecord(error.to_string())
}

fn invalid_record<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidRecord(detail.into()))
}

fn stream_order<T>(detail: impl Into<String>) -> Result<T> {
    Err(RelationalProjectionError::InvalidStreamOrder(detail.into()))
}
