use rusqlite::{params, Connection, OptionalExtension};

use crate::{Result, StoreError};

use super::maintenance::{RepairPhase, RepairState, StepCharge};

pub(super) fn merge_session_state(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let canonical_id = required(state.canonical_session_id.as_deref())?.to_owned();
    let preferred_id = required(state.preferred_session_id.as_deref())?.to_owned();
    conn.execute(
        r#"
        UPDATE sessions
        SET (
            history_record_id,
            parent_session_id,
            root_session_id,
            external_agent_id,
            agent_type,
            role_hint,
            is_primary,
            status,
            fidelity,
            transcript_blob_id,
            started_at_ms,
            ended_at_ms,
            updated_at_ms,
            visibility,
            sync_state,
            sync_version,
            metadata_json
        ) = (
            SELECT
                COALESCE(preferred.history_record_id, sessions.history_record_id),
                COALESCE(preferred.parent_session_id, sessions.parent_session_id),
                COALESCE(preferred.root_session_id, sessions.root_session_id),
                COALESCE(preferred.external_agent_id, sessions.external_agent_id),
                preferred.agent_type,
                COALESCE(preferred.role_hint, sessions.role_hint),
                preferred.is_primary,
                preferred.status,
                preferred.fidelity,
                COALESCE(preferred.transcript_blob_id, sessions.transcript_blob_id),
                MIN(sessions.started_at_ms, preferred.started_at_ms),
                COALESCE(
                    MAX(sessions.ended_at_ms, preferred.ended_at_ms),
                    sessions.ended_at_ms,
                    preferred.ended_at_ms
                ),
                MAX(sessions.updated_at_ms, preferred.updated_at_ms),
                preferred.visibility,
                preferred.sync_state,
                MAX(sessions.sync_version, preferred.sync_version),
                preferred.metadata_json
            FROM sessions preferred
            WHERE preferred.id = ?2
        )
        WHERE id = ?1
        "#,
        params![canonical_id, preferred_id],
    )?;
    state.component_member_cursor = None;
    state.component_event_session_id = None;
    state.component_event_rowid = None;
    state.phase = RepairPhase::StageEvents;
    Ok(StepCharge::unit(
        byte_budget,
        &[&canonical_id, &preferred_id],
    ))
}

pub(super) fn advance_session_repair(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    match state.phase {
        RepairPhase::DuplicateSessions => select_duplicate_session(conn, state, byte_budget),
        RepairPhase::DuplicateSessionLinks => redirect_session_link(conn, state, byte_budget),
        RepairPhase::DuplicateSessionParents => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "sessions",
            "id",
            "parent_session_id",
            "idx_sessions_parent_session_id",
            RepairPhase::DuplicateSessionRoots,
        ),
        RepairPhase::DuplicateSessionRoots => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "sessions",
            "id",
            "root_session_id",
            "idx_sessions_root_session_id",
            RepairPhase::DuplicateSessionEdgesFrom,
        ),
        RepairPhase::DuplicateSessionEdgesFrom => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "session_edges",
            "id",
            "from_session_id",
            "idx_session_edges_from_session_id",
            RepairPhase::DuplicateSessionEdgesTo,
        ),
        RepairPhase::DuplicateSessionEdgesTo => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "session_edges",
            "id",
            "to_session_id",
            "idx_session_edges_to_session_id",
            RepairPhase::DuplicateSessionRuns,
        ),
        RepairPhase::DuplicateSessionRuns => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "runs",
            "id",
            "session_id",
            "idx_runs_session_id",
            RepairPhase::DuplicateSessionSummaries,
        ),
        RepairPhase::DuplicateSessionSummaries => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "summaries",
            "id",
            "session_id",
            "idx_summaries_session_id",
            RepairPhase::DuplicateSessionEvents,
        ),
        RepairPhase::DuplicateSessionEvents => redirect_indexed_reference(
            conn,
            state,
            byte_budget,
            "events",
            "id",
            "session_id",
            "idx_events_session_id",
            RepairPhase::DuplicateSessionLookup,
        ),
        RepairPhase::DuplicateSessionLookup => redirect_lookup_session(conn, state, byte_budget),
        RepairPhase::DuplicateSessionAliases => redirect_session_alias(conn, state, byte_budget),
        RepairPhase::DuplicateSessionFinish => finish_duplicate_session(conn, state, byte_budget),
        RepairPhase::FinalizeCanonical => finalize_canonical_session(conn, state, byte_budget),
        _ => Err(StoreError::Sql(rusqlite::Error::InvalidQuery)),
    }
}

fn select_duplicate_session(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let component_id = required(state.component_id.as_deref())?;
    let canonical_id = required(state.canonical_session_id.as_deref())?;
    let duplicate_id = conn
        .query_row(
            r#"
            SELECT repair.session_id
            FROM provider_session_repair_group_sessions AS repair
            JOIN sessions AS stored ON stored.id = repair.session_id
            WHERE repair.component_id = ?1
              AND repair.session_id <> ?2
            ORDER BY repair.session_id
            LIMIT 1
            "#,
            params![component_id, canonical_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(duplicate_id) = duplicate_id else {
        state.phase = RepairPhase::FinalizeCanonical;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    state.duplicate_session_id = Some(duplicate_id.clone());
    state.reference_cursor = None;
    state.phase = RepairPhase::DuplicateSessionLinks;
    Ok(StepCharge::unit(byte_budget, &[&duplicate_id]))
}

fn redirect_session_link(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_session_id.as_deref())?;
    let canonical_id = required(state.canonical_session_id.as_deref())?;
    let link = conn
        .query_row(
            r#"
            SELECT id, history_record_id, target_type, target_id, link_type
            FROM history_record_links
            WHERE (?1 IS NULL OR id > ?1)
            ORDER BY id
            LIMIT 1
            "#,
            [state.reference_cursor.as_deref()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((id, history_record_id, target_type, target_id, link_type)) = link else {
        state.reference_cursor = None;
        state.phase = RepairPhase::DuplicateSessionParents;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };

    if target_type == "session" && target_id == duplicate_id {
        let canonical_exists = conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM history_record_links
                WHERE history_record_id = ?1
                  AND target_type = 'session'
                  AND target_id = ?2
                  AND link_type = ?3
            )
            "#,
            params![history_record_id, canonical_id, link_type],
            |row| row.get::<_, bool>(0),
        )?;
        if canonical_exists {
            conn.execute("DELETE FROM history_record_links WHERE id = ?1", [&id])?;
        } else {
            conn.execute(
                "UPDATE history_record_links SET target_id = ?2 WHERE id = ?1",
                params![id, canonical_id],
            )?;
        }
    }
    state.reference_cursor = Some(id.clone());
    Ok(StepCharge::unit(
        byte_budget,
        &[
            &id,
            &history_record_id,
            &target_type,
            &target_id,
            &link_type,
        ],
    ))
}

#[allow(clippy::too_many_arguments)]
fn redirect_indexed_reference(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
    table: &str,
    id_column: &str,
    reference_column: &str,
    index: &str,
    next_phase: RepairPhase,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_session_id.as_deref())?;
    let canonical_id = required(state.canonical_session_id.as_deref())?;
    let select_sql = format!(
        "SELECT {id_column} FROM {table} INDEXED BY {index} WHERE {reference_column} = ?1 LIMIT 1"
    );
    let row_id = conn
        .query_row(&select_sql, [duplicate_id], |row| row.get::<_, String>(0))
        .optional()?;
    let Some(row_id) = row_id else {
        state.phase = next_phase;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    let update_sql = format!("UPDATE {table} SET {reference_column} = ?2 WHERE {id_column} = ?1");
    conn.execute(&update_sql, params![row_id, canonical_id])?;
    Ok(StepCharge::unit(byte_budget, &[&row_id]))
}

fn redirect_lookup_session(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_session_id.as_deref())?;
    let canonical_id = required(state.canonical_session_id.as_deref())?;
    let row = conn
        .query_row(
            r#"
            SELECT event_id, session_id
            FROM event_search_lookup
            WHERE (?1 IS NULL OR event_id > ?1)
            ORDER BY event_id
            LIMIT 1
            "#,
            [state.reference_cursor.as_deref()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let Some((event_id, session_id)) = row else {
        state.reference_cursor = None;
        state.phase = RepairPhase::DuplicateSessionAliases;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    if session_id.as_deref() == Some(duplicate_id) {
        conn.execute(
            "UPDATE event_search_lookup SET session_id = ?2 WHERE event_id = ?1",
            params![event_id, canonical_id],
        )?;
    }
    state.reference_cursor = Some(event_id.clone());
    Ok(StepCharge::unit(byte_budget, &[&event_id]))
}

fn redirect_session_alias(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_session_id.as_deref())?;
    let canonical_id = required(state.canonical_session_id.as_deref())?;
    let alias = conn
        .query_row(
            r#"
            SELECT alias_id, session_id
            FROM session_aliases
            WHERE (?1 IS NULL OR alias_id > ?1)
            ORDER BY alias_id
            LIMIT 1
            "#,
            [state.reference_cursor.as_deref()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((alias_id, session_id)) = alias else {
        state.reference_cursor = None;
        state.phase = RepairPhase::DuplicateSessionFinish;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    if session_id == duplicate_id {
        conn.execute(
            "UPDATE session_aliases SET session_id = ?2 WHERE alias_id = ?1",
            params![alias_id, canonical_id],
        )?;
    }
    state.reference_cursor = Some(alias_id.clone());
    Ok(StepCharge::unit(byte_budget, &[&alias_id, &session_id]))
}

fn finish_duplicate_session(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_session_id.as_deref())?.to_owned();
    let canonical_id = required(state.canonical_session_id.as_deref())?.to_owned();
    conn.execute(
        r#"
        INSERT OR REPLACE INTO session_aliases
        (alias_id, session_id, reason, created_at_ms)
        VALUES (?1, ?2, 'provider_source_identity_repair', unixepoch('subsec') * 1000)
        "#,
        params![duplicate_id, canonical_id],
    )?;
    conn.execute("DELETE FROM sessions WHERE id = ?1", [&duplicate_id])?;
    state.duplicate_session_id = None;
    state.reference_cursor = None;
    state.phase = RepairPhase::DuplicateSessions;
    Ok(StepCharge::unit(
        byte_budget,
        &[&duplicate_id, &canonical_id],
    ))
}

fn finalize_canonical_session(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let canonical_id = required(state.canonical_session_id.as_deref())?.to_owned();
    let preferred_source_id = required(state.preferred_source_id.as_deref())?.to_owned();
    conn.execute(
        "UPDATE sessions SET capture_source_id = ?2 WHERE id = ?1",
        params![canonical_id, preferred_source_id],
    )?;
    state.phase = RepairPhase::EventIdentitiesCleanup;
    Ok(StepCharge::unit(
        byte_budget,
        &[&canonical_id, &preferred_source_id],
    ))
}

fn required(value: Option<&str>) -> Result<&str> {
    value.ok_or_else(|| StoreError::Sql(rusqlite::Error::InvalidQuery))
}
