use rusqlite::{params, Connection, OptionalExtension};
use tempfile::TempDir;

use super::{
    dto::{ZedNativeEvent, ZedNativePage, ZedNativeSession, ZedNativeSink},
    ZedNativePathError, ZedNativeResult,
};

const ZED_STAGING_BATCH_CANDIDATES: i64 = 1_024;
const ZED_STAGING_MAX_RELATIONSHIP_DEPTH: i64 = 1_024;

pub(super) struct ZedNativeStaging {
    connection: Connection,
    _directory: TempDir,
}

pub(super) struct ZedStagedSession {
    pub(super) session: ZedNativeSession,
    pub(super) parent_thread_id: Option<String>,
    pub(super) root_thread_id: String,
    pub(super) estimated_bytes: usize,
}

pub(super) struct ZedStagedEvent {
    pub(super) ordinal: u64,
    pub(super) event: ZedNativeEvent,
    pub(super) estimated_bytes: usize,
}

impl ZedNativeStaging {
    pub(super) fn new() -> ZedNativeResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix("ctx-zed-nativepath-stage-")
            .tempdir()?;
        let path = directory.path().join("stage.sqlite");
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=OFF;
             PRAGMA synchronous=OFF;
             PRAGMA temp_store=FILE;
             CREATE TABLE staged_sessions (
                 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
                 thread_id TEXT NOT NULL UNIQUE,
                 parent_thread_id TEXT,
                 payload_json TEXT NOT NULL,
                 estimated_bytes INTEGER NOT NULL
             );
             CREATE TABLE staged_events (
                 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
                 payload_json TEXT NOT NULL,
                 estimated_bytes INTEGER NOT NULL,
                 mutation_units INTEGER NOT NULL
             );
             CREATE TABLE staged_rejections (
                 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
                 reason TEXT NOT NULL
             );",
        )?;
        Ok(Self {
            connection,
            _directory: directory,
        })
    }

    pub(super) fn session_count(&self) -> ZedNativeResult<u64> {
        count_rows(&self.connection, "staged_sessions")
    }

    pub(super) fn event_count(&self) -> ZedNativeResult<u64> {
        count_rows(&self.connection, "staged_events")
    }

    pub(super) fn rejection_count(&self) -> ZedNativeResult<u64> {
        count_rows(&self.connection, "staged_rejections")
    }

    pub(super) fn rejection_samples(&self, limit: usize) -> ZedNativeResult<Vec<String>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self
            .connection
            .prepare("SELECT reason FROM staged_rejections ORDER BY ordinal LIMIT ?1")?;
        let rows = statement.query_map([limit], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(ZedNativePathError::from)
    }

    pub(super) fn session_relationship(
        &self,
        thread_id: &str,
    ) -> ZedNativeResult<Option<(Option<String>, String)>> {
        self.connection
            .query_row(
                &format!(
                    "WITH RECURSIVE ancestors(thread_id, parent_thread_id, depth) AS (
                         SELECT thread_id, parent_thread_id, 0
                         FROM staged_sessions
                         WHERE thread_id = ?1
                         UNION ALL
                         SELECT parent.thread_id, parent.parent_thread_id, child.depth + 1
                         FROM staged_sessions parent
                         JOIN ancestors child ON parent.thread_id = child.parent_thread_id
                         WHERE child.depth < {ZED_STAGING_MAX_RELATIONSHIP_DEPTH}
                     )
                     SELECT original.parent_thread_id,
                            COALESCE(
                                (SELECT thread_id FROM ancestors
                                 WHERE parent_thread_id IS NULL
                                 ORDER BY depth DESC LIMIT 1),
                                original.thread_id
                            )
                     FROM staged_sessions original
                     WHERE original.thread_id = ?1"
                ),
                [thread_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(ZedNativePathError::from)
    }

    pub(super) fn validate_relationships(&self) -> ZedNativeResult<()> {
        let total = self.session_count()?;
        let reachable = self.connection.query_row(
            &format!(
                "WITH RECURSIVE session_tree(thread_id, depth) AS (
                     SELECT child.thread_id, 0
                     FROM staged_sessions child
                     WHERE child.parent_thread_id IS NULL
                        OR NOT EXISTS (
                            SELECT 1 FROM staged_sessions parent
                            WHERE parent.thread_id = child.parent_thread_id
                        )
                     UNION ALL
                     SELECT child.thread_id, parent.depth + 1
                     FROM staged_sessions child
                     JOIN session_tree parent
                       ON child.parent_thread_id = parent.thread_id
                     WHERE parent.depth < {ZED_STAGING_MAX_RELATIONSHIP_DEPTH}
                 )
                 SELECT COUNT(*) FROM session_tree"
            ),
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if u64::try_from(reachable).unwrap_or_default() != total {
            return Err(ZedNativePathError::UnsupportedSchema(
                "Zed thread relationships contain a cycle or exceed the bounded depth".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn session_batch(
        &self,
        offset: u64,
        max_mutation_units: usize,
        max_bytes: usize,
    ) -> ZedNativeResult<Vec<ZedStagedSession>> {
        let offset = i64::try_from(offset).map_err(|_| {
            ZedNativePathError::UnsupportedSchema(
                "Zed staged session cursor exceeds SQLite limits".to_owned(),
            )
        })?;
        let mut statement = self.connection.prepare(&format!(
            "WITH RECURSIVE session_tree(
                 thread_id, effective_parent_thread_id, root_thread_id, depth
             ) AS (
                 SELECT child.thread_id, NULL, child.thread_id, 0
                 FROM staged_sessions child
                 WHERE child.parent_thread_id IS NULL
                    OR NOT EXISTS (
                        SELECT 1 FROM staged_sessions parent
                        WHERE parent.thread_id = child.parent_thread_id
                    )
                 UNION ALL
                 SELECT child.thread_id, child.parent_thread_id,
                        parent.root_thread_id, parent.depth + 1
                 FROM staged_sessions child
                 JOIN session_tree parent
                   ON child.parent_thread_id = parent.thread_id
                 WHERE parent.depth < {ZED_STAGING_MAX_RELATIONSHIP_DEPTH}
             )
             SELECT staged.payload_json, tree.effective_parent_thread_id,
                    tree.root_thread_id, staged.estimated_bytes
             FROM session_tree tree
             JOIN staged_sessions staged ON staged.thread_id = tree.thread_id
             ORDER BY tree.depth, tree.thread_id COLLATE BINARY
             LIMIT {ZED_STAGING_BATCH_CANDIDATES} OFFSET ?1"
        ))?;
        let mut rows = statement.query([offset])?;
        let mut batch = Vec::new();
        let mut mutation_units = 2_usize;
        let mut bytes = 0_usize;
        while let Some(row) = rows.next()? {
            let payload: String = row.get(0)?;
            let parent_thread_id: Option<String> = row.get(1)?;
            let root_thread_id: String = row.get(2)?;
            let estimated_bytes = usize::try_from(row.get::<_, i64>(3)?).unwrap_or(usize::MAX);
            let units = if parent_thread_id.is_some() { 4 } else { 3 };
            if !batch.is_empty()
                && (mutation_units.saturating_add(units) > max_mutation_units
                    || bytes.saturating_add(estimated_bytes) > max_bytes)
            {
                break;
            }
            if units.saturating_add(2) > max_mutation_units || estimated_bytes > max_bytes {
                return Err(ZedNativePathError::UnsupportedSchema(
                    "one Zed session exceeds the bounded Store publication group".to_owned(),
                ));
            }
            let session = serde_json::from_str(&payload)
                .map_err(|error| ZedNativePathError::Capture(crate::CaptureError::Json(error)))?;
            mutation_units = mutation_units.saturating_add(units);
            bytes = bytes.saturating_add(estimated_bytes);
            batch.push(ZedStagedSession {
                session,
                parent_thread_id,
                root_thread_id,
                estimated_bytes,
            });
        }
        Ok(batch)
    }

    pub(super) fn event_batch(
        &self,
        after_ordinal: u64,
        max_mutation_units: usize,
        max_bytes: usize,
    ) -> ZedNativeResult<Vec<ZedStagedEvent>> {
        let after_ordinal = i64::try_from(after_ordinal).map_err(|_| {
            ZedNativePathError::UnsupportedSchema(
                "Zed staged event cursor exceeds SQLite limits".to_owned(),
            )
        })?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT ordinal, payload_json, estimated_bytes, mutation_units
             FROM staged_events
             WHERE ordinal > ?1
             ORDER BY ordinal
             LIMIT {ZED_STAGING_BATCH_CANDIDATES}"
        ))?;
        let mut rows = statement.query([after_ordinal])?;
        let mut batch = Vec::new();
        let mut mutation_units = 2_usize;
        let mut bytes = 0_usize;
        while let Some(row) = rows.next()? {
            let ordinal = u64::try_from(row.get::<_, i64>(0)?).unwrap_or(u64::MAX);
            let payload: String = row.get(1)?;
            let estimated_bytes = usize::try_from(row.get::<_, i64>(2)?).unwrap_or(usize::MAX);
            let units = usize::try_from(row.get::<_, i64>(3)?).unwrap_or(usize::MAX);
            if !batch.is_empty()
                && (mutation_units.saturating_add(units) > max_mutation_units
                    || bytes.saturating_add(estimated_bytes) > max_bytes)
            {
                break;
            }
            if units.saturating_add(2) > max_mutation_units || estimated_bytes > max_bytes {
                return Err(ZedNativePathError::UnsupportedSchema(
                    "one Zed event exceeds the bounded Store publication group".to_owned(),
                ));
            }
            let event = serde_json::from_str(&payload)
                .map_err(|error| ZedNativePathError::Capture(crate::CaptureError::Json(error)))?;
            mutation_units = mutation_units.saturating_add(units);
            bytes = bytes.saturating_add(estimated_bytes);
            batch.push(ZedStagedEvent {
                ordinal,
                event,
                estimated_bytes,
            });
        }
        Ok(batch)
    }
}

impl ZedNativeSink for ZedNativeStaging {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
        let transaction = self.connection.transaction()?;
        for session in page.sessions {
            let payload = serde_json::to_string(&session)
                .map_err(|error| ZedNativePathError::Capture(crate::CaptureError::Json(error)))?;
            let payload_bytes = i64::try_from(payload.len()).unwrap_or(i64::MAX);
            transaction.execute(
                "INSERT INTO staged_sessions
                     (thread_id, parent_thread_id, payload_json, estimated_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    session.thread_id,
                    session.parent_thread_id,
                    payload,
                    payload_bytes,
                ],
            )?;
        }
        for event in page.events {
            let payload = serde_json::to_string(&event)
                .map_err(|error| ZedNativePathError::Capture(crate::CaptureError::Json(error)))?;
            let payload_bytes = i64::try_from(payload.len()).unwrap_or(i64::MAX);
            let mutation_units = 1_usize.saturating_add(event.safe_file_touches.len());
            transaction.execute(
                "INSERT INTO staged_events
                     (payload_json, estimated_bytes, mutation_units)
                VALUES (?1, ?2, ?3)",
                params![
                    payload,
                    payload_bytes,
                    i64::try_from(mutation_units).unwrap_or(i64::MAX),
                ],
            )?;
        }
        for rejection in page.rejections {
            transaction.execute(
                "INSERT INTO staged_rejections (reason) VALUES (?1)",
                [rejection.reason],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn count_rows(connection: &Connection, table: &str) -> ZedNativeResult<u64> {
    let count = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get::<_, i64>(0)
    })?;
    u64::try_from(count).map_err(|_| {
        ZedNativePathError::UnsupportedSchema(format!(
            "Zed staging table {table} has an invalid row count"
        ))
    })
}
