impl Store {
    pub(crate) fn track_provider_file_publication_event(&self, event_id: Uuid) -> Result<()> {
        self.track_provider_file_publication_entity(
            "event",
            "events",
            event_id,
            r#"
            source.id = COALESCE(
            entity.capture_source_id,
            (
                SELECT event_session.capture_source_id FROM sessions AS event_session
                WHERE event_session.id = entity.session_id
            ),
            (
                SELECT run_session.capture_source_id
                FROM runs AS event_run
                JOIN sessions AS run_session ON run_session.id = event_run.session_id
                WHERE event_run.id = entity.run_id
            ),
            (
                SELECT event_run.source_id FROM runs AS event_run
                WHERE event_run.id = entity.run_id
            ))
            "#,
        )
    }

    pub(crate) fn track_provider_file_publication_run(&self, run_id: Uuid) -> Result<()> {
        self.track_provider_file_publication_entity(
            "run",
            "runs",
            run_id,
            r#"
            source.id = COALESCE(entity.source_id, (
                SELECT run_session.capture_source_id FROM sessions AS run_session
                WHERE run_session.id = entity.session_id
            ))
            "#,
        )
    }

    pub(crate) fn track_provider_file_publication_file_touched(
        &self,
        file_touched_id: Uuid,
    ) -> Result<()> {
        self.track_provider_file_publication_entity(
            "file_touched",
            "files_touched",
            file_touched_id,
            r#"
            source.id = COALESCE(entity.source_id, (
                SELECT file_event.capture_source_id FROM events AS file_event
                WHERE file_event.id = entity.event_id
            ), (
                SELECT file_session.capture_source_id
                FROM events AS file_event
                JOIN sessions AS file_session ON file_session.id = file_event.session_id
                WHERE file_event.id = entity.event_id
            ), (
                SELECT file_run.source_id
                FROM events AS file_event
                JOIN runs AS file_run ON file_run.id = file_event.run_id
                WHERE file_event.id = entity.event_id
            ), (
                SELECT file_run.source_id FROM runs AS file_run
                WHERE file_run.id = entity.run_id
            ), (
                SELECT file_session.capture_source_id
                FROM runs AS file_run
                JOIN sessions AS file_session ON file_session.id = file_run.session_id
                WHERE file_run.id = entity.run_id
            ))
            "#,
        )
    }

    pub(crate) fn track_provider_file_publication_session_edge(
        &self,
        session_edge_id: Uuid,
    ) -> Result<()> {
        self.track_provider_file_publication_entity(
            "session_edge",
            "session_edges",
            session_edge_id,
            r#"
            source.id = COALESCE(entity.source_id, (
                SELECT edge_session.capture_source_id FROM sessions AS edge_session
                WHERE edge_session.id = entity.from_session_id
            ), (
                SELECT edge_session.capture_source_id FROM sessions AS edge_session
                WHERE edge_session.id = entity.to_session_id
            ))
            "#,
        )
    }

    pub(crate) fn track_provider_file_publication_session(&self, session_id: Uuid) -> Result<()> {
        self.track_provider_file_publication_entity(
            "session",
            "sessions",
            session_id,
            "source.id = entity.capture_source_id",
        )
    }

    pub(crate) fn ensure_provider_file_capture_source_write_allowed(
        &self,
        source: &CaptureSource,
    ) -> Result<()> {
        if let Some(active) = self.provider_file_publication.borrow().as_ref() {
            if self.provider_file_write_scope.get() != Some(active.scope_id)
                || !capture_source_matches_owner(source, active)
            {
                return Err(active_owner_mismatch(active));
            }
            let existing_owned = self
                .conn
                .query_row(
                    &format!(
                        "SELECT EXISTS (SELECT 1 FROM capture_sources AS source WHERE source.id = ?1 AND {})",
                        material_owner_predicate("source", "?2", "?3", "?4", "?5")
                    ),
                    params![
                        source.id.to_string(),
                        active.provider.as_str(),
                        &active.material_source_format,
                        &active.material_source_root,
                        &active.source_path,
                    ],
                    |row| row.get::<_, bool>(0),
                )?;
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM capture_sources WHERE id = ?1)",
                params![source.id.to_string()],
                |row| row.get(0),
            )?;
            if exists && !existing_owned {
                return Err(active_owner_mismatch(active));
            }
            return Ok(());
        }
        self.ensure_provider_file_source_ids_write_allowed(&[], &[Some(source.id)])?;
        let Some(source_format) = source.descriptor.source_format.as_deref() else {
            return Ok(());
        };
        let marker = self
            .conn
            .query_row(
                &format!(
                    r#"
                    SELECT replacement.replacement_id, replacement.provider,
                           replacement.source_path
                    FROM provider_file_publications AS replacement
                    WHERE replacement.provider = ?1
                      AND replacement.material_source_format = ?2
                      AND (
                          (?3 IS NOT NULL AND ?3 = replacement.source_path AND (
                              ?4 = replacement.material_source_root
                              OR ?4 = ?3
                              OR ?4 IS NULL
                          ))
                          OR (?3 IS NULL AND ?4 = replacement.source_path)
                      )
                      AND ({})
                    LIMIT 1
                    "#,
                    effective_provider_file_publication_predicate("replacement")
                ),
                params![
                    source.descriptor.provider.as_str(),
                    source_format,
                    source.descriptor.raw_source_path.as_deref(),
                    source.descriptor.source_root.as_deref(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        self.ensure_provider_file_marker_write_allowed(marker)
    }

    pub(crate) fn ensure_provider_file_session_write_allowed(
        &self,
        session: &Session,
    ) -> Result<()> {
        let existing = self.stored_session_effective_source_id(session.id)?;
        let mut existing_sources = vec![existing];
        if let Some((transcript, record)) = self
            .conn
            .query_row(
                "SELECT transcript_blob_id, history_record_id FROM sessions WHERE id = ?1",
                params![session.id.to_string()],
                two_optional_uuids_from_row,
            )
            .optional()?
        {
            existing_sources.extend(self.session_owned_reference_source_ids(transcript, record)?);
        }
        let mut incoming_sources = vec![session.capture_source_id];
        incoming_sources.extend(self.session_owned_reference_source_ids(
            session.transcript_blob_id,
            session.history_record_id,
        )?);
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_event_write_allowed(
        &self,
        event_id: Uuid,
        event: &Event,
    ) -> Result<()> {
        let existing = self.stored_event_effective_source_id(event_id)?;
        let incoming = self.event_effective_source_id(
            event.capture_source_id,
            event.session_id,
            event.run_id,
        )?;
        let mut existing_sources = vec![existing];
        if let Some((session, run, artifact, record)) = self
            .conn
            .query_row(
                "SELECT session_id, run_id, payload_blob_id, history_record_id FROM events WHERE id = ?1",
                params![event_id.to_string()],
                four_optional_uuids_from_row,
            )
            .optional()?
        {
            existing_sources.extend(self.event_reference_source_ids(
                session, run, artifact, record,
            )?);
        }
        let mut incoming_sources = vec![incoming];
        incoming_sources.extend(self.event_reference_source_ids(
            event.session_id,
            event.run_id,
            event.payload_blob_id,
            event.history_record_id,
        )?);
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_run_write_allowed(&self, run: &Run) -> Result<()> {
        let existing = self.stored_run_effective_source_id(run.id)?;
        let incoming = self.run_effective_source_id(run.source_id, run.session_id)?;
        let mut existing_sources = vec![existing];
        if let Some((session, input, output, record)) = self
            .conn
            .query_row(
                "SELECT session_id, input_blob_id, output_blob_id, history_record_id FROM runs WHERE id = ?1",
                params![run.id.to_string()],
                four_optional_uuids_from_row,
            )
            .optional()?
        {
            existing_sources.extend(self.run_reference_source_ids(session, input, output, record)?);
        }
        let mut incoming_sources = vec![incoming];
        incoming_sources.extend(self.run_reference_source_ids(
            run.session_id,
            run.input_blob_id,
            run.output_blob_id,
            run.history_record_id,
        )?);
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_touched_write_allowed(
        &self,
        file: &FileTouched,
    ) -> Result<()> {
        let existing = self.stored_file_touched_effective_source_id(file.id)?;
        let incoming =
            self.file_touched_effective_source_id(file.source_id, file.event_id, file.run_id)?;
        let mut existing_sources = vec![existing];
        if let Some((event, run, workspace, record)) = self
            .conn
            .query_row(
                "SELECT event_id, run_id, vcs_workspace_id, history_record_id FROM files_touched WHERE id = ?1",
                params![file.id.to_string()],
                four_optional_uuids_from_row,
            )
            .optional()?
        {
            existing_sources.extend(self.file_reference_source_ids(
                event, run, workspace, record,
            )?);
        }
        let mut incoming_sources = vec![incoming];
        incoming_sources.extend(self.file_reference_source_ids(
            file.event_id,
            file.run_id,
            file.vcs_workspace_id,
            file.history_record_id,
        )?);
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_session_edge_write_allowed(
        &self,
        edge: &SessionEdge,
    ) -> Result<()> {
        let existing = self.stored_session_edge_effective_source_id(edge.id)?;
        let incoming = self.session_edge_effective_source_id(
            edge.source_id,
            edge.from_session_id,
            edge.to_session_id,
        )?;
        self.ensure_provider_file_source_ids_write_allowed(&[existing], &[incoming])
    }

    pub(crate) fn ensure_provider_file_direct_source_write_allowed(
        &self,
        table: &'static str,
        entity_id: Uuid,
        incoming_source_id: Option<Uuid>,
    ) -> Result<()> {
        match table {
            "artifacts"
            | "summaries"
            | "history_record_links"
            | "history_records"
            | "vcs_workspaces"
            | "vcs_changes" => {}
            _ => unreachable!("unsupported provider-owned table"),
        }
        let existing = self
            .conn
            .query_row(
                &format!("SELECT source_id FROM {table} WHERE id = ?1"),
                params![entity_id.to_string()],
                optional_uuid_from_first_column,
            )
            .optional()?
            .flatten();
        self.ensure_provider_file_source_ids_write_allowed(&[existing], &[incoming_source_id])
    }

    pub(crate) fn ensure_provider_file_summary_write_allowed(
        &self,
        summary: &Summary,
    ) -> Result<()> {
        let existing = self.direct_entity_source_id("summaries", summary.id)?;
        let mut existing_sources = vec![existing];
        if let Some((session, record)) = self
            .conn
            .query_row(
                "SELECT session_id, history_record_id FROM summaries WHERE id = ?1",
                params![summary.id.to_string()],
                two_optional_uuids_from_row,
            )
            .optional()?
        {
            if let Some(session) = session {
                existing_sources.push(self.direct_entity_source_id("sessions", session)?);
            }
            if let Some(record) = record {
                self.push_history_record_reference_source(&mut existing_sources, record)?;
            }
        }
        let mut incoming_sources = vec![summary.source_id];
        if let Some(session) = summary.session_id {
            incoming_sources.push(self.direct_entity_source_id("sessions", session)?);
        }
        if let Some(record) = summary.history_record_id {
            self.push_history_record_reference_source(&mut incoming_sources, record)?;
        }
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_history_record_link_write_allowed(
        &self,
        link_id: Uuid,
        link: &HistoryRecordLink,
    ) -> Result<()> {
        let existing = self.direct_entity_source_id("history_record_links", link_id)?;
        let mut existing_sources = vec![existing];
        if let Some((record, target_type, target)) = self
            .conn
            .query_row(
                "SELECT history_record_id, target_type, target_id FROM history_record_links WHERE id = ?1",
                params![link_id.to_string()],
                |row| {
                    Ok((
                        parse_uuid_text(row.get(0)?)?,
                        row.get::<_, String>(1)?,
                        parse_uuid_text(row.get(2)?)?,
                    ))
                },
            )
            .optional()?
        {
            self.push_history_record_reference_source(&mut existing_sources, record)?;
            existing_sources.push(self.link_target_source_id(&target_type, target)?);
        }
        let mut incoming_sources = vec![
            link.source_id,
            self.link_target_source_id(link.target_type.as_str(), link.target_id)?,
        ];
        self.push_history_record_reference_source(&mut incoming_sources, link.history_record_id)?;
        self.ensure_provider_file_source_ids_write_allowed(&existing_sources, &incoming_sources)
    }

    pub(crate) fn ensure_provider_file_vcs_change_write_allowed(
        &self,
        change_id: Uuid,
        change: &VcsChange,
    ) -> Result<()> {
        let existing_source = self
            .conn
            .query_row(
                r#"
                SELECT COALESCE(change.source_id, workspace.source_id)
                FROM vcs_changes AS change
                LEFT JOIN vcs_workspaces AS workspace
                  ON workspace.id = change.vcs_workspace_id
                WHERE change.id = ?1
                "#,
                params![change_id.to_string()],
                optional_uuid_from_first_column,
            )
            .optional()?
            .flatten();
        let incoming_source = match change.source_id {
            Some(source_id) => Some(source_id),
            None => self.direct_entity_source_id("vcs_workspaces", change.vcs_workspace_id)?,
        };
        self.ensure_provider_file_source_ids_write_allowed(&[existing_source], &[incoming_source])
    }

    pub(crate) fn ensure_provider_file_history_record_write_allowed(
        &self,
        record_id: Uuid,
    ) -> Result<()> {
        if let Some(active) = self.provider_file_publication.borrow().as_ref() {
            let exact_capability = active.lifecycle.load(Ordering::Acquire)
                && self.provider_file_write_scope.get() == Some(active.scope_id);
            let record_exists: bool = self.conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM history_records WHERE id = ?1)",
                params![record_id.to_string()],
                |row| row.get(0),
            )?;
            if exact_capability && !record_exists {
                return Ok(());
            }
            return Err(active_owner_mismatch(active));
        }
        let marker = self
            .conn
            .query_row(
                &format!(
                    r#"
                    WITH affected(source_id) AS (
                        SELECT source_id FROM history_records WHERE id = ?1
                        UNION SELECT capture_source_id FROM sessions WHERE history_record_id = ?1
                        UNION SELECT COALESCE(run.source_id, session.capture_source_id)
                              FROM runs AS run
                              LEFT JOIN sessions AS session ON session.id = run.session_id
                              WHERE run.history_record_id = ?1
                        UNION SELECT COALESCE(
                                         event.capture_source_id,
                                         session.capture_source_id,
                                         run_session.capture_source_id,
                                         run.source_id
                                     )
                              FROM events AS event
                              LEFT JOIN sessions AS session ON session.id = event.session_id
                              LEFT JOIN runs AS run ON run.id = event.run_id
                              LEFT JOIN sessions AS run_session ON run_session.id = run.session_id
                              WHERE event.history_record_id = ?1
                        UNION SELECT source_id FROM summaries WHERE history_record_id = ?1
                        UNION SELECT source_id FROM history_record_links WHERE history_record_id = ?1
                        UNION SELECT COALESCE(
                                         file.source_id,
                                         event.capture_source_id,
                                         event_session.capture_source_id,
                                         run.source_id,
                                         run_session.capture_source_id
                                     )
                              FROM files_touched AS file
                              LEFT JOIN events AS event ON event.id = file.event_id
                              LEFT JOIN sessions AS event_session ON event_session.id = event.session_id
                              LEFT JOIN runs AS run ON run.id = COALESCE(file.run_id, event.run_id)
                              LEFT JOIN sessions AS run_session ON run_session.id = run.session_id
                              WHERE file.history_record_id = ?1
                        UNION SELECT source_id FROM history_record_tags WHERE history_record_id = ?1
                        UNION SELECT source_id FROM record_edges
                              WHERE from_record_id = ?1 OR to_record_id = ?1
                        UNION SELECT source_id FROM audit_log
                              WHERE target_table = 'history_records' AND target_id = ?1
                    )
                    SELECT publication.replacement_id, publication.provider,
                           publication.source_path
                    FROM affected
                    JOIN capture_sources AS source ON source.id = affected.source_id
                    JOIN provider_file_publications AS publication ON {}
                    LIMIT 1
                    "#,
                    material_source_matches_replacement_predicate("source", "publication")
                ),
                params![record_id.to_string()],
                provider_file_marker_from_row,
            )
            .optional()?;
        self.ensure_provider_file_marker_write_allowed(marker)
    }

    pub(crate) fn track_provider_file_publication_history_record(
        &self,
        record_id: Uuid,
    ) -> Result<()> {
        let active = self.provider_file_publication.borrow();
        let Some(active) = active.as_ref() else {
            return Ok(());
        };
        if !active.lifecycle.load(Ordering::Acquire)
            || self.provider_file_write_scope.get() != Some(active.scope_id)
            || !active.attached
        {
            return Err(active_owner_mismatch(active));
        }
        self.conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {STAGING_SEEN_TABLE} (replacement_id, entity_kind, entity_id) VALUES (?1, 'history_record', ?2)"
            ),
            params![active.scope_id.to_string(), record_id.to_string()],
        )?;
        Ok(())
    }

    pub(crate) fn track_provider_file_publication_capture_source(
        &self,
        source_id: Uuid,
    ) -> Result<()> {
        let active = self.provider_file_publication.borrow();
        let Some(active) = active.as_ref() else {
            return Ok(());
        };
        if !active.lifecycle.load(Ordering::Acquire)
            || self.provider_file_write_scope.get() != Some(active.scope_id)
        {
            return Err(active_owner_mismatch(active));
        }
        let replacement_id = active.scope_id.to_string();
        self.conn.execute(
            &format!(
                "INSERT OR IGNORE INTO {STAGING_SEEN_TABLE} \
                 (replacement_id, entity_kind, entity_id) VALUES (?1, ?2, ?3)"
            ),
            params![
                &replacement_id,
                CURRENT_CAPTURE_SOURCE_KIND,
                source_id.to_string()
            ],
        )?;
        Ok(())
    }

    pub(crate) fn ensure_provider_file_session_assignment_write_allowed(
        &self,
        session_id: Uuid,
        record_id: Uuid,
    ) -> Result<()> {
        let affected_sources = r#"
            SELECT capture_source_id AS source_id FROM sessions WHERE id = ?1
            UNION SELECT COALESCE(
                             event.capture_source_id,
                             session.capture_source_id,
                             run_session.capture_source_id,
                             run.source_id
                         )
                  FROM events AS event
                  LEFT JOIN sessions AS session ON session.id = event.session_id
                  LEFT JOIN runs AS run ON run.id = event.run_id
                  LEFT JOIN sessions AS run_session ON run_session.id = run.session_id
                  WHERE event.session_id = ?1
            UNION SELECT COALESCE(run.source_id, session.capture_source_id)
                  FROM runs AS run
                  LEFT JOIN sessions AS session ON session.id = run.session_id
                  WHERE run.session_id = ?1
        "#;
        if let Some(active) = self.provider_file_publication.borrow().as_ref() {
            if self.provider_file_write_scope.get() != Some(active.scope_id) {
                return Err(active_owner_mismatch(active));
            }
            let invalid: bool = self.conn.query_row(
                &format!(
                    r#"
                    SELECT EXISTS (
                        SELECT 1
                        FROM ({affected_sources}) AS affected
                        LEFT JOIN capture_sources AS source ON source.id = affected.source_id
                        WHERE affected.source_id IS NULL OR NOT ({})
                    )
                    "#,
                    material_owner_predicate("source", "?2", "?3", "?4", "?5")
                ),
                params![
                    session_id.to_string(),
                    active.provider.as_str(),
                    &active.material_source_format,
                    &active.material_source_root,
                    &active.source_path,
                ],
                |row| row.get(0),
            )?;
            if invalid {
                return Err(active_owner_mismatch(active));
            }
        } else {
            let marker = self
                .conn
                .query_row(
                    &format!(
                        r#"
                        SELECT publication.replacement_id, publication.provider,
                               publication.source_path
                        FROM ({affected_sources}) AS affected
                        JOIN capture_sources AS source ON source.id = affected.source_id
                        JOIN provider_file_publications AS publication ON {}
                        LIMIT 1
                        "#,
                        material_source_matches_replacement_predicate("source", "publication",)
                    ),
                    params![session_id.to_string()],
                    provider_file_marker_from_row,
                )
                .optional()?;
            self.ensure_provider_file_marker_write_allowed(marker)?;
        }
        let record_source = self
            .conn
            .query_row(
                "SELECT source_id FROM history_records WHERE id = ?1",
                params![record_id.to_string()],
                optional_uuid_from_first_column,
            )
            .optional()?
            .flatten();
        self.ensure_provider_file_source_ids_write_allowed(&[record_source], &[])
    }

    pub(crate) fn ensure_provider_file_vcs_workspace_reference_write_allowed(
        &self,
        workspace_id: Option<Uuid>,
    ) -> Result<()> {
        if workspace_id.is_none() {
            if let Some(active) = self.provider_file_publication.borrow().as_ref() {
                return Err(active_owner_mismatch(active));
            }
            return Ok(());
        }
        let source_id = workspace_id
            .map(|id| {
                self.conn
                    .query_row(
                        "SELECT source_id FROM vcs_workspaces WHERE id = ?1",
                        params![id.to_string()],
                        optional_uuid_from_first_column,
                    )
                    .optional()
                    .map(|value| value.flatten())
                    .map_err(StoreError::from)
            })
            .transpose()?
            .flatten();
        self.ensure_provider_file_source_ids_write_allowed(&[source_id], &[])
    }

    pub(crate) fn track_provider_file_publication_direct_entity(
        &self,
        entity_kind: &'static str,
        table: &'static str,
        entity_id: Uuid,
    ) -> Result<()> {
        match (entity_kind, table) {
            ("artifact", "artifacts")
            | ("summary", "summaries")
            | ("history_record_link", "history_record_links")
            | ("vcs_workspace", "vcs_workspaces")
            | ("vcs_change", "vcs_changes") => {}
            _ => unreachable!("unsupported provider-owned entity"),
        }
        let effective_source_predicate = if table == "vcs_changes" {
            "source.id = COALESCE(entity.source_id, (SELECT workspace.source_id FROM vcs_workspaces AS workspace WHERE workspace.id = entity.vcs_workspace_id))"
        } else {
            "source.id = entity.source_id"
        };
        self.track_provider_file_publication_entity(
            entity_kind,
            table,
            entity_id,
            effective_source_predicate,
        )
    }
}
