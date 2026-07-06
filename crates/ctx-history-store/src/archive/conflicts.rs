use ctx_history_core::{
    Artifact, ArtifactKind, CaptureProvider, CaptureSource, Event, FileTouched, HistoryRecordLink,
    Run, Session, SessionHistoryArchive, Summary, VcsChange, VcsWorkspace,
};
use std::collections::HashMap;

use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::artifacts::{artifact_from_row, artifact_select_sql};
use crate::events::{event_from_row, event_select_sql};
use crate::events::{parse_provider_event_dedupe_key, reject_provider_event_hash_conflict_tx};
use crate::files::{file_touched_from_row, file_touched_select_sql};
use crate::records::{history_record_link_from_row, history_record_link_select_sql};
use crate::runs::{run_from_row, run_select_sql};
use crate::sessions::{session_from_row, session_select_sql};
use crate::sources::capture_source_from_row;
use crate::summaries::{summary_from_row, summary_select_sql};
use crate::vcs::{
    vcs_change_from_row, vcs_change_select_sql, vcs_workspace_from_row, vcs_workspace_select_sql,
};
use crate::{Result, StoreError};

pub(super) fn reject_import_conflicts(
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

pub(super) fn reject_capture_source_import_conflict(
    tx: &Transaction<'_>,
    source_id: Uuid,
) -> Result<()> {
    if row_exists(tx, "capture_sources", source_id)? {
        return Err(StoreError::ImportConflict {
            kind: "capture_source",
            id: source_id,
        });
    }
    Ok(())
}

pub(super) fn reject_import_invariant_conflicts(
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

fn row_exists(tx: &Transaction<'_>, table: &str, id: Uuid) -> Result<bool> {
    let sql = format!("SELECT 1 FROM {table} WHERE id = ?1");
    Ok(tx
        .query_row(&sql, params![id.to_string()], |_| Ok(()))
        .optional()?
        .is_some())
}

fn reject_rich_import_conflicts(
    tx: &Transaction<'_>,
    archive: &SessionHistoryArchive,
) -> Result<()> {
    if archive.schema_version < 2 && archive.version < 2 {
        return Ok(());
    }

    let archive_sources = archive
        .capture_sources
        .iter()
        .map(|source| (source.id, source))
        .collect::<HashMap<_, _>>();

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
        if session.external_session_id.is_some() {
            reject_entity_conflict(
                existing_source_scoped_session_by_external_session(tx, &archive_sources, session)?,
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

pub(super) fn reject_archive_event_internal_conflicts(
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

fn reject_entity_conflict<T: PartialEq>(
    existing: Option<T>,
    incoming: &T,
    kind: &'static str,
    id: Uuid,
) -> Result<()> {
    if let Some(existing) = existing {
        if existing != *incoming {
            return Err(StoreError::ImportConflict { kind, id });
        }
    }
    Ok(())
}

fn existing_capture_source_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<CaptureSource>> {
    tx.query_row(
        "SELECT id, kind, provider, machine_id, process_id, cwd, raw_source_path, source_format, source_root, source_identity, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json FROM capture_sources WHERE id = ?1",
        params![id.to_string()],
        capture_source_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_session_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Session>> {
    tx.query_row(
        session_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        session_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_session_by_external_session(
    tx: &Transaction<'_>,
    provider: CaptureProvider,
    external_session_id: &str,
) -> Result<Vec<Session>> {
    let mut stmt = tx.prepare(
        session_select_sql(
            "WHERE provider = ?1 AND external_session_id = ?2 ORDER BY started_at_ms DESC",
        )
        .as_str(),
    )?;
    let rows = stmt.query_map(
        params![provider.as_str(), external_session_id],
        session_from_row,
    )?;
    crate::connection::collect_rows(rows)
}

fn existing_source_scoped_session_by_external_session(
    tx: &Transaction<'_>,
    archive_sources: &HashMap<Uuid, &CaptureSource>,
    session: &Session,
) -> Result<Option<Session>> {
    let Some(external_session_id) = session.external_session_id.as_deref() else {
        return Ok(None);
    };
    let incoming_source = session_source(tx, archive_sources, session.capture_source_id)?;
    for existing in existing_session_by_external_session(tx, session.provider, external_session_id)?
    {
        if sessions_share_external_source(tx, incoming_source.as_ref(), session, &existing)? {
            return Ok(Some(existing));
        }
    }
    Ok(None)
}

fn session_source(
    tx: &Transaction<'_>,
    archive_sources: &HashMap<Uuid, &CaptureSource>,
    source_id: Option<Uuid>,
) -> Result<Option<CaptureSource>> {
    let Some(source_id) = source_id else {
        return Ok(None);
    };
    if let Some(source) = archive_sources.get(&source_id) {
        return Ok(Some((*source).clone()));
    }
    existing_capture_source_by_id(tx, source_id)
}

fn sessions_share_external_source(
    tx: &Transaction<'_>,
    incoming_source: Option<&CaptureSource>,
    incoming: &Session,
    existing: &Session,
) -> Result<bool> {
    let Some(incoming_source_id) = incoming.capture_source_id else {
        return Ok(true);
    };
    let Some(existing_source_id) = existing.capture_source_id else {
        return Ok(true);
    };
    if incoming_source_id == existing_source_id {
        return Ok(true);
    }
    let Some(incoming_source) = incoming_source else {
        return Ok(true);
    };
    let Some(existing_source) = existing_capture_source_by_id(tx, existing_source_id)? else {
        return Ok(true);
    };
    Ok(capture_sources_share_identity(
        incoming_source,
        &existing_source,
    ))
}

fn capture_sources_share_identity(left: &CaptureSource, right: &CaptureSource) -> bool {
    let left = &left.descriptor;
    let right = &right.descriptor;
    if left.provider != right.provider {
        return false;
    }
    if left.source_format != right.source_format {
        return false;
    }
    if let (Some(left), Some(right)) = (
        left.source_identity.as_deref(),
        right.source_identity.as_deref(),
    ) {
        if left == right {
            return true;
        }
    }
    if let (Some(left), Some(right)) = (
        left.raw_source_path.as_deref(),
        right.raw_source_path.as_deref(),
    ) {
        return left == right;
    }
    match (left.source_root.as_deref(), right.source_root.as_deref()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn existing_run_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Run>> {
    tx.query_row(
        run_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        run_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_dedupe_key(tx: &Transaction<'_>, dedupe_key: &str) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE dedupe_key = ?1").as_str(),
        params![dedupe_key],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_event_by_seq(tx: &Transaction<'_>, seq: u64) -> Result<Option<Event>> {
    tx.query_row(
        event_select_sql("WHERE seq = ?1").as_str(),
        params![seq as i64],
        event_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Artifact>> {
    tx.query_row(
        artifact_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        artifact_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_hash_kind(
    tx: &Transaction<'_>,
    blob_hash: &str,
    kind: ArtifactKind,
) -> Result<Option<Artifact>> {
    tx.query_row(
        artifact_select_sql("WHERE blob_hash = ?1 AND kind = ?2").as_str(),
        params![blob_hash, kind.as_str()],
        artifact_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_artifact_by_identity(
    tx: &Transaction<'_>,
    artifact: &Artifact,
) -> Result<Option<Artifact>> {
    existing_artifact_by_hash_kind(tx, &artifact.blob_hash, artifact.kind)
}

fn existing_vcs_workspace_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<VcsWorkspace>> {
    tx.query_row(
        vcs_workspace_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        vcs_workspace_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_workspace_by_identity(
    tx: &Transaction<'_>,
    workspace: &VcsWorkspace,
) -> Result<Option<VcsWorkspace>> {
    tx.query_row(
        vcs_workspace_select_sql("WHERE kind = ?1 AND repo_fingerprint = ?2").as_str(),
        params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
        vcs_workspace_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_change_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<VcsChange>> {
    tx.query_row(
        vcs_change_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        vcs_change_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_vcs_change_by_identity(
    tx: &Transaction<'_>,
    change: &VcsChange,
) -> Result<Option<VcsChange>> {
    tx.query_row(
        vcs_change_select_sql("WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3")
            .as_str(),
        params![
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str()
        ],
        vcs_change_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_summary_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<Summary>> {
    tx.query_row(
        summary_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        summary_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_file_touched_by_id(tx: &Transaction<'_>, id: Uuid) -> Result<Option<FileTouched>> {
    tx.query_row(
        file_touched_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        file_touched_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_history_record_link_by_id(
    tx: &Transaction<'_>,
    id: Uuid,
) -> Result<Option<HistoryRecordLink>> {
    tx.query_row(
        history_record_link_select_sql("WHERE id = ?1").as_str(),
        params![id.to_string()],
        history_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}

fn existing_history_record_link_by_identity(
    tx: &Transaction<'_>,
    link: &HistoryRecordLink,
) -> Result<Option<HistoryRecordLink>> {
    tx.query_row(
        history_record_link_select_sql(
            "WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        )
        .as_str(),
        params![
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        history_record_link_from_row,
    )
    .optional()
    .map_err(StoreError::from)
}
