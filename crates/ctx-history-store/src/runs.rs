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

pub(crate) fn provider_output_run_is_retained_failure(run: &Run) -> bool {
    let is_provider_output_run = run
        .sync
        .metadata
        .get("source")
        .and_then(serde_json::Value::as_str)
        == Some("provider_command_output");
    !is_provider_output_run || matches!(run.status, RunStatus::Failed | RunStatus::Cancelled)
}

impl Store {
    pub fn upsert_run(&self, run: &Run) -> Result<()> {
        self.with_import_batch_write(|| {
            if !provider_output_run_is_retained_failure(run) {
                return Ok(());
            }
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
            self.journal_run_mutated(run.id)?;
            Ok(())
        })
    }

    pub fn insert_run_if_absent(&self, run: &Run) -> Result<bool> {
        self.with_import_batch_write(|| {
            if !provider_output_run_is_retained_failure(run) {
                return Ok(false);
            }
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
            if changed > 0 {
                self.journal_run_mutated(run.id)?;
            }
            Ok(changed > 0)
        })
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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use ctx_history_core::{Fidelity, SyncMetadata};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn provider_output_run(id: Uuid, status: RunStatus) -> Run {
        let now: DateTime<Utc> = "2026-07-23T00:00:00Z".parse().unwrap();
        Run {
            id,
            history_record_id: None,
            session_id: None,
            run_type: RunType::Command,
            status,
            started_at: now,
            ended_at: Some(now),
            exit_code: None,
            cwd: None,
            command_preview: Some("cargo test".to_owned()),
            input_blob_id: None,
            output_blob_id: None,
            timestamps: EntityTimestamps {
                created_at: now,
                updated_at: now,
            },
            source_id: None,
            sync: SyncMetadata {
                fidelity: Fidelity::Imported,
                metadata: json!({"source": "provider_command_output"}),
                ..SyncMetadata::default()
            },
        }
    }

    #[test]
    fn provider_output_success_and_partial_runs_are_elided_but_failures_remain() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let success = provider_output_run(Uuid::new_v4(), RunStatus::Succeeded);
        let partial = provider_output_run(Uuid::new_v4(), RunStatus::Partial);
        let failure = provider_output_run(Uuid::new_v4(), RunStatus::Failed);

        store.upsert_run(&success).unwrap();
        assert!(!store.insert_run_if_absent(&partial).unwrap());
        assert!(store.insert_run_if_absent(&failure).unwrap());

        assert!(matches!(
            store.get_run(success.id),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.get_run(partial.id),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(store.get_run(failure.id).unwrap().status, RunStatus::Failed);
    }
}
