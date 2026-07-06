use ctx_history_core::{EntityTimestamps, FileTouched};
use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_timestamp_ms, optional_uuid_string, parse_optional_uuid,
    parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTouchScope {
    pub history_record_ids: BTreeSet<Uuid>,
    pub session_ids: BTreeSet<Uuid>,
    pub run_ids: BTreeSet<Uuid>,
    pub event_ids: BTreeSet<Uuid>,
    pub source_ids: BTreeSet<Uuid>,
}

impl FileTouchScope {
    pub fn is_empty(&self) -> bool {
        self.history_record_ids.is_empty()
            && self.session_ids.is_empty()
            && self.run_ids.is_empty()
            && self.event_ids.is_empty()
            && self.source_ids.is_empty()
    }
}

impl Store {
    pub fn upsert_file_touched(&self, file: &FileTouched) -> Result<()> {
        self.conn.execute(
                r#"
                INSERT INTO files_touched
                (id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
                ON CONFLICT(id) DO UPDATE SET
                    history_record_id = excluded.history_record_id,
                    run_id = excluded.run_id,
                    event_id = excluded.event_id,
                    vcs_workspace_id = excluded.vcs_workspace_id,
                    path = excluded.path,
                    change_kind = excluded.change_kind,
                    old_path = excluded.old_path,
                    line_count_delta = excluded.line_count_delta,
                    confidence = excluded.confidence,
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
                    file.id.to_string(),
                    optional_uuid_string(file.history_record_id),
                    optional_uuid_string(file.run_id),
                    optional_uuid_string(file.event_id),
                    optional_uuid_string(file.vcs_workspace_id),
                    file.path.as_str(),
                    file.change_kind.map(|kind| kind.as_str()),
                    file.old_path.as_deref(),
                    file.line_count_delta,
                    file.confidence.as_str(),
                    timestamp_ms(file.timestamps.created_at),
                    timestamp_ms(file.timestamps.updated_at),
                    optional_uuid_string(file.source_id),
                    file.sync.visibility.as_str(),
                    file.sync.fidelity.as_str(),
                    file.sync.sync_state.as_str(),
                    file.sync.sync_version as i64,
                    optional_timestamp_ms(file.sync.deleted_at),
                    serde_json::to_string(&file.sync.metadata)?,
                ],
            )?;
        Ok(())
    }

    pub fn file_touched_exists(&self, id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM files_touched WHERE id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub(crate) fn list_files_touched(&self) -> Result<Vec<FileTouched>> {
        let mut stmt = self
            .conn
            .prepare(file_touched_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn files_touched_for_record(&self, record_id: Uuid) -> Result<Vec<FileTouched>> {
        let mut stmt = self.conn.prepare(
                file_touched_select_sql(
                    r#"
                    WHERE history_record_id = ?1
                       OR run_id IN (
                            SELECT id FROM runs
                            WHERE history_record_id = ?1
                               OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       )
                       OR event_id IN (
                            SELECT id FROM events
                            WHERE history_record_id = ?1
                               OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                       )
                    ORDER BY updated_at_ms DESC, id
                    "#,
                )
                .as_str(),
            )?;
        let rows = stmt.query_map(params![record_id.to_string()], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn files_touched_for_record_matching(
        &self,
        record_id: Uuid,
        file: &str,
    ) -> Result<Vec<FileTouched>> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(Vec::new());
        };
        let mut stmt = self.conn.prepare(
                file_touched_select_sql(
                    r#"
                    WHERE (
                        history_record_id = ?1
                        OR run_id IN (
                             SELECT id FROM runs
                             WHERE history_record_id = ?1
                                OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                        )
                        OR event_id IN (
                             SELECT id FROM events
                             WHERE history_record_id = ?1
                                OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                        )
                    )
                    AND (
                        path = ?2
                        OR old_path = ?2
                        OR path LIKE ?3 ESCAPE '\'
                        OR old_path LIKE ?3 ESCAPE '\'
                    )
                    ORDER BY updated_at_ms DESC, id
                    "#,
                )
                .as_str(),
            )?;
        let rows = stmt.query_map(
            params![record_id.to_string(), exact, suffix],
            file_touched_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn file_touch_scope(&self, file: &str) -> Result<FileTouchScope> {
        let Some((exact, suffix)) = file_touch_match_values(file) else {
            return Ok(FileTouchScope::default());
        };
        let mut scope = FileTouchScope::default();
        let mut stmt = self.conn.prepare(
            r#"
                SELECT
                    COALESCE(
                        ft.history_record_id,
                        e.history_record_id,
                        r.history_record_id,
                        event_session.history_record_id,
                        run_session.history_record_id,
                        source_session.history_record_id
                    ),
                    COALESCE(e.session_id, r.session_id, source_session.id),
                    ft.run_id,
                    ft.event_id,
                    ft.source_id
                FROM files_touched ft
                LEFT JOIN events e ON e.id = ft.event_id
                LEFT JOIN runs r ON r.id = ft.run_id
                LEFT JOIN sessions event_session ON event_session.id = e.session_id
                LEFT JOIN sessions run_session ON run_session.id = r.session_id
                LEFT JOIN sessions source_session ON source_session.capture_source_id = ft.source_id
                WHERE ft.path = ?1
                   OR ft.old_path = ?1
                   OR ft.path LIKE ?2 ESCAPE '\'
                   OR ft.old_path LIKE ?2 ESCAPE '\'
                "#,
        )?;
        let rows = stmt.query_map(params![exact, suffix], |row| {
            Ok((
                parse_optional_uuid(row.get(0)?)?,
                parse_optional_uuid(row.get(1)?)?,
                parse_optional_uuid(row.get(2)?)?,
                parse_optional_uuid(row.get(3)?)?,
                parse_optional_uuid(row.get(4)?)?,
            ))
        })?;
        for row in rows {
            let (record_id, session_id, run_id, event_id, source_id) = row?;
            if let Some(id) = record_id {
                scope.history_record_ids.insert(id);
            }
            if let Some(id) = session_id {
                scope.session_ids.insert(id);
            }
            if let Some(id) = run_id {
                scope.run_ids.insert(id);
            }
            if let Some(id) = event_id {
                scope.event_ids.insert(id);
            }
            if let Some(id) = source_id {
                scope.source_ids.insert(id);
            }
        }
        Ok(scope)
    }
}

fn file_touch_match_values(file: &str) -> Option<(String, String)> {
    let exact = file.trim();
    if exact.is_empty() {
        return None;
    }
    let suffix = exact.trim_start_matches(['/', '\\']);
    Some((
        exact.to_owned(),
        format!("%/{}", escape_like_pattern(suffix)),
    ))
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

pub(crate) fn file_touched_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM files_touched {tail}"
    )
}

pub(crate) fn file_touched_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileTouched> {
    Ok(FileTouched {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        history_record_id: parse_optional_uuid(row.get(1)?)?,
        run_id: parse_optional_uuid(row.get(2)?)?,
        event_id: parse_optional_uuid(row.get(3)?)?,
        vcs_workspace_id: parse_optional_uuid(row.get(4)?)?,
        path: row.get(5)?,
        change_kind: row
            .get::<_, Option<String>>(6)?
            .map(parse_text_enum::<ctx_history_core::FileChangeKind>)
            .transpose()?,
        old_path: row.get(7)?,
        line_count_delta: row.get(8)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(9)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(10)?)?,
            updated_at: ms_to_time(row.get(11)?)?,
        },
        source_id: parse_optional_uuid(row.get(12)?)?,
        sync: sync_metadata_from_row(row, 13, 14, 15, 16, 17, 18)?,
    })
}
