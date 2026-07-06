use ctx_history_core::{EntityTimestamps, VcsChange, VcsWorkspace};
use rusqlite::params;
use uuid::Uuid;

use crate::connection::{
    collect_rows, ms_to_time, optional_ms_to_time, optional_timestamp_ms, optional_uuid_string,
    parse_optional_uuid, parse_text_enum, parse_uuid, timestamp_ms,
};
use crate::sync::sync_metadata_from_row;
use crate::{Result, Store, StoreError};

impl Store {
    pub fn upsert_vcs_workspace(&self, workspace: &VcsWorkspace) -> Result<Uuid> {
        self.conn.execute(
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
        self.conn
            .query_row(
                "SELECT id FROM vcs_workspaces WHERE kind = ?1 AND repo_fingerprint = ?2",
                params![workspace.kind.as_str(), workspace.repo_fingerprint.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn list_vcs_workspaces(&self) -> Result<Vec<VcsWorkspace>> {
        let mut stmt = self
            .conn
            .prepare(vcs_workspace_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], vcs_workspace_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_vcs_change(&self, change: &VcsChange) -> Result<Uuid> {
        self.conn.execute(
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
        self.conn
                .query_row(
                    "SELECT id FROM vcs_changes WHERE vcs_workspace_id = ?1 AND kind = ?2 AND change_id = ?3",
                    params![change.vcs_workspace_id.to_string(), change.kind.as_str(), change.change_id.as_str()],
                    |row| parse_uuid(row.get::<_, String>(0)?),
                )
                .map_err(StoreError::from)
    }

    pub(crate) fn list_vcs_changes(&self) -> Result<Vec<VcsChange>> {
        let mut stmt = self
            .conn
            .prepare(vcs_change_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], vcs_change_from_row)?;
        collect_rows(rows)
    }

    pub fn vcs_changes_for_record(&self, record_id: Uuid) -> Result<Vec<VcsChange>> {
        let mut stmt = self.conn.prepare(
            vcs_change_select_sql(
                r#"
                    WHERE id IN (
                        SELECT target_id
                        FROM history_record_links
                        WHERE history_record_id = ?1 AND target_type = 'vcs_change'
                    )
                    ORDER BY updated_at_ms DESC, id
                    "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], vcs_change_from_row)?;
        collect_rows(rows)
    }
}

pub(crate) fn vcs_workspace_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, kind, root_path, repo_fingerprint, primary_remote_url_normalized, host, owner, name, monorepo_subpath, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM vcs_workspaces {tail}"
    )
}

pub(crate) fn vcs_workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VcsWorkspace> {
    Ok(VcsWorkspace {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        kind: parse_text_enum::<ctx_history_core::VcsKind>(row.get::<_, String>(1)?)?,
        root_path: row.get(2)?,
        repo_fingerprint: row.get(3)?,
        primary_remote_url_normalized: row.get(4)?,
        host: parse_text_enum::<ctx_history_core::VcsHost>(row.get::<_, String>(5)?)?,
        owner: row.get(6)?,
        name: row.get(7)?,
        monorepo_subpath: row.get(8)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}

pub(crate) fn vcs_change_select_sql(tail: &str) -> String {
    format!(
        "SELECT id, vcs_workspace_id, kind, change_id, parent_change_ids_json, branch_or_bookmark, tree_hash, author_time_ms, confidence, created_at_ms, updated_at_ms, source_id, visibility, fidelity, sync_state, sync_version, deleted_at_ms, metadata_json FROM vcs_changes {tail}"
    )
}

pub(crate) fn vcs_change_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VcsChange> {
    Ok(VcsChange {
        id: parse_uuid(row.get::<_, String>(0)?)?,
        vcs_workspace_id: parse_uuid(row.get::<_, String>(1)?)?,
        kind: parse_text_enum::<ctx_history_core::VcsChangeKind>(row.get::<_, String>(2)?)?,
        change_id: row.get(3)?,
        parent_change_ids: serde_json::from_str(&row.get::<_, String>(4)?)
            .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?,
        branch_or_bookmark: row.get(5)?,
        tree_hash: row.get(6)?,
        author_time: optional_ms_to_time(row.get(7)?)?,
        confidence: parse_text_enum::<ctx_history_core::Confidence>(row.get::<_, String>(8)?)?,
        timestamps: EntityTimestamps {
            created_at: ms_to_time(row.get(9)?)?,
            updated_at: ms_to_time(row.get(10)?)?,
        },
        source_id: parse_optional_uuid(row.get(11)?)?,
        sync: sync_metadata_from_row(row, 12, 13, 14, 15, 16, 17)?,
    })
}
