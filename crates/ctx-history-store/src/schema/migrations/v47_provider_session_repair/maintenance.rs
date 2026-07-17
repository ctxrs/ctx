use std::time::{Duration, Instant};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{Result, Store, StoreError};

use super::maintenance_events;
use super::maintenance_sessions;
use super::PROVIDER_SESSION_REPAIR_SCHEMA_SQL;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RepairPhase {
    Discover,
    ComponentSeed,
    ComponentExpand,
    ComponentKeysCleanup,
    MergeSession,
    StageEvents,
    ApplyEvents,
    DuplicateEventLinks,
    DuplicateEventFiles,
    DuplicateEventAliases,
    DuplicateEventFinish,
    DuplicateSessions,
    DuplicateSessionLinks,
    DuplicateSessionParents,
    DuplicateSessionRoots,
    DuplicateSessionEdgesFrom,
    DuplicateSessionEdgesTo,
    DuplicateSessionRuns,
    DuplicateSessionSummaries,
    DuplicateSessionEvents,
    DuplicateSessionLookup,
    DuplicateSessionAliases,
    DuplicateSessionFinish,
    FinalizeCanonical,
    EventIdentitiesCleanup,
    ComponentRowsCleanup,
    Complete,
}

impl Default for RepairPhase {
    fn default() -> Self {
        Self::Discover
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct RepairState {
    pub(super) phase: RepairPhase,
    discovery_started: bool,
    discovery_provider: Option<String>,
    discovery_external_session_id: Option<String>,
    discovery_rowid: i64,
    active_provider: Option<String>,
    active_external_session_id: Option<String>,
    pub(super) component_id: Option<String>,
    component_count: usize,
    component_format_seen: bool,
    component_source_format: Option<String>,
    component_format_conflict: bool,
    pub(super) canonical_session_id: Option<String>,
    canonical_created_at_ms: i64,
    pub(super) preferred_session_id: Option<String>,
    pub(super) preferred_source_id: Option<String>,
    preferred_has_identity: bool,
    preferred_has_format: bool,
    preferred_updated_at_ms: i64,
    preferred_created_at_ms: i64,
    preferred_tiebreak_id: Option<String>,
    pub(super) component_member_cursor: Option<String>,
    pub(super) component_event_session_id: Option<String>,
    pub(super) component_event_rowid: Option<i64>,
    pub(super) duplicate_event_id: Option<String>,
    pub(super) canonical_event_id: Option<String>,
    pub(super) duplicate_session_id: Option<String>,
    pub(super) reference_cursor: Option<String>,
}

#[derive(Debug)]
pub(super) struct SessionCandidate {
    pub(super) id: String,
    pub(super) source_id: String,
    pub(super) raw_source_path: Option<String>,
    pub(super) source_format: Option<String>,
    pub(super) source_identity: Option<String>,
    pub(super) created_at_ms: i64,
    pub(super) updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StepCharge {
    pub(super) rows: usize,
    pub(super) bytes: usize,
}

impl StepCharge {
    pub(super) fn unit(_byte_budget: usize, values: &[&str]) -> Self {
        let bytes = values
            .iter()
            .fold(1usize, |total, value| total.saturating_add(value.len()))
            .max(1);
        Self { rows: 1, bytes }
    }
}

impl Store {
    /// Advances legacy provider-session duplicate repair within explicit
    /// row, byte, and SQLite-time limits.
    ///
    /// The tuple is `(processed_rows, processed_bytes, complete)`.
    pub fn repair_provider_session_duplicates(
        &self,
        max_rows: usize,
        max_bytes: usize,
        max_sqlite_time: Duration,
    ) -> Result<(usize, usize, bool)> {
        ensure_repair_schema(&self.conn)?;
        let initially_complete = repair_complete(&self.conn)?;
        if initially_complete || max_rows == 0 || max_bytes == 0 {
            return Ok((0, 0, initially_complete));
        }

        let started = Instant::now();
        let timeout = max_sqlite_time.max(Duration::from_millis(1));
        let progress_started = started;
        self.conn
            .progress_handler(1_000, Some(move || progress_started.elapsed() >= timeout));

        let mut processed_rows = 0usize;
        let mut processed_bytes = 0usize;
        let mut complete = false;
        let result = (|| -> Result<()> {
            while processed_rows < max_rows && processed_bytes < max_bytes {
                if started.elapsed() >= timeout {
                    break;
                }
                let byte_budget = max_bytes.saturating_sub(processed_bytes);
                let step = advance_one_bounded(&self.conn, byte_budget);
                match step {
                    Ok(Some((charge, step_complete))) => {
                        complete = step_complete;
                        processed_rows = processed_rows.saturating_add(charge.rows);
                        processed_bytes = processed_bytes.saturating_add(charge.bytes);
                        if complete {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) if sqlite_operation_interrupted(&error) => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        result?;
        Ok((processed_rows, processed_bytes, complete))
    }
}

fn advance_one_bounded(
    conn: &Connection,
    byte_budget: usize,
) -> Result<Option<(StepCharge, bool)>> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let step = (|| -> Result<(StepCharge, bool)> {
        let mut state = load_state(conn)?;
        let charge = advance_repair_step(conn, &mut state, byte_budget)?;
        let complete = state.phase == RepairPhase::Complete;
        if charge.bytes > byte_budget {
            return Ok((charge, complete));
        }
        save_state(conn, &state, complete)?;
        Ok((charge, complete))
    })();
    match step {
        Ok((charge, complete)) if charge.bytes <= byte_budget => {
            if let Err(error) = conn.execute_batch("COMMIT") {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            Ok(Some((charge, complete)))
        }
        Ok(_) => {
            conn.execute_batch("ROLLBACK")?;
            Ok(None)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn ensure_repair_schema(conn: &Connection) -> Result<()> {
    let state_table_existed = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'provider_session_repair_state')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if state_table_existed {
        return Ok(());
    }
    conn.execute_batch(PROVIDER_SESSION_REPAIR_SCHEMA_SQL)?;
    conn.execute(
        r#"
        INSERT OR IGNORE INTO provider_session_repair_state
        (singleton, state_json, completed, updated_at_ms)
        VALUES (1, '{"phase":"complete"}', 1, unixepoch('subsec') * 1000)
        "#,
        [],
    )?;
    Ok(())
}

fn repair_complete(conn: &Connection) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT completed FROM provider_session_repair_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(true))
}

fn load_state(conn: &Connection) -> Result<RepairState> {
    let state_json = conn.query_row(
        "SELECT state_json FROM provider_session_repair_state WHERE singleton = 1",
        [],
        |row| row.get::<_, String>(0),
    )?;
    Ok(serde_json::from_str(&state_json)?)
}

fn save_state(conn: &Connection, state: &RepairState, complete: bool) -> Result<()> {
    conn.execute(
        r#"
        UPDATE provider_session_repair_state
        SET state_json = ?1,
            completed = ?2,
            updated_at_ms = unixepoch('subsec') * 1000
        WHERE singleton = 1
        "#,
        params![serde_json::to_string(state)?, complete],
    )?;
    Ok(())
}

fn sqlite_operation_interrupted(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Sql(rusqlite::Error::SqliteFailure(sqlite_error, _))
            if sqlite_error.code == ErrorCode::OperationInterrupted
    )
}

fn advance_repair_step(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    match state.phase {
        RepairPhase::Discover => advance_discovery(conn, state, byte_budget),
        RepairPhase::ComponentSeed => seed_component(conn, state, byte_budget),
        RepairPhase::ComponentExpand => expand_component(conn, state, byte_budget),
        RepairPhase::ComponentKeysCleanup => cleanup_component_key(conn, state, byte_budget),
        RepairPhase::MergeSession => {
            maintenance_sessions::merge_session_state(conn, state, byte_budget)
        }
        RepairPhase::StageEvents
        | RepairPhase::ApplyEvents
        | RepairPhase::DuplicateEventLinks
        | RepairPhase::DuplicateEventFiles
        | RepairPhase::DuplicateEventAliases
        | RepairPhase::DuplicateEventFinish => {
            maintenance_events::advance_event_repair(conn, state, byte_budget)
        }
        RepairPhase::DuplicateSessions
        | RepairPhase::DuplicateSessionLinks
        | RepairPhase::DuplicateSessionParents
        | RepairPhase::DuplicateSessionRoots
        | RepairPhase::DuplicateSessionEdgesFrom
        | RepairPhase::DuplicateSessionEdgesTo
        | RepairPhase::DuplicateSessionRuns
        | RepairPhase::DuplicateSessionSummaries
        | RepairPhase::DuplicateSessionEvents
        | RepairPhase::DuplicateSessionLookup
        | RepairPhase::DuplicateSessionAliases
        | RepairPhase::DuplicateSessionFinish
        | RepairPhase::FinalizeCanonical => {
            maintenance_sessions::advance_session_repair(conn, state, byte_budget)
        }
        RepairPhase::EventIdentitiesCleanup => cleanup_event_identity(conn, state, byte_budget),
        RepairPhase::ComponentRowsCleanup => cleanup_component_row(conn, state, byte_budget),
        RepairPhase::Complete => Ok(StepCharge { rows: 0, bytes: 0 }),
    }
}

#[derive(Debug)]
struct DiscoveryRow {
    rowid: i64,
    session_id: String,
    provider: String,
    external_session_id: Option<String>,
    deleted_at_ms: Option<i64>,
    source_id: Option<String>,
    source_kind: Option<String>,
    raw_source_path: Option<String>,
    source_format: Option<String>,
    source_identity: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl DiscoveryRow {
    fn candidate(&self) -> Option<SessionCandidate> {
        let source_id = self.source_id.as_ref()?;
        self.external_session_id.as_ref()?;
        let has_identity = self
            .source_identity
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        let has_path = self
            .raw_source_path
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        if self.deleted_at_ms.is_some()
            || self.source_kind.as_deref() != Some("provider_import")
            || (!has_identity && !has_path)
        {
            return None;
        }
        Some(SessionCandidate {
            id: self.session_id.clone(),
            source_id: source_id.clone(),
            raw_source_path: self.raw_source_path.clone(),
            source_format: self.source_format.clone(),
            source_identity: self.source_identity.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        })
    }

    fn charge(&self, byte_budget: usize) -> StepCharge {
        let mut values = vec![self.session_id.as_str(), self.provider.as_str()];
        for value in [
            self.external_session_id.as_deref(),
            self.source_id.as_deref(),
            self.raw_source_path.as_deref(),
            self.source_format.as_deref(),
            self.source_identity.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            values.push(value);
        }
        StepCharge::unit(byte_budget, &values)
    }
}

fn advance_discovery(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let Some(row) = next_discovery_row(conn, state)? else {
        if group_session_count(conn)? >= 2 {
            state.phase = RepairPhase::ComponentSeed;
        } else {
            conn.execute("DELETE FROM provider_session_repair_group_sessions", [])?;
            state.phase = RepairPhase::Complete;
        }
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    let charge = row.charge(byte_budget);
    let candidate = row.candidate();

    if let (Some(candidate), Some(external_session_id)) =
        (candidate.as_ref(), row.external_session_id.as_ref())
    {
        let same_active_group = state.active_provider.as_deref() == Some(row.provider.as_str())
            && state.active_external_session_id.as_deref() == Some(external_session_id.as_str());
        if state.active_provider.is_some() && !same_active_group {
            if group_session_count(conn)? >= 2 {
                state.phase = RepairPhase::ComponentSeed;
                return Ok(charge);
            }
            conn.execute("DELETE FROM provider_session_repair_group_sessions", [])?;
            state.active_provider = None;
            state.active_external_session_id = None;
        }
        if state.active_provider.is_none() {
            state.active_provider = Some(row.provider.clone());
            state.active_external_session_id = Some(external_session_id.clone());
        }
        insert_group_session(conn, candidate)?;
    }

    state.discovery_started = true;
    state.discovery_provider = Some(row.provider);
    state.discovery_external_session_id = row.external_session_id;
    state.discovery_rowid = row.rowid;
    Ok(charge)
}

fn next_discovery_row(conn: &Connection, state: &RepairState) -> Result<Option<DiscoveryRow>> {
    const SELECT: &str = r#"
        SELECT s.rowid, s.id, s.provider, s.external_session_id, s.deleted_at_ms,
               cs.id, cs.kind, cs.raw_source_path,
               COALESCE(cs.source_format, json_extract(cs.metadata_json, '$.source_format')),
               cs.source_identity, s.created_at_ms, s.updated_at_ms
        FROM sessions AS s INDEXED BY idx_sessions_provider_external_session_id
        LEFT JOIN capture_sources AS cs ON cs.id = s.capture_source_id
    "#;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(DiscoveryRow {
            rowid: row.get(0)?,
            session_id: row.get(1)?,
            provider: row.get(2)?,
            external_session_id: row.get(3)?,
            deleted_at_ms: row.get(4)?,
            source_id: row.get(5)?,
            source_kind: row.get(6)?,
            raw_source_path: row.get(7)?,
            source_format: row.get(8)?,
            source_identity: row.get(9)?,
            created_at_ms: row.get(10)?,
            updated_at_ms: row.get(11)?,
        })
    };
    if !state.discovery_started {
        return Ok(conn
            .query_row(
                &format!("{SELECT} ORDER BY s.provider, s.external_session_id, s.rowid LIMIT 1"),
                [],
                map_row,
            )
            .optional()?);
    }

    let provider = state.discovery_provider.as_deref().unwrap_or_default();
    let sql = if state.discovery_external_session_id.is_some() {
        format!(
            r#"{SELECT}
            WHERE s.provider > ?1
               OR (s.provider = ?1 AND (
                    s.external_session_id > ?2
                    OR (s.external_session_id = ?2 AND s.rowid > ?3)
               ))
            ORDER BY s.provider, s.external_session_id, s.rowid
            LIMIT 1"#
        )
    } else {
        format!(
            r#"{SELECT}
            WHERE s.provider > ?1
               OR (s.provider = ?1 AND (
                    (s.external_session_id IS NULL AND s.rowid > ?3)
                    OR s.external_session_id IS NOT NULL
               ))
            ORDER BY s.provider, s.external_session_id, s.rowid
            LIMIT 1"#
        )
    };
    Ok(conn
        .query_row(
            &sql,
            params![
                provider,
                state.discovery_external_session_id.as_deref(),
                state.discovery_rowid
            ],
            map_row,
        )
        .optional()?)
}

fn insert_group_session(conn: &Connection, candidate: &SessionCandidate) -> Result<()> {
    conn.execute(
        r#"
        INSERT OR IGNORE INTO provider_session_repair_group_sessions
        (session_id, source_id, raw_source_path, source_format, source_identity,
         created_at_ms, updated_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
        params![
            candidate.id,
            candidate.source_id,
            candidate.raw_source_path,
            candidate.source_format,
            candidate.source_identity,
            candidate.created_at_ms,
            candidate.updated_at_ms,
        ],
    )?;
    Ok(())
}

fn group_session_count(conn: &Connection) -> Result<usize> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM provider_session_repair_group_sessions",
        [],
        |row| row.get(0),
    )?)
}

fn seed_component(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let candidate = conn
        .query_row(
            r#"
            SELECT session_id, source_id, raw_source_path, source_format,
                   source_identity, created_at_ms, updated_at_ms
            FROM provider_session_repair_group_sessions
            WHERE component_id IS NULL
            ORDER BY created_at_ms, session_id
            LIMIT 1
            "#,
            [],
            session_candidate_from_row,
        )
        .optional()?;
    let Some(candidate) = candidate else {
        state.active_provider = None;
        state.active_external_session_id = None;
        state.phase = RepairPhase::Discover;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };

    reset_component_state(state);
    state.component_id = Some(candidate.id.clone());
    add_component_member(conn, state, &candidate)?;
    state.phase = RepairPhase::ComponentExpand;
    Ok(candidate_charge(byte_budget, &candidate))
}

fn expand_component(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let key = conn
        .query_row(
            r#"
            SELECT key_kind, key_value, scan_cursor_session_id
            FROM provider_session_repair_component_keys
            WHERE complete = 0
            ORDER BY key_kind, key_value
            LIMIT 1
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((key_kind, key_value, cursor)) = key else {
        state.phase = RepairPhase::ComponentKeysCleanup;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };

    let (column, index) = match key_kind.as_str() {
        "identity" => (
            "source_identity",
            "idx_provider_session_repair_group_identity",
        ),
        "path" => ("raw_source_path", "idx_provider_session_repair_group_path"),
        _ => {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
    };
    let sql = format!(
        r#"
        SELECT session_id, source_id, raw_source_path, source_format,
               source_identity, created_at_ms, updated_at_ms
        FROM provider_session_repair_group_sessions INDEXED BY {index}
        WHERE {column} = ?1
          AND component_id IS NULL
          AND (?2 IS NULL OR session_id > ?2)
        ORDER BY session_id
        LIMIT 1
        "#
    );
    let candidate = conn
        .query_row(&sql, params![key_value, cursor], session_candidate_from_row)
        .optional()?;
    if let Some(candidate) = candidate {
        add_component_member(conn, state, &candidate)?;
        conn.execute(
            r#"
            UPDATE provider_session_repair_component_keys
            SET scan_cursor_session_id = ?3
            WHERE key_kind = ?1 AND key_value = ?2
            "#,
            params![key_kind, key_value, candidate.id],
        )?;
        return Ok(candidate_charge(byte_budget, &candidate));
    }

    conn.execute(
        r#"
        UPDATE provider_session_repair_component_keys
        SET complete = 1
        WHERE key_kind = ?1 AND key_value = ?2
        "#,
        params![key_kind, key_value],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&key_value]))
}

fn cleanup_component_key(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let key = conn
        .query_row(
            "SELECT key_kind, key_value FROM provider_session_repair_component_keys ORDER BY key_kind, key_value LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((key_kind, key_value)) = key else {
        state.phase = if state.component_count >= 2 && !state.component_format_conflict {
            RepairPhase::MergeSession
        } else {
            RepairPhase::ComponentRowsCleanup
        };
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    conn.execute(
        "DELETE FROM provider_session_repair_component_keys WHERE key_kind = ?1 AND key_value = ?2",
        params![key_kind, key_value],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&key_value]))
}

fn add_component_member(
    conn: &Connection,
    state: &mut RepairState,
    candidate: &SessionCandidate,
) -> Result<()> {
    let component_id = state
        .component_id
        .as_deref()
        .ok_or_else(|| StoreError::Sql(rusqlite::Error::InvalidQuery))?;
    conn.execute(
        "UPDATE provider_session_repair_group_sessions SET component_id = ?2 WHERE session_id = ?1",
        params![candidate.id, component_id],
    )?;
    for (kind, value) in [
        ("identity", candidate.source_identity.as_deref()),
        ("path", candidate.raw_source_path.as_deref()),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            conn.execute(
                r#"
                INSERT OR IGNORE INTO provider_session_repair_component_keys
                (key_kind, key_value)
                VALUES (?1, ?2)
                "#,
                params![kind, value],
            )?;
        }
    }

    state.component_count = state.component_count.saturating_add(1);
    match candidate.source_format.as_ref() {
        Some(source_format) if !state.component_format_seen => {
            state.component_format_seen = true;
            state.component_source_format = Some(source_format.clone());
        }
        Some(source_format)
            if state.component_source_format.as_deref() != Some(source_format.as_str()) =>
        {
            state.component_format_conflict = true;
        }
        _ => {}
    }

    let canonical_is_better = state.canonical_session_id.is_none()
        || (candidate.created_at_ms, candidate.id.as_str())
            < (
                state.canonical_created_at_ms,
                state.canonical_session_id.as_deref().unwrap_or_default(),
            );
    if canonical_is_better {
        state.canonical_session_id = Some(candidate.id.clone());
        state.canonical_created_at_ms = candidate.created_at_ms;
    }

    let preferred_key = (
        candidate.source_identity.is_some(),
        candidate.source_format.is_some(),
        candidate.updated_at_ms,
        candidate.created_at_ms,
        candidate.id.as_str(),
    );
    let current_preferred_key = (
        state.preferred_has_identity,
        state.preferred_has_format,
        state.preferred_updated_at_ms,
        state.preferred_created_at_ms,
        state.preferred_tiebreak_id.as_deref().unwrap_or_default(),
    );
    if state.preferred_session_id.is_none() || preferred_key > current_preferred_key {
        state.preferred_session_id = Some(candidate.id.clone());
        state.preferred_source_id = Some(candidate.source_id.clone());
        state.preferred_has_identity = candidate.source_identity.is_some();
        state.preferred_has_format = candidate.source_format.is_some();
        state.preferred_updated_at_ms = candidate.updated_at_ms;
        state.preferred_created_at_ms = candidate.created_at_ms;
        state.preferred_tiebreak_id = Some(candidate.id.clone());
    }
    Ok(())
}

fn cleanup_event_identity(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let identity = conn
        .query_row(
            "SELECT provider_index, provider_hash FROM provider_session_repair_event_identities ORDER BY provider_index, provider_hash LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((provider_index, provider_hash)) = identity else {
        state.phase = RepairPhase::ComponentRowsCleanup;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    conn.execute(
        "DELETE FROM provider_session_repair_event_identities WHERE provider_index = ?1 AND provider_hash = ?2",
        params![provider_index, provider_hash],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&provider_hash]))
}

fn cleanup_component_row(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let component_id = state
        .component_id
        .as_deref()
        .ok_or_else(|| StoreError::Sql(rusqlite::Error::InvalidQuery))?;
    let session_id = conn
        .query_row(
            r#"
            SELECT session_id
            FROM provider_session_repair_group_sessions
            WHERE component_id = ?1
            ORDER BY session_id
            LIMIT 1
            "#,
            [component_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(session_id) = session_id else {
        reset_component_state(state);
        state.phase = RepairPhase::ComponentSeed;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    conn.execute(
        "DELETE FROM provider_session_repair_group_sessions WHERE session_id = ?1",
        [&session_id],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&session_id]))
}

fn reset_component_state(state: &mut RepairState) {
    state.component_id = None;
    state.component_count = 0;
    state.component_format_seen = false;
    state.component_source_format = None;
    state.component_format_conflict = false;
    state.canonical_session_id = None;
    state.canonical_created_at_ms = 0;
    state.preferred_session_id = None;
    state.preferred_source_id = None;
    state.preferred_has_identity = false;
    state.preferred_has_format = false;
    state.preferred_updated_at_ms = 0;
    state.preferred_created_at_ms = 0;
    state.preferred_tiebreak_id = None;
    state.component_member_cursor = None;
    state.component_event_session_id = None;
    state.component_event_rowid = None;
    state.duplicate_event_id = None;
    state.canonical_event_id = None;
    state.duplicate_session_id = None;
    state.reference_cursor = None;
}

fn session_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionCandidate> {
    Ok(SessionCandidate {
        id: row.get(0)?,
        source_id: row.get(1)?,
        raw_source_path: row.get(2)?,
        source_format: row.get(3)?,
        source_identity: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn candidate_charge(byte_budget: usize, candidate: &SessionCandidate) -> StepCharge {
    let mut values = vec![candidate.id.as_str(), candidate.source_id.as_str()];
    for value in [
        candidate.raw_source_path.as_deref(),
        candidate.source_format.as_deref(),
        candidate.source_identity.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        values.push(value);
    }
    StepCharge::unit(byte_budget, &values)
}
