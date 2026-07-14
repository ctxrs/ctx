mod conflicts;
mod import;
#[cfg(test)]
mod tests;

use std::{
    fs::File,
    io::{self, BufReader, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    utc_now, Artifact, CaptureSourceDescriptor, Fidelity, SessionHistoryArchive,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::archive::conflicts::{
    reject_archive_event_internal_conflicts, reject_capture_source_import_conflict,
    reject_import_stage_conflicts,
};
use crate::archive::import::{
    archive_import_stage_len, import_archive_stage_range_tx, upsert_capture_source_tx,
    ArchiveImportStage,
};
use crate::object_store::{
    ensure_regular_blob_file, file_snapshot, object_relative_path, path_matches_file_snapshot,
    sha256_reader_hex, FileSnapshot, LEGACY_BLOBS_DIR,
};
use crate::{sqlite_amplifying_write_estimate, Result, Store, StoreError};

const ARCHIVE_IMPORT_BATCH_ROWS: usize = 64;
const ARCHIVE_IMPORT_BATCH_BYTES: u64 = 4 * 1024 * 1024;
const ARCHIVE_IMPORT_PROGRESS_TABLE: &str = "archive_import_progress";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveImportOutcome {
    pub search_projection_ready: bool,
    pub maintenance_error: Option<String>,
}

struct ArchiveWriteTransaction<'a> {
    conn: &'a rusqlite::Connection,
}

#[derive(Debug)]
struct ValidatedArchiveBlob {
    id: Uuid,
    path: PathBuf,
    snapshot: FileSnapshot,
}

struct ValidatedArchiveBlobs {
    blobs: Vec<ValidatedArchiveBlob>,
}

impl ValidatedArchiveBlobs {
    fn revalidate_paths(&self) -> Result<()> {
        for blob in &self.blobs {
            if !path_matches_file_snapshot(&blob.path, &blob.snapshot)? {
                return Err(StoreError::ArchiveArtifactChangedDuringValidation { id: blob.id });
            }
        }
        Ok(())
    }
}

impl std::ops::Deref for ArchiveWriteTransaction<'_> {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
    }
}

impl Store {
    pub fn export_archive(&self) -> Result<SessionHistoryArchive> {
        Ok(SessionHistoryArchive {
            schema_version: 2,
            version: 2,
            records: self.list_records(usize::MAX)?,
            capture_sources: self.list_capture_sources()?,
            sessions: self.list_sessions()?,
            runs: self.list_runs()?,
            events: self.list_events()?,
            artifact_records: self.list_artifacts()?,
            vcs_workspaces: self.list_vcs_workspaces()?,
            vcs_changes: self.list_vcs_changes()?,
            history_record_links: self.list_history_record_links()?,
            summaries: self.list_summaries()?,
            files_touched: self.list_files_touched()?,
        })
    }

    pub fn import_archive(
        &mut self,
        archive: &SessionHistoryArchive,
        overwrite: bool,
    ) -> Result<ArchiveImportOutcome> {
        self.import_archive_after_commit(archive, overwrite, |_| {})
    }

    fn import_archive_after_commit(
        &mut self,
        archive: &SessionHistoryArchive,
        overwrite: bool,
        after_commit: impl FnOnce(&rusqlite::Connection),
    ) -> Result<ArchiveImportOutcome> {
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        let blob_dir = self.object_dir.clone();
        let validated_blobs = validate_archive_artifact_record_blobs(&blob_dir, archive)?;
        let fingerprint = archive_import_fingerprint(archive, b"archive")?;
        self.ensure_disk_headroom(
            estimate_archive_write_bytes(&self.path, archive)?,
            "history archive import",
        )?;
        self.import_archive_bounded(
            archive,
            overwrite,
            &fingerprint,
            None,
            &validated_blobs,
            |_| Ok(()),
        )?;
        Ok(self.finish_archive_import_maintenance(after_commit))
    }

    fn import_archive_bounded(
        &mut self,
        archive: &SessionHistoryArchive,
        overwrite: bool,
        fingerprint: &str,
        record_source_id: Option<Uuid>,
        validated_blobs: &ValidatedArchiveBlobs,
        initialize: impl FnOnce(&ArchiveWriteTransaction<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut initialize = Some(initialize);
        if !archive_requires_batched_import(archive)? {
            let total_estimate = estimate_archive_write_bytes(&self.path, archive)?;
            let mut stage = ArchiveImportStage::Records;
            while stage != ArchiveImportStage::Complete {
                let end = archive_import_stage_len(archive, stage);
                self.validate_archive_stage_snapshot(archive, stage, 0, end, overwrite)?;
                stage = stage.next();
            }
            return self.with_write_transaction(|| {
                self.ensure_disk_headroom(total_estimate, "history archive import")?;
                let tx = ArchiveWriteTransaction { conn: &self.conn };
                initialize.take().expect("archive initializer runs once")(&tx)?;
                let mut stage = ArchiveImportStage::Records;
                while stage != ArchiveImportStage::Complete {
                    let end = archive_import_stage_len(archive, stage);
                    reject_import_stage_conflicts(&tx, archive, stage, 0, end, overwrite)?;
                    import_archive_stage_range_tx(&tx, archive, stage, 0, end, record_source_id)?;
                    stage = stage.next();
                }
                self.schedule_search_projection_refresh()?;
                validated_blobs.revalidate_paths()?;
                self.ensure_disk_headroom(total_estimate, "history archive import")
            });
        }
        let mut progress = None;
        self.with_write_transaction(|| {
            self.ensure_disk_headroom(crate::INDEXING_WAL_DELTA_BYTES, "history archive progress")?;
            let tx = ArchiveWriteTransaction { conn: &self.conn };
            ensure_archive_import_progress_table(&tx)?;
            progress = archive_import_progress(&tx, fingerprint)?;
            if let Some((_, _, stored_overwrite)) = progress {
                if stored_overwrite != overwrite {
                    return Err(StoreError::ArchiveImportResumeMismatch);
                }
            } else {
                initialize.take().expect("archive initializer runs once")(&tx)?;
                set_archive_import_progress(
                    &tx,
                    fingerprint,
                    ArchiveImportStage::Records,
                    0,
                    overwrite,
                )?;
                progress = Some((ArchiveImportStage::Records, 0, overwrite));
            }
            validated_blobs.revalidate_paths()?;
            self.ensure_disk_headroom(crate::INDEXING_WAL_DELTA_BYTES, "history archive progress")
        })?;
        let (mut stage, mut cursor, _) = progress.expect("archive progress initialized");

        while stage != ArchiveImportStage::Complete {
            let stage_len = archive_import_stage_len(archive, stage);
            if cursor >= stage_len {
                stage = stage.next();
                cursor = 0;
                self.with_write_transaction(|| {
                    self.ensure_disk_headroom(
                        crate::INDEXING_WAL_DELTA_BYTES,
                        "history archive progress",
                    )?;
                    let tx = ArchiveWriteTransaction { conn: &self.conn };
                    set_archive_import_progress(&tx, fingerprint, stage, cursor, overwrite)?;
                    validated_blobs.revalidate_paths()?;
                    self.ensure_disk_headroom(
                        crate::INDEXING_WAL_DELTA_BYTES,
                        "history archive progress",
                    )
                })?;
                continue;
            }
            let end = archive_import_batch_end(archive, stage, cursor, stage_len)?;
            self.validate_archive_stage_snapshot(archive, stage, cursor, end, overwrite)?;
            let batch_estimate = archive_import_batch_write_estimate(archive, stage, cursor, end)?;
            let next_stage = if end == stage_len {
                stage.next()
            } else {
                stage
            };
            let next_cursor = if end == stage_len { 0 } else { end };
            let slice = self.begin_indexing_slice()?;
            let batch_result = self.with_write_transaction(|| {
                self.ensure_disk_headroom(batch_estimate, "history archive import batch")?;
                let tx = ArchiveWriteTransaction { conn: &self.conn };
                reject_import_stage_conflicts(&tx, archive, stage, cursor, end, overwrite)?;
                import_archive_stage_range_tx(&tx, archive, stage, cursor, end, record_source_id)?;
                set_archive_import_progress(&tx, fingerprint, next_stage, next_cursor, overwrite)?;
                // This is deliberately the final operation before COMMIT.
                validated_blobs.revalidate_paths()?;
                self.ensure_disk_headroom(batch_estimate, "history archive import batch")
            });
            let pacing_result = self.finish_indexing_slice(slice);
            batch_result?;
            pacing_result?;
            stage = next_stage;
            cursor = next_cursor;
        }

        self.with_write_transaction(|| {
            self.ensure_disk_headroom(
                crate::INDEXING_WAL_DELTA_BYTES,
                "history archive publication",
            )?;
            self.schedule_search_projection_refresh()?;
            self.conn.execute(
                &format!("DELETE FROM {ARCHIVE_IMPORT_PROGRESS_TABLE} WHERE fingerprint = ?1"),
                [fingerprint],
            )?;
            // The successful import is not published until blob identity has
            // been checked at the final pre-commit point.
            validated_blobs.revalidate_paths()?;
            self.ensure_disk_headroom(
                crate::INDEXING_WAL_DELTA_BYTES,
                "history archive publication",
            )
        })
    }

    pub fn import_archive_from_capture_source(
        &mut self,
        archive: &SessionHistoryArchive,
        source_id: Uuid,
        source: &CaptureSourceDescriptor,
        occurred_at: DateTime<Utc>,
        fidelity: Fidelity,
        overwrite: bool,
    ) -> Result<ArchiveImportOutcome> {
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        let blob_dir = self.object_dir.clone();
        let validated_blobs = validate_archive_artifact_record_blobs(&blob_dir, archive)?;
        let context = serde_json::to_vec(&(source_id, source, occurred_at, fidelity))?;
        let fingerprint = archive_import_fingerprint(archive, &context)?;
        self.ensure_disk_headroom(
            estimate_archive_write_bytes(&self.path, archive)?,
            "capture archive import",
        )?;
        self.import_archive_bounded(
            archive,
            overwrite,
            &fingerprint,
            Some(source_id),
            &validated_blobs,
            |tx| {
                if !overwrite {
                    reject_capture_source_import_conflict(tx, source_id)?;
                }
                upsert_capture_source_tx(tx, source_id, source, occurred_at, fidelity)
            },
        )?;
        Ok(self.finish_archive_import_maintenance(|_| {}))
    }

    fn finish_archive_import_maintenance(
        &self,
        after_commit: impl FnOnce(&rusqlite::Connection),
    ) -> ArchiveImportOutcome {
        after_commit(&self.conn);
        let mut maintenance_errors = Vec::new();
        if let Err(error) = self.run_search_projection_maintenance_slice() {
            maintenance_errors.push(error.to_string());
        }
        let search_projection_ready = match self.search_projection_ready() {
            Ok(ready) => ready,
            Err(error) => {
                maintenance_errors.push(error.to_string());
                false
            }
        };
        ArchiveImportOutcome {
            search_projection_ready,
            maintenance_error: (!maintenance_errors.is_empty())
                .then(|| maintenance_errors.join("; ")),
        }
    }

    fn validate_archive_stage_snapshot(
        &self,
        archive: &SessionHistoryArchive,
        stage: ArchiveImportStage,
        start: usize,
        end: usize,
        overwrite: bool,
    ) -> Result<()> {
        self.with_read_snapshot(|| {
            let tx = ArchiveWriteTransaction { conn: &self.conn };
            reject_import_stage_conflicts(&tx, archive, stage, start, end, overwrite)
        })
    }
}

pub fn validate_archive_version(archive: &SessionHistoryArchive) -> Result<()> {
    if matches!((archive.schema_version, archive.version), (1, 1) | (2, 2)) {
        Ok(())
    } else {
        Err(StoreError::UnsupportedArchiveVersion(
            archive.schema_version.max(archive.version),
        ))
    }
}

fn expected_archive_blob_path(id: Uuid, blob_hash: &str) -> Result<String> {
    if blob_hash.get(..2).is_none() {
        return Err(StoreError::ArchiveArtifactPathMismatch { id });
    }
    Ok(object_relative_path(blob_hash))
}

fn validate_archive_artifact_record_blobs(
    blob_dir: &Path,
    archive: &SessionHistoryArchive,
) -> Result<ValidatedArchiveBlobs> {
    let mut blobs = Vec::with_capacity(archive.artifact_records.len());
    for artifact in &archive.artifact_records {
        blobs.push(validate_archive_artifact_record_blob(blob_dir, artifact)?);
    }
    Ok(ValidatedArchiveBlobs { blobs })
}

fn validate_archive_artifact_record_blob(
    blob_dir: &Path,
    artifact: &Artifact,
) -> Result<ValidatedArchiveBlob> {
    let expected_path = expected_archive_blob_path(artifact.id, &artifact.blob_hash)?;
    let legacy_path = {
        let shard = &artifact.blob_hash[..2];
        format!("{LEGACY_BLOBS_DIR}/{shard}/{}", artifact.blob_hash)
    };
    if artifact.blob_path != expected_path && artifact.blob_path != legacy_path {
        return Err(StoreError::ArchiveArtifactPathMismatch { id: artifact.id });
    }

    let absolute_path = blob_dir
        .join(&artifact.blob_hash[..2])
        .join(&artifact.blob_hash);
    ensure_regular_blob_file(artifact.id, &absolute_path).map_err(|error| match error {
        StoreError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            StoreError::ArchiveArtifactMissingContent { id: artifact.id }
        }
        error => error,
    })?;
    let file = File::open(&absolute_path)?;
    let before = file_snapshot(&file)?;
    let (hash, byte_size) = sha256_reader_hex(&mut BufReader::new(&file))?;
    let after = file_snapshot(&file)?;
    if before != after {
        return Err(StoreError::ArchiveArtifactChangedDuringValidation { id: artifact.id });
    }
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if byte_size != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id: artifact.id });
    }
    Ok(ValidatedArchiveBlob {
        id: artifact.id,
        path: absolute_path,
        snapshot: after,
    })
}

fn ensure_archive_import_progress_table(tx: &ArchiveWriteTransaction<'_>) -> Result<()> {
    tx.execute_batch(&format!(
        r#"
        CREATE TABLE IF NOT EXISTS {ARCHIVE_IMPORT_PROGRESS_TABLE} (
            fingerprint TEXT PRIMARY KEY,
            stage INTEGER NOT NULL,
            cursor INTEGER NOT NULL,
            overwrite INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );
        "#
    ))?;
    Ok(())
}

fn archive_import_progress(
    tx: &ArchiveWriteTransaction<'_>,
    fingerprint: &str,
) -> Result<Option<(ArchiveImportStage, usize, bool)>> {
    use rusqlite::OptionalExtension;

    let value = tx
        .query_row(
            &format!(
                "SELECT stage, cursor, overwrite FROM {ARCHIVE_IMPORT_PROGRESS_TABLE} WHERE fingerprint = ?1"
            ),
            [fingerprint],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            },
        )
        .optional()?;
    value
        .map(|(stage, cursor, overwrite)| {
            let stage = ArchiveImportStage::from_i64(stage)
                .ok_or(StoreError::ArchiveImportProgressCorrupt)?;
            let cursor =
                usize::try_from(cursor).map_err(|_| StoreError::ArchiveImportProgressCorrupt)?;
            Ok((stage, cursor, overwrite))
        })
        .transpose()
}

fn set_archive_import_progress(
    tx: &ArchiveWriteTransaction<'_>,
    fingerprint: &str,
    stage: ArchiveImportStage,
    cursor: usize,
    overwrite: bool,
) -> Result<()> {
    tx.execute(
        &format!(
            r#"
            INSERT INTO {ARCHIVE_IMPORT_PROGRESS_TABLE}
                (fingerprint, stage, cursor, overwrite, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(fingerprint) DO UPDATE SET
                stage = excluded.stage,
                cursor = excluded.cursor,
                overwrite = excluded.overwrite,
                updated_at_ms = excluded.updated_at_ms
            "#
        ),
        rusqlite::params![
            fingerprint,
            stage as i64,
            i64::try_from(cursor).unwrap_or(i64::MAX),
            overwrite,
            utc_now().timestamp_millis(),
        ],
    )?;
    Ok(())
}

fn archive_import_batch_end(
    archive: &SessionHistoryArchive,
    stage: ArchiveImportStage,
    start: usize,
    stage_len: usize,
) -> Result<usize> {
    let mut end = start;
    let mut bytes = 0_u64;
    while end < stage_len && end.saturating_sub(start) < ARCHIVE_IMPORT_BATCH_ROWS {
        let item_bytes = archive_import_item_size(archive, stage, end)?;
        if end > start && bytes.saturating_add(item_bytes) > ARCHIVE_IMPORT_BATCH_BYTES {
            break;
        }
        bytes = bytes.saturating_add(item_bytes);
        end += 1;
        if bytes >= ARCHIVE_IMPORT_BATCH_BYTES {
            break;
        }
    }
    Ok(end.max(start.saturating_add(1)).min(stage_len))
}

fn archive_import_batch_write_estimate(
    archive: &SessionHistoryArchive,
    stage: ArchiveImportStage,
    start: usize,
    end: usize,
) -> Result<u64> {
    let bytes = (start..end).try_fold(0_u64, |total, index| {
        archive_import_item_size(archive, stage, index).map(|bytes| total.saturating_add(bytes))
    })?;
    Ok(bytes.saturating_mul(3).max(crate::INDEXING_WAL_DELTA_BYTES))
}

fn archive_requires_batched_import(archive: &SessionHistoryArchive) -> Result<bool> {
    let entity_count = archive
        .records
        .len()
        .saturating_add(archive.capture_sources.len())
        .saturating_add(archive.sessions.len())
        .saturating_add(archive.runs.len())
        .saturating_add(archive.events.len())
        .saturating_add(archive.artifact_records.len())
        .saturating_add(archive.vcs_workspaces.len())
        .saturating_add(archive.vcs_changes.len())
        .saturating_add(archive.history_record_links.len())
        .saturating_add(archive.summaries.len())
        .saturating_add(archive.files_touched.len());
    Ok(entity_count > ARCHIVE_IMPORT_BATCH_ROWS
        || serialized_size(archive)? > ARCHIVE_IMPORT_BATCH_BYTES)
}

fn archive_import_item_size(
    archive: &SessionHistoryArchive,
    stage: ArchiveImportStage,
    index: usize,
) -> Result<u64> {
    match stage {
        ArchiveImportStage::Records => serialized_size(&archive.records[index]),
        ArchiveImportStage::CaptureSources => serialized_size(&archive.capture_sources[index]),
        ArchiveImportStage::VcsWorkspaces => serialized_size(&archive.vcs_workspaces[index]),
        ArchiveImportStage::Artifacts => serialized_size(&archive.artifact_records[index]),
        ArchiveImportStage::Sessions => serialized_size(&archive.sessions[index]),
        ArchiveImportStage::Runs => serialized_size(&archive.runs[index]),
        ArchiveImportStage::Events => serialized_size(&archive.events[index]),
        ArchiveImportStage::VcsChanges => serialized_size(&archive.vcs_changes[index]),
        ArchiveImportStage::Summaries => serialized_size(&archive.summaries[index]),
        ArchiveImportStage::FilesTouched => serialized_size(&archive.files_touched[index]),
        ArchiveImportStage::HistoryRecordLinks => {
            serialized_size(&archive.history_record_links[index])
        }
        ArchiveImportStage::Complete => Ok(0),
    }
}

#[derive(Default)]
struct SaturatingCountWriter(u64);

impl Write for SaturatingCountWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(buffer.len() as u64);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size(value: &impl Serialize) -> Result<u64> {
    let mut writer = SaturatingCountWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}

struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn archive_import_fingerprint(archive: &SessionHistoryArchive, context: &[u8]) -> Result<String> {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, archive)?;
    writer.0.update([0]);
    writer.0.update(context);
    Ok(format!("{:x}", writer.0.finalize()))
}

fn estimate_archive_write_bytes(
    sqlite_path: &Path,
    archive: &SessionHistoryArchive,
) -> Result<u64> {
    let serialized_archive_bytes = serialized_size(archive)?;
    let blob_bytes = archive
        .artifact_records
        .iter()
        .fold(0_u64, |total, artifact| {
            total.saturating_add(artifact.byte_size)
        });
    let incoming_amplification = serialized_archive_bytes
        .saturating_add(blob_bytes)
        .saturating_mul(3);
    let existing_database_amplification = sqlite_amplifying_write_estimate(sqlite_path, 2, 0)?;
    Ok(existing_database_amplification
        .saturating_add(incoming_amplification)
        .max(crate::INDEXING_WAL_DELTA_BYTES))
}
