const RECONCILIATION_SCAN_MAX_BYTES: usize = 1024 * 1024;
const RECONCILIATION_SCAN_QUERY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(250);
const LEGACY_DIRECT_CURSOR_PREFIX: &str = "legacy-direct-rowid:";
const LEGACY_INDIRECT_CURSOR_PREFIX: &str = "legacy-indirect-rowid:";

enum LegacyReconciliationCursor {
    Direct(Option<i64>),
    Indirect(Option<i64>),
}

struct ReconciliationQueryRows {
    candidates: Vec<(String, bool, String)>,
    exhausted: bool,
}

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
        if phase == CLEANUP_PHASE_AUDIT_LOG
            && matches!(
                source_cursor,
                Some(PRIOR_HISTORY_RECORD_CURSOR | PRIOR_CAPTURE_SOURCE_CURSOR)
            )
        {
            return self.reconcile_prior_seen_batch(
                &replacement_id,
                source_cursor.ok_or(StoreError::InvalidProviderFilePublicationScope)?,
                entity_cursor,
                limit,
            );
        }
        let scan = self.reconciliation_batch_rows(
            &replacement_id,
            phase,
            source_cursor,
            entity_cursor,
            limit,
        )?;
        if scan.owned_entity_ids.is_empty() {
            let batch = ReconciliationBatch {
                visited: scan.visited,
                phase_complete: scan.phase_complete,
                source_cursor: scan.source_cursor,
                entity_cursor: scan.entity_cursor,
                removed: ProviderFileReconciliationCounts::default(),
            };
            return self.begin_prior_seen_cleanup_if_needed(phase, &replacement_id, batch);
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
        let batch = ReconciliationBatch {
            visited: scan.visited,
            phase_complete: scan.phase_complete,
            source_cursor: scan.source_cursor,
            entity_cursor: scan.entity_cursor,
            removed,
        };
        self.begin_prior_seen_cleanup_if_needed(phase, &replacement_id, batch)
    }

    fn begin_prior_seen_cleanup_if_needed(
        &self,
        phase: i64,
        replacement_id: &str,
        mut batch: ReconciliationBatch,
    ) -> Result<ReconciliationBatch> {
        if phase != CLEANUP_PHASE_AUDIT_LOG || !batch.phase_complete {
            return Ok(batch);
        }
        let next_cursor = if self
            .provider_file_publication_seen_kind_exists(replacement_id, PRIOR_HISTORY_RECORD_KIND)?
        {
            Some(PRIOR_HISTORY_RECORD_CURSOR)
        } else if self
            .provider_file_publication_seen_kind_exists(replacement_id, PRIOR_CAPTURE_SOURCE_KIND)?
        {
            Some(PRIOR_CAPTURE_SOURCE_CURSOR)
        } else {
            None
        };
        if let Some(next_cursor) = next_cursor {
            batch.phase_complete = false;
            batch.source_cursor = Some(next_cursor.to_owned());
            batch.entity_cursor = None;
        }
        Ok(batch)
    }

    fn reconcile_prior_seen_batch(
        &self,
        replacement_id: &str,
        prior_cursor: &str,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationBatch> {
        let prior_kind = match prior_cursor {
            PRIOR_HISTORY_RECORD_CURSOR => PRIOR_HISTORY_RECORD_KIND,
            PRIOR_CAPTURE_SOURCE_CURSOR => PRIOR_CAPTURE_SOURCE_KIND,
            _ => return Err(StoreError::InvalidProviderFilePublicationScope),
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
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT entity_id FROM {STAGING_SEEN_TABLE} \
             WHERE replacement_id = ?1 AND entity_kind = ?2 \
               AND (?3 IS NULL OR entity_id > ?3) \
             ORDER BY entity_id LIMIT ?4"
        ))?;
        let rows = stmt.query_map(
            params![replacement_id, prior_kind, entity_cursor, sqlite_limit],
            |row| row.get::<_, String>(0),
        )?;
        let mut ids = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        let complete = ids.len() <= limit;
        if !complete {
            ids.pop();
        }
        if ids.is_empty() {
            let next_cursor = if prior_cursor == PRIOR_HISTORY_RECORD_CURSOR
                && self.provider_file_publication_seen_kind_exists(
                    replacement_id,
                    PRIOR_CAPTURE_SOURCE_KIND,
                )? {
                Some(PRIOR_CAPTURE_SOURCE_CURSOR.to_owned())
            } else {
                None
            };
            return Ok(ReconciliationBatch {
                visited: 0,
                phase_complete: next_cursor.is_none(),
                source_cursor: next_cursor,
                entity_cursor: None,
                removed: ProviderFileReconciliationCounts::default(),
            });
        }

        self.conn.execute(
            &format!("DELETE FROM {STAGING_BATCH_TABLE} WHERE replacement_id = ?1"),
            params![replacement_id],
        )?;
        {
            let mut insert = self.conn.prepare_cached(&format!(
                "INSERT INTO {STAGING_BATCH_TABLE} \
                 (replacement_id, source_id, entity_id) VALUES (?1, ?2, ?3)"
            ))?;
            for id in &ids {
                insert.execute(params![replacement_id, prior_cursor, id])?;
            }
        }
        let removed = if prior_kind == PRIOR_HISTORY_RECORD_KIND {
            ProviderFileReconciliationCounts {
                history_records: self.delete_unseen_history_record_batch(replacement_id)?,
                ..ProviderFileReconciliationCounts::default()
            }
        } else {
            self.delete_unseen_capture_source_batch(replacement_id)?;
            ProviderFileReconciliationCounts::default()
        };
        self.conn.execute(
            &format!(
                "DELETE FROM {STAGING_SEEN_TABLE} WHERE replacement_id = ?1 \
                 AND entity_kind = ?2 AND entity_id IN ( \
                     SELECT entity_id FROM {STAGING_BATCH_TABLE} WHERE replacement_id = ?1 \
                 )"
            ),
            params![replacement_id, prior_kind],
        )?;

        let last_id = ids.last().cloned();
        let next_cursor = if complete
            && prior_cursor == PRIOR_HISTORY_RECORD_CURSOR
            && self.provider_file_publication_seen_kind_exists(
                replacement_id,
                PRIOR_CAPTURE_SOURCE_KIND,
            )? {
            Some(PRIOR_CAPTURE_SOURCE_CURSOR.to_owned())
        } else if complete {
            None
        } else {
            Some(prior_cursor.to_owned())
        };
        Ok(ReconciliationBatch {
            visited: ids.len(),
            phase_complete: complete && next_cursor.is_none(),
            source_cursor: next_cursor,
            entity_cursor: (!complete).then_some(last_id).flatten(),
            removed,
        })
    }

    fn provider_file_publication_seen_kind_exists(
        &self,
        replacement_id: &str,
        entity_kind: &str,
    ) -> Result<bool> {
        self.conn
            .query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM {STAGING_SEEN_TABLE} \
                     WHERE replacement_id = ?1 AND entity_kind = ?2)"
                ),
                params![replacement_id, entity_kind],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    fn reconciliation_batch_rows(
        &self,
        replacement_id: &str,
        phase: i64,
        source_cursor: Option<&str>,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationScan> {
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
        let optimized_indexes_present = self.conn.query_row(
            crate::schema::indexes::RECONCILIATION_INDEXES_PRESENT_SQL,
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if optimized_indexes_present {
            return self.optimized_reconciliation_batch_rows(
                replacement_id,
                phase,
                &current_source,
                entity_cursor,
                limit,
            );
        }
        self.legacy_reconciliation_batch_rows(
            replacement_id,
            phase,
            &current_source,
            entity_cursor,
            limit,
        )
    }

    fn optimized_reconciliation_batch_rows(
        &self,
        replacement_id: &str,
        phase: i64,
        current_source: &str,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationScan> {
        let spec = reconciliation_phase_spec(phase)
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
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
        let cursor = entity_cursor
            .map(|value| rusqlite::types::Value::Text(value.to_owned()))
            .unwrap_or(rusqlite::types::Value::Null);
        let query = self.query_reconciliation_rows(
            spec.owner_select_sql,
            current_source,
            cursor,
            sqlite_limit,
        )?;
        let mut candidates = query.candidates;
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
        let source_complete = query.exhausted && candidates.len() <= limit;
        if !source_complete {
            candidates.truncate(limit);
        }
        // Charge the remaining slice for this owner query so the outer phase
        // loop cannot multiply the per-query byte and time envelopes.
        let visited = limit;
        let last_candidate = candidates.last().map(|(_, _, cursor)| cursor.clone());
        let owned_entity_ids = candidates
            .into_iter()
            .filter_map(|(id, owned, _)| owned.then_some(id))
            .collect::<Vec<_>>();
        let (next_source, next_entity) = if source_complete {
            let next = self.next_reconciliation_source(replacement_id, current_source)?;
            (next, None)
        } else {
            (Some(current_source.to_owned()), last_candidate)
        };
        Ok(ReconciliationScan {
            visited,
            phase_complete: next_source.is_none(),
            batch_source_id: Some(current_source.to_owned()),
            source_cursor: next_source,
            entity_cursor: next_entity,
            owned_entity_ids,
        })
    }

    fn legacy_reconciliation_batch_rows(
        &self,
        replacement_id: &str,
        phase: i64,
        current_source: &str,
        entity_cursor: Option<&str>,
        limit: usize,
    ) -> Result<ReconciliationScan> {
        let spec = legacy_reconciliation_phase_spec(phase)
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
        let cursor = parse_legacy_reconciliation_cursor(entity_cursor)?;
        let (sql, rowid_cursor, indirect) = match cursor {
            LegacyReconciliationCursor::Direct(rowid) => {
                (spec.direct_owner_select_sql, rowid, false)
            }
            LegacyReconciliationCursor::Indirect(rowid) => (
                spec.indirect_owner_scan_sql
                    .ok_or(StoreError::InvalidProviderFilePublicationScope)?,
                rowid,
                true,
            ),
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
        let cursor_value = rowid_cursor
            .map(rusqlite::types::Value::Integer)
            .unwrap_or(rusqlite::types::Value::Null);
        let query =
            self.query_reconciliation_rows(sql, current_source, cursor_value, sqlite_limit)?;
        let mut candidates = query.candidates;
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
        let stage_complete = query.exhausted && candidates.len() <= limit;
        if !stage_complete {
            candidates.truncate(limit);
        }
        // Charge the remaining slice for this owner query so the outer phase
        // loop cannot multiply the per-query byte and time envelopes.
        let visited = limit;
        let last_rowid = candidates
            .last()
            .map(|(_, _, cursor)| parse_legacy_rowid(cursor))
            .transpose()?;
        let owned_entity_ids = candidates
            .into_iter()
            .filter_map(|(id, owned, _)| owned.then_some(id))
            .collect::<Vec<_>>();

        if !stage_complete {
            let rowid = last_rowid.ok_or(StoreError::ProviderFileReconciliationInconsistent {
                entity: "legacy reconciliation cursor",
            })?;
            return Ok(ReconciliationScan {
                visited,
                phase_complete: false,
                batch_source_id: Some(current_source.to_owned()),
                source_cursor: Some(current_source.to_owned()),
                entity_cursor: Some(format_legacy_reconciliation_cursor(indirect, Some(rowid))),
                owned_entity_ids,
            });
        }

        if !indirect && spec.indirect_owner_scan_sql.is_some() {
            return Ok(ReconciliationScan {
                visited,
                phase_complete: false,
                batch_source_id: Some(current_source.to_owned()),
                source_cursor: Some(current_source.to_owned()),
                entity_cursor: Some(format_legacy_reconciliation_cursor(true, None)),
                owned_entity_ids,
            });
        }

        let next_source = self.next_reconciliation_source(replacement_id, current_source)?;
        Ok(ReconciliationScan {
            visited,
            phase_complete: next_source.is_none(),
            batch_source_id: Some(current_source.to_owned()),
            source_cursor: next_source,
            entity_cursor: None,
            owned_entity_ids,
        })
    }

    fn next_reconciliation_source(
        &self,
        replacement_id: &str,
        current_source: &str,
    ) -> Result<Option<String>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT source_id FROM {STAGING_PRIOR_SOURCES_TABLE} \
                     WHERE replacement_id = ?1 AND source_id > ?2 \
                     ORDER BY source_id LIMIT 1"
                ),
                params![replacement_id, current_source],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    fn query_reconciliation_rows(
        &self,
        sql: &str,
        source_id: &str,
        cursor: rusqlite::types::Value,
        sqlite_limit: i64,
    ) -> Result<ReconciliationQueryRows> {
        let started = std::time::Instant::now();
        self.conn.progress_handler(
            1_000,
            Some(move || started.elapsed() >= RECONCILIATION_SCAN_QUERY_TIMEOUT),
        );
        let mut candidates = Vec::new();
        let mut bytes = 0usize;
        let mut exhausted = true;
        let query_result = (|| -> Result<()> {
            let mut stmt = self.conn.prepare(sql)?;
            let mut rows = stmt.query(params![source_id, cursor, sqlite_limit])?;
            while let Some(row) = rows.next()? {
                let id = row.get::<_, String>(0)?;
                let owned = row.get::<_, bool>(1)?;
                let row_cursor = row.get::<_, String>(2)?;
                let row_bytes = id
                    .len()
                    .checked_add(row_cursor.len())
                    .and_then(|bytes| bytes.checked_add(source_id.len()))
                    .and_then(|bytes| bytes.checked_add(36))
                    .ok_or(StoreError::ProviderFileReconciliationInconsistent {
                        entity: "reconciliation scan byte count",
                    })?;
                if row_bytes > RECONCILIATION_SCAN_MAX_BYTES {
                    return Err(StoreError::ProviderFileReconciliationInconsistent {
                        entity: "reconciliation entity exceeds scan byte limit",
                    });
                }
                if bytes.saturating_add(row_bytes) > RECONCILIATION_SCAN_MAX_BYTES {
                    exhausted = false;
                    break;
                }
                bytes += row_bytes;
                candidates.push((id, owned, row_cursor));
            }
            Ok(())
        })();
        self.conn.progress_handler(0, None::<fn() -> bool>);
        match query_result {
            Ok(()) => Ok(ReconciliationQueryRows {
                candidates,
                exhausted,
            }),
            Err(StoreError::Sql(rusqlite::Error::SqliteFailure(error, _)))
                if error.code == rusqlite::ErrorCode::OperationInterrupted
                    && started.elapsed() >= RECONCILIATION_SCAN_QUERY_TIMEOUT =>
            {
                if candidates.is_empty() {
                    return Err(StoreError::ProviderFileReconciliationInconsistent {
                        entity: "reconciliation scan made no progress before deadline",
                    });
                }
                Ok(ReconciliationQueryRows {
                    candidates,
                    exhausted: false,
                })
            }
            Err(error) => Err(error),
        }
    }
}

fn parse_legacy_reconciliation_cursor(cursor: Option<&str>) -> Result<LegacyReconciliationCursor> {
    let Some(cursor) = cursor else {
        return Ok(LegacyReconciliationCursor::Direct(None));
    };
    if let Some(rowid) = cursor.strip_prefix(LEGACY_DIRECT_CURSOR_PREFIX) {
        return Ok(LegacyReconciliationCursor::Direct(
            parse_optional_legacy_rowid(rowid)?,
        ));
    }
    if let Some(rowid) = cursor.strip_prefix(LEGACY_INDIRECT_CURSOR_PREFIX) {
        return Ok(LegacyReconciliationCursor::Indirect(
            parse_optional_legacy_rowid(rowid)?,
        ));
    }
    Err(StoreError::ProviderFileReconciliationInconsistent {
        entity: "legacy reconciliation cursor",
    })
}

fn parse_optional_legacy_rowid(rowid: &str) -> Result<Option<i64>> {
    if rowid.is_empty() {
        Ok(None)
    } else {
        parse_legacy_rowid(rowid).map(Some)
    }
}

fn parse_legacy_rowid(rowid: &str) -> Result<i64> {
    rowid
        .parse::<i64>()
        .map_err(|_| StoreError::ProviderFileReconciliationInconsistent {
            entity: "legacy reconciliation rowid cursor",
        })
}

fn format_legacy_reconciliation_cursor(indirect: bool, rowid: Option<i64>) -> String {
    let prefix = if indirect {
        LEGACY_INDIRECT_CURSOR_PREFIX
    } else {
        LEGACY_DIRECT_CURSOR_PREFIX
    };
    match rowid {
        Some(rowid) => format!("{prefix}{rowid}"),
        None => prefix.to_owned(),
    }
}
