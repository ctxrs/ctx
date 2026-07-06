#[allow(unused_imports)]
use super::*;

impl Store {
    pub fn events_for_session(&self, session_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms").as_str(),
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub fn events_for_session_limited(&self, session_id: Uuid, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql("WHERE session_id = ?1 ORDER BY seq, occurred_at_ms LIMIT ?2")
                .as_str(),
        )?;
        let rows = stmt.query_map(
            params![
                session_id.to_string(),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            event_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn events_for_session_window(
        &self,
        event: &Event,
        before: usize,
        after: usize,
    ) -> Result<Vec<Event>> {
        let Some(session_id) = event.session_id else {
            return Ok(vec![event.clone()]);
        };
        let event_seq = i64::try_from(event.seq).unwrap_or(i64::MAX);
        let mut events = if before == 0 {
            Vec::new()
        } else {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC, occurred_at_ms DESC LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(before).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            let mut rows = collect_rows(rows)?;
            rows.reverse();
            rows
        };
        events.push(event.clone());
        if after > 0 {
            let mut stmt = self.conn.prepare(
                event_select_sql(
                    "WHERE session_id = ?1 AND seq > ?2 ORDER BY seq, occurred_at_ms LIMIT ?3",
                )
                .as_str(),
            )?;
            let rows = stmt.query_map(
                params![
                    session_id.to_string(),
                    event_seq,
                    i64::try_from(after).unwrap_or(i64::MAX)
                ],
                event_from_row,
            )?;
            events.extend(collect_rows(rows)?);
        }
        Ok(events)
    }

    pub fn events_for_record(&self, record_id: Uuid) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            event_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                ORDER BY seq, occurred_at_ms
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], event_from_row)?;
        collect_rows(rows)
    }

    pub(crate) fn list_events(&self) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(event_select_sql("ORDER BY seq, occurred_at_ms, id").as_str())?;
        let rows = stmt.query_map([], event_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_artifact(&self, artifact: &Artifact) -> Result<Uuid> {
        self.conn.execute(
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
        self.conn
            .query_row(
                "SELECT id FROM artifacts WHERE blob_hash = ?1 AND kind = ?2",
                params![artifact.blob_hash.as_str(), artifact.kind.as_str()],
                |row| parse_uuid(row.get::<_, String>(0)?),
            )
            .map_err(StoreError::from)
    }

    pub(crate) fn list_artifacts(&self) -> Result<Vec<Artifact>> {
        let mut stmt = self
            .conn
            .prepare(artifact_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], artifact_from_row)?;
        collect_rows(rows)
    }

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

    pub fn get_or_create_local_device(&self) -> Result<LocalDeviceIdentity> {
        if let Some(device) = self.local_device()? {
            return Ok(device);
        }
        let now = utc_now();
        let device = LocalDeviceIdentity {
            id: new_id(),
            stable_device_id: format!("ctx-device-{}", new_id().simple()),
            created_at: now,
            updated_at: now,
        };
        self.conn.execute(
            r#"
            INSERT INTO local_devices
            (id, stable_device_id, created_at_ms, updated_at_ms, metadata_json)
            VALUES (?1, ?2, ?3, ?3, '{}')
            "#,
            params![
                device.id.to_string(),
                device.stable_device_id.as_str(),
                timestamp_ms(now),
            ],
        )?;
        Ok(device)
    }

    pub fn register_local_workspace(
        &self,
        root_path: impl AsRef<Path>,
        repo_fingerprint: &str,
        vcs_workspace_id: Option<Uuid>,
    ) -> Result<LocalWorkspaceIdentity> {
        let device = self.get_or_create_local_device()?;
        let root = root_path.as_ref();
        let root_path_hash = sha256_hex(root.display().to_string().as_bytes());
        let display_root = root.display().to_string();
        let now = utc_now();
        let id = new_id();
        self.conn.execute(
            r#"
            INSERT INTO local_workspaces
            (
                id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                display_root, created_at_ms, updated_at_ms, metadata_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, '{}')
            ON CONFLICT(device_id, repo_fingerprint, root_path_hash) DO UPDATE SET
                vcs_workspace_id = COALESCE(excluded.vcs_workspace_id, local_workspaces.vcs_workspace_id),
                display_root = excluded.display_root,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                id.to_string(),
                device.id.to_string(),
                optional_uuid_string(vcs_workspace_id),
                repo_fingerprint,
                root_path_hash,
                display_root,
                timestamp_ms(now),
            ],
        )?;
        self.conn
            .query_row(
                r#"
                SELECT id, device_id, vcs_workspace_id, repo_fingerprint, root_path_hash,
                       display_root, created_at_ms, updated_at_ms
                FROM local_workspaces
                WHERE device_id = ?1 AND repo_fingerprint = ?2 AND root_path_hash = ?3
                "#,
                params![device.id.to_string(), repo_fingerprint, root_path_hash],
                local_workspace_from_row,
            )
            .map_err(StoreError::from)
    }

    pub fn local_device(&self) -> Result<Option<LocalDeviceIdentity>> {
        self.conn
            .query_row(
                "SELECT id, stable_device_id, created_at_ms, updated_at_ms FROM local_devices ORDER BY created_at_ms, id LIMIT 1",
                [],
                local_device_from_row,
            )
            .optional()
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

    pub fn upsert_summary(&self, summary: &Summary) -> Result<()> {
        self.conn.execute(
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

    pub(crate) fn list_summaries(&self) -> Result<Vec<Summary>> {
        let mut stmt = self
            .conn
            .prepare(summary_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], summary_from_row)?;
        collect_rows(rows)
    }

    pub fn upsert_file_touched(&self, file: &FileTouched) -> Result<()> {
        self.conn.execute(
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

    pub fn file_touched_exists(&self, id: Uuid) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM files_touched WHERE id = ?1",
                params![id.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub(crate) fn list_files_touched(&self) -> Result<Vec<FileTouched>> {
        let mut stmt = self
            .conn
            .prepare(file_touched_select_sql("ORDER BY updated_at_ms, id").as_str())?;
        let rows = stmt.query_map([], file_touched_from_row)?;
        collect_rows(rows)
    }

    pub fn artifacts_for_record(&self, record_id: Uuid) -> Result<Vec<Artifact>> {
        let mut stmt = self.conn.prepare(
            artifact_select_sql(
                r#"
                WHERE id IN (
                    SELECT transcript_blob_id
                    FROM sessions
                    WHERE history_record_id = ?1 AND transcript_blob_id IS NOT NULL
                    UNION
                    SELECT input_blob_id
                    FROM runs
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND input_blob_id IS NOT NULL
                    UNION
                    SELECT output_blob_id
                    FROM runs
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND output_blob_id IS NOT NULL
                    UNION
                    SELECT payload_blob_id
                    FROM events
                    WHERE (history_record_id = ?1
                       OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1))
                       AND payload_blob_id IS NOT NULL
                    UNION
                    SELECT target_id
                    FROM history_record_links
                    WHERE history_record_id = ?1 AND target_type = 'artifact'
                )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], artifact_from_row)?;
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

    pub fn summaries_for_record(&self, record_id: Uuid) -> Result<Vec<Summary>> {
        let mut stmt = self.conn.prepare(
            summary_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], summary_from_row)?;
        collect_rows(rows)
    }

    pub fn files_touched_for_record(&self, record_id: Uuid) -> Result<Vec<FileTouched>> {
        let mut stmt = self.conn.prepare(
            file_touched_select_sql(
                r#"
                WHERE history_record_id = ?1
                   OR run_id IN (
                        SELECT id FROM runs
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                   OR event_id IN (
                        SELECT id FROM events
                        WHERE history_record_id = ?1
                           OR session_id IN (SELECT id FROM sessions WHERE history_record_id = ?1)
                   )
                ORDER BY updated_at_ms DESC, id
                "#,
            )
            .as_str(),
        )?;
        let rows = stmt.query_map(params![record_id.to_string()], file_touched_from_row)?;
        collect_rows(rows)
    }
}
