use rusqlite::params;

use super::{CatalogIndexedStatus, SourceImportFile};
use crate::connection::capped_i64;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn mark_source_import_file_indexed(
        &self,
        file: &SourceImportFile,
        indexed_at_ms: i64,
    ) -> Result<()> {
        let metadata_json = serde_json::to_string(&file.metadata)?;
        self.with_atomic_write(|| {
            let changed = self.conn.execute(
                r#"
                    UPDATE source_import_files
                    SET indexed_at_ms = ?9,
                        indexed_file_size_bytes = ?5,
                        indexed_file_modified_at_ms = ?6,
                        indexed_status = ?10,
                        indexed_error = NULL
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND source_format = ?4
                      AND file_size_bytes = ?5
                      AND file_modified_at_ms = ?6
                      AND observed_at_ms = ?7
                      AND metadata_json = ?8
                      AND is_stale = 0
                    "#,
                params![
                    file.provider.as_str(),
                    file.source_root,
                    file.source_path,
                    file.source_format,
                    capped_i64(file.file_size_bytes),
                    file.file_modified_at_ms,
                    file.observed_at_ms,
                    metadata_json,
                    indexed_at_ms,
                    CatalogIndexedStatus::Indexed.as_str(),
                ],
            )?;
            require_exact_source_import_observation_completion(file, "indexed", changed)
        })
    }

    pub fn mark_source_import_file_failed(
        &self,
        file: &SourceImportFile,
        error: &str,
        indexed_at_ms: i64,
    ) -> Result<()> {
        let metadata_json = serde_json::to_string(&file.metadata)?;
        self.with_atomic_write(|| {
            let changed = self.conn.execute(
                r#"
                    UPDATE source_import_files
                    SET indexed_at_ms = ?9,
                        indexed_file_size_bytes = NULL,
                        indexed_file_modified_at_ms = NULL,
                        indexed_status = ?11,
                        indexed_error = ?10
                    WHERE provider = ?1
                      AND source_root = ?2
                      AND source_path = ?3
                      AND source_format = ?4
                      AND file_size_bytes = ?5
                      AND file_modified_at_ms = ?6
                      AND observed_at_ms = ?7
                      AND metadata_json = ?8
                      AND is_stale = 0
                    "#,
                params![
                    file.provider.as_str(),
                    file.source_root,
                    file.source_path,
                    file.source_format,
                    capped_i64(file.file_size_bytes),
                    file.file_modified_at_ms,
                    file.observed_at_ms,
                    metadata_json,
                    indexed_at_ms,
                    error,
                    CatalogIndexedStatus::Failed.as_str(),
                ],
            )?;
            require_exact_source_import_observation_completion(file, "failed", changed)
        })
    }
}

fn require_exact_source_import_observation_completion(
    file: &SourceImportFile,
    operation: &'static str,
    changed: usize,
) -> Result<()> {
    if changed == 1 {
        return Ok(());
    }
    Err(StoreError::SourceImportObservationConflict {
        operation,
        provider: file.provider.as_str().to_owned(),
        source_path: file.source_path.clone(),
    })
}
