use ctx_history_core::{EntityTimestamps, Summary};
use rusqlite::params;
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_timestamp_ms, optional_uuid_string, parse_optional_uuid,
    parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store};

impl Store {
    pub fn upsert_summary(&self, summary: &Summary) -> Result<()> {
        self.conn.execute(
                r#"
                INSERT INTO summaries
                (id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
                ON CONFLICT(id) DO UPDATE SET
                    history_record_id = excluded.history_record_id,
                    session_id = excluded.session_id,
                    kind = excluded.kind,
                    model_or_source = excluded.model_or_source,
                    text = excluded.text,
                    citations_json = excluded.citations_json,
                    updated_at_ms = excluded.updated_at_ms,
                    source_id = excluded.source_id,
                    visibility = excluded.visibility,
                    fidelity = excluded.fidelity,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    deleted_at_ms = excluded.deleted_at_ms,
                    metadata_json = excluded.metadata_json
                "#,
                params![
                    summary.id.to_string(),
                    optional_uuid_string(summary.history_record_id),
                    optional_uuid_string(summary.session_id),
                    summary.kind.as_str(),
                    summary.model_or_source.as_deref(),
                    summary.text.as_str(),
                    serde_json::to_string(&summary.citations)?,
                    timestamp_ms(summary.timestamps.created_at),
                    timestamp_ms(summary.timestamps.updated_at),
                    optional_uuid_string(summary.source_id),
                    summary.sync.visibility.as_str(),
                    summary.sync.fidelity.as_str(),
                    summary.sync.sync_state.as_str(),
                    summary.sync.sync_version as i64,
                    optional_timestamp_ms(summary.sync.deleted_at),
                    serde_json::to_string(&summary.sync.metadata)?,
                ],
            )?;
        Ok(())
    }

    pub(crate) fn list_summaries(&self) -> Result<Vec<Summary>> {
        let mut stmt = self
            .conn
            .prepare(summary_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], summary_from_row)?;
        collect_rows(rows)
    }

    pub fn summaries_for_record(&self, record_id: Uuid) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            summary_select_sql(
                r#"
                    WHERE history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    ORDER BY updated_at_ms DESC, id
                    "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], summary_from_row)?;
        collect_rows(rows)
    }
}

pub(crate) fn summary_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM summaries {tail}"
    )
}

pub(crate) fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    Ok(Summary {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        kind: parse_text_enum::<ctx_history_core::SummaryKind>(row.get::<_, String>(3)?)?,
        model_or_source: row.get(4)?,
        text: row.get(5)?,
        citations: serde_json::from_str(&row.get::<_, String>(6)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(7)?)?,
            updated_at: ms_to_time(row.get(8)?)?,
        },
        source_id: parse_optional_uuid(row.get(9)?)?,
        sync: sync_metadata_from_row(row, 10, 11, 12, 13, 14, 15)?,
    })
}
