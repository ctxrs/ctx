use ctx_history_core::{
    EntityTimestamps, Fidelity, SyncCursor, SyncMetadata, SyncState, Visibility,
};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    ms_to_time, nonnegative_i64_to_u64, optional_ms_to_time, optional_timestamp_ms, parse_json,
    parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_sync_cursor(&self, cursor: &SyncCursor) -> Result<Uuid> {
        if let Some(existing) =
            self.get_sync_cursor(cursor.team_id.as_deref(), &cursor.device_id, &cursor.stream)?
        {
            self.conn.execute(
                r#"
                    UPDATE sync_cursors
                    SET cursor = ?1, last_synced_at_ms = ?2, updated_at_ms = ?3
                    WHERE id = ?4
                    "#,
                params![
                    cursor.cursor.as_str(),
                    optional_timestamp_ms(cursor.last_synced_at),
                    timestamp_ms(cursor.timestamps.updated_at),
                    existing.id.to_string(),
                ],
            )?;
            return Ok(existing.id);
        }

        self.conn.execute(
                r#"
                INSERT INTO sync_cursors
                (id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(team_id, device_id, stream) DO UPDATE SET
                    cursor = excluded.cursor,
                    last_synced_at_ms = excluded.last_synced_at_ms,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                params![
                    cursor.id.to_string(),
                    cursor.team_id.as_deref(),
                    cursor.device_id.as_str(),
                    cursor.stream.as_str(),
                    cursor.cursor.as_str(),
                    optional_timestamp_ms(cursor.last_synced_at),
                    timestamp_ms(cursor.timestamps.created_at),
                    timestamp_ms(cursor.timestamps.updated_at),
                ],
            )?;
        self.conn
                .query_row(
                    "SELECT id FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                    params![cursor.team_id.as_deref(), cursor.device_id.as_str(), cursor.stream.as_str()],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .map_err(StoreError::from)
    }

    pub fn get_sync_cursor(
        &self,
        team_id: Option<&str>,
        device_id: &str,
        stream: &str,
    ) -> Result<Option<SyncCursor>> {
        self.conn
                .query_row(
                    "SELECT id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms FROM sync_cursors WHERE team_id IS ?1 AND device_id = ?2 AND stream = ?3",
                    params![team_id, device_id, stream],
                    sync_cursor_from_row,
                )
                .optional()
                .map_err(StoreError::from)
    }
}

fn sync_cursor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncCursor> {
    Ok(SyncCursor {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        team_id: row.get(1)?,
        device_id: row.get(2)?,
        stream: row.get(3)?,
        cursor: row.get(4)?,
        last_synced_at: optional_ms_to_time(row.get(5)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(6)?)?,
            updated_at: ms_to_time(row.get(7)?)?,
        },
    })
}

pub(crate) fn sync_metadata_from_row(
    row: &rusqlite::Row<'_>,
    visibility_index: usize,
    fidelity_index: usize,
    sync_state_index: usize,
    sync_version_index: usize,
    deleted_at_index: usize,
    metadata_index: usize,
) -> rusqlite::Result<SyncMetadata> {
    Ok(SyncMetadata {
        visibility: parse_text_enum::<Visibility>(row.get::<_, String>(visibility_index)?)?,
        fidelity: parse_text_enum::<Fidelity>(row.get::<_, String>(fidelity_index)?)?,
        sync_state: parse_text_enum::<SyncState>(row.get::<_, String>(sync_state_index)?)?,
        sync_version: nonnegative_i64_to_u64(row.get(sync_version_index)?)?,
        deleted_at: optional_ms_to_time(row.get(deleted_at_index)?)?,
        metadata: parse_json(row.get::<_, String>(metadata_index)?)?,
    })
}
