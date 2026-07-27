mod conflicts;
mod import;
#[cfg(test)]
mod tests;

use std::{collections::BTreeSet, fs, path::Path};

use ctx_history_core::{Artifact, SessionHistoryArchive};
use uuid::Uuid;

use crate::archive::conflicts::{
    reject_archive_event_internal_conflicts, reject_import_conflicts,
    reject_import_invariant_conflicts,
};
use crate::archive::import::{import_rich_archive_entities_tx, upsert_record_tx};
use crate::object_store::{
    ensure_regular_blob_file, object_relative_path, sha256_hex, BlobWriteGuard, LEGACY_BLOBS_DIR,
};
use crate::{Result, Store, StoreError};

#[derive(Debug, Default)]
pub(crate) struct ImportedArchiveCanonicalIds {
    pub(crate) event_ids: BTreeSet<Uuid>,
    pub(crate) vcs_change_ids: BTreeSet<Uuid>,
}

fn strip_local_verified_content_locators(events: &mut [ctx_history_core::Event]) {
    for event in events {
        crate::canonical_observations::strip_local_complete_content_metadata(
            &mut event.sync.metadata,
        );
    }
}

impl Store {
    pub fn export_archive(&self) -> Result<SessionHistoryArchive> {
        let mut events = self.list_events()?;
        strip_local_verified_content_locators(&mut events);
        Ok(SessionHistoryArchive {
            schema_version: 2,
            version: 2,
            records: self.list_records(usize::MAX)?,
            capture_sources: self.list_capture_sources()?,
            sessions: self.list_sessions()?,
            runs: self.list_runs()?,
            events,
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
    ) -> Result<()> {
        let mut archive = archive.clone();
        strip_local_verified_content_locators(&mut archive.events);
        let archive = &archive;
        validate_archive_version(archive)?;
        reject_archive_event_internal_conflicts(archive)?;
        let blob_dir = self.object_dir.clone();
        self.invalidate_event_search_projection_capabilities();
        let tx = self.conn.transaction()?;
        reject_import_invariant_conflicts(&tx, archive)?;
        if !overwrite {
            reject_import_conflicts(&tx, archive)?;
        }
        let journal_dependencies =
            crate::projection_journal::capture_archive_journal_dependencies(&tx, archive)?;
        let mut blob_guard = BlobWriteGuard::default();
        for record in &archive.records {
            upsert_record_tx(&tx, record, None)?;
        }
        let canonical_ids =
            import_rich_archive_entities_tx(&tx, &blob_dir, archive, &mut blob_guard)?;
        crate::projection_journal::journal_archive_mutations(&tx, archive, &canonical_ids, None)?;
        crate::projection_journal::journal_archive_dependencies(&tx, journal_dependencies)?;
        crate::search::projections::rebuild_search_projection(&tx)?;
        let search_capabilities =
            crate::search::projections::detect_event_search_projection_capabilities(&tx)?;
        tx.commit()?;
        self.cache_event_search_projection_capabilities(search_capabilities);
        blob_guard.commit();
        Ok(())
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
) -> Result<()> {
    for artifact in &archive.artifact_records {
        validate_archive_artifact_record_blob(blob_dir, artifact)?;
    }
    Ok(())
}

fn validate_archive_artifact_record_blob(blob_dir: &Path, artifact: &Artifact) -> Result<()> {
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
    if !absolute_path.exists() {
        return Err(StoreError::ArchiveArtifactMissingContent { id: artifact.id });
    }
    ensure_regular_blob_file(artifact.id, &absolute_path)?;
    let content = fs::read(&absolute_path)?;
    let hash = sha256_hex(&content);
    if hash != artifact.blob_hash {
        return Err(StoreError::ArchiveArtifactHashMismatch { id: artifact.id });
    }
    if content.len() as u64 != artifact.byte_size {
        return Err(StoreError::ArchiveArtifactSizeMismatch { id: artifact.id });
    }
    Ok(())
}
