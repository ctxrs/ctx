#[allow(unused_imports)]
use super::*;

pub(crate) fn ensure_regular_blob_file(id: Uuid, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(StoreError::ArchiveArtifactNonRegularFile {
            id,
            path: path.to_path_buf(),
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

pub(crate) fn reject_import_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    for record in &archive.records {
        if row_exists(tx, "history_records", record.id)? {
            return Err(StoreError::ImportConflict {
                kind: "record",
                id: record.id,
            });
        }
    }
    reject_rich_import_conflicts(tx, archive)?;
    Ok(())
}

pub(crate) fn reject_import_invariant_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    for event in &archive.events {
        if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict_tx(tx, dedupe_key)?;
        }
    }
    Ok(())
}

pub(crate) fn reject_rich_import_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    for source in &archive.capture_sources {
        reject_entity_conflict(
            existing_capture_source_by_id(tx, source.id)?,
            source,
            "capture_source",
            source.id,
        )?;
    }
    for workspace in &archive.vcs_workspaces {
        reject_entity_conflict(
            existing_vcs_workspace_by_id(tx, workspace.id)?,
            workspace,
            "vcs_workspace",
            workspace.id,
        )?;
        reject_entity_conflict(
            existing_vcs_workspace_by_identity(tx, workspace)?,
            workspace,
            "vcs_workspace",
            workspace.id,
        )?;
    }
    for artifact in &archive.artifact_records {
        reject_entity_conflict(
            existing_artifact_by_id(tx, artifact.id)?,
            artifact,
            "artifact",
            artifact.id,
        )?;
        reject_entity_conflict(
            existing_artifact_by_identity(tx, artifact)?,
            artifact,
            "artifact",
            artifact.id,
        )?;
    }
    for session in &archive.sessions {
        reject_entity_conflict(
            existing_session_by_id(tx, session.id)?,
            session,
            "session",
            session.id,
        )?;
        if let Some(external_session_id) = &session.external_session_id {
            reject_entity_conflict(
                existing_session_by_external_session(tx, session.provider, external_session_id)?,
                session,
                "session",
                session.id,
            )?;
        }
    }
    for run in &archive.runs {
        reject_entity_conflict(existing_run_by_id(tx, run.id)?, run, "run", run.id)?;
    }
    for event in &archive.events {
        reject_entity_conflict(
            existing_event_by_id(tx, event.id)?,
            event,
            "event",
            event.id,
        )?;
        reject_entity_conflict(
            existing_event_by_seq(tx, event.seq)?,
            event,
            "event",
            event.id,
        )?;
        if let Some(dedupe_key) = &event.dedupe_key {
            reject_provider_event_hash_conflict_tx(tx, dedupe_key)?;
            reject_entity_conflict(
                existing_event_by_dedupe_key(tx, dedupe_key)?,
                event,
                "event",
                event.id,
            )?;
        }
    }
    for change in &archive.vcs_changes {
        reject_entity_conflict(
            existing_vcs_change_by_id(tx, change.id)?,
            change,
            "vcs_change",
            change.id,
        )?;
        reject_entity_conflict(
            existing_vcs_change_by_identity(tx, change)?,
            change,
            "vcs_change",
            change.id,
        )?;
    }
    for summary in &archive.summaries {
        reject_entity_conflict(
            existing_summary_by_id(tx, summary.id)?,
            summary,
            "summary",
            summary.id,
        )?;
    }
    for file in &archive.files_touched {
        reject_entity_conflict(
            existing_file_touched_by_id(tx, file.id)?,
            file,
            "file_touched",
            file.id,
        )?;
    }
    for link in &archive.history_record_links {
        reject_entity_conflict(
            existing_history_record_link_by_id(tx, link.id)?,
            link,
            "history_record_link",
            link.id,
        )?;
        reject_entity_conflict(
            existing_history_record_link_by_identity(tx, link)?,
            link,
            "history_record_link",
            link.id,
        )?;
    }
    Ok(())
}

pub(crate) fn reject_archive_event_internal_conflicts(
    archive: &SessionHistoryArchive,
) -> Result<()> {
    let mut seen_seq: HashMap<u64, &Event> = HashMap::new();
    let mut seen_provider_events: HashMap<(String, String, Option<String>, u64), String> =
        HashMap::new();

    for event in &archive.events {
        if let Some(existing) = seen_seq.insert(event.seq, event) {
            if existing != event {
                return Err(StoreError::ImportConflict {
                    kind: "event",
                    id: event.id,
                });
            }
        }

        let Some(dedupe_key) = &event.dedupe_key else {
            continue;
        };
        let Some(parsed) = parse_provider_event_dedupe_key(dedupe_key) else {
            continue;
        };
        let key = (
            parsed.provider,
            parsed.external_session_id,
            parsed.source_id,
            parsed.provider_index,
        );
        if let Some(existing_hash) = seen_provider_events.get(&key) {
            if existing_hash != &parsed.payload_hash {
                return Err(StoreError::ProviderEventConflict {
                    provider: key.0,
                    external_session_id: key.1,
                    provider_index: key.3,
                    existing_hash: existing_hash.clone(),
                    new_hash: parsed.payload_hash,
                });
            }
        } else {
            seen_provider_events.insert(key, parsed.payload_hash);
        }
    }

    Ok(())
}

pub(crate) fn expected_archive_blob_path(id: Uuid, blob_hash: &str) -> Result<String> {
    if blob_hash.get(..2).is_none() {
        return Err(StoreError::ArchiveArtifactPathMismatch { id });
    }
    Ok(object_relative_path(blob_hash))
}

pub(crate) fn validate_archive_artifact_record_blobs(
    blob_dir: &Path,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    for artifact in &archive.artifact_records {
        validate_archive_artifact_record_blob(blob_dir, artifact)?;
    }
    Ok(())
}

pub(crate) fn validate_archive_artifact_record_blob(
    blob_dir: &Path,
    artifact: &Artifact,
) -> Result<()> {
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

pub(crate) fn import_rich_archive_entities_tx(
    tx: &Transaction<'_>,
    blob_dir: &Path,
    archive: &SessionHistoryArchive,
    _blob_guard: &mut BlobWriteGuard,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    validate_archive_artifact_record_blobs(blob_dir, archive)?;

    for source in &archive.capture_sources {
        upsert_imported_capture_source_tx(tx, source)?;
    }
    for workspace in &archive.vcs_workspaces {
        upsert_vcs_workspace_tx(tx, workspace)?;
    }
    for artifact in &archive.artifact_records {
        upsert_artifact_tx(tx, artifact)?;
    }
    for session in &archive.sessions {
        upsert_session_tx(tx, session)?;
    }
    for run in &archive.runs {
        upsert_run_tx(tx, run)?;
    }
    for event in &archive.events {
        upsert_event_tx(tx, event)?;
    }
    for change in &archive.vcs_changes {
        upsert_vcs_change_tx(tx, change)?;
    }
    for summary in &archive.summaries {
        upsert_summary_tx(tx, summary)?;
    }
    for file in &archive.files_touched {
        upsert_file_touched_tx(tx, file)?;
    }
    for link in &archive.history_record_links {
        upsert_history_record_link_tx(tx, link)?;
    }
    Ok(())
}
