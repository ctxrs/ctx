use ctx_history_core::{
    Artifact, CaptureSource, CaptureSourceDescriptor, Event, Fidelity, FileTouched, HistoryRecord,
    HistoryRecordLink, Run, Session, SessionHistoryArchive, Summary, VcsChange, VcsWorkspace,
};
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

use super::validate_archive_artifact_record_blobs;
use crate::connection::{optional_timestamp_ms, optional_uuid_string, parse_uuid, timestamp_ms};
use crate::object_store::BlobWriteGuard;
use crate::{Result, StoreError};

pub(super) fn upsert_capture_source_tx(
    tx: &Transaction<'_>,
    source_id: Uuid,
    source: &CaptureSourceDescriptor,
    occurred_at: DateTime<Utc>,
    fidelity: Fidelity,
) -> Result<()> {
    let occurred_at_ms = timestamp_ms(occurred_at);
    tx.execute(
        r#"
        INSERT INTO capture_sources
        (
            id, kind, provider, machine_id, process_id, cwd, raw_source_path,
            external_session_id, started_at_ms, ended_at_ms, fidelity,
            visibility, sync_state, sync_version, metadata_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, 'local_only', 'local_only', 0, '{}')
        ON CONFLICT(id) DO UPDATE SET
            kind = excluded.kind,
            provider = excluded.provider,
            machine_id = excluded.machine_id,
            process_id = excluded.process_id,
            cwd = excluded.cwd,
            raw_source_path = excluded.raw_source_path,
            external_session_id = excluded.external_session_id,
            started_at_ms = excluded.started_at_ms,
            fidelity = excluded.fidelity
        "#,
        params![
            source_id.to_string(),
            source.kind.as_str(),
            source.provider.as_str(),
            source.machine_id.as_str(),
            source.process_id.map(i64::from),
            source.cwd.as_deref(),
            source.raw_source_path.as_deref(),
            source.external_session_id.as_deref(),
            occurred_at_ms,
            fidelity.as_str(),
        ],
    )?;
    Ok(())
}

pub(super) fn import_rich_archive_entities_tx(
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

fn upsert_imported_capture_source_tx(tx: &Transaction<'_>, source: &CaptureSource) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO capture_sources
        (id, kind, provider, machine_id, process_id, cwd, raw_source_path, external_session_id, started_at_ms, ended_at_ms, fidelity, visibility, sync_state, sync_version, metadata_json)
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

fn upsert_session_tx(tx: &Transaction<'_>, session: &Session) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO sessions
        (id, history_record_id, parent_session_id, root_session_id, capture_source_id, provider, external_session_id, external_agent_id, agent_type, role_hint, is_primary, status, fidelity, transcript_blob_id, started_at_ms, ended_at_ms, created_at_ms, updated_at_ms, visibility, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            parent_session_id = excluded.parent_session_id,
            root_session_id = excluded.root_session_id,
            capture_source_id = excluded.capture_source_id,
            provider = excluded.provider,
            external_session_id = excluded.external_session_id,
            external_agent_id = excluded.external_agent_id,
            agent_type = excluded.agent_type,
            role_hint = excluded.role_hint,
            is_primary = excluded.is_primary,
            status = excluded.status,
            fidelity = excluded.fidelity,
            transcript_blob_id = excluded.transcript_blob_id,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            visibility = excluded.visibility,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            session.id.to_string(),
            optional_uuid_string(session.history_record_id),
            optional_uuid_string(session.parent_session_id),
            optional_uuid_string(session.root_session_id),
            optional_uuid_string(session.capture_source_id),
            session.provider.as_str(),
            session.external_session_id.as_deref(),
            session.external_agent_id.as_deref(),
            session.agent_type.as_str(),
            session.role_hint.as_deref(),
            session.is_primary as i64,
            session.status.as_str(),
            session.sync.fidelity.as_str(),
            optional_uuid_string(session.transcript_blob_id),
            timestamp_ms(session.started_at),
            optional_timestamp_ms(session.ended_at),
            timestamp_ms(session.timestamps.created_at),
            timestamp_ms(session.timestamps.updated_at),
            session.sync.visibility.as_str(),
            session.sync.sync_state.as_str(),
            session.sync.sync_version as i64,
            optional_timestamp_ms(session.sync.deleted_at),
            serde_json::to_string(&session.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_run_tx(tx: &Transaction<'_>, run: &Run) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO runs
        (id, history_record_id, session_id, run_type, status, started_at_ms, ended_at_ms, exit_code, cwd, command_preview, input_blob_id, output_blob_id, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            run_type = excluded.run_type,
            status = excluded.status,
            started_at_ms = excluded.started_at_ms,
            ended_at_ms = excluded.ended_at_ms,
            exit_code = excluded.exit_code,
            cwd = excluded.cwd,
            command_preview = excluded.command_preview,
            input_blob_id = excluded.input_blob_id,
            output_blob_id = excluded.output_blob_id,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            run.id.to_string(),
            optional_uuid_string(run.history_record_id),
            optional_uuid_string(run.session_id),
            run.run_type.as_str(),
            run.status.as_str(),
            timestamp_ms(run.started_at),
            optional_timestamp_ms(run.ended_at),
            run.exit_code,
            run.cwd.as_deref(),
            run.command_preview.as_deref(),
            optional_uuid_string(run.input_blob_id),
            optional_uuid_string(run.output_blob_id),
            timestamp_ms(run.timestamps.created_at),
            timestamp_ms(run.timestamps.updated_at),
            optional_uuid_string(run.source_id),
            run.sync.visibility.as_str(),
            run.sync.fidelity.as_str(),
            run.sync.sync_state.as_str(),
            run.sync.sync_version as i64,
            optional_timestamp_ms(run.sync.deleted_at),
            serde_json::to_string(&run.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_event_tx(tx: &Transaction<'_>, event: &Event) -> Result<Uuid> {
    let event_id = if let Some(dedupe_key) = &event.dedupe_key {
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM events WHERE dedupe_key = ?1",
                params![dedupe_key],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .optional()?
        {
            existing
        } else {
            event.id
        }
    } else {
        event.id
    };

    tx.execute(
        r#"
        INSERT INTO events
        (id, seq, history_record_id, session_id, run_id, event_type, role, occurred_at_ms, capture_source_id, payload_json, payload_blob_id, dedupe_key, visibility, redaction_state, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            seq = excluded.seq,
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            run_id = excluded.run_id,
            event_type = excluded.event_type,
            role = excluded.role,
            occurred_at_ms = excluded.occurred_at_ms,
            capture_source_id = excluded.capture_source_id,
            payload_json = excluded.payload_json,
            payload_blob_id = excluded.payload_blob_id,
            dedupe_key = excluded.dedupe_key,
            visibility = excluded.visibility,
            redaction_state = excluded.redaction_state,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            event_id.to_string(),
            event.seq as i64,
            optional_uuid_string(event.history_record_id),
            optional_uuid_string(event.session_id),
            optional_uuid_string(event.run_id),
            event.event_type.as_str(),
            event.role.map(|role| role.as_str()),
            timestamp_ms(event.occurred_at),
            optional_uuid_string(event.capture_source_id),
            serde_json::to_string(&event.payload)?,
            optional_uuid_string(event.payload_blob_id),
            event.dedupe_key.as_deref(),
            event.sync.visibility.as_str(),
            event.redaction_state.as_str(),
            event.sync.fidelity.as_str(),
            event.sync.sync_state.as_str(),
            event.sync.sync_version as i64,
            optional_timestamp_ms(event.sync.deleted_at),
            serde_json::to_string(&event.sync.metadata)?,
        ],
    )?;
    Ok(event_id)
}

fn upsert_artifact_tx(tx: &Transaction<'_>, artifact: &Artifact) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO artifacts
        (id, kind, blob_hash, blob_path, byte_size, media_type, preview_text, redaction_state, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT DO UPDATE SET
            blob_path = excluded.blob_path,
            byte_size = excluded.byte_size,
            media_type = excluded.media_type,
            preview_text = excluded.preview_text,
            redaction_state = excluded.redaction_state,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            artifact.id.to_string(),
            artifact.kind.as_str(),
            artifact.blob_hash.as_str(),
            artifact.blob_path.as_str(),
            artifact.byte_size as i64,
            artifact.media_type.as_deref(),
            artifact.preview_text.as_deref(),
            artifact.redaction_state.as_str(),
            timestamp_ms(artifact.timestamps.created_at),
            timestamp_ms(artifact.timestamps.updated_at),
            optional_uuid_string(artifact.source_id),
            artifact.sync.visibility.as_str(),
            artifact.sync.fidelity.as_str(),
            artifact.sync.sync_state.as_str(),
            artifact.sync.sync_version as i64,
            optional_timestamp_ms(artifact.sync.deleted_at),
            serde_json::to_string(&artifact.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
        params![artifact.blob_hash.as_str(), artifact.kind.as_str()],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_vcs_workspace_tx(tx: &Transaction<'_>, workspace: &VcsWorkspace) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO vcs_workspaces
        (id, kind, root_path, repo_fingerprint, primary_remote_url_normalized, host, owner, name, monorepo_subpath, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(kind, repo_fingerprint) DO UPDATE SET
            root_path = excluded.root_path,
            primary_remote_url_normalized = excluded.primary_remote_url_normalized,
            host = excluded.host,
            owner = excluded.owner,
            name = excluded.name,
            monorepo_subpath = excluded.monorepo_subpath,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            workspace.id.to_string(),
            workspace.kind.as_str(),
            workspace.root_path.as_str(),
            workspace.repo_fingerprint.as_str(),
            workspace.primary_remote_url_normalized.as_deref(),
            workspace.host.as_str(),
            workspace.owner.as_deref(),
            workspace.name.as_deref(),
            workspace.monorepo_subpath.as_deref(),
            timestamp_ms(workspace.timestamps.created_at),
            timestamp_ms(workspace.timestamps.updated_at),
            optional_uuid_string(workspace.source_id),
            workspace.sync.visibility.as_str(),
            workspace.sync.fidelity.as_str(),
            workspace.sync.sync_state.as_str(),
            workspace.sync.sync_version as i64,
            optional_timestamp_ms(workspace.sync.deleted_at),
            serde_json::to_string(&workspace.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM vcs_workspaces WHERE kind = ?1 AND repo_fingerprint = ?2",
        params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

fn upsert_vcs_change_tx(tx: &Transaction<'_>, change: &VcsChange) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO vcs_changes
        (id, vcs_workspace_id, kind, change_id, parent_change_ids_json, branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(vcs_workspace_id, kind, change_id) DO UPDATE SET
            parent_change_ids_json = excluded.parent_change_ids_json,
            branch_or_bookmark = excluded.branch_or_bookmark,
            tree_hash = excluded.tree_hash,
            author_time_ms = excluded.author_time_ms,
            confidence = excluded.confidence,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            change.id.to_string(),
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str(),
            serde_json::to_string(&change.parent_change_ids)?,
            change.branch_or_bookmark.as_deref(),
            change.tree_hash.as_deref(),
            optional_timestamp_ms(change.author_time),
            change.confidence.as_str(),
            timestamp_ms(change.timestamps.created_at),
            timestamp_ms(change.timestamps.updated_at),
            optional_uuid_string(change.source_id),
            change.sync.visibility.as_str(),
            change.sync.fidelity.as_str(),
            change.sync.sync_state.as_str(),
            change.sync.sync_version as i64,
            optional_timestamp_ms(change.sync.deleted_at),
            serde_json::to_string(&change.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM vcs_changes WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3",
        params![
            change.vcs_workspace_id.to_string(),
            change.kind.as_str(),
            change.change_id.as_str()
        ],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}

pub(super) fn upsert_record_tx(
    tx: &Transaction<'_>,
    record: &HistoryRecord,
    source_id: Option<Uuid>,
) -> Result<()> {
    let created_at_ms = timestamp_ms(record.created_at);
    let updated_at_ms = timestamp_ms(record.updated_at);
    tx.execute(
        r#"
        INSERT INTO history_records
        (
            id, title, summary, status, started_at_ms, last_activity_at_ms,
            created_at_ms, updated_at_ms, source_id, body, tags_json, kind,
            workspace, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            summary = excluded.summary,
            status = excluded.status,
            started_at_ms = excluded.started_at_ms,
            last_activity_at_ms = excluded.last_activity_at_ms,
            created_at_ms = excluded.created_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            source_id = COALESCE(excluded.source_id, history_records.source_id),
            body = excluded.body,
            tags_json = excluded.tags_json,
            kind = excluded.kind,
            workspace = excluded.workspace,
            created_at = excluded.created_at,
            updated_at = excluded.updated_at
        "#,
        params![
            record.id.to_string(),
            record.title,
            record.body,
            created_at_ms,
            updated_at_ms,
            source_id.map(|id| id.to_string()),
            record.body,
            serde_json::to_string(&record.tags)?,
            record.kind,
            record.workspace,
            record.created_at.to_rfc3339(),
            record.updated_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn upsert_summary_tx(tx: &Transaction<'_>, summary: &Summary) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO summaries
        (id, history_record_id, session_id, kind, model_or_source, text, citations_json, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            session_id = excluded.session_id,
            kind = excluded.kind,
            model_or_source = excluded.model_or_source,
            text = excluded.text,
            citations_json = excluded.citations_json,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            summary.id.to_string(),
            optional_uuid_string(summary.history_record_id),
            optional_uuid_string(summary.session_id),
            summary.kind.as_str(),
            summary.model_or_source.as_deref(),
            summary.text.as_str(),
            serde_json::to_string(&summary.citations)?,
            timestamp_ms(summary.timestamps.created_at),
            timestamp_ms(summary.timestamps.updated_at),
            optional_uuid_string(summary.source_id),
            summary.sync.visibility.as_str(),
            summary.sync.fidelity.as_str(),
            summary.sync.sync_state.as_str(),
            summary.sync.sync_version as i64,
            optional_timestamp_ms(summary.sync.deleted_at),
            serde_json::to_string(&summary.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_file_touched_tx(tx: &Transaction<'_>, file: &FileTouched) -> Result<()> {
    tx.execute(
        r#"
        INSERT INTO files_touched
        (id, history_record_id, run_id, event_id, vcs_workspace_id, path, change_kind, old_path, line_count_delta, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(id) DO UPDATE SET
            history_record_id = excluded.history_record_id,
            run_id = excluded.run_id,
            event_id = excluded.event_id,
            vcs_workspace_id = excluded.vcs_workspace_id,
            path = excluded.path,
            change_kind = excluded.change_kind,
            old_path = excluded.old_path,
            line_count_delta = excluded.line_count_delta,
            confidence = excluded.confidence,
            updated_at_ms = excluded.updated_at_ms,
            source_id = excluded.source_id,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            file.id.to_string(),
            optional_uuid_string(file.history_record_id),
            optional_uuid_string(file.run_id),
            optional_uuid_string(file.event_id),
            optional_uuid_string(file.vcs_workspace_id),
            file.path.as_str(),
            file.change_kind.map(|kind| kind.as_str()),
            file.old_path.as_deref(),
            file.line_count_delta,
            file.confidence.as_str(),
            timestamp_ms(file.timestamps.created_at),
            timestamp_ms(file.timestamps.updated_at),
            optional_uuid_string(file.source_id),
            file.sync.visibility.as_str(),
            file.sync.fidelity.as_str(),
            file.sync.sync_state.as_str(),
            file.sync.sync_version as i64,
            optional_timestamp_ms(file.sync.deleted_at),
            serde_json::to_string(&file.sync.metadata)?,
        ],
    )?;
    Ok(())
}

fn upsert_history_record_link_tx(tx: &Transaction<'_>, link: &HistoryRecordLink) -> Result<Uuid> {
    tx.execute(
        r#"
        INSERT INTO history_record_links
        (id, history_record_id, target_type, target_id, link_type, confidence, source_id, created_at_ms, updated_at_ms, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
        ON CONFLICT(history_record_id, target_type, target_id, link_type) DO UPDATE SET
            confidence = excluded.confidence,
            source_id = excluded.source_id,
            updated_at_ms = excluded.updated_at_ms,
            visibility = excluded.visibility,
            fidelity = excluded.fidelity,
            sync_state = excluded.sync_state,
            sync_version = excluded.sync_version,
            deleted_at_ms = excluded.deleted_at_ms,
            metadata_json = excluded.metadata_json
        "#,
        params![
            link.id.to_string(),
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str(),
            link.confidence.as_str(),
            optional_uuid_string(link.source_id),
            timestamp_ms(link.timestamps.created_at),
            timestamp_ms(link.timestamps.updated_at),
            link.sync.visibility.as_str(),
            link.sync.fidelity.as_str(),
            link.sync.sync_state.as_str(),
            link.sync.sync_version as i64,
            optional_timestamp_ms(link.sync.deleted_at),
            serde_json::to_string(&link.sync.metadata)?,
        ],
    )?;
    tx.query_row(
        "SELECT id FROM history_record_links WHERE history_record_id = ?1 AND target_type = ?2 AND target_id = ?3 AND link_type = ?4",
        params![
            link.history_record_id.to_string(),
            link.target_type.as_str(),
            link.target_id.to_string(),
            link.link_type.as_str()
        ],
        |row| parse_uuid(row.get::<_, String>(0)?),
    )
    .map_err(StoreError::from)
}
