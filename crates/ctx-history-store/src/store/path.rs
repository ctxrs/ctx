#[allow(unused_imports)]
use super::*;

pub(crate) fn migrate_legacy_history_layout(data_root: &Path) -> Result<bool> {
    let legacy_dir = data_root.join(LEGACY_HISTORY_DIR_NAME);
    if !legacy_dir.is_dir() {
        return Ok(false);
    }

    let mut moves = Vec::new();
    push_legacy_move(
        &mut moves,
        legacy_dir.join("work.sqlite"),
        data_root.join("work.sqlite"),
    );
    push_legacy_move(
        &mut moves,
        legacy_dir.join("config.toml"),
        data_root.join("config.toml"),
    );
    push_legacy_move(&mut moves, legacy_dir.join("logs"), data_root.join("logs"));
    push_legacy_move(
        &mut moves,
        legacy_dir.join("device.json"),
        data_root.join("device.json"),
    );

    let object_candidates = [
        legacy_dir.join(OBJECTS_DIR),
        legacy_dir.join(LEGACY_BLOBS_DIR),
    ];
    let spool_candidates = [
        legacy_dir.join(SPOOL_DIR),
        legacy_dir.join(LEGACY_INBOX_DIR),
    ];
    if multiple_existing_paths(&object_candidates) || multiple_existing_paths(&spool_candidates) {
        return Ok(false);
    }

    if let Some(object_source) = unique_existing_path(&object_candidates) {
        push_legacy_move(&mut moves, object_source, data_root.join(OBJECTS_DIR));
    }

    if let Some(spool_source) = unique_existing_path(&spool_candidates) {
        push_legacy_move(&mut moves, spool_source, data_root.join(SPOOL_DIR));
    }

    if moves.is_empty() || moves.iter().any(|(_, dest)| dest.exists()) {
        return Ok(false);
    }

    for (source, dest) in moves {
        fs::rename(source, dest)?;
    }
    let _ = fs::remove_dir(&legacy_dir);
    Ok(true)
}

pub(crate) fn push_legacy_move(
    moves: &mut Vec<(PathBuf, PathBuf)>,
    source: PathBuf,
    dest: PathBuf,
) {
    if source.exists() {
        moves.push((source, dest));
    }
}

pub(crate) fn unique_existing_path(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut existing = paths.iter().filter(|path| path.exists());
    let first = existing.next()?.clone();
    if existing.next().is_some() {
        return None;
    }
    Some(first)
}

pub(crate) fn multiple_existing_paths(paths: &[PathBuf]) -> bool {
    paths.iter().filter(|path| path.exists()).take(2).count() > 1
}

pub(crate) fn object_relative_path(hash: &str) -> String {
    let shard = &hash[..2];
    format!("{OBJECTS_DIR}/{shard}/{hash}")
}

#[derive(Debug, Default)]
pub(crate) struct BlobWriteGuard {
    pub(crate) created_paths: Vec<PathBuf>,
    pub(crate) committed: bool,
}

impl Drop for BlobWriteGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in self.created_paths.iter().rev() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
pub(crate) fn restrict_private_dir(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_file(_path: &Path) -> Result<()> {
    Ok(())
}

pub(crate) fn upsert_vcs_workspace_tx(
    tx: &Transaction<'_>,
    workspace: &VcsWorkspace,
) -> Result<Uuid> {
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
