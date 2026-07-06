use ctx_history_core::{EntityTimestamps, Run, RunStatus, RunType};
use rusqlite::params;
use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_ms_to_time, optional_timestamp_ms, optional_uuid_string,
    parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_run(&self, run: &Run) -> Result<()> {
        self.conn.execute(
                r#"
                INSERT INTO runs
                (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                ON CONFLICT(id) DO UPDATE SET
                    history_record_id = excluded.history_record_id,
                    session_id = excluded.session_id,
                    run_type = excluded.run_type,
                    status = excluded.status,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    exit_code = excluded.exit_code,
                    cwd = excluded.cwd,
                    command_preview = excluded.command_preview,
                    input_blob_id = excluded.input_blob_id,
                    output_blob_id = excluded.output_blob_id,
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
                    run.id.to_string(),
                    optional_uuid_string(run.history_record_id),
                    optional_uuid_string(run.session_id),
                    run.run_type.as_str(),
                    run.status.as_str(),
                    timestamp_ms(run.started_at),
                    optional_timestamp_ms(run.ended_at),
                    run.exit_code,
                    run.cwd.as_deref(),
                    run.command_preview.as_deref(),
                    optional_uuid_string(run.input_blob_id),
                    optional_uuid_string(run.output_blob_id),
                    timestamp_ms(run.timestamps.created_at),
                    timestamp_ms(run.timestamps.updated_at),
                    optional_uuid_string(run.source_id),
                    run.sync.visibility.as_str(),
                    run.sync.fidelity.as_str(),
                    run.sync.sync_state.as_str(),
                    run.sync.sync_version as i64,
                    optional_timestamp_ms(run.sync.deleted_at),
                    serde_json::to_string(&run.sync.metadata)?,
                ],
            )?;
        Ok(())
    }

    pub fn insert_run_if_absent(&self, run: &Run) -> Result<bool> {
        let changed = self
                .conn
                .prepare_cached(
                    r#"
                    INSERT OR IGNORE INTO runs
                    (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                    "#,
                )?
                .execute(params![
                    run.id.to_string(),
                    optional_uuid_string(run.history_record_id),
                    optional_uuid_string(run.session_id),
                    run.run_type.as_str(),
                    run.status.as_str(),
                    timestamp_ms(run.started_at),
                    optional_timestamp_ms(run.ended_at),
                    run.exit_code,
                    run.cwd.as_deref(),
                    run.command_preview.as_deref(),
                    optional_uuid_string(run.input_blob_id),
                    optional_uuid_string(run.output_blob_id),
                    timestamp_ms(run.timestamps.created_at),
                    timestamp_ms(run.timestamps.updated_at),
                    optional_uuid_string(run.source_id),
                    run.sync.visibility.as_str(),
                    run.sync.fidelity.as_str(),
                    run.sync.sync_state.as_str(),
                    run.sync.sync_version as i64,
                    optional_timestamp_ms(run.sync.deleted_at),
                    serde_json::to_string(&run.sync.metadata)?,
                ])?;
        Ok(changed > 0)
    }

    pub fn get_run(&self, id: Uuid) -> Result<Run> {
        self.conn
            .query_row(
                run_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                run_from_row,
            )
            .optional()?
            .ok_or(StoreError::NotFound(id))
    }

    pub fn runs_for_session(&self, session_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("WHERE session_id = ?1 ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map(params![session_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    pub fn runs_for_record(&self, record_id: Uuid) -> Result<Vec<Run>> {
        let mut stmt = self.conn.prepare(
            run_select_sql(
                r#"
                    WHERE history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                    ORDER BY started_at_ms, id
                    "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], run_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn list_runs(&self) -> Result<Vec<Run>> {
        let mut stmt = self
            .conn
            .prepare(run_select_sql("ORDER BY started_at_ms, id").as_str())?;
        let rows = stmt.query_map([], run_from_row)?;
        collect_rows(rows)
    }
}

pub(crate) fn run_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM runs {tail}"
    )
}

pub(crate) fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        session_id: parse_optional_uuid(row.get(2)?)?,
        run_type: parse_text_enum::<RunType>(row.get::<_, String>(3)?)?,
        status: parse_text_enum::<RunStatus>(row.get::<_, String>(4)?)?,
        started_at: ms_to_time(row.get(5)?)?,
        ended_at: optional_ms_to_time(row.get(6)?)?,
        exit_code: row.get(7)?,
        cwd: row.get(8)?,
        command_preview: row.get(9)?,
        input_blob_id: parse_optional_uuid(row.get(10)?)?,
        output_blob_id: parse_optional_uuid(row.get(11)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(12)?)?,
            updated_at: ms_to_time(row.get(13)?)?,
        },
        source_id: parse_optional_uuid(row.get(14)?)?,
        sync: sync_metadata_from_row(row, 15, 16, 17, 18, 19, 20)?,
    })
}
