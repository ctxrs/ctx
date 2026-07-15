impl Store {
    fn reconcile_phase_batch(
        &self,
        scope: &ProviderFilePublicationScope,
        phase: i64,
        source_cursor: Option<&str>,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationBatch> {
        let replacement_id = scope.scope_id.to_string();
        let scan = self.reconciliation_batch_rows(
            &replacement_id,
            phase,
            source_cursor,
            entity_cursor,
            limit,
        )?;
        if scan.owned_entity_ids.is_empty() {
            return Ok(ReconciliationBatch {
                visited: scan.visited,
                phase_complete: scan.phase_complete,
                source_cursor: scan.source_cursor,
                entity_cursor: scan.entity_cursor,
                removed: ProviderFileReconciliationCounts::default(),
            });
        }
        self.conn.execute(
            &format!("DELETE FROM {STAGING_BATCH_TABLE} WHERE replacement_id = ?1"),
            params![&replacement_id],
        )?;
        {
            let mut insert = self.conn.prepare_cached(&format!(
                "INSERT INTO {STAGING_BATCH_TABLE} \
                 (replacement_id, source_id, entity_id) VALUES (?1, ?2, ?3)"
            ))?;
            let source_id = scan.batch_source_id.as_deref().ok_or(
                StoreError::ProviderFileReconciliationInconsistent {
                    entity: "source cursor",
                },
            )?;
            for entity_id in &scan.owned_entity_ids {
                insert.execute(params![&replacement_id, source_id, entity_id])?;
            }
        }
        let removed = match phase {
            CLEANUP_PHASE_LINKS => ProviderFileReconciliationCounts {
                history_record_links: self.delete_unseen_batch(
                    &replacement_id,
                    "history_record_link",
                    "history_record_links",
                )?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_FILES => ProviderFileReconciliationCounts {
                files_touched: self.delete_unseen_batch(
                    &replacement_id,
                    "file_touched",
                    "files_touched",
                )?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_EDGES => ProviderFileReconciliationCounts {
                session_edges: self.delete_unseen_batch(
                    &replacement_id,
                    "session_edge",
                    "session_edges",
                )?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_SUMMARIES => ProviderFileReconciliationCounts {
                summaries: self.delete_unseen_batch(&replacement_id, "summary", "summaries")?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_EVENTS => ProviderFileReconciliationCounts {
                events: self.delete_unseen_event_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_RUNS => ProviderFileReconciliationCounts {
                runs: self.delete_unseen_run_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_SESSIONS => ProviderFileReconciliationCounts {
                sessions_tombstoned: self.tombstone_unseen_session_batch(scope)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_VCS_CHANGES => ProviderFileReconciliationCounts {
                vcs_changes: self.delete_unseen_vcs_change_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_ARTIFACTS => ProviderFileReconciliationCounts {
                artifacts: self.delete_unseen_artifact_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_HISTORY_RECORD_TAGS => ProviderFileReconciliationCounts {
                history_record_tags: self.delete_history_record_tag_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_RECORD_EDGES => ProviderFileReconciliationCounts {
                record_edges: self.delete_unseen_batch(
                    &replacement_id,
                    "record_edge",
                    "record_edges",
                )?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_HISTORY_RECORDS => ProviderFileReconciliationCounts {
                history_records: self.delete_unseen_history_record_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_VCS_WORKSPACES => ProviderFileReconciliationCounts {
                vcs_workspaces: self.delete_unseen_vcs_workspace_batch(&replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            },
            CLEANUP_PHASE_AUDIT_LOG => ProviderFileReconciliationCounts {
                audit_log_entries: self.delete_unseen_batch(
                    &replacement_id,
                    "audit_log",
                    "audit_log",
                )?,
                ..ProviderFileReconciliationCounts::default()
            },
            _ => unreachable!(),
        };
        Ok(ReconciliationBatch {
            visited: scan.visited,
            phase_complete: scan.phase_complete,
            source_cursor: scan.source_cursor,
            entity_cursor: scan.entity_cursor,
            removed,
        })
    }

    fn reconciliation_batch_rows(
        &self,
        replacement_id: &str,
        phase: i64,
        source_cursor: Option<&str>,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationScan> {
        let spec = reconciliation_phase_spec(phase)
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
        let current_source = match source_cursor {
            Some(source_id) => self
                .conn
                .query_row(
                    &format!(
                        "SELECT source_id FROM {STAGING_PRIOR_SOURCES_TABLE} \
                         WHERE replacement_id = ?1 AND source_id = ?2"
                    ),
                    params![replacement_id, source_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?,
            None => self
                .conn
                .query_row(
                    &format!(
                        "SELECT source_id FROM {STAGING_PRIOR_SOURCES_TABLE} \
                         WHERE replacement_id = ?1 ORDER BY source_id LIMIT 1"
                    ),
                    params![replacement_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?,
        };
        let Some(current_source) = current_source else {
            return Ok(ReconciliationScan {
                visited: 0,
                phase_complete: true,
                batch_source_id: None,
                source_cursor: None,
                entity_cursor: None,
                owned_entity_ids: Vec::new(),
            });
        };
        let sqlite_limit = i64::try_from(limit.checked_add(1).ok_or(
            StoreError::ProviderFileReconciliationLimitOutOfRange {
                value: limit,
                max: PROVIDER_FILE_RECONCILIATION_MAX_ROWS,
            },
        )?)
        .map_err(|_| StoreError::ProviderFileReconciliationLimitOutOfRange {
            value: limit,
            max: PROVIDER_FILE_RECONCILIATION_MAX_ROWS,
        })?;
        let mut stmt = self.conn.prepare(spec.owner_select_sql)?;
        let rows = stmt.query_map(
            params![&current_source, entity_cursor, sqlite_limit],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )?;
        let mut candidates = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        #[cfg(test)]
        {
            self.provider_file_reconciliation_queries.set(
                self.provider_file_reconciliation_queries
                    .get()
                    .saturating_add(1),
            );
            self.provider_file_reconciliation_candidates.set(
                self.provider_file_reconciliation_candidates
                    .get()
                    .saturating_add(candidates.len()),
            );
        }
        let source_complete = candidates.len() <= limit;
        if !source_complete {
            candidates.pop();
        }
        let visited = candidates.len().max(1).min(limit);
        let last_candidate = candidates.last().map(|(id, _)| id.clone());
        let owned_entity_ids = candidates
            .into_iter()
            .filter_map(|(id, owned)| owned.then_some(id))
            .collect::<Vec<_>>();
        let (next_source, next_entity) = if source_complete {
            let next = self
                .conn
                .query_row(
                    &format!(
                        "SELECT source_id FROM {STAGING_PRIOR_SOURCES_TABLE} \
                         WHERE replacement_id = ?1 AND source_id > ?2 \
                         ORDER BY source_id LIMIT 1"
                    ),
                    params![replacement_id, &current_source],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            (next, None)
        } else {
            (Some(current_source.clone()), last_candidate)
        };
        Ok(ReconciliationScan {
            visited,
            phase_complete: next_source.is_none(),
            batch_source_id: Some(current_source),
            source_cursor: next_source,
            entity_cursor: next_entity,
            owned_entity_ids,
        })
    }
}
