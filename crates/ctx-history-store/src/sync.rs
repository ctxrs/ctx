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

    pub fn compare_and_set_sync_cursor(
        &self,
        expected: Option<&SyncCursor>,
        cursor: &SyncCursor,
    ) -> Result<bool> {
        let changed = match expected {
            Some(expected)
                if expected.team_id == cursor.team_id
                    && expected.device_id == cursor.device_id
                    && expected.stream == cursor.stream =>
            {
                self.conn.execute(
                    r#"
                    UPDATE sync_cursors
                    SET cursor = ?1, last_synced_at_ms = ?2, updated_at_ms = ?3
                    WHERE id = ?4
                      AND team_id IS ?5
                      AND device_id = ?6
                      AND stream = ?7
                      AND cursor = ?8
                      AND last_synced_at_ms IS ?9
                      AND created_at_ms = ?10
                      AND updated_at_ms = ?11
                    "#,
                    params![
                        cursor.cursor.as_str(),
                        optional_timestamp_ms(cursor.last_synced_at),
                        timestamp_ms(cursor.timestamps.updated_at),
                        expected.id.to_string(),
                        expected.team_id.as_deref(),
                        expected.device_id.as_str(),
                        expected.stream.as_str(),
                        expected.cursor.as_str(),
                        optional_timestamp_ms(expected.last_synced_at),
                        timestamp_ms(expected.timestamps.created_at),
                        timestamp_ms(expected.timestamps.updated_at),
                    ],
                )?
            }
            Some(_) => return Ok(false),
            None => self.conn.execute(
                r#"
                INSERT OR IGNORE INTO sync_cursors
                    (id, team_id, device_id, stream, cursor, last_synced_at_ms, created_at_ms, updated_at_ms)
                SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
                WHERE NOT EXISTS (
                    SELECT 1
                    FROM sync_cursors
                    WHERE team_id IS ?2 AND device_id = ?3 AND stream = ?4
                )
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
            )?,
        };
        Ok(changed == 1)
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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use tempfile::TempDir;

    use super::*;

    fn test_cursor(value: &str, offset_seconds: i64) -> SyncCursor {
        let created_at = DateTime::<Utc>::UNIX_EPOCH + TimeDelta::seconds(10);
        let updated_at = created_at + TimeDelta::seconds(offset_seconds);
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: "test-machine".to_owned(),
            stream: "provider:test:source:test".to_owned(),
            cursor: value.to_owned(),
            last_synced_at: Some(updated_at),
            timestamps: EntityTimestamps {
                created_at,
                updated_at,
            },
        }
    }

    fn test_store() -> (TempDir, Store) {
        let temp = tempfile::tempdir().expect("create store temp directory");
        let store = Store::open(temp.path().join("history.db")).expect("open store");
        (temp, store)
    }

    #[test]
    fn compare_and_set_sync_cursor_inserts_only_when_absent() {
        let (_temp, store) = test_store();
        let cursor = test_cursor("first", 0);

        assert!(store
            .compare_and_set_sync_cursor(None, &cursor)
            .expect("insert absent cursor"));
        assert!(!store
            .compare_and_set_sync_cursor(None, &cursor)
            .expect("reject stale absent cursor"));

        assert_eq!(
            store
                .get_sync_cursor(None, &cursor.device_id, &cursor.stream)
                .expect("load cursor"),
            Some(cursor)
        );
    }

    #[test]
    fn compare_and_set_sync_cursor_updates_exact_prior_row() {
        let (_temp, store) = test_store();
        let first = test_cursor("first", 0);
        store
            .upsert_sync_cursor(&first)
            .expect("insert initial cursor");
        let expected = store
            .get_sync_cursor(None, &first.device_id, &first.stream)
            .expect("load initial cursor")
            .expect("initial cursor exists");
        let mut next = test_cursor("second", 1);
        next.device_id.clone_from(&expected.device_id);
        next.stream.clone_from(&expected.stream);

        assert!(store
            .compare_and_set_sync_cursor(Some(&expected), &next)
            .expect("publish next cursor"));

        let stored = store
            .get_sync_cursor(None, &next.device_id, &next.stream)
            .expect("load next cursor")
            .expect("next cursor exists");
        assert_eq!(stored.id, expected.id);
        assert_eq!(stored.timestamps.created_at, expected.timestamps.created_at);
        assert_eq!(stored.cursor, next.cursor);
        assert_eq!(stored.last_synced_at, next.last_synced_at);
        assert_eq!(stored.timestamps.updated_at, next.timestamps.updated_at);
    }

    #[test]
    fn compare_and_set_sync_cursor_rejects_stale_or_inexact_prior_row() {
        let (_temp, store) = test_store();
        let first = test_cursor("first", 0);
        store
            .upsert_sync_cursor(&first)
            .expect("insert initial cursor");
        let expected = store
            .get_sync_cursor(None, &first.device_id, &first.stream)
            .expect("load initial cursor")
            .expect("initial cursor exists");
        let mut next = test_cursor("second", 1);
        next.device_id.clone_from(&expected.device_id);
        next.stream.clone_from(&expected.stream);
        assert!(store
            .compare_and_set_sync_cursor(Some(&expected), &next)
            .expect("publish next cursor"));

        let mut stale_metadata = expected.clone();
        stale_metadata.timestamps.updated_at += TimeDelta::milliseconds(1);
        let mut third = test_cursor("third", 2);
        third.device_id.clone_from(&expected.device_id);
        third.stream.clone_from(&expected.stream);
        assert!(!store
            .compare_and_set_sync_cursor(Some(&expected), &third)
            .expect("reject stale cursor"));
        assert!(!store
            .compare_and_set_sync_cursor(Some(&stale_metadata), &third)
            .expect("reject inexact cursor"));

        let stored = store
            .get_sync_cursor(None, &next.device_id, &next.stream)
            .expect("load current cursor")
            .expect("current cursor exists");
        assert_eq!(stored.cursor, "second");
    }

    #[test]
    fn compare_and_set_sync_cursor_insert_is_single_winner_across_connections() {
        let temp = tempfile::tempdir().expect("create store temp directory");
        let path = temp.path().join("history.db");
        let first = Store::open(&path).expect("open first store");
        let second = Store::open(&path).expect("open second store");
        let cursor = test_cursor("first", 0);

        assert!(first
            .compare_and_set_sync_cursor(None, &cursor)
            .expect("first cursor insert wins"));
        assert!(!second
            .compare_and_set_sync_cursor(None, &cursor)
            .expect("second cursor insert loses"));
        assert_eq!(
            second
                .get_sync_cursor(None, &cursor.device_id, &cursor.stream)
                .expect("load winning cursor")
                .expect("winning cursor exists")
                .cursor,
            "first"
        );
    }

    #[test]
    fn compare_and_set_sync_cursor_rolls_back_with_enclosing_transaction() {
        let (_temp, store) = test_store();
        let cursor = test_cursor("first", 0);

        store.begin_immediate_batch().expect("begin transaction");
        assert!(store
            .compare_and_set_sync_cursor(None, &cursor)
            .expect("insert cursor in transaction"));
        store.rollback_batch().expect("rollback transaction");

        assert!(store
            .get_sync_cursor(None, &cursor.device_id, &cursor.stream)
            .expect("load cursor after rollback")
            .is_none());
    }
}
