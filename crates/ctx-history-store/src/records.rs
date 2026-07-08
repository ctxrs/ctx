use ctx_history_core::{EntityTimestamps, HistoryRecord, HistoryRecordLink};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_timestamp_ms, optional_uuid_string, parse_optional_uuid,
    parse_text_enum, parse_time, parse_uuid, timestamp_ms,
};
use crate::schema::ddl::table_exists;
use crate::search::analyzer::scriptgram_match_query;
use crate::search::projections::{
    event_scriptgram_table_ready, fts_match_query, record_scriptgram_table_ready,
    upsert_record_search_projection,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_history_record_link(&self, link: &HistoryRecordLink) -> Result<Uuid> {
        self.conn.execute(
                r#"
                INSERT INTO history_record_links
                (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
                    confidence = excluded.confidence,
                    source_id = excluded.source_id,
                    updated_at_ms = excluded.updated_at_ms,
                    visibility = excluded.visibility,
                    fidelity = excluded.fidelity,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    deleted_at_ms = excluded.deleted_at_ms,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    link.id.to_string(),
                    link.history_record_id.to_string(),
                    link.target_type.as_str(),
                    link.target_id.to_string(),
                    link.link_type.as_str(),
                    link.confidence.as_str(),
                    optional_uuid_string(link.source_id),
                    timestamp_ms(link.timestamps.created_at),
                    timestamp_ms(link.timestamps.updated_at),
                    link.sync.visibility.as_str(),
                    link.sync.fidelity.as_str(),
                    link.sync.sync_state.as_str(),
                    link.sync.sync_version as i64,
                    optional_timestamp_ms(link.sync.deleted_at),
                    serde_json::to_string(&link.sync.metadata)?,
                ],
            )?;
        self.conn
                .query_row(
                    "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
                    params![
                        link.history_record_id.to_string(),
                        link.target_type.as_str(),
                        link.target_id.to_string(),
                        link.link_type.as_str()
                    ],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .map_err(StoreError::from)
    }

    pub(crate) fn list_history_record_links(&self) -> Result<Vec<HistoryRecordLink>> {
        let mut stmt = self
            .conn
            .prepare(history_record_link_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], history_record_link_from_row)?;
        collect_rows(rows)
    }

    pub fn insert_record(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
                INSERT INTO history_records
                (
                    id, title, summary, status, started_at_ms, last_activity_at_ms,
                    created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn upsert_record(&self, record: &HistoryRecord) -> Result<()> {
        self.upsert_record_row(record)?;
        upsert_record_search_projection(&self.conn, record)?;
        Ok(())
    }

    pub fn delete_orphan_record(&self, record_id: Uuid) -> Result<bool> {
        let record_id = record_id.to_string();
        let deleted = self.conn.execute(
            r#"
            DELETE FROM history_records
            WHERE id = ?1
              AND NOT EXISTS (SELECT 1 FROM sessions WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM runs WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM events WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM history_record_links WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM summaries WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM files_touched WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM history_record_tags WHERE history_record_id = ?1)
              AND NOT EXISTS (SELECT 1 FROM record_edges WHERE from_record_id = ?1 OR to_record_id = ?1)
            "#,
            params![&record_id],
        )?;
        if deleted > 0 && table_exists(&self.conn, "ctx_history_search")? {
            self.conn.execute(
                "DELETE FROM ctx_history_search WHERE record_id = ?1",
                params![&record_id],
            )?;
        }
        Ok(deleted > 0)
    }

    pub fn upsert_records(&self, records: &[HistoryRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        self.begin_immediate_batch()?;
        for record in records {
            if let Err(err) = self.upsert_record_row(record) {
                let _ = self.rollback_batch();
                return Err(err);
            }
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        for record in records {
            upsert_record_search_projection(&self.conn, record)?;
        }
        Ok(())
    }

    fn upsert_record_row(&self, record: &HistoryRecord) -> Result<()> {
        let created_at_ms = timestamp_ms(record.created_at);
        let updated_at_ms = timestamp_ms(record.updated_at);
        self.conn.execute(
            r#"
                INSERT INTO history_records
                (
                    id, title, summary, status, started_at_ms, last_activity_at_ms,
                    created_at_ms, updated_at_ms, body, tags_json, kind, workspace,
                    created_at, updated_at
                )
                VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(id) DO UPDATE SET
                    title = excluded.title,
                    summary = excluded.summary,
                    status = excluded.status,
                    started_at_ms = excluded.started_at_ms,
                    last_activity_at_ms = excluded.last_activity_at_ms,
                    created_at_ms = excluded.created_at_ms,
                    updated_at_ms = excluded.updated_at_ms,
                    body = excluded.body,
                    tags_json = excluded.tags_json,
                    kind = excluded.kind,
                    workspace = excluded.workspace,
                    created_at = excluded.created_at,
                    updated_at = excluded.updated_at
                "#,
            params![
                record.id.to_string(),
                record.title,
                record.body,
                created_at_ms,
                updated_at_ms,
                record.body,
                serde_json::to_string(&record.tags)?,
                record.kind,
                record.workspace,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_record(&self, id: Uuid) -> Result<HistoryRecord> {
        self.conn
            .query_row(
                record_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                record_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn list_records(&self, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.list_records_page(limit, 0)
    }

    pub fn list_records_page(&self, limit: usize, offset: usize) -> Result<Vec<HistoryRecord>> {
        let mut stmt = self.conn.prepare(
            record_select_sql("ORDER BY created_at DESC, id LIMIT ?1 OFFSET ?2").as_str(),
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    pub fn search_records(&self, query: &str, limit: usize) -> Result<Vec<HistoryRecord>> {
        self.search_records_page(query, limit, 0)
    }

    pub fn search_records_page(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<HistoryRecord>> {
        if fts_match_query(query).is_none() && scriptgram_match_query(query).is_none() {
            return Ok(Vec::new());
        }
        if let Some(records) = self.search_records_fts(query, limit, offset)? {
            return Ok(records);
        }
        let like = format!("%{query}%");
        let mut stmt = self.conn.prepare(
                record_select_sql(
                    "WHERE title LIKE ?1 OR body LIKE ?1 OR tags_json LIKE ?1 ORDER BY created_at DESC, id LIMIT ?2 OFFSET ?3",
                )
                .as_str(),
            )?;
        let rows = stmt.query_map(params![like, limit as i64, offset as i64], record_from_row)?;
        collect_rows(rows)
    }

    fn search_records_fts(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<Vec<HistoryRecord>>> {
        if !table_exists(&self.conn, "ctx_history_search")? {
            return Ok(None);
        }
        let match_query = fts_match_query(query);
        let has_event_search = table_exists(&self.conn, "event_search")?;
        let has_artifact_search = table_exists(&self.conn, "artifact_search")?;
        let has_record_scriptgram = record_scriptgram_table_ready(&self.conn)?;
        let has_event_scriptgram = event_scriptgram_table_ready(&self.conn)?;
        let scriptgram_query = if has_record_scriptgram || has_event_scriptgram {
            scriptgram_match_query(query)
        } else {
            None
        };
        match (match_query, scriptgram_query) {
            (Some(match_query), Some(scriptgram_query)) => {
                let mut selects = vec![r#"
                    SELECT record_id, bm25(ctx_history_search)
                    FROM ctx_history_search
                    WHERE ctx_history_search MATCH ?1
                    "#
                .to_owned()];
                if has_event_search && has_artifact_search {
                    selects.push(
                        r#"
                        SELECT history_record_id, bm25(event_search)
                        FROM event_search
                        WHERE event_search MATCH ?1 AND history_record_id IS NOT NULL
                        "#
                        .to_owned(),
                    );
                    selects.push(
                        r#"
                        SELECT history_record_id, bm25(artifact_search)
                        FROM artifact_search
                        WHERE artifact_search MATCH ?1 AND history_record_id IS NOT NULL
                        "#
                        .to_owned(),
                    );
                }
                if has_record_scriptgram {
                    selects.push(
                        r#"
                        SELECT record_id, bm25(ctx_history_search_scriptgram) + 0.35
                        FROM ctx_history_search_scriptgram
                        WHERE ctx_history_search_scriptgram MATCH ?2
                        "#
                        .to_owned(),
                    );
                }
                if has_event_scriptgram {
                    selects.push(
                        r#"
                        SELECT history_record_id, bm25(event_search_scriptgram) + 0.35
                        FROM event_search_scriptgram
                        WHERE event_search_scriptgram MATCH ?2 AND history_record_id IS NOT NULL
                        "#
                        .to_owned(),
                    );
                }
                let sql = format!(
                    r#"
                    WITH matches(record_id, score) AS (
                        {}
                    )
                    SELECT record_id
                    FROM matches
                    WHERE record_id IS NOT NULL
                    GROUP BY record_id
                    ORDER BY MIN(score), record_id
                    LIMIT ?3 OFFSET ?4
                    "#,
                    selects.join(" UNION ALL ")
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(
                    params![match_query, scriptgram_query, limit as i64, offset as i64],
                    |row| row.get::<_, String>(0),
                )?;
                let mut records = Vec::new();
                for row in rows {
                    records.push(self.get_record(parse_uuid(row?)?)?);
                }
                Ok(Some(records))
            }
            (Some(match_query), None) => {
                let records = if has_event_search && has_artifact_search {
                    r#"
                    WITH matches(record_id, score) AS (
                        SELECT record_id, bm25(ctx_history_search)
                        FROM ctx_history_search
                        WHERE ctx_history_search MATCH ?1
                        UNION ALL
                        SELECT history_record_id, bm25(event_search)
                        FROM event_search
                        WHERE event_search MATCH ?1 AND history_record_id IS NOT NULL
                        UNION ALL
                        SELECT history_record_id, bm25(artifact_search)
                        FROM artifact_search
                        WHERE artifact_search MATCH ?1 AND history_record_id IS NOT NULL
                    )
                    SELECT record_id
                    FROM matches
                    WHERE record_id IS NOT NULL
                    GROUP BY record_id
                    ORDER BY MIN(score), record_id
                    LIMIT ?2 OFFSET ?3
                    "#
                } else {
                    r#"
                    SELECT record_id
                    FROM ctx_history_search
                    WHERE ctx_history_search MATCH ?1
                    ORDER BY bm25(ctx_history_search), record_id
                    LIMIT ?2 OFFSET ?3
                    "#
                };
                let mut stmt = self.conn.prepare(records)?;
                let rows = stmt
                    .query_map(params![match_query, limit as i64, offset as i64], |row| {
                        row.get::<_, String>(0)
                    })?;
                let mut records = Vec::new();
                for row in rows {
                    records.push(self.get_record(parse_uuid(row?)?)?);
                }
                Ok(Some(records))
            }
            (None, Some(_)) => self.search_records_scriptgram(query, limit, offset),
            (None, None) => Ok(Some(Vec::new())),
        }
    }

    fn search_records_scriptgram(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Option<Vec<HistoryRecord>>> {
        let has_record_scriptgram = record_scriptgram_table_ready(&self.conn)?;
        let has_event_scriptgram = event_scriptgram_table_ready(&self.conn)?;
        if !has_record_scriptgram && !has_event_scriptgram {
            return Ok(Some(Vec::new()));
        }
        let Some(match_query) = scriptgram_match_query(query) else {
            return Ok(Some(Vec::new()));
        };
        let sql = match (has_record_scriptgram, has_event_scriptgram) {
            (true, true) => {
                r#"
                WITH matches(record_id, score) AS (
                    SELECT record_id, bm25(ctx_history_search_scriptgram) + 0.35
                    FROM ctx_history_search_scriptgram
                    WHERE ctx_history_search_scriptgram MATCH ?1
                    UNION ALL
                    SELECT history_record_id, bm25(event_search_scriptgram) + 0.35
                    FROM event_search_scriptgram
                    WHERE event_search_scriptgram MATCH ?1 AND history_record_id IS NOT NULL
                )
                SELECT record_id
                FROM matches
                WHERE record_id IS NOT NULL
                GROUP BY record_id
                ORDER BY MIN(score), record_id
                LIMIT ?2 OFFSET ?3
                "#
            }
            (true, false) => {
                r#"
                SELECT record_id
                FROM ctx_history_search_scriptgram
                WHERE ctx_history_search_scriptgram MATCH ?1
                ORDER BY bm25(ctx_history_search_scriptgram), record_id
                LIMIT ?2 OFFSET ?3
                "#
            }
            (false, true) => {
                r#"
                SELECT history_record_id
                FROM event_search_scriptgram
                WHERE event_search_scriptgram MATCH ?1 AND history_record_id IS NOT NULL
                GROUP BY history_record_id
                ORDER BY MIN(bm25(event_search_scriptgram)), history_record_id
                LIMIT ?2 OFFSET ?3
                "#
            }
            (false, false) => unreachable!("scriptgram tables checked above"),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![match_query, limit as i64, offset as i64], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for row in rows {
            records.push(self.get_record(parse_uuid(row?)?)?);
        }
        Ok(Some(records))
    }
}

pub(crate) fn history_record_link_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM history_record_links {tail}"
    )
}

pub(crate) fn history_record_link_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<HistoryRecordLink> {
    Ok(HistoryRecordLink {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_uuid(row.get::<_, String>(1)?)?,
        target_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkTargetType>(
            row.get::<_, String>(2)?,
        )?,
        target_id: parse_uuid(row.get::<_, String>(3)?)?,
        link_type: parse_text_enum::<ctx_history_core::HistoryRecordLinkType>(
            row.get::<_, String>(4)?,
        )?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(5)?)?,
        source_id: parse_optional_uuid(row.get(6)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        sync: sync_metadata_from_row(row, 9, 10, 11, 12, 13, 14)?,
    })
}

pub(crate) fn record_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, title, body, tags_json, kind, workspace, created_at, updated_at FROM history_records {tail}"
    )
}

pub(crate) fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecord> {
    let tags_json: String = row.get(3)?;
    Ok(HistoryRecord {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        title: row.get(1)?,
        body: row.get(2)?,
        tags: serde_json::from_str(&tags_json)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        kind: row.get(4)?,
        workspace: row.get(5)?,
        created_at: parse_time(row.get::<_, String>(6)?)?,
        updated_at: parse_time(row.get::<_, String>(7)?)?,
    })
}
