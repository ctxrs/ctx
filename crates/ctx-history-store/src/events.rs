use ctx_history_core::{CaptureProvider, Event, EventRole, EventType, RedactionState};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_timestamp_ms, optional_uuid_string,
    parse_json, parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::search::projections::{
    insert_event_search_projection_for_event, upsert_event_search_projection_for_event,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn provider_event_dedupe_key(
        provider: CaptureProvider,
        external_session_id: &str,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!(
            "provider:{}:{}:{}:{}",
            provider.as_str(),
            external_session_id,
            provider_index,
            payload_hash
        )
    }

    pub fn provider_source_event_dedupe_key(
        source_id: Uuid,
        provider_index: u64,
        payload_hash: &str,
    ) -> String {
        format!("provider-source:{source_id}:{provider_index}:{payload_hash}")
    }

    pub fn upsert_event(&self, event: &Event) -> Result<Uuid> {
        let event_id = if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            if let Some(existing_id) = self
                .conn
                .query_row(
                    "SELECT id FROM events WHERE dedupe_key = ?1",
                    params![dedupe_key],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .optional()?
            {
                return Ok(existing_id);
            }
            event.id
        } else {
            event.id
        };

        self.conn.execute(
                r#"
                INSERT INTO events
                (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ON CONFLICT(id) DO UPDATE SET
                    seq = excluded.seq,
                    history_record_id = excluded.history_record_id,
                    session_id = excluded.session_id,
                    run_id = excluded.run_id,
                    event_type = excluded.event_type,
                    role = excluded.role,
                    occurred_at_ms = excluded.occurred_at_ms,
                    capture_source_id = excluded.capture_source_id,
                    payload_json = excluded.payload_json,
                    payload_blob_id = excluded.payload_blob_id,
                    dedupe_key = excluded.dedupe_key,
                    visibility = excluded.visibility,
                    redaction_state = excluded.redaction_state,
                    fidelity = excluded.fidelity,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    deleted_at_ms = excluded.deleted_at_ms,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    event_id.to_string(),
                    event.seq as i64,
                    optional_uuid_string(event.history_record_id),
                    optional_uuid_string(event.session_id),
                    optional_uuid_string(event.run_id),
                    event.event_type.as_str(),
                    event.role.map(|role| role.as_str()),
                    timestamp_ms(event.occurred_at),
                    optional_uuid_string(event.capture_source_id),
                    serde_json::to_string(&event.payload)?,
                    optional_uuid_string(event.payload_blob_id),
                    event.dedupe_key.as_deref(),
                    event.sync.visibility.as_str(),
                    event.redaction_state.as_str(),
                    event.sync.fidelity.as_str(),
                    event.sync.sync_state.as_str(),
                    event.sync.sync_version as i64,
                    optional_timestamp_ms(event.sync.deleted_at),
                    serde_json::to_string(&event.sync.metadata)?,
                ],
            )?;
        upsert_event_search_projection_for_event(&self.conn, event_id, event)?;
        if let Some(dedupe_key) = &event.dedupe_key {
            return self.event_id_by_dedupe_key(dedupe_key);
        }
        Ok(event_id)
    }

    pub fn insert_event_if_absent(&self, event: &Event) -> Result<bool> {
        let changed = self
                .conn
                .prepare_cached(
                    r#"
                    INSERT OR IGNORE INTO events
                    (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                    "#,
                )?
                .execute(params![
                    event.id.to_string(),
                    event.seq as i64,
                    optional_uuid_string(event.history_record_id),
                    optional_uuid_string(event.session_id),
                    optional_uuid_string(event.run_id),
                    event.event_type.as_str(),
                    event.role.map(|role| role.as_str()),
                    timestamp_ms(event.occurred_at),
                    optional_uuid_string(event.capture_source_id),
                    serde_json::to_string(&event.payload)?,
                    optional_uuid_string(event.payload_blob_id),
                    event.dedupe_key.as_deref(),
                    event.sync.visibility.as_str(),
                    event.redaction_state.as_str(),
                    event.sync.fidelity.as_str(),
                    event.sync.sync_state.as_str(),
                    event.sync.sync_version as i64,
                    optional_timestamp_ms(event.sync.deleted_at),
                    serde_json::to_string(&event.sync.metadata)?,
                ])?;
        if changed == 0 {
            if let Some(dedupe_key) = &event.dedupe_key {
                reject_provider_event_hash_conflict(&self.conn, dedupe_key)?;
            }
        }
        if changed > 0 {
            insert_event_search_projection_for_event(&self.conn, event)?;
        }
        Ok(changed > 0)
    }

    pub fn event_id_by_dedupe_key(&self, dedupe_key: &str) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn event_id_by_seq(&self, seq: u64) -> Result<Uuid> {
        self.conn
            .query_row(
                "SELECT id FROM events WHERE seq = ?1",
                params![seq as i64],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub fn get_event(&self, id: Uuid) -> Result<Event> {
        self.conn
            .query_row(
                event_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                event_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn events_by_id_prefix(&self, prefix: &str) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("WHERE id LIKE ?1 ORDER BY id LIMIT 2").as_str())?;
        let rows = stmt.query_map(params![format!("{prefix}%")], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session_limited(&self, session_id: Uuid, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms LIMIT ?2")
                .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                session_id.to_string(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            event_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn events_for_session_window(
        &self,
        event: &Event,
        before: usize,
        after: usize,
    ) -> Result<Vec<Event>> {
        let Some(session_id) = event.session_id else {
            return Ok(vec![event.clone()]);
        };
        let event_seq = i64::try_from(event.seq).unwrap_or(i64::MAX);
        let mut events = if before == 0 {
            Vec::new()
        } else {
            let mut stmt = self.conn.prepare(
                    event_select_sql(
                        "WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC, occurred_at_ms DESC LIMIT ?3",
                    )
                    .as_str(),
                )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(before).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            let mut rows = collect_rows(rows)?;
            rows.reverse();
            rows
        };
        events.push(event.clone());
        if after > 0 {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq > ?2 ORDER BY seq, occurred_at_ms LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(after).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            events.extend(collect_rows(rows)?);
        }
        Ok(events)
    }

    pub fn events_for_record(&self, record_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
                event_select_sql(
                    r#"
                    WHERE history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       OR run_id IN (
                            SELECT id FROM runs
                            WHERE history_record_id = ?1
                               OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       )
                    ORDER BY seq, occurred_at_ms
                    "#,
                )
                .as_str(),
            )?;
        let rows = stmt.query_map(params![record_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn list_events(&self) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("ORDER BY seq, occurred_at_ms, id").as_str())?;
        let rows = stmt.query_map([], event_from_row)?;
        collect_rows(rows)
    }

    pub fn max_events_per_history_record(&self) -> Result<i64> {
        let max_events = self.conn.query_row(
            r#"
                SELECT COALESCE(MAX(event_count), 0)
                FROM (
                    SELECT COUNT(*) AS event_count
                    FROM events
                    GROUP BY history_record_id
                )
                "#,
            [],
            |row| row.get(0),
        )?;
        Ok(max_events)
    }

    pub fn has_at_least_events(&self, threshold: i64) -> Result<bool> {
        if threshold <= 0 {
            return Ok(true);
        }
        let exists = self.conn.query_row(
            r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM events
                    LIMIT 1 OFFSET ?1
                )
                "#,
            params![threshold - 1],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }
}

pub(crate) fn reject_provider_event_hash_conflict(
    conn: &Connection,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = conn.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_tx(
    tx: &Transaction<'_>,
    dedupe_key: &str,
) -> Result<()> {
    let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    let prefix = provider_event_dedupe_key_prefix(&parsed);
    let upper_bound = provider_event_dedupe_key_upper_bound(&prefix);
    let mut stmt = tx.prepare(
        "SELECT dedupe_key FROM events
         WHERE dedupe_key >= ?1 AND dedupe_key < ?2
         ORDER BY dedupe_key",
    )?;
    let rows = stmt.query_map(params![prefix, upper_bound], |row| row.get::<_, String>(0))?;
    reject_provider_event_hash_conflict_from_rows(dedupe_key, rows)
}

pub(crate) fn reject_provider_event_hash_conflict_from_rows(
    dedupe_key: &str,
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>>,
) -> Result<()> {
    let Some(incoming) = parse_provider_event_dedupe_key(dedupe_key) else {
        return Ok(());
    };
    for row in rows {
        let existing_key = row?;
        let Some(existing) = parse_provider_event_dedupe_key(&existing_key) else {
            continue;
        };
        if existing.has_same_event_identity(&incoming)
            && existing.payload_hash != incoming.payload_hash
        {
            return Err(StoreError::ProviderEventConflict {
                provider: incoming.provider,
                external_session_id: incoming.external_session_id,
                provider_index: incoming.provider_index,
                existing_hash: existing.payload_hash,
                new_hash: incoming.payload_hash,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ParsedProviderEventDedupeKey {
    pub(crate) provider: String,
    pub(crate) external_session_id: String,
    pub(crate) source_id: Option<String>,
    pub(crate) provider_index: u64,
    pub(crate) payload_hash: String,
}

impl ParsedProviderEventDedupeKey {
    fn has_same_event_identity(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.external_session_id == other.external_session_id
            && self.source_id == other.source_id
            && self.provider_index == other.provider_index
    }
}

fn provider_event_dedupe_key_prefix(parsed: &ParsedProviderEventDedupeKey) -> String {
    if let Some(source_id) = &parsed.source_id {
        format!("provider-source:{source_id}:{}:", parsed.provider_index)
    } else {
        format!(
            "provider:{}:{}:{}:",
            parsed.provider, parsed.external_session_id, parsed.provider_index
        )
    }
}

fn provider_event_dedupe_key_upper_bound(prefix: &str) -> String {
    let mut upper_bound = prefix.to_owned();
    upper_bound.push(char::MAX);
    upper_bound
}

pub(crate) fn parse_provider_event_dedupe_key(
    dedupe_key: &str,
) -> Option<ParsedProviderEventDedupeKey> {
    if let Some(rest) = dedupe_key.strip_prefix("provider-source:") {
        let mut parts = rest.splitn(3, ':');
        let source_id = parts.next()?.to_owned();
        let provider_index = parts.next()?.parse().ok()?;
        let payload_hash = parts.next()?.to_owned();
        if source_id.is_empty() || payload_hash.is_empty() {
            return None;
        }
        return Some(ParsedProviderEventDedupeKey {
            provider: "provider-source".to_owned(),
            external_session_id: source_id.clone(),
            source_id: Some(source_id),
            provider_index,
            payload_hash,
        });
    }

    let mut parts = dedupe_key.splitn(5, ':');
    let prefix = parts.next()?;
    if prefix != "provider" {
        return None;
    }
    let provider = parts.next()?.to_owned();
    let external_session_id = parts.next()?.to_owned();
    let provider_index = parts.next()?.parse().ok()?;
    let payload_hash = parts.next()?.to_owned();
    if provider.is_empty() || external_session_id.is_empty() || payload_hash.is_empty() {
        None
    } else {
        Some(ParsedProviderEventDedupeKey {
            provider,
            external_session_id,
            source_id: None,
            provider_index,
            payload_hash,
        })
    }
}

pub(crate) fn event_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM events {tail}"
    )
}

pub(crate) fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        seq: nonnegative_i64_to_u64(row.get(1)?)?,
        history_record_id: parse_optional_uuid(row.get(2)?)?,
        session_id: parse_optional_uuid(row.get(3)?)?,
        run_id: parse_optional_uuid(row.get(4)?)?,
        event_type: parse_text_enum::<EventType>(row.get::<_, String>(5)?)?,
        role: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<EventRole>)
            .transpose()?,
        occurred_at: ms_to_time(row.get(7)?)?,
        capture_source_id: parse_optional_uuid(row.get(8)?)?,
        payload: parse_json(row.get::<_, String>(9)?)?,
        payload_blob_id: parse_optional_uuid(row.get(10)?)?,
        dedupe_key: row.get(11)?,
        redaction_state: parse_text_enum::<RedactionState>(row.get::<_, String>(13)?)?,
        sync: sync_metadata_from_row(row, 12, 14, 15, 16, 17, 18)?,
    })
}
