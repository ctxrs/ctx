use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};

use super::sessions::{catalog_session_from_row, catalog_session_select_sql};
use super::CatalogSession;
use crate::connection::{capped_i64, collect_rows, nonnegative_i64_to_u64};
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSourceIndexState {
    pub last_imported_file_size_bytes: Option<u64>,
    pub last_imported_file_modified_at_ms: Option<i64>,
    pub last_imported_event_count: Option<u64>,
    pub last_imported_at_ms: Option<i64>,
    pub last_imported_file_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogIndexedStatus {
    Pending,
    Indexed,
    Failed,
}

impl CatalogIndexedStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Indexed => "indexed",
            Self::Failed => "failed",
        }
    }
}

impl Store {
    pub fn mark_catalog_source_observation_indexed(
        &self,
        session: &CatalogSession,
        file_sha256: Option<&str>,
        event_count: Option<u64>,
        indexed_at_ms: i64,
    ) -> Result<()> {
        let metadata_json = serde_json::to_string(&session.metadata)?;
        self.with_atomic_write(|| {
            if catalog_source_observation_already_indexed(
                self,
                session,
                &metadata_json,
                file_sha256,
                event_count,
            )? {
                return Ok(());
            }
            let changed = self.conn.execute(
                r#"
                    UPDATE catalog_sessions
                    SET indexed_at_ms = ?9,
                        indexed_file_size_bytes = ?5,
                        indexed_file_modified_at_ms = ?6,
                        indexed_status = ?11,
                        indexed_error = NULL,
                        indexed_event_count = ?10,
                        last_imported_at_ms = ?9,
                        last_imported_file_size_bytes = ?5,
                        last_imported_file_modified_at_ms = ?6,
                        last_imported_file_sha256 = ?12,
                        last_imported_event_count = ?10
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND is_stale = 0
                      AND (
                          (json_type(?8, '$.inventory_file_change_token_v1') IS NULL
                           AND json_type(metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                          OR (
                              source_format = ?4
                              AND file_size_bytes = ?5
                              AND file_modified_at_ms = ?6
                              AND cataloged_at_ms = ?7
                              AND metadata_json = ?8
                          )
                      )
                    "#,
                params![
                    session.provider.as_str(),
                    session.source_root,
                    session.source_path,
                    session.source_format,
                    capped_i64(session.file_size_bytes),
                    session.file_modified_at_ms,
                    session.cataloged_at_ms,
                    metadata_json,
                    indexed_at_ms,
                    event_count.map(capped_i64),
                    CatalogIndexedStatus::Indexed.as_str(),
                    file_sha256,
                ],
            )?;
            require_exact_catalog_observation_completion(session, "indexed", changed)
        })
    }

    pub fn mark_catalog_source_observation_failed(
        &self,
        session: &CatalogSession,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<()> {
        let metadata_json = serde_json::to_string(&session.metadata)?;
        self.with_atomic_write(|| {
            let changed = self.conn.execute(
                r#"
                    UPDATE catalog_sessions
                    SET indexed_at_ms = ?9,
                        indexed_file_size_bytes = NULL,
                        indexed_file_modified_at_ms = NULL,
                        indexed_status = ?11,
                        indexed_error = ?10,
                        indexed_event_count = NULL
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND is_stale = 0
                      AND (
                          (json_type(?8, '$.inventory_file_change_token_v1') IS NULL
                           AND json_type(metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                          OR (
                              source_format = ?4
                              AND file_size_bytes = ?5
                              AND file_modified_at_ms = ?6
                              AND cataloged_at_ms = ?7
                              AND metadata_json = ?8
                          )
                      )
                    "#,
                params![
                    session.provider.as_str(),
                    session.source_root,
                    session.source_path,
                    session.source_format,
                    capped_i64(session.file_size_bytes),
                    session.file_modified_at_ms,
                    session.cataloged_at_ms,
                    metadata_json,
                    indexed_at_ms,
                    error,
                    CatalogIndexedStatus::Failed.as_str(),
                ],
            )?;
            require_exact_catalog_observation_completion(session, "failed", changed)
        })
    }
}

fn catalog_source_observation_already_indexed(
    store: &Store,
    session: &CatalogSession,
    metadata_json: &str,
    file_sha256: Option<&str>,
    event_count: Option<u64>,
) -> Result<bool> {
    store
        .conn
        .query_row(
            r#"
                SELECT EXISTS(
                    SELECT 1
                    FROM catalog_sessions
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND source_format = ?4
                      AND file_size_bytes = ?5
                      AND file_modified_at_ms = ?6
                      AND cataloged_at_ms = ?7
                      AND metadata_json = ?8
                      AND is_stale = 0
                      AND indexed_file_size_bytes = ?5
                      AND indexed_file_modified_at_ms = ?6
                      AND indexed_status = ?9
                      AND indexed_error IS NULL
                      AND indexed_event_count IS ?10
                      AND last_imported_file_size_bytes = ?5
                      AND last_imported_file_modified_at_ms = ?6
                      AND last_imported_file_sha256 IS ?11
                      AND last_imported_event_count IS ?10
                )
                "#,
            params![
                session.provider.as_str(),
                session.source_root,
                session.source_path,
                session.source_format,
                capped_i64(session.file_size_bytes),
                session.file_modified_at_ms,
                session.cataloged_at_ms,
                metadata_json,
                CatalogIndexedStatus::Indexed.as_str(),
                event_count.map(capped_i64),
                file_sha256,
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn require_exact_catalog_observation_completion(
    session: &CatalogSession,
    operation: &'static str,
    changed: usize,
) -> Result<()> {
    if changed == 1 {
        return Ok(());
    }
    Err(StoreError::SourceImportObservationConflict {
        operation,
        provider: session.provider.as_str().to_owned(),
        source_path: session.source_path.clone(),
    })
}

impl Store {
    pub fn catalog_source_index_state(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        source_path: &str,
    ) -> Result<Option<CatalogSourceIndexState>> {
        self.conn
            .query_row(
                r#"
                    SELECT last_imported_file_size_bytes,
                           last_imported_file_modified_at_ms,
                           last_imported_event_count,
                           last_imported_at_ms,
                           last_imported_file_sha256
                    FROM catalog_sessions
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND is_stale = 0
                    "#,
                params![provider.as_str(), source_root, source_path],
                |row| {
                    let last_imported_file_size_bytes = row
                        .get::<_, Option<i64>>(0)?
                        .map(nonnegative_i64_to_u64)
                        .transpose()?;
                    let last_imported_event_count = row
                        .get::<_, Option<i64>>(2)?
                        .map(nonnegative_i64_to_u64)
                        .transpose()?;
                    Ok(CatalogSourceIndexState {
                        last_imported_file_size_bytes,
                        last_imported_file_modified_at_ms: row.get(1)?,
                        last_imported_event_count,
                        last_imported_at_ms: row.get(3)?,
                        last_imported_file_sha256: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::from)
    }
}

impl Store {
    pub fn list_pending_catalog_sessions(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        self.list_pending_catalog_sessions_for_projection(provider, source_root, true)
    }

    pub fn list_pending_catalog_sessions_without_local_projection(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<CatalogSession>> {
        self.list_pending_catalog_sessions_for_projection(provider, source_root, false)
    }

    fn list_pending_catalog_sessions_for_projection(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        require_local_projection: bool,
    ) -> Result<Vec<CatalogSession>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                       AND {}
                     ORDER BY session_started_at_ms, source_path",
                catalog_session_select_sql(""),
                catalog_pending_import_condition_sql("catalog_sessions", require_local_projection)
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            catalog_session_from_row,
        )?;
        collect_rows(rows)
    }
}

pub(super) fn catalog_pending_import_condition_sql(
    alias: &str,
    require_local_projection: bool,
) -> String {
    let missing_local_projection = if require_local_projection {
        format!(
            r#"
            OR NOT EXISTS (
                SELECT 1
                FROM sessions AS session
                LEFT JOIN capture_sources AS source
                  ON source.id = session.capture_source_id
                WHERE session.provider = {alias}.provider
                  AND {alias}.external_session_id IS NOT NULL
                  AND session.external_session_id = {alias}.external_session_id
                  AND (
                      session.capture_source_id IS NULL
                      OR source.source_root = {alias}.source_root
                  )
                LIMIT 1
            )
            "#
        )
    } else {
        String::new()
    };
    format!(
        r#"
        (
            {alias}.indexed_status != 'indexed'
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
            {missing_local_projection}
        )
        "#
    )
}

pub(super) fn catalog_indexed_count_sql(require_local_projection: bool) -> String {
    let local_projection_exists = if require_local_projection {
        r#"
      AND EXISTS (
        SELECT 1
        FROM sessions AS session
        LEFT JOIN capture_sources AS source
          ON source.id = session.capture_source_id
        WHERE session.provider = catalog.provider
          AND catalog.external_session_id IS NOT NULL
          AND session.external_session_id = catalog.external_session_id
          AND (
              session.capture_source_id IS NULL
              OR source.source_root = catalog.source_root
          )
        LIMIT 1
      )
        "#
    } else {
        ""
    };
    format!(
        r#"
    SELECT COUNT(*)
    FROM catalog_sessions AS catalog
    WHERE catalog.is_stale = 0
      AND catalog.indexed_status = 'indexed'
      AND catalog.indexed_file_size_bytes = catalog.file_size_bytes
      AND catalog.indexed_file_modified_at_ms = catalog.file_modified_at_ms
      {local_projection_exists}
    "#
    )
}
