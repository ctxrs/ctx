use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, Fidelity, SyncMetadata, SyncState,
    Visibility,
};
use rusqlite::{params, OptionalExtension};
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, nonnegative_i64_to_u32, nonnegative_i64_to_u64, optional_ms_to_time,
    optional_timestamp_ms, parse_json, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_capture_source(&self, source: &CaptureSource) -> Result<()> {
        self.conn.execute(
            r#"
                INSERT INTO capture_sources
                (
                    id, kind, provider, machine_id, process_id, cwd, raw_source_path,
                    external_session_id, started_at_ms, ended_at_ms, fidelity,
                    visibility, sync_state, sync_version, metadata_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(id) DO UPDATE SET
                    kind = excluded.kind,
                    provider = excluded.provider,
                    machine_id = excluded.machine_id,
                    process_id = excluded.process_id,
                    cwd = excluded.cwd,
                    raw_source_path = excluded.raw_source_path,
                    external_session_id = excluded.external_session_id,
                    started_at_ms = excluded.started_at_ms,
                    ended_at_ms = excluded.ended_at_ms,
                    fidelity = excluded.fidelity,
                    visibility = excluded.visibility,
                    sync_state = excluded.sync_state,
                    sync_version = excluded.sync_version,
                    metadata_json = excluded.metadata_json
                "#,
            params![
                source.id.to_string(),
                source.descriptor.kind.as_str(),
                source.descriptor.provider.as_str(),
                source.descriptor.machine_id.as_str(),
                source.descriptor.process_id.map(i64::from),
                source.descriptor.cwd.as_deref(),
                source.descriptor.raw_source_path.as_deref(),
                source.descriptor.external_session_id.as_deref(),
                timestamp_ms(source.started_at),
                optional_timestamp_ms(source.ended_at),
                source.sync.fidelity.as_str(),
                source.sync.visibility.as_str(),
                source.sync.sync_state.as_str(),
                source.sync.sync_version as i64,
                serde_json::to_string(&source.sync.metadata)?,
            ],
        )?;
        Ok(())
    }

    pub fn get_capture_source(&self, id: Uuid) -> Result<CaptureSource> {
        self.conn
                .query_row(
                    "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
                    params![id.to_string()],
                    capture_source_from_row,
                )
                .optional()?
                .ok_or(StoreError::NotFound(id))
    }

    pub fn list_capture_sources(&self) -> Result<Vec<CaptureSource>> {
        let mut stmt = self.conn.prepare(
                "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources ORDER BY started_at_ms, id",
            )?;
        let rows = stmt.query_map([], capture_source_from_row)?;
        collect_rows(rows)
    }

    pub fn capture_source_count(&self) -> Result<usize> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM capture_sources", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn capture_source_by_external_session(
        &self,
        provider: CaptureProvider,
        external_session_id: &str,
    ) -> Result<Option<CaptureSource>> {
        self.conn
                .query_row(
                    "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC LIMIT 1",
                    params![provider.as_str(), external_session_id],
                    capture_source_from_row,
                )
                .optional()
                .map_err(StoreError::from)
    }

    pub fn has_provider_data(&self, provider: CaptureProvider) -> Result<bool> {
        let exists = self.conn.query_row(
            r#"
                SELECT
                    EXISTS(
                        SELECT 1
                        FROM sessions
                        WHERE provider = ?1
                        LIMIT 1
                    )
                    OR EXISTS(
                        SELECT 1
                        FROM capture_sources
                        WHERE provider = ?1
                        LIMIT 1
                    )
                "#,
            params![provider.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }
}

pub(crate) fn capture_source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaptureSource> {
    Ok(CaptureSource {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        descriptor: CaptureSourceDescriptor {
            kind: parse_text_enum::<ctx_history_core::CaptureSourceKind>(row.get::<_, String>(1)?)?,
            provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(2)?)?,
            machine_id: row.get(3)?,
            process_id: row
                .get::<_, Option<i64>>(4)?
                .map(nonnegative_i64_to_u32)
                .transpose()?,
            cwd: row.get(5)?,
            raw_source_path: row.get(6)?,
            external_session_id: row.get(7)?,
        },
        started_at: ms_to_time(row.get(8)?)?,
        ended_at: optional_ms_to_time(row.get(9)?)?,
        sync: SyncMetadata {
            fidelity: parse_text_enum::<Fidelity>(row.get::<_, String>(10)?)?,
            visibility: parse_text_enum::<Visibility>(row.get::<_, String>(11)?)?,
            sync_state: parse_text_enum::<SyncState>(row.get::<_, String>(12)?)?,
            sync_version: nonnegative_i64_to_u64(row.get(13)?)?,
            deleted_at: None,
            metadata: parse_json(row.get::<_, String>(14)?)?,
        },
    })
}
