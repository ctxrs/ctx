use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    CoreContentPolicyStatus, CoreRecord, ProjectionContractError, RepositoryBinding,
    RepositoryVcsObservationKind as VcsKind, SourceKey,
};
use rusqlite::{params, Connection, OptionalExtension, Statement};
use serde::Serialize;
use uuid::Uuid;

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

#[derive(Debug, Default)]
pub(super) struct SourceProjectionSnapshot {
    pub(super) counts: ProjectionCounts,
    session_ids: BTreeSet<String>,
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
                let source_key = statements.insert_source(&metadata)?;
                current = Some(OpenSource {
                    source_id,
                    source: metadata.source,
                    source_key,
                    parser_revision: metadata.parser_revision,
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
                if record.parser_revision != open.parser_revision {
                    return invalid_record(
                        "event parser revision does not match its source metadata",
                    );
                }
                statements.insert_core_record(open.source_key, &record)?;
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
    source_key: i64,
    parser_revision: String,
    expected_events: u64,
    received_events: u64,
}

struct MaterializationStatements<'conn> {
    source: Statement<'conn>,
    session: Statement<'conn>,
    event: Statement<'conn>,
    repository_binding: Statement<'conn>,
    repository_binding_key: Statement<'conn>,
    event_repository: Statement<'conn>,
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
                    source_id, source_digest, provider, source_format, schema_variant,
                    provider_identity_version, parser_revision, revision_digest,
                    indexed_event_count, health
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 RETURNING source_key",
            )?,
            session: conn.prepare(
                "INSERT INTO core_sessions (
                    ctx_session_id, session_digest, source_key, parent_ctx_session_id,
                    parent_session_digest, root_ctx_session_id, root_session_digest,
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
                    parent_session_digest = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.parent_session_digest ELSE core_sessions.parent_session_digest END,
                    root_ctx_session_id = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.root_ctx_session_id ELSE core_sessions.root_ctx_session_id END,
                    root_session_digest = CASE
                        WHEN excluded.first_event_seq < core_sessions.first_event_seq
                        THEN excluded.root_session_digest ELSE core_sessions.root_session_digest END,
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
                 WHERE core_sessions.session_digest = excluded.session_digest
                   AND core_sessions.source_key = excluded.source_key
                 RETURNING session_key",
            )?,
            event: conn.prepare(
                "INSERT INTO core_events (
                    ctx_event_id, event_digest, session_key, native_event_id_json, event_seq,
                    event_type, role, occurred_at_ms, normalization_revision,
                    content_policy_revision, content_policy_status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 RETURNING event_key",
            )?,
            repository_binding: conn.prepare(
                "INSERT OR IGNORE INTO core_repository_bindings (
                    descriptor_key, binding_id, logical_repository_id, checkout_id,
                    worktree_id, git_object_format, association_policy_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            repository_binding_key: conn.prepare(
                "SELECT repository_binding_key
                 FROM core_repository_bindings
                 WHERE descriptor_key = ?1",
            )?,
            event_repository: conn.prepare(
                "INSERT INTO core_event_repositories (event_key, repository_binding_key)
                 VALUES (?1, ?2)",
            )?,
            alias: conn.prepare(
                "INSERT INTO core_repository_aliases (
                    repository_binding_key, ordinal, kind, host, namespace, name, remote_name
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            evidence: conn.prepare(
                "INSERT INTO core_repository_evidence (
                    repository_binding_key, ordinal, kind, confidence
                 ) VALUES (?1, ?2, ?3, ?4)",
            )?,
            abstention: conn.prepare(
                "INSERT INTO core_repository_abstentions (
                    event_key, ordinal, evidence_kind, reason, association_policy_revision
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
            file: conn.prepare(
                "INSERT INTO core_file_observations (
                    event_key, repository_binding_key, ordinal, relative_path,
                    prior_relative_path, observation_kind, observed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            vcs: conn.prepare(
                "INSERT INTO core_vcs_observations (
                    event_key, repository_binding_key, ordinal, observation_kind,
                    object_format, object_id, reference_name, relative_path, outcome_json,
                    observed_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?,
            vcs_parent: conn.prepare(
                "INSERT INTO core_vcs_parent_objects (
                    event_key, observation_ordinal, parent_ordinal, object_format, object_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
        })
    }

    fn insert_source(&mut self, metadata: &RelationalSourceMetadata) -> Result<i64> {
        let source = &metadata.source;
        self.source
            .query_row(
                params![
                    source.identity().as_uuid().to_string(),
                    source.identity().digest().as_slice(),
                    source.provider(),
                    source.source_format(),
                    source.schema_variant(),
                    i64::from(source.provider_identity_version()),
                    metadata.parser_revision,
                    metadata.revision_digest.as_slice(),
                    sqlite_i64(metadata.indexed_event_count, "source indexed events")?,
                    metadata.health.as_str(),
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn insert_core_record(&mut self, source_key: i64, record: &CoreRecord) -> Result<()> {
        let session_key = self.insert_session(source_key, record)?;
        let event_key = self.insert_event(session_key, record)?;
        self.insert_repository_metadata(event_key, record)
    }

    fn insert_session(&mut self, source_key: i64, record: &CoreRecord) -> Result<i64> {
        let session_key = self
            .session
            .query_row(
                params![
                    record.session_id.as_uuid().to_string(),
                    record.session_id.digest().as_slice(),
                    source_key,
                    record.parent_session_id.map(|id| id.as_uuid().to_string()),
                    record.parent_session_id.map(|id| id.digest().to_vec()),
                    record.root_session_id.as_uuid().to_string(),
                    record.root_session_id.digest().as_slice(),
                    record.provider_session_id,
                    record.agent_type,
                    i64::from(record.is_primary),
                    record.branch,
                    record.workspace,
                    record.cwd,
                    sqlite_u64_ordered_text(record.event_sequence),
                    record.occurred_at_unix_ms,
                ],
                |row| row.get(0),
            )
            .optional()?;
        session_key.ok_or_else(|| {
            RelationalProjectionError::InvalidRecord(
                "session UUID collides with a different Core identity".to_owned(),
            )
        })
    }

    fn insert_event(&mut self, session_key: i64, record: &CoreRecord) -> Result<i64> {
        let native_event_id = record
            .native_event_id
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.event
            .query_row(
                params![
                    record.event_id.as_uuid().to_string(),
                    record.event_id.digest().as_slice(),
                    session_key,
                    native_event_id,
                    sqlite_u64_ordered_text(record.event_sequence),
                    record.event_type,
                    record.role,
                    record.occurred_at_unix_ms,
                    i64::from(record.normalization_revision),
                    i64::from(record.content.policy_revision),
                    content_policy_status(&record.content.policy_status),
                ],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    fn insert_repository_metadata(&mut self, event_key: i64, record: &CoreRecord) -> Result<()> {
        let mut binding_keys = BTreeMap::new();
        for binding in &record.repository_bindings {
            let descriptor_key = repository_descriptor_key(binding)?;
            let git_object_format = binding
                .git_object_format
                .as_ref()
                .map(enum_text)
                .transpose()?;
            let inserted = self.repository_binding.execute(params![
                descriptor_key,
                binding.binding_id,
                binding.logical_repository_id,
                binding.checkout_id,
                binding.worktree_id,
                git_object_format,
                i64::from(binding.association_policy_revision),
            ])?;
            let repository_binding_key = self
                .repository_binding_key
                .query_row([&descriptor_key], |row| row.get::<_, i64>(0))?;
            if inserted != 0 {
                for (ordinal, alias) in binding.aliases.iter().enumerate() {
                    self.alias.execute(params![
                        repository_binding_key,
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
                        repository_binding_key,
                        sqlite_i64(ordinal as u64, "repository evidence ordinal")?,
                        enum_text(&evidence.kind)?,
                        enum_text(&evidence.confidence)?,
                    ])?;
                }
            }
            self.event_repository
                .execute(params![event_key, repository_binding_key])?;
            binding_keys.insert(binding.binding_id.clone(), repository_binding_key);
        }
        for (ordinal, abstention) in record.repository_abstentions.iter().enumerate() {
            self.abstention.execute(params![
                event_key,
                sqlite_i64(ordinal as u64, "repository abstention ordinal")?,
                enum_text(&abstention.evidence_kind)?,
                enum_text(&abstention.reason)?,
                i64::from(abstention.association_policy_revision),
            ])?;
        }
        for (ordinal, observation) in record.repository_file_observations.iter().enumerate() {
            let repository_binding_key = binding_key(
                &binding_keys,
                &observation.repository_binding_id,
                "file observation",
            )?;
            self.file.execute(params![
                event_key,
                repository_binding_key,
                sqlite_i64(ordinal as u64, "file observation ordinal")?,
                observation.relative_path,
                observation.prior_relative_path,
                enum_text(&observation.kind)?,
                record.occurred_at_unix_ms,
            ])?;
        }
        for (ordinal, observation) in record.repository_vcs_observations.iter().enumerate() {
            let repository_binding_key = binding_key(
                &binding_keys,
                &observation.repository_binding_id,
                "VCS observation",
            )?;
            let (observation_kind, outcome_json) = vcs_observation_payload(&observation.kind)?;
            self.vcs.execute(params![
                event_key,
                repository_binding_key,
                sqlite_i64(ordinal as u64, "VCS observation ordinal")?,
                observation_kind,
                observation
                    .object_id
                    .as_ref()
                    .map(|id| enum_text(&id.format))
                    .transpose()?,
                observation.object_id.as_ref().map(|id| id.hex.as_str()),
                observation.reference,
                observation.relative_path,
                outcome_json,
                record.occurred_at_unix_ms,
            ])?;
            for (parent_ordinal, parent) in observation.parent_object_ids.iter().enumerate() {
                self.vcs_parent.execute(params![
                    event_key,
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

fn repository_descriptor_key(binding: &RepositoryBinding) -> Result<String> {
    serde_json::to_string(&(
        binding.binding_id.as_str(),
        binding.logical_repository_id.as_str(),
        binding.checkout_id.as_deref(),
        binding.worktree_id.as_deref(),
        &binding.aliases,
        &binding.git_object_format,
        &binding.evidence,
        binding.association_policy_revision,
    ))
    .map_err(Into::into)
}

fn binding_key(
    binding_keys: &BTreeMap<String, i64>,
    binding_id: &str,
    relation: &'static str,
) -> Result<i64> {
    binding_keys.get(binding_id).copied().ok_or_else(|| {
        RelationalProjectionError::InvalidRecord(format!(
            "{relation} references absent repository binding {binding_id}"
        ))
    })
}

pub(super) fn prune_orphan_repository_bindings(conn: &Connection) -> Result<()> {
    conn.execute(
        "DELETE FROM core_repository_bindings
         WHERE NOT EXISTS (
             SELECT 1
             FROM core_event_repositories AS event_repository
                  INDEXED BY core_event_repositories_binding
             WHERE event_repository.repository_binding_key =
                   core_repository_bindings.repository_binding_key
         )",
        [],
    )?;
    Ok(())
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

pub(super) fn stored_projection_counts(conn: &Connection) -> Result<ProjectionCounts> {
    conn.query_row(
        "SELECT source_count, session_count, event_count, repository_binding_count,
                file_observation_count, vcs_observation_count
         FROM core_relational_state
         WHERE singleton = 1",
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

pub(super) fn source_projection_snapshot(
    conn: &Connection,
    source_ids: &BTreeSet<String>,
) -> Result<SourceProjectionSnapshot> {
    let mut snapshot = SourceProjectionSnapshot::default();
    let mut source = conn.prepare(
        "SELECT source_key
         FROM core_sources
         WHERE source_id = ?1",
    )?;
    let mut sessions = conn.prepare(
        "SELECT ctx_session_id
         FROM core_sessions INDEXED BY core_sessions_source
         WHERE source_key = ?1",
    )?;
    let mut event_count = conn.prepare(
        "SELECT COUNT(*)
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         WHERE session.source_key = ?1",
    )?;
    let mut repository_count = conn.prepare(
        "SELECT COUNT(*)
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         JOIN core_event_repositories AS repository
           ON repository.event_key = event.event_key
         WHERE session.source_key = ?1",
    )?;
    let mut file_count = conn.prepare(
        "SELECT COUNT(*)
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         JOIN core_file_observations AS file ON file.event_key = event.event_key
         WHERE session.source_key = ?1",
    )?;
    let mut vcs_count = conn.prepare(
        "SELECT COUNT(*)
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         JOIN core_vcs_observations AS vcs ON vcs.event_key = event.event_key
         WHERE session.source_key = ?1",
    )?;

    for source_id in source_ids {
        let source_key = source
            .query_row([source_id], |row| row.get::<_, i64>(0))
            .optional()?;
        let Some(source_key) = source_key else {
            continue;
        };
        snapshot.counts.sources = checked_add_count(snapshot.counts.sources, 1, "source count")?;

        let mut rows = sessions.query([source_key])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            if !snapshot.session_ids.insert(session_id) {
                return invalid_record("a session identity belongs to multiple affected sources");
            }
            snapshot.counts.sessions =
                checked_add_count(snapshot.counts.sessions, 1, "session count")?;
        }
        snapshot.counts.events = checked_add_count(
            snapshot.counts.events,
            event_count.query_row([source_key], |row| row.get(0))?,
            "event count",
        )?;
        snapshot.counts.repository_bindings = checked_add_count(
            snapshot.counts.repository_bindings,
            repository_count.query_row([source_key], |row| row.get(0))?,
            "repository binding count",
        )?;
        snapshot.counts.file_observations = checked_add_count(
            snapshot.counts.file_observations,
            file_count.query_row([source_key], |row| row.get(0))?,
            "file observation count",
        )?;
        snapshot.counts.vcs_observations = checked_add_count(
            snapshot.counts.vcs_observations,
            vcs_count.query_row([source_key], |row| row.get(0))?,
            "VCS observation count",
        )?;
    }
    Ok(snapshot)
}

pub(super) fn validate_projected_generation(
    conn: &Connection,
    generation: &ValidatedGeneration,
) -> Result<ProjectionCounts> {
    let counts = projection_counts(conn)?;
    validate_generation_counts(counts, generation)?;

    let invalid_foreign_keys: i64 =
        conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    let invalid_sources: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM core_sources AS source
         WHERE source.indexed_event_count != (
             SELECT COUNT(*)
             FROM core_sessions AS session INDEXED BY core_sessions_source
             JOIN core_events AS event INDEXED BY core_events_session_seq
               ON event.session_key = session.session_key
             WHERE session.source_key = source.source_key
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid_foreign_keys != 0 || invalid_sources != 0 {
        return invalid_record("projected Core ownership or counts are incoherent");
    }
    validate_identity_rows(conn, "SELECT source_id, source_digest FROM core_sources")?;
    validate_identity_rows(
        conn,
        "SELECT ctx_session_id, session_digest FROM core_sessions",
    )?;
    validate_identity_rows(conn, "SELECT ctx_event_id, event_digest FROM core_events")?;
    validate_session_relationships(conn)?;
    Ok(counts)
}

pub(super) fn validate_incremental_projected_generation(
    conn: &Connection,
    generation: &ValidatedGeneration,
    prior_counts: ProjectionCounts,
    old: &SourceProjectionSnapshot,
    new: &SourceProjectionSnapshot,
    changed_source_ids: &BTreeSet<String>,
) -> Result<ProjectionCounts> {
    let counts = ProjectionCounts {
        sources: replace_count(
            prior_counts.sources,
            old.counts.sources,
            new.counts.sources,
            "source count",
        )?,
        sessions: replace_count(
            prior_counts.sessions,
            old.counts.sessions,
            new.counts.sessions,
            "session count",
        )?,
        events: replace_count(
            prior_counts.events,
            old.counts.events,
            new.counts.events,
            "event count",
        )?,
        repository_bindings: replace_count(
            prior_counts.repository_bindings,
            old.counts.repository_bindings,
            new.counts.repository_bindings,
            "repository binding count",
        )?,
        file_observations: replace_count(
            prior_counts.file_observations,
            old.counts.file_observations,
            new.counts.file_observations,
            "file observation count",
        )?,
        vcs_observations: replace_count(
            prior_counts.vcs_observations,
            old.counts.vcs_observations,
            new.counts.vcs_observations,
            "VCS observation count",
        )?,
    };
    validate_generation_counts(counts, generation)?;
    validate_changed_rows(conn, changed_source_ids)?;

    let affected_session_ids = old
        .session_ids
        .union(&new.session_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    validate_session_reverse_lineage(conn, &affected_session_ids)?;
    Ok(counts)
}

fn validate_generation_counts(
    counts: ProjectionCounts,
    generation: &ValidatedGeneration,
) -> Result<()> {
    let projected_events = sqlite_u64(counts.events, "event count")?;
    if projected_events != generation.indexed_documents {
        return Err(RelationalProjectionError::GenerationEventCountMismatch {
            expected: generation.indexed_documents,
            projected: projected_events,
        });
    }
    if sqlite_u64(counts.sources, "source count")? != generation.sources.len() as u64 {
        return invalid_record("projected source count does not match the Core generation");
    }
    Ok(())
}

fn validate_changed_rows(conn: &Connection, source_ids: &BTreeSet<String>) -> Result<()> {
    let mut source = conn.prepare(
        "SELECT source_key, source_id, source_digest, indexed_event_count
         FROM core_sources
         WHERE source_id = ?1",
    )?;
    let mut sessions = conn.prepare(
        "SELECT child.ctx_session_id, child.session_digest,
                child.parent_ctx_session_id, child.parent_session_digest,
                parent.session_digest,
                child.root_ctx_session_id, child.root_session_digest,
                root.session_digest
         FROM core_sessions AS child INDEXED BY core_sessions_source
         LEFT JOIN core_sessions AS parent
           ON parent.ctx_session_id = child.parent_ctx_session_id
         LEFT JOIN core_sessions AS root
           ON root.ctx_session_id = child.root_ctx_session_id
         WHERE child.source_key = ?1",
    )?;
    let mut events = conn.prepare(
        "SELECT event.ctx_event_id, event.event_digest
         FROM core_sessions AS session INDEXED BY core_sessions_source
         JOIN core_events AS event INDEXED BY core_events_session_seq
           ON event.session_key = session.session_key
         WHERE session.source_key = ?1",
    )?;
    for source_id in source_ids {
        let Some((source_key, stored_source_id, source_digest, indexed_events)) = source
            .query_row([source_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .optional()?
        else {
            return invalid_record(format!("changed source {source_id} was not materialized"));
        };
        validate_identity_pair(&stored_source_id, &source_digest)?;

        let mut session_rows = sessions.query([source_key])?;
        while let Some(row) = session_rows.next()? {
            let session_id: String = row.get(0)?;
            let session_digest: Vec<u8> = row.get(1)?;
            validate_identity_pair(&session_id, &session_digest)?;
            validate_lineage_reference(row.get(2)?, row.get(3)?, row.get(4)?, "parent session")?;
            validate_lineage_reference(row.get(5)?, row.get(6)?, row.get(7)?, "root session")?;
        }

        let mut event_rows = events.query([source_key])?;
        let mut actual_events = 0_i64;
        while let Some(row) = event_rows.next()? {
            validate_identity_pair(&row.get::<_, String>(0)?, &row.get::<_, Vec<u8>>(1)?)?;
            actual_events = checked_add_count(actual_events, 1, "event count")?;
        }
        if actual_events != indexed_events {
            return invalid_record("changed source event count does not match Core metadata");
        }
    }
    Ok(())
}

fn validate_identity_rows(conn: &Connection, sql: &str) -> Result<()> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        validate_identity_pair(&row.get::<_, String>(0)?, &row.get::<_, Vec<u8>>(1)?)?;
    }
    Ok(())
}

fn validate_identity_pair(id: &str, digest: &[u8]) -> Result<()> {
    let uuid = Uuid::parse_str(id)
        .map_err(|_| RelationalProjectionError::InvalidRecord("stored UUID is malformed".into()))?;
    if digest.len() != 32 {
        return invalid_record("stored identity digest is malformed");
    }
    let mut expected_uuid = [0_u8; 16];
    expected_uuid.copy_from_slice(&digest[..16]);
    expected_uuid[6] = 0x80 | (expected_uuid[6] & 0x0f);
    expected_uuid[8] = 0x80 | (expected_uuid[8] & 0x3f);
    if Uuid::from_bytes(expected_uuid) != uuid {
        return invalid_record("stored UUID collides with a different full identity digest");
    }
    Ok(())
}

fn validate_session_relationships(conn: &Connection) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT child.parent_ctx_session_id, child.parent_session_digest,
                parent.session_digest
         FROM core_sessions AS child
         LEFT JOIN core_sessions AS parent
           ON parent.ctx_session_id = child.parent_ctx_session_id
         WHERE child.parent_ctx_session_id IS NOT NULL
            OR child.parent_session_digest IS NOT NULL
         UNION ALL
         SELECT child.root_ctx_session_id, child.root_session_digest,
                root.session_digest
         FROM core_sessions AS child
         LEFT JOIN core_sessions AS root
           ON root.ctx_session_id = child.root_ctx_session_id",
    )?;
    let rows = statement.query([])?;
    validate_lineage_rows(rows)
}

fn validate_session_reverse_lineage(
    conn: &Connection,
    affected_session_ids: &BTreeSet<String>,
) -> Result<()> {
    let mut parent = conn.prepare(
        "SELECT child.parent_ctx_session_id, child.parent_session_digest,
                target.session_digest
         FROM core_sessions AS child INDEXED BY core_sessions_parent
         LEFT JOIN core_sessions AS target
           ON target.ctx_session_id = child.parent_ctx_session_id
         WHERE child.parent_ctx_session_id = ?1",
    )?;
    let mut root = conn.prepare(
        "SELECT child.root_ctx_session_id, child.root_session_digest,
                target.session_digest
         FROM core_sessions AS child INDEXED BY core_sessions_root
         LEFT JOIN core_sessions AS target
           ON target.ctx_session_id = child.root_ctx_session_id
         WHERE child.root_ctx_session_id = ?1",
    )?;
    for session_id in affected_session_ids {
        validate_lineage_rows(parent.query([session_id])?)?;
        validate_lineage_rows(root.query([session_id])?)?;
    }
    Ok(())
}

fn validate_lineage_rows(mut rows: rusqlite::Rows<'_>) -> Result<()> {
    while let Some(row) = rows.next()? {
        validate_lineage_reference(
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<Vec<u8>>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
            "session lineage",
        )?;
    }
    Ok(())
}

fn validate_lineage_reference(
    target_id: Option<String>,
    reference_digest: Option<Vec<u8>>,
    target_digest: Option<Vec<u8>>,
    relation: &'static str,
) -> Result<()> {
    if target_id.is_none() && reference_digest.is_none() && target_digest.is_none() {
        return Ok(());
    }
    let (Some(target_id), Some(reference_digest)) = (target_id, reference_digest) else {
        return invalid_record(format!("projected {relation} identity is incomplete"));
    };
    validate_identity_pair(&target_id, &reference_digest)?;
    if target_digest
        .as_deref()
        .is_some_and(|target| target != reference_digest)
    {
        return invalid_record(format!("projected {relation} identity is incoherent"));
    }
    Ok(())
}

fn checked_add_count(left: i64, right: i64, label: &'static str) -> Result<i64> {
    left.checked_add(right)
        .ok_or(RelationalProjectionError::CountOverflow(label))
}

fn replace_count(total: i64, old: i64, new: i64, label: &'static str) -> Result<i64> {
    if total < old {
        return invalid_record(format!(
            "stored {label} is smaller than the affected-source count"
        ));
    }
    let retained = total
        .checked_sub(old)
        .ok_or(RelationalProjectionError::CountOverflow(label))?;
    checked_add_count(retained, new, label)
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

fn vcs_observation_payload(kind: &VcsKind) -> Result<(&'static str, Option<String>)> {
    let (kind, outcome) = match kind {
        VcsKind::Head => ("head", None),
        VcsKind::Commit => ("commit", None),
        VcsKind::Branch => ("branch", None),
        VcsKind::Worktree => ("worktree", None),
        VcsKind::Change => ("change", None),
        VcsKind::Reference => ("reference", None),
        VcsKind::Outcome(outcome) => ("outcome", Some(serde_json::to_string(outcome.as_ref())?)),
    };
    Ok((kind, outcome))
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
