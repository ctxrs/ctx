impl Store {
    fn delete_unseen_batch(&self, entity_kind: &'static str, table: &'static str) -> Result<usize> {
        self.conn
            .execute(
                &format!(
                    r#"
                    DELETE FROM {table}
                    WHERE id IN (SELECT entity_id FROM {STAGING_SCHEMA}.batch)
                      AND NOT EXISTS (
                        SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                        WHERE seen.entity_kind = ?1 AND seen.entity_id = {table}.id
                      )
                    "#
                ),
                params![entity_kind],
            )
            .map_err(StoreError::from)
    }

    fn delete_unseen_event_batch(&self) -> Result<usize> {
        let stale_ids = self.unseen_batch_ids("event")?;
        if stale_ids.is_empty() {
            return Ok(0);
        }
        let surviving_reference: bool = self.conn.query_row(
            &format!(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM files_touched AS file
                    WHERE file.event_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (
                            SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                            WHERE seen.entity_kind = 'event'
                              AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id
                        )
                    )
                )
                "#
            ),
            [],
            |row| row.get(0),
        )?;
        if surviving_reference {
            return Err(StoreError::ProviderFileReconciliationInconsistent { entity: "event" });
        }
        let mut semantic_removed = 0usize;
        for id in &stale_ids {
            semantic_removed += semantic_searchable_document_count_from_stored_event(
                &self.conn,
                Uuid::parse_str(id)?,
            )?;
        }
        for table in [
            "event_search",
            "event_search_scriptgram",
            "event_search_lookup",
        ] {
            if table_exists(&self.conn, table)? {
                self.conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE event_id IN (
                            SELECT entity_id FROM {STAGING_SCHEMA}.batch
                            WHERE NOT EXISTS (
                                SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                                WHERE seen.entity_kind = 'event'
                                  AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id
                            )
                        )"
                    ),
                    [],
                )?;
            }
        }
        let removed = self.delete_unseen_batch("event", "events")?;
        decrement_semantic_searchable_item_stats_if_cached(&self.conn, semantic_removed)?;
        Ok(removed)
    }

    fn delete_unseen_run_batch(&self) -> Result<usize> {
        let surviving_reference: bool = self.conn.query_row(
            &format!(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM events AS event
                    WHERE event.run_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (
                            SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                            WHERE seen.entity_kind = 'run'
                              AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id
                        )
                    )
                    UNION ALL
                    SELECT 1 FROM files_touched AS file
                    WHERE file.run_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (
                            SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                            WHERE seen.entity_kind = 'run'
                              AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id
                        )
                    )
                )
                "#
            ),
            [],
            |row| row.get(0),
        )?;
        if surviving_reference {
            return Err(StoreError::ProviderFileReconciliationInconsistent { entity: "run" });
        }
        self.delete_unseen_batch("run", "runs")
    }

    fn delete_unseen_vcs_change_batch(&self) -> Result<usize> {
        let surviving_link: bool = self.conn.query_row(
            &format!(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM history_record_links AS link
                    WHERE link.target_type = 'vcs_change'
                      AND link.target_id IN (
                          SELECT entity_id FROM {STAGING_SCHEMA}.batch
                          WHERE NOT EXISTS (
                              SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                              WHERE seen.entity_kind = 'vcs_change'
                                AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id
                          )
                      )
                )
                "#
            ),
            [],
            |row| row.get(0),
        )?;
        if surviving_link {
            return Err(StoreError::ProviderFileReconciliationInconsistent {
                entity: "VCS change",
            });
        }
        self.delete_unseen_batch("vcs_change", "vcs_changes")
    }

    fn delete_unseen_artifact_batch(&self) -> Result<usize> {
        let eligible = format!(
            r#"
            id IN (SELECT entity_id FROM {STAGING_SCHEMA}.batch)
            AND NOT EXISTS (
                SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                WHERE seen.entity_kind = 'artifact' AND seen.entity_id = artifacts.id
            )
            AND NOT EXISTS (SELECT 1 FROM sessions WHERE transcript_blob_id = artifacts.id)
            AND NOT EXISTS (
                SELECT 1 FROM runs
                WHERE input_blob_id = artifacts.id OR output_blob_id = artifacts.id
            )
            AND NOT EXISTS (SELECT 1 FROM events WHERE payload_blob_id = artifacts.id)
            AND NOT EXISTS (
                SELECT 1 FROM history_record_links
                WHERE target_type = 'artifact' AND target_id = artifacts.id
            )
            "#
        );
        if table_exists(&self.conn, "artifact_search")? {
            self.conn.execute(
                &format!(
                    "DELETE FROM artifact_search WHERE artifact_id IN (
                        SELECT id FROM artifacts WHERE {eligible}
                    )"
                ),
                [],
            )?;
        }
        self.conn
            .execute(&format!("DELETE FROM artifacts WHERE {eligible}"), [])
            .map_err(StoreError::from)
    }

    fn delete_unseen_vcs_workspace_batch(&self) -> Result<usize> {
        let surviving_reference: bool = self.conn.query_row(
            &format!(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM vcs_changes WHERE vcs_workspace_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen WHERE seen.entity_kind = 'vcs_workspace' AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id)
                    )
                    UNION ALL
                    SELECT 1 FROM files_touched WHERE vcs_workspace_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen WHERE seen.entity_kind = 'vcs_workspace' AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id)
                    )
                    UNION ALL
                    SELECT 1 FROM history_records WHERE primary_vcs_workspace_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen WHERE seen.entity_kind = 'vcs_workspace' AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id)
                    )
                    UNION ALL
                    SELECT 1 FROM local_workspaces WHERE vcs_workspace_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen WHERE seen.entity_kind = 'vcs_workspace' AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id)
                    )
                    UNION ALL
                    SELECT 1 FROM history_record_links WHERE target_type = 'vcs_workspace' AND target_id IN (
                        SELECT entity_id FROM {STAGING_SCHEMA}.batch
                        WHERE NOT EXISTS (SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen WHERE seen.entity_kind = 'vcs_workspace' AND seen.entity_id = {STAGING_SCHEMA}.batch.entity_id)
                    )
                )
                "#
            ),
            [],
            |row| row.get(0),
        )?;
        if surviving_reference {
            return Err(StoreError::ProviderFileReconciliationInconsistent {
                entity: "VCS workspace",
            });
        }
        self.delete_unseen_batch("vcs_workspace", "vcs_workspaces")
    }

    fn delete_history_record_tag_batch(&self) -> Result<usize> {
        self.conn
            .execute(
                &format!(
                    "DELETE FROM history_record_tags WHERE rowid IN (
                        SELECT CAST(entity_id AS INTEGER) FROM {STAGING_SCHEMA}.batch
                    )"
                ),
                [],
            )
            .map_err(StoreError::from)
    }

    fn delete_unseen_history_record_batch(&self) -> Result<usize> {
        let eligible = format!(
            r#"
            id IN (SELECT entity_id FROM {STAGING_SCHEMA}.batch)
            AND NOT EXISTS (
                SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                WHERE seen.entity_kind = 'history_record'
                  AND seen.entity_id = history_records.id
            )
            AND NOT EXISTS (SELECT 1 FROM sessions WHERE history_record_id = history_records.id)
            AND NOT EXISTS (SELECT 1 FROM runs WHERE history_record_id = history_records.id)
            AND NOT EXISTS (SELECT 1 FROM events WHERE history_record_id = history_records.id)
            AND NOT EXISTS (SELECT 1 FROM summaries WHERE history_record_id = history_records.id)
            AND NOT EXISTS (SELECT 1 FROM files_touched WHERE history_record_id = history_records.id)
            AND NOT EXISTS (
                SELECT 1 FROM history_record_links
                WHERE history_record_id = history_records.id
            )
            AND NOT EXISTS (
                SELECT 1 FROM history_record_tags
                WHERE history_record_id = history_records.id
            )
            AND NOT EXISTS (
                SELECT 1 FROM record_edges
                WHERE from_record_id = history_records.id OR to_record_id = history_records.id
            )
            "#
        );
        for table in ["ctx_history_search", "ctx_history_search_scriptgram"] {
            if table_exists(&self.conn, table)? {
                self.conn.execute(
                    &format!(
                        "DELETE FROM {table} WHERE record_id IN (
                            SELECT id FROM history_records WHERE {eligible}
                        )"
                    ),
                    [],
                )?;
            }
        }
        self.conn
            .execute(&format!("DELETE FROM history_records WHERE {eligible}"), [])
            .map_err(StoreError::from)
    }

    fn unseen_batch_ids(&self, entity_kind: &'static str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT batch.entity_id
            FROM {STAGING_SCHEMA}.batch AS batch
            WHERE NOT EXISTS (
                SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                WHERE seen.entity_kind = ?1 AND seen.entity_id = batch.entity_id
            )
            "#
        ))?;
        let rows = stmt.query_map(params![entity_kind], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StoreError::from)
    }

    fn tombstone_unseen_session_batch(
        &self,
        scope: &ProviderFilePublicationScope,
    ) -> Result<usize> {
        self.conn
            .execute(
                &format!(
                    r#"
                    UPDATE sessions
                    SET deleted_at_ms = ?1, updated_at_ms = max(updated_at_ms, ?1),
                        transcript_blob_id = NULL
                    WHERE id IN (SELECT entity_id FROM {STAGING_SCHEMA}.batch)
                      AND deleted_at_ms IS NULL
                      AND NOT EXISTS (
                        SELECT 1 FROM {STAGING_SCHEMA}.seen AS seen
                        WHERE seen.entity_kind = 'session' AND seen.entity_id = sessions.id
                      )
                      AND NOT EXISTS (SELECT 1 FROM events WHERE events.session_id = sessions.id)
                      AND NOT EXISTS (SELECT 1 FROM runs WHERE runs.session_id = sessions.id)
                      AND NOT EXISTS (
                        SELECT 1 FROM session_edges AS edge
                        WHERE edge.from_session_id = sessions.id OR edge.to_session_id = sessions.id
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM summaries
                        WHERE summaries.session_id = sessions.id
                          AND summaries.deleted_at_ms IS NULL
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM history_record_links AS link
                        WHERE link.target_type = 'session' AND link.target_id = sessions.id
                          AND link.deleted_at_ms IS NULL
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM files_touched AS file
                        JOIN capture_sources AS source ON source.id = file.source_id
                        WHERE source.provider = sessions.provider
                          AND source.external_session_id = sessions.external_session_id
                      )
                      AND NOT EXISTS (
                        SELECT 1 FROM sessions AS related
                        WHERE related.id != sessions.id AND related.deleted_at_ms IS NULL
                          AND (
                            related.capture_source_id IS NULL
                            OR related.capture_source_id NOT IN (
                                SELECT id FROM {STAGING_SCHEMA}.prior_sources
                            )
                          )
                          AND (
                            related.parent_session_id = sessions.id
                            OR related.root_session_id = sessions.id
                          )
                      )
                    "#
                ),
                params![scope.file_modified_at_ms],
            )
            .map_err(StoreError::from)
    }

    fn bump_semantic_replacement_revision(&self) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE semantic_replacement_revision SET current_revision = current_revision + 1 WHERE singleton = 1",
            [],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidProviderFileCheckpoint(
                "semantic content revision state is missing",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    fn inject_provider_file_fault(&self, fault: ProviderFileFaultPoint) {
        self.provider_file_fault.set(Some(fault));
    }

    fn take_provider_file_fault(&self, fault: ProviderFileFaultPoint) -> bool {
        #[cfg(test)]
        {
            if self.provider_file_fault.get() == Some(fault) {
                self.provider_file_fault.set(None);
                return true;
            }
        }
        let _ = fault;
        false
    }
}
