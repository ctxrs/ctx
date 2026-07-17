use rusqlite::{params, Connection, OptionalExtension};

use crate::events::parse_provider_event_dedupe_key;
use crate::search::projections::mark_search_projection_rebuild_required;
use crate::{Result, StoreError};

use super::maintenance::{RepairPhase, RepairState, StepCharge};

#[derive(Debug)]
struct EventCandidate {
    rowid: i64,
    id: String,
    seq: i64,
    dedupe_key: Option<String>,
    provider_index: Option<u64>,
    provider_hash: Option<String>,
}

impl EventCandidate {
    fn identity(&self) -> Option<(String, String)> {
        match (self.provider_index, self.provider_hash.as_ref()) {
            (Some(index), Some(hash)) if !hash.is_empty() => {
                return Some((index.to_string(), hash.clone()));
            }
            _ => {}
        }
        self.dedupe_key
            .as_deref()
            .and_then(parse_provider_event_dedupe_key)
            .map(|parsed| (parsed.provider_index.to_string(), parsed.payload_hash))
    }

    fn charge(&self, byte_budget: usize) -> StepCharge {
        let mut values = vec![self.id.as_str()];
        for value in [self.dedupe_key.as_deref(), self.provider_hash.as_deref()]
            .into_iter()
            .flatten()
        {
            values.push(value);
        }
        StepCharge::unit(byte_budget, &values)
    }
}

pub(super) fn advance_event_repair(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    match state.phase {
        RepairPhase::StageEvents => stage_event(conn, state, byte_budget),
        RepairPhase::ApplyEvents => apply_event(conn, state, byte_budget),
        RepairPhase::DuplicateEventLinks => redirect_event_link(conn, state, byte_budget),
        RepairPhase::DuplicateEventFiles => redirect_event_file(conn, state, byte_budget),
        RepairPhase::DuplicateEventAliases => redirect_event_alias(conn, state, byte_budget),
        RepairPhase::DuplicateEventFinish => finish_duplicate_event(conn, state, byte_budget),
        _ => Err(StoreError::Sql(rusqlite::Error::InvalidQuery)),
    }
}

fn stage_event(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    if state.component_event_session_id.is_none() {
        let component_id = required(state.component_id.as_deref())?;
        let session_id = conn
            .query_row(
                r#"
                SELECT session_id
                FROM provider_session_repair_group_sessions
                WHERE component_id = ?1
                  AND (?2 IS NULL OR session_id > ?2)
                ORDER BY session_id
                LIMIT 1
                "#,
                params![component_id, state.component_member_cursor.as_deref()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(session_id) = session_id else {
            state.component_member_cursor = None;
            state.phase = RepairPhase::ApplyEvents;
            return Ok(StepCharge::unit(byte_budget, &[]));
        };
        state.component_event_session_id = Some(session_id.clone());
        state.component_event_rowid = None;
        return Ok(StepCharge::unit(byte_budget, &[&session_id]));
    }

    let session_id = required(state.component_event_session_id.as_deref())?.to_owned();
    let event = conn
        .query_row(
            r#"
            SELECT rowid, id, seq, dedupe_key,
                   json_extract(metadata_json, '$.provider_event_index'),
                   json_extract(metadata_json, '$.provider_event_hash')
            FROM events INDEXED BY idx_events_session_id
            WHERE session_id = ?1
              AND (?2 IS NULL OR rowid > ?2)
            ORDER BY rowid
            LIMIT 1
            "#,
            params![session_id, state.component_event_rowid],
            event_candidate_from_row,
        )
        .optional()?;
    let Some(event) = event else {
        state.component_member_cursor = state.component_event_session_id.take();
        state.component_event_rowid = None;
        return Ok(StepCharge::unit(byte_budget, &[&session_id]));
    };

    let identity = event.identity();
    conn.execute(
        r#"
        INSERT OR IGNORE INTO provider_session_repair_events
        (event_id, seq, session_id, provider_index, provider_hash)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        params![
            event.id,
            event.seq,
            session_id,
            identity.as_ref().map(|(index, _)| index.as_str()),
            identity.as_ref().map(|(_, hash)| hash.as_str()),
        ],
    )?;
    state.component_event_rowid = Some(event.rowid);
    Ok(event.charge(byte_budget))
}

fn apply_event(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let event = conn
        .query_row(
            r#"
            SELECT event_id, provider_index, provider_hash
            FROM provider_session_repair_events
            ORDER BY seq, event_id
            LIMIT 1
            "#,
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((event_id, provider_index, provider_hash)) = event else {
        state.phase = RepairPhase::DuplicateSessions;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };

    if let (Some(provider_index), Some(provider_hash)) =
        (provider_index.as_deref(), provider_hash.as_deref())
    {
        let canonical_event_id = conn
            .query_row(
                r#"
                SELECT event_id
                FROM provider_session_repair_event_identities
                WHERE provider_index = ?1 AND provider_hash = ?2
                "#,
                params![provider_index, provider_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(canonical_event_id) = canonical_event_id {
            state.duplicate_event_id = Some(event_id.clone());
            state.canonical_event_id = Some(canonical_event_id);
            state.reference_cursor = None;
            state.phase = RepairPhase::DuplicateEventLinks;
            return Ok(StepCharge::unit(
                byte_budget,
                &[&event_id, provider_index, provider_hash],
            ));
        }
        conn.execute(
            r#"
            INSERT INTO provider_session_repair_event_identities
            (provider_index, provider_hash, event_id)
            VALUES (?1, ?2, ?3)
            "#,
            params![provider_index, provider_hash, event_id],
        )?;
    }

    move_event_to_canonical_session(conn, state, &event_id)?;
    conn.execute(
        "DELETE FROM provider_session_repair_events WHERE event_id = ?1",
        [&event_id],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&event_id]))
}

fn redirect_event_link(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_event_id.as_deref())?;
    let canonical_id = required(state.canonical_event_id.as_deref())?;
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
        state.phase = RepairPhase::DuplicateEventFiles;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };

    if target_type == "event" && target_id == duplicate_id {
        let canonical_exists = conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM history_record_links
                WHERE history_record_id = ?1
                  AND target_type = 'event'
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

fn redirect_event_file(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_event_id.as_deref())?;
    let canonical_id = required(state.canonical_event_id.as_deref())?;
    let file_id = conn
        .query_row(
            r#"
            SELECT id
            FROM files_touched INDEXED BY idx_files_touched_event_id
            WHERE event_id = ?1
            LIMIT 1
            "#,
            [duplicate_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(file_id) = file_id else {
        state.phase = RepairPhase::DuplicateEventAliases;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    conn.execute(
        "UPDATE files_touched SET event_id = ?2 WHERE id = ?1",
        params![file_id, canonical_id],
    )?;
    Ok(StepCharge::unit(byte_budget, &[&file_id]))
}

fn redirect_event_alias(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_event_id.as_deref())?;
    let canonical_id = required(state.canonical_event_id.as_deref())?;
    let alias = conn
        .query_row(
            r#"
            SELECT alias_id, event_id
            FROM event_aliases
            WHERE (?1 IS NULL OR alias_id > ?1)
            ORDER BY alias_id
            LIMIT 1
            "#,
            [state.reference_cursor.as_deref()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((alias_id, event_id)) = alias else {
        state.reference_cursor = None;
        state.phase = RepairPhase::DuplicateEventFinish;
        return Ok(StepCharge::unit(byte_budget, &[]));
    };
    if event_id == duplicate_id {
        conn.execute(
            "UPDATE event_aliases SET event_id = ?2 WHERE alias_id = ?1",
            params![alias_id, canonical_id],
        )?;
    }
    state.reference_cursor = Some(alias_id.clone());
    Ok(StepCharge::unit(byte_budget, &[&alias_id, &event_id]))
}

fn finish_duplicate_event(
    conn: &Connection,
    state: &mut RepairState,
    byte_budget: usize,
) -> Result<StepCharge> {
    let duplicate_id = required(state.duplicate_event_id.as_deref())?.to_owned();
    let canonical_id = required(state.canonical_event_id.as_deref())?.to_owned();
    invalidate_projection(conn, state)?;
    conn.execute(
        r#"
        INSERT OR REPLACE INTO event_aliases
        (alias_id, event_id, reason, created_at_ms)
        VALUES (?1, ?2, 'provider_source_identity_repair', unixepoch('subsec') * 1000)
        "#,
        params![duplicate_id, canonical_id],
    )?;
    conn.execute(
        "DELETE FROM event_search_lookup WHERE event_id = ?1",
        [&duplicate_id],
    )?;
    conn.execute("DELETE FROM events WHERE id = ?1", [&duplicate_id])?;
    conn.execute(
        "DELETE FROM provider_session_repair_events WHERE event_id = ?1",
        [&duplicate_id],
    )?;
    state.duplicate_event_id = None;
    state.canonical_event_id = None;
    state.reference_cursor = None;
    state.phase = RepairPhase::ApplyEvents;
    Ok(StepCharge::unit(
        byte_budget,
        &[&duplicate_id, &canonical_id],
    ))
}

fn move_event_to_canonical_session(
    conn: &Connection,
    state: &mut RepairState,
    event_id: &str,
) -> Result<()> {
    let canonical_session_id = required(state.canonical_session_id.as_deref())?.to_owned();
    invalidate_projection(conn, state)?;
    conn.execute(
        "UPDATE events SET session_id = ?2 WHERE id = ?1",
        params![event_id, canonical_session_id],
    )?;
    conn.execute(
        "UPDATE event_search_lookup SET session_id = ?2 WHERE event_id = ?1",
        params![event_id, canonical_session_id],
    )?;
    Ok(())
}

fn invalidate_projection(conn: &Connection, _state: &mut RepairState) -> Result<()> {
    mark_search_projection_rebuild_required(conn)
}

fn event_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventCandidate> {
    let provider_index = row
        .get::<_, Option<i64>>(4)?
        .and_then(|value| u64::try_from(value).ok());
    Ok(EventCandidate {
        rowid: row.get(0)?,
        id: row.get(1)?,
        seq: row.get(2)?,
        dedupe_key: row.get(3)?,
        provider_index,
        provider_hash: row.get(5)?,
    })
}

fn required(value: Option<&str>) -> Result<&str> {
    value.ok_or_else(|| StoreError::Sql(rusqlite::Error::InvalidQuery))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_identity_falls_back_to_legacy_and_source_scoped_dedupe_keys() {
        for key in [
            "provider:claude:session:7:event-hash",
            "provider-source:018fe2e4-2266-7000-8000-000000000001:7:event-hash",
        ] {
            assert_eq!(
                EventCandidate {
                    rowid: 1,
                    id: "event".to_owned(),
                    seq: 1,
                    dedupe_key: Some(key.to_owned()),
                    provider_index: None,
                    provider_hash: None,
                }
                .identity(),
                Some(("7".to_owned(), "event-hash".to_owned()))
            );
        }
    }
}
