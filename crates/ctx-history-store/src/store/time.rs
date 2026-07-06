#[allow(unused_imports)]
use super::*;

pub(crate) fn timestamp_ms(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

pub(crate) fn time_ms(value: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub(crate) fn upsert_capture_source_tx(
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

pub(crate) fn upsert_vcs_change_tx(tx: &Transaction<'_>, change: &VcsChange) -> Result<Uuid> {
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

pub(crate) fn optional_timestamp_ms(value: Option<DateTime<Utc>>) -> Option<i64> {
    value.map(timestamp_ms)
}

pub(crate) fn optional_ms_to_time(value: Option<i64>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value.map(ms_to_time).transpose()
}
