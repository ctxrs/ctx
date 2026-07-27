use ctx_history_core::CaptureProvider;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::connection::{
    capped_i64, collect_rows, nonnegative_i64_to_u64, parse_json, parse_text_enum,
};
use crate::{Result, Store};

const SOURCE_IMPORT_FILE_PAGE_SIZE: usize = 64;
const INVENTORY_CONTROL: &str = "inventory_control_v1";
const INVENTORY_GENERATION: &str = "inventory_generation_v1";
const INVENTORY_PHASE: &str = "inventory_phase_v1";
const INVENTORY_DISCOVERY_COMPLETE: &str = "inventory_discovery_complete_v1";
const INVENTORY_RECONCILIATION_STAGE: &str = "inventory_reconciliation_stage_v1";
const INVENTORY_STALE_KEYSET: &str = "inventory_stale_keyset_v1";
const RESERVED_INVENTORY_CONTROL_FIELDS: [&str; 6] = [
    INVENTORY_CONTROL,
    INVENTORY_GENERATION,
    INVENTORY_PHASE,
    INVENTORY_DISCOVERY_COMPLETE,
    INVENTORY_RECONCILIATION_STAGE,
    INVENTORY_STALE_KEYSET,
];

#[derive(Debug, Clone, PartialEq)]
pub struct SourceImportFile {
    pub provider: CaptureProvider,
    pub source_format: String,
    pub source_root: String,
    pub source_path: String,
    pub file_size_bytes: u64,
    pub file_modified_at_ms: i64,
    pub observed_at_ms: i64,
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PersistedInventoryPhase {
    Discovering,
    Reconciling,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PersistedReconciliationStage {
    Preference,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceImportInventoryControl {
    provider: CaptureProvider,
    source_format: String,
    source_root: String,
    file_size_bytes: u64,
    file_modified_at_ms: i64,
    observed_at_ms: i64,
}

impl SourceImportInventoryControl {
    pub fn new(
        provider: CaptureProvider,
        source_format: impl Into<String>,
        source_root: impl Into<String>,
        file_size_bytes: u64,
        file_modified_at_ms: i64,
        observed_at_ms: i64,
    ) -> Self {
        Self {
            provider,
            source_format: source_format.into(),
            source_root: source_root.into(),
            file_size_bytes,
            file_modified_at_ms,
            observed_at_ms,
        }
    }

    pub fn provider(&self) -> CaptureProvider {
        self.provider
    }

    pub fn source_root(&self) -> &str {
        &self.source_root
    }

    pub fn observed_at_ms(&self) -> i64 {
        self.observed_at_ms
    }

    pub fn discovering_file(
        &self,
        source_files: usize,
        source_bytes: u64,
    ) -> Result<SourceImportFile> {
        self.file(
            PersistedInventoryPhase::Discovering,
            None,
            None,
            source_files,
            source_bytes,
        )
    }

    pub fn reconciling_preference_file(
        &self,
        stale_keyset: Option<&str>,
        source_files: usize,
        source_bytes: u64,
    ) -> Result<SourceImportFile> {
        self.file(
            PersistedInventoryPhase::Reconciling,
            Some(PersistedReconciliationStage::Preference),
            stale_keyset,
            source_files,
            source_bytes,
        )
    }

    pub fn reconciling_missing_file(
        &self,
        stale_keyset: Option<&str>,
        source_files: usize,
        source_bytes: u64,
    ) -> Result<SourceImportFile> {
        self.file(
            PersistedInventoryPhase::Reconciling,
            Some(PersistedReconciliationStage::Missing),
            stale_keyset,
            source_files,
            source_bytes,
        )
    }

    pub fn complete_file(
        &self,
        stale_keyset: Option<&str>,
        source_files: usize,
        source_bytes: u64,
    ) -> Result<SourceImportFile> {
        self.file(
            PersistedInventoryPhase::Complete,
            None,
            stale_keyset,
            source_files,
            source_bytes,
        )
    }

    fn file(
        &self,
        phase: PersistedInventoryPhase,
        reconciliation_stage: Option<PersistedReconciliationStage>,
        stale_keyset: Option<&str>,
        source_files: usize,
        source_bytes: u64,
    ) -> Result<SourceImportFile> {
        let metadata = serde_json::to_value(PersistedInventoryControl {
            inventory_unit: "source_root".to_owned(),
            inventory_control_v1: true,
            inventory_generation_v1: self.observed_at_ms,
            inventory_phase_v1: phase,
            inventory_discovery_complete_v1: phase != PersistedInventoryPhase::Discovering,
            inventory_reconciliation_stage_v1: reconciliation_stage,
            inventory_stale_keyset_v1: stale_keyset.map(str::to_owned),
            source_files,
            source_bytes,
        })?;
        let file = SourceImportFile {
            provider: self.provider,
            source_format: self.source_format.clone(),
            source_root: self.source_root.clone(),
            source_path: self.source_root.clone(),
            file_size_bytes: self.file_size_bytes,
            file_modified_at_ms: self.file_modified_at_ms,
            observed_at_ms: self.observed_at_ms,
            metadata,
        };
        file.is_inventory_control()?;
        Ok(file)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedInventoryControl {
    inventory_unit: String,
    inventory_control_v1: bool,
    inventory_generation_v1: i64,
    inventory_phase_v1: PersistedInventoryPhase,
    inventory_discovery_complete_v1: bool,
    inventory_reconciliation_stage_v1: Option<PersistedReconciliationStage>,
    inventory_stale_keyset_v1: Option<String>,
    source_files: usize,
    source_bytes: u64,
}

impl SourceImportFile {
    pub(super) fn is_inventory_control(&self) -> Result<bool> {
        Ok(self.persisted_inventory_control()?.is_some())
    }

    pub(super) fn inventory_control_allows_reconciliation(&self) -> Result<bool> {
        Ok(self
            .persisted_inventory_control()?
            .is_some_and(|control| control.inventory_discovery_complete_v1))
    }

    fn persisted_inventory_control(&self) -> Result<Option<PersistedInventoryControl>> {
        let Some(metadata) = self.metadata.as_object() else {
            return Ok(None);
        };
        if !RESERVED_INVENTORY_CONTROL_FIELDS
            .iter()
            .any(|field| metadata.contains_key(*field))
        {
            return Ok(None);
        }
        for field in [INVENTORY_RECONCILIATION_STAGE, INVENTORY_STALE_KEYSET] {
            if !metadata.contains_key(field) {
                return Err(
                    invalid_inventory_control(format!("missing required field {field}")).into(),
                );
            }
        }
        let control: PersistedInventoryControl = serde_json::from_value(self.metadata.clone())?;
        if self.source_path != self.source_root {
            return Err(invalid_inventory_control("source_path must equal source_root").into());
        }
        if control.inventory_unit != "source_root" {
            return Err(invalid_inventory_control("inventory_unit must be source_root").into());
        }
        if !control.inventory_control_v1 {
            return Err(invalid_inventory_control("inventory_control_v1 must be true").into());
        }
        if control.inventory_generation_v1 != self.observed_at_ms {
            return Err(invalid_inventory_control(
                "inventory_generation_v1 must equal observed_at_ms",
            )
            .into());
        }
        let discovery_complete = control.inventory_phase_v1 != PersistedInventoryPhase::Discovering;
        if control.inventory_discovery_complete_v1 != discovery_complete {
            return Err(invalid_inventory_control(
                "inventory_discovery_complete_v1 does not match inventory_phase_v1",
            )
            .into());
        }
        if control.inventory_phase_v1 == PersistedInventoryPhase::Discovering
            && control.inventory_stale_keyset_v1.is_some()
        {
            return Err(invalid_inventory_control(
                "inventory_stale_keyset_v1 must be null while discovering",
            )
            .into());
        }
        match (
            control.inventory_phase_v1,
            control.inventory_reconciliation_stage_v1,
        ) {
            (PersistedInventoryPhase::Reconciling, Some(_))
            | (PersistedInventoryPhase::Discovering | PersistedInventoryPhase::Complete, None) => {}
            _ => {
                return Err(invalid_inventory_control(
                    "inventory_reconciliation_stage_v1 does not match inventory_phase_v1",
                )
                .into());
            }
        }
        Ok(Some(control))
    }
}

fn invalid_inventory_control(message: impl Into<String>) -> serde_json::Error {
    <serde_json::Error as serde::de::Error>::custom(format!(
        "invalid source import inventory control: {}",
        message.into()
    ))
}

impl Store {
    pub fn upsert_source_import_files(&self, files: &[SourceImportFile]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        for file in files {
            file.is_inventory_control()?;
        }
        let mut stmt = self.conn.prepare(
            r#"
                INSERT INTO source_import_files (
                    provider, source_format, source_root, source_path,
                    file_size_bytes, file_modified_at_ms, observed_at_ms, is_stale,
                    metadata_json
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)
                ON CONFLICT(provider, source_root, source_path) DO UPDATE SET
                    source_format = excluded.source_format,
                    file_size_bytes = excluded.file_size_bytes,
                    file_modified_at_ms = excluded.file_modified_at_ms,
                    observed_at_ms = excluded.observed_at_ms,
                    is_stale = 0,
                    indexed_at_ms = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_at_ms
                        ELSE NULL
                    END,
                    indexed_file_size_bytes = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_size_bytes
                        ELSE NULL
                    END,
                    indexed_file_modified_at_ms = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_file_modified_at_ms
                        ELSE NULL
                    END,
                    indexed_status = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_status
                        ELSE 'pending'
                    END,
                    indexed_error = CASE
                        WHEN source_import_files.source_format IS excluded.source_format
                         AND source_import_files.file_size_bytes = excluded.file_size_bytes
                         AND source_import_files.file_modified_at_ms = excluded.file_modified_at_ms
                         AND ((json_extract(excluded.metadata_json, '$.inventory_unit') IS NOT 'source_root'
                               AND json_type(excluded.metadata_json, '$.inventory_file_change_token_v1') IS NULL)
                              OR source_import_files.metadata_json IS excluded.metadata_json)
                        THEN source_import_files.indexed_error
                        ELSE NULL
                    END,
                    metadata_json = excluded.metadata_json
                WHERE source_import_files.source_format IS NOT excluded.source_format
                   OR source_import_files.file_size_bytes != excluded.file_size_bytes
                   OR source_import_files.file_modified_at_ms != excluded.file_modified_at_ms
                   OR source_import_files.observed_at_ms != excluded.observed_at_ms
                   OR source_import_files.is_stale != 0
                   OR source_import_files.metadata_json IS NOT excluded.metadata_json
                "#,
        )?;
        for file in files {
            stmt.execute(params![
                file.provider.as_str(),
                file.source_format.as_str(),
                file.source_root.as_str(),
                file.source_path.as_str(),
                capped_i64(file.file_size_bytes),
                file.file_modified_at_ms,
                file.observed_at_ms,
                serde_json::to_string(&file.metadata)?,
            ])?;
        }
        Ok(())
    }

    pub fn reset_source_import_files_pending(&self, files: &[SourceImportFile]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }
        let mut stmt = self.conn.prepare(
            r#"
                UPDATE source_import_files
                SET indexed_at_ms = NULL,
                    indexed_file_size_bytes = NULL,
                    indexed_file_modified_at_ms = NULL,
                    indexed_status = 'pending',
                    indexed_error = NULL
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?3
                  AND is_stale = 0
                  AND json_type(metadata_json, '$.inventory_unit') IS NOT 'source_root'
                "#,
        )?;
        for file in files {
            stmt.execute(params![
                file.provider.as_str(),
                file.source_root.as_str(),
                file.source_path.as_str(),
            ])?;
        }
        Ok(())
    }

    pub fn next_source_import_observed_at_ms(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        requested_at_ms: i64,
    ) -> Result<i64> {
        let latest = self
            .conn
            .query_row(
                r#"
                SELECT observed_at_ms
                FROM source_import_files
                WHERE provider = ?1
                  AND source_root = ?2
                  AND source_path = ?2
                "#,
                params![provider.as_str(), source_root],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(latest
            .map(|latest| requested_at_ms.max(latest.saturating_add(1)))
            .unwrap_or(requested_at_ms))
    }

    pub fn list_pending_source_import_files(
        &self,
        provider: CaptureProvider,
        source_root: &str,
    ) -> Result<Vec<SourceImportFile>> {
        let mut stmt = self.conn.prepare(
            format!(
                "{} WHERE provider = ?1
                       AND source_root = ?2
                       AND is_stale = 0
                       AND json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
                       AND {}
                       AND {}
                     ORDER BY source_path",
                source_import_file_select_sql(""),
                source_import_file_is_not_control_sql("source_import_files"),
                source_import_file_pending_condition_sql("source_import_files")
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(
            params![provider.as_str(), source_root],
            source_import_file_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn list_pending_source_import_files_page(
        &self,
        provider: CaptureProvider,
        source_root: &str,
        after_source_path: Option<&str>,
    ) -> Result<Vec<SourceImportFile>> {
        let sql = source_import_pending_page_sql(after_source_path.is_some());
        let mut stmt = self.conn.prepare(&sql)?;
        match after_source_path {
            Some(after_source_path) => collect_rows(stmt.query_map(
                params![provider.as_str(), source_root, after_source_path],
                source_import_file_from_row,
            )?),
            None => collect_rows(stmt.query_map(
                params![provider.as_str(), source_root],
                source_import_file_from_row,
            )?),
        }
    }
}

pub(super) fn source_import_file_select_sql(tail: &str) -> String {
    format!(
        "SELECT provider, source_format, source_root, source_path, file_size_bytes, file_modified_at_ms, observed_at_ms, metadata_json FROM source_import_files {tail}"
    )
}

pub(super) fn source_import_pending_page_sql(has_cursor: bool) -> String {
    let cursor_clause = if has_cursor {
        "AND source_path > ?3"
    } else {
        ""
    };
    format!(
        "{} WHERE provider = ?1
               AND source_root = ?2
               AND is_stale = 0
               AND json_type(metadata_json, '$.inventory_missing_generation_v1') IS NULL
               {}
               AND {}
               AND {}
             ORDER BY source_path
             LIMIT {}",
        source_import_file_select_sql(""),
        cursor_clause,
        source_import_file_is_not_control_sql("source_import_files"),
        source_import_file_pending_condition_sql("source_import_files"),
        SOURCE_IMPORT_FILE_PAGE_SIZE,
    )
}

pub(super) fn source_import_file_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SourceImportFile> {
    Ok(SourceImportFile {
        provider: parse_text_enum::<CaptureProvider>(row.get::<_, String>(0)?)?,
        source_format: row.get(1)?,
        source_root: row.get(2)?,
        source_path: row.get(3)?,
        file_size_bytes: nonnegative_i64_to_u64(row.get(4)?)?,
        file_modified_at_ms: row.get(5)?,
        observed_at_ms: row.get(6)?,
        metadata: parse_json(row.get::<_, String>(7)?)?,
    })
}

pub(super) fn source_import_file_pending_condition_sql(alias: &str) -> String {
    format!(
        r#"
        (
            {alias}.indexed_status != 'indexed'
            OR {alias}.indexed_file_size_bytes IS NULL
            OR {alias}.indexed_file_modified_at_ms IS NULL
            OR {alias}.indexed_file_size_bytes != {alias}.file_size_bytes
            OR {alias}.indexed_file_modified_at_ms != {alias}.file_modified_at_ms
        )
        "#
    )
}

pub(super) fn source_import_file_is_not_control_sql(alias: &str) -> String {
    RESERVED_INVENTORY_CONTROL_FIELDS
        .iter()
        .map(|field| format!("json_type({alias}.metadata_json, '$.{field}') IS NULL"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

pub(super) fn source_import_file_is_current_sql(alias: &str) -> String {
    format!(
        "{} AND json_type({alias}.metadata_json, '$.inventory_missing_generation_v1') IS NULL",
        source_import_file_is_not_control_sql(alias)
    )
}
