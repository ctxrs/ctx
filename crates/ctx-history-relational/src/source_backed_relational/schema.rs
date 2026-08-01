use rusqlite::{Connection, OptionalExtension};

use super::{
    RelationalProjectionError, Result, RELATIONAL_PROJECTION_CONTRACT_VERSION,
    RELATIONAL_PROJECTION_SCHEMA_VERSION,
};

pub(super) const SCHEMA_SQL: &str = r#"
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS core_relational_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    contract_version INTEGER NOT NULL,
    build_generation INTEGER NOT NULL,
    active_generation_id TEXT,
    active_manifest_version INTEGER,
    active_core_record_version INTEGER,
    active_core_record_contract_fingerprint TEXT,
    active_lexical_schema_version INTEGER,
    active_policy_schema_hash TEXT,
    active_materializer_revision INTEGER,
    target_generation_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('empty', 'ready', 'behind')),
    source_count INTEGER NOT NULL,
    session_count INTEGER NOT NULL,
    event_count INTEGER NOT NULL,
    repository_binding_count INTEGER NOT NULL,
    file_observation_count INTEGER NOT NULL,
    vcs_observation_count INTEGER NOT NULL,
    last_error TEXT
);

CREATE TABLE IF NOT EXISTS core_sources (
    source_key INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL UNIQUE,
    source_digest BLOB NOT NULL UNIQUE CHECK (length(source_digest) = 32),
    provider TEXT NOT NULL,
    source_format TEXT NOT NULL,
    schema_variant TEXT NOT NULL,
    provider_identity_version INTEGER NOT NULL,
    parser_revision TEXT NOT NULL,
    revision_digest BLOB NOT NULL,
    indexed_event_count INTEGER NOT NULL,
    health TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS core_sessions (
    session_key INTEGER PRIMARY KEY,
    ctx_session_id TEXT NOT NULL UNIQUE,
    session_digest BLOB NOT NULL UNIQUE CHECK (length(session_digest) = 32),
    source_key INTEGER NOT NULL REFERENCES core_sources(source_key) ON DELETE CASCADE,
    parent_ctx_session_id TEXT,
    parent_session_digest BLOB CHECK (
        parent_session_digest IS NULL OR length(parent_session_digest) = 32
    ),
    root_ctx_session_id TEXT NOT NULL,
    root_session_digest BLOB NOT NULL CHECK (length(root_session_digest) = 32),
    provider_session_id TEXT,
    agent_type TEXT NOT NULL,
    is_primary INTEGER NOT NULL,
    branch TEXT,
    workspace TEXT,
    cwd TEXT,
    first_event_seq TEXT NOT NULL
        CHECK (length(first_event_seq) = 20 AND first_event_seq NOT GLOB '*[^0-9]*'),
    started_at_ms INTEGER,
    ended_at_ms INTEGER,
    health TEXT NOT NULL,
    CHECK (
        (parent_ctx_session_id IS NULL AND parent_session_digest IS NULL)
        OR (parent_ctx_session_id IS NOT NULL AND parent_session_digest IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS core_sessions_source ON core_sessions(source_key);
CREATE INDEX IF NOT EXISTS core_sessions_parent
ON core_sessions(parent_ctx_session_id, parent_session_digest);
CREATE INDEX IF NOT EXISTS core_sessions_root
ON core_sessions(root_ctx_session_id, root_session_digest);

CREATE TABLE IF NOT EXISTS core_events (
    event_key INTEGER PRIMARY KEY,
    ctx_event_id TEXT NOT NULL UNIQUE,
    event_digest BLOB NOT NULL UNIQUE CHECK (length(event_digest) = 32),
    session_key INTEGER NOT NULL REFERENCES core_sessions(session_key) ON DELETE CASCADE,
    native_event_id_json TEXT,
    event_seq TEXT NOT NULL
        CHECK (length(event_seq) = 20 AND event_seq NOT GLOB '*[^0-9]*'),
    event_type TEXT NOT NULL,
    role TEXT,
    occurred_at_ms INTEGER,
    normalization_revision INTEGER NOT NULL,
    content_policy_revision INTEGER NOT NULL,
    content_policy_status TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS core_events_session_seq
ON core_events(session_key, event_seq, ctx_event_id);

CREATE TABLE IF NOT EXISTS core_repository_bindings (
    repository_binding_key INTEGER PRIMARY KEY,
    descriptor_key TEXT NOT NULL UNIQUE,
    binding_id TEXT NOT NULL,
    logical_repository_id TEXT NOT NULL,
    checkout_id TEXT,
    worktree_id TEXT,
    git_object_format TEXT,
    association_policy_revision INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS core_repository_bindings_logical
ON core_repository_bindings(logical_repository_id, repository_binding_key);

CREATE TABLE IF NOT EXISTS core_event_repositories (
    event_key INTEGER NOT NULL REFERENCES core_events(event_key) ON DELETE CASCADE,
    repository_binding_key INTEGER NOT NULL
        REFERENCES core_repository_bindings(repository_binding_key) ON DELETE RESTRICT,
    PRIMARY KEY (event_key, repository_binding_key)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS core_event_repositories_binding
ON core_event_repositories(repository_binding_key, event_key);

CREATE TABLE IF NOT EXISTS core_repository_aliases (
    repository_binding_key INTEGER NOT NULL
        REFERENCES core_repository_bindings(repository_binding_key) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    host TEXT NOT NULL,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    remote_name TEXT,
    PRIMARY KEY (repository_binding_key, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS core_repository_evidence (
    repository_binding_key INTEGER NOT NULL
        REFERENCES core_repository_bindings(repository_binding_key) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    confidence TEXT NOT NULL,
    PRIMARY KEY (repository_binding_key, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS core_repository_abstentions (
    event_key INTEGER NOT NULL REFERENCES core_events(event_key) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    evidence_kind TEXT NOT NULL,
    reason TEXT NOT NULL,
    association_policy_revision INTEGER NOT NULL,
    PRIMARY KEY (event_key, ordinal)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS core_file_observations (
    event_key INTEGER NOT NULL,
    repository_binding_key INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    relative_path TEXT NOT NULL,
    prior_relative_path TEXT,
    observation_kind TEXT NOT NULL,
    observed_at_ms INTEGER,
    PRIMARY KEY (event_key, ordinal),
    FOREIGN KEY (event_key, repository_binding_key)
        REFERENCES core_event_repositories(event_key, repository_binding_key) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS core_file_observations_repository_path
ON core_file_observations(repository_binding_key, relative_path);

CREATE TABLE IF NOT EXISTS core_vcs_observations (
    event_key INTEGER NOT NULL,
    repository_binding_key INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    observation_kind TEXT NOT NULL,
    object_format TEXT,
    object_id TEXT,
    reference_name TEXT,
    relative_path TEXT,
    outcome_json TEXT,
    observed_at_ms INTEGER,
    PRIMARY KEY (event_key, ordinal),
    FOREIGN KEY (event_key, repository_binding_key)
        REFERENCES core_event_repositories(event_key, repository_binding_key) ON DELETE CASCADE
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS core_vcs_observations_repository
ON core_vcs_observations(repository_binding_key, observation_kind);

CREATE TABLE IF NOT EXISTS core_vcs_parent_objects (
    event_key INTEGER NOT NULL,
    observation_ordinal INTEGER NOT NULL,
    parent_ordinal INTEGER NOT NULL,
    object_format TEXT NOT NULL,
    object_id TEXT NOT NULL,
    PRIMARY KEY (event_key, observation_ordinal, parent_ordinal),
    FOREIGN KEY (event_key, observation_ordinal)
        REFERENCES core_vcs_observations(event_key, ordinal) ON DELETE CASCADE
) WITHOUT ROWID;

DROP VIEW IF EXISTS ctx_sessions;
CREATE VIEW ctx_sessions AS
SELECT
    s.ctx_session_id,
    s.parent_ctx_session_id,
    s.root_ctx_session_id,
    src.source_id,
    src.provider,
    src.source_format,
    s.provider_session_id,
    s.agent_type,
    s.is_primary,
    s.branch,
    s.workspace,
    s.cwd,
    s.started_at_ms,
    s.ended_at_ms,
    s.health
FROM core_sessions s
JOIN core_sources src ON src.source_key = s.source_key;

DROP VIEW IF EXISTS ctx_events;
CREATE VIEW ctx_events AS
SELECT
    e.ctx_event_id,
    s.ctx_session_id,
    src.source_id,
    src.provider,
    src.source_format,
    s.provider_session_id,
    e.native_event_id_json,
    e.event_seq,
    e.event_type,
    e.role,
    e.occurred_at_ms,
    src.parser_revision,
    e.normalization_revision,
    e.content_policy_revision,
    e.content_policy_status,
    s.branch,
    s.workspace,
    s.cwd
FROM core_events e
JOIN core_sessions s ON s.session_key = e.session_key
JOIN core_sources src ON src.source_key = s.source_key;

DROP VIEW IF EXISTS ctx_files_touched;
CREATE VIEW ctx_files_touched AS
SELECT
    e.ctx_event_id || ':' || f.ordinal AS ctx_file_touch_id,
    e.ctx_event_id,
    s.ctx_session_id,
    src.source_id,
    src.provider,
    src.source_format,
    r.binding_id AS repository_binding_id,
    r.logical_repository_id,
    f.relative_path AS path,
    f.prior_relative_path AS old_path,
    f.observation_kind,
    f.observed_at_ms
FROM core_file_observations f
JOIN core_events e ON e.event_key = f.event_key
JOIN core_sessions s ON s.session_key = e.session_key
JOIN core_sources src ON src.source_key = s.source_key
JOIN core_repository_bindings r
  ON r.repository_binding_key = f.repository_binding_key;

DROP VIEW IF EXISTS ctx_sources;
CREATE VIEW ctx_sources AS
SELECT
    src.source_id,
    src.provider,
    src.source_format,
    src.schema_variant,
    src.provider_identity_version,
    src.parser_revision,
    src.indexed_event_count,
    src.health
FROM core_sources src;

DROP VIEW IF EXISTS ctx_repositories;
CREATE VIEW ctx_repositories AS
SELECT
    e.ctx_event_id,
    s.ctx_session_id,
    r.binding_id AS repository_binding_id,
    r.logical_repository_id,
    r.checkout_id,
    r.worktree_id,
    r.git_object_format,
    r.association_policy_revision
FROM core_event_repositories er
JOIN core_events e ON e.event_key = er.event_key
JOIN core_sessions s ON s.session_key = e.session_key
JOIN core_repository_bindings r
  ON r.repository_binding_key = er.repository_binding_key;

DROP VIEW IF EXISTS ctx_vcs_observations;
CREATE VIEW ctx_vcs_observations AS
SELECT
    e.ctx_event_id,
    s.ctx_session_id,
    r.binding_id AS repository_binding_id,
    r.logical_repository_id,
    v.observation_kind,
    v.object_format,
    v.object_id,
    v.reference_name,
    v.relative_path,
    v.outcome_json,
    v.observed_at_ms
FROM core_vcs_observations v
JOIN core_events e ON e.event_key = v.event_key
JOIN core_sessions s ON s.session_key = e.session_key
JOIN core_repository_bindings r
  ON r.repository_binding_key = v.repository_binding_key;

DROP VIEW IF EXISTS ctx_repository_abstentions;
CREATE VIEW ctx_repository_abstentions AS
SELECT
    e.ctx_event_id,
    s.ctx_session_id,
    a.evidence_kind,
    a.reason,
    a.association_policy_revision
FROM core_repository_abstentions a
JOIN core_events e ON e.event_key = a.event_key
JOIN core_sessions s ON s.session_key = e.session_key;

DROP VIEW IF EXISTS ctx_projection_metadata;
CREATE VIEW ctx_projection_metadata AS
SELECT
    schema_version,
    contract_version,
    active_materializer_revision AS materializer_revision,
    build_generation,
    active_generation_id AS core_generation_id,
    target_generation_id AS target_core_generation_id,
    status,
    source_count,
    session_count,
    event_count,
    repository_binding_count,
    file_observation_count AS file_touch_count,
    vcs_observation_count,
    last_error,
    active_manifest_version AS core_manifest_version,
    active_core_record_version AS core_record_version,
    active_core_record_contract_fingerprint AS core_record_contract_fingerprint,
    active_lexical_schema_version AS core_lexical_schema_version,
    active_policy_schema_hash AS core_policy_schema_hash
FROM core_relational_state
WHERE singleton = 1;
"#;

pub(super) fn initialize(conn: &Connection) -> Result<()> {
    if let Some((schema_version, contract_version)) = existing_schema_versions(conn)? {
        if schema_version != i64::from(RELATIONAL_PROJECTION_SCHEMA_VERSION)
            || contract_version != i64::from(RELATIONAL_PROJECTION_CONTRACT_VERSION)
        {
            return Err(RelationalProjectionError::UnsupportedSchema {
                schema_version,
                contract_version,
            });
        }
    }
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute(
        "INSERT OR IGNORE INTO core_relational_state (
            singleton, schema_version, contract_version, build_generation, status,
            source_count, session_count, event_count, repository_binding_count,
            file_observation_count, vcs_observation_count
         ) VALUES (1, ?1, ?2, 0, 'empty', 0, 0, 0, 0, 0, 0)",
        [
            i64::from(RELATIONAL_PROJECTION_SCHEMA_VERSION),
            i64::from(RELATIONAL_PROJECTION_CONTRACT_VERSION),
        ],
    )?;
    verify(conn)
}

pub(super) fn verify(conn: &Connection) -> Result<()> {
    let state = existing_schema_versions(conn)?.ok_or(RelationalProjectionError::MissingSchema)?;
    if state.0 != i64::from(RELATIONAL_PROJECTION_SCHEMA_VERSION)
        || state.1 != i64::from(RELATIONAL_PROJECTION_CONTRACT_VERSION)
    {
        return Err(RelationalProjectionError::UnsupportedSchema {
            schema_version: state.0,
            contract_version: state.1,
        });
    }
    for view in [
        "ctx_sessions",
        "ctx_events",
        "ctx_files_touched",
        "ctx_sources",
        "ctx_repositories",
        "ctx_vcs_observations",
        "ctx_repository_abstentions",
        "ctx_projection_metadata",
    ] {
        let exists = conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type = 'view' AND name = ?1",
                [view],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(RelationalProjectionError::MissingStableView(
                view.to_owned(),
            ));
        }
    }
    verify_active_generation(conn)
}

fn existing_schema_versions(conn: &Connection) -> Result<Option<(i64, i64)>> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            ["core_relational_state"],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !exists {
        return Ok(None);
    }
    conn.query_row(
        "SELECT schema_version, contract_version
         FROM core_relational_state WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()?
    .map(Some)
    .ok_or(RelationalProjectionError::MissingSchema)
}

fn verify_active_generation(conn: &Connection) -> Result<()> {
    let row = conn.query_row(
        "SELECT status, active_generation_id, active_manifest_version,
                active_core_record_version, active_core_record_contract_fingerprint,
                active_lexical_schema_version, active_policy_schema_hash,
                active_materializer_revision
         FROM core_relational_state WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<i64>>(7)?,
            ))
        },
    )?;
    let evidence = row.2.is_some()
        || row.3.is_some()
        || row.4.is_some()
        || row.5.is_some()
        || row.6.is_some()
        || row.7.is_some();
    let Some(generation_id) = row.1 else {
        if evidence || row.0 == "ready" {
            return Err(RelationalProjectionError::IncompatibleState(
                "active generation evidence is incomplete".to_owned(),
            ));
        }
        return Ok(());
    };
    if generation_id.len() != 64
        || [row.2, row.3, row.5, row.7]
            .into_iter()
            .any(|value| value.is_none())
        || row.4.as_deref().is_none_or(str::is_empty)
        || row.6.as_deref().is_none_or(str::is_empty)
    {
        return Err(RelationalProjectionError::IncompatibleState(
            "active generation receipt is incomplete".to_owned(),
        ));
    }
    Ok(())
}
