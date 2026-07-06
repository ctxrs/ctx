use ctx_history_core::{Artifact, ArtifactKind, EntityTimestamps, RedactionState};
use rusqlite::params;
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u64, optional_timestamp_ms, optional_uuid_string,
    parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_artifact(&self, artifact: &Artifact) -> Result<Uuid> {
        self.conn.execute(
                r#"
                INSERT INTO artifacts
                (id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                ON CONFLICT DO UPDATE SET
                    blob_path = excluded.blob_path,
                    byte_size = excluded.byte_size,
                    media_type = excluded.media_type,
                    preview_text = excluded.preview_text,
                    redaction_state = excluded.redaction_state,
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
                    artifact.id.to_string(),
                    artifact.kind.as_str(),
                    artifact.blob_hash.as_str(),
                    artifact.blob_path.as_str(),
                    artifact.byte_size as i64,
                    artifact.media_type.as_deref(),
                    artifact.preview_text.as_deref(),
                    artifact.redaction_state.as_str(),
                    timestamp_ms(artifact.timestamps.created_at),
                    timestamp_ms(artifact.timestamps.updated_at),
                    optional_uuid_string(artifact.source_id),
                    artifact.sync.visibility.as_str(),
                    artifact.sync.fidelity.as_str(),
                    artifact.sync.sync_state.as_str(),
                    artifact.sync.sync_version as i64,
                    optional_timestamp_ms(artifact.sync.deleted_at),
                    serde_json::to_string(&artifact.sync.metadata)?,
                ],
            )?;
        self.conn
            .query_row(
                "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
                params![artifact.blob_hash.as_str(), artifact.kind.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn list_artifacts(&self) -> Result<Vec<Artifact>> {
        let mut stmt = self
            .conn
            .prepare(artifact_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], artifact_from_row)?;
        collect_rows(rows)
    }

    pub fn artifacts_for_record(&self, record_id: Uuid) -> Result<Vec<Artifact>> {
        let mut stmt = self.conn.prepare(
            artifact_select_sql(
                r#"
                    WHERE id IN (
                        SELECT transcript_blob_id
                        FROM sessions
                        WHERE history_record_id = ?1 AND transcript_blob_id IS NOT NULL
                        UNION
                        SELECT input_blob_id
                        FROM runs
                        WHERE (history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                           AND input_blob_id IS NOT NULL
                        UNION
                        SELECT output_blob_id
                        FROM runs
                        WHERE (history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                           AND output_blob_id IS NOT NULL
                        UNION
                        SELECT payload_blob_id
                        FROM events
                        WHERE (history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                           AND payload_blob_id IS NOT NULL
                        UNION
                        SELECT target_id
                        FROM history_record_links
                        WHERE history_record_id = ?1 AND target_type = 'artifact'
                    )
                    ORDER BY updated_at_ms DESC, id
                    "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], artifact_from_row)?;
        collect_rows(rows)
    }
}

pub(crate) fn artifact_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM artifacts {tail}"
    )
}

pub(crate) fn artifact_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Artifact> {
    Ok(Artifact {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        kind: parse_text_enum::<ArtifactKind>(row.get::<_, String>(1)?)?,
        blob_hash: row.get(2)?,
        blob_path: row.get(3)?,
        byte_size: nonnegative_i64_to_u64(row.get(4)?)?,
        media_type: row.get(5)?,
        preview_text: row.get(6)?,
        redaction_state: parse_text_enum::<RedactionState>(row.get::<_, String>(7)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(8)?)?,
            updated_at: ms_to_time(row.get(9)?)?,
        },
        source_id: parse_optional_uuid(row.get(10)?)?,
        sync: sync_metadata_from_row(row, 11, 12, 13, 14, 15, 16)?,
    })
}
