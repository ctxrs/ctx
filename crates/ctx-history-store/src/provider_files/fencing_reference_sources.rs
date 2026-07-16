impl Store {
    fn session_reference_source_ids(
        &self,
        parent: Option<Uuid>,
        root: Option<Uuid>,
        transcript: Option<Uuid>,
        record: Option<Uuid>,
    ) -> Result<Vec<Option<Uuid>>> {
        let mut sources = Vec::new();
        for (table, id) in [
            ("sessions", parent),
            ("sessions", root),
            ("artifacts", transcript),
            ("history_records", record),
        ] {
            if let Some(id) = id {
                sources.push(self.direct_entity_source_id(table, id)?);
            }
        }
        Ok(sources)
    }

    fn event_reference_source_ids(
        &self,
        session: Option<Uuid>,
        run: Option<Uuid>,
        artifact: Option<Uuid>,
        record: Option<Uuid>,
    ) -> Result<Vec<Option<Uuid>>> {
        let mut sources = Vec::new();
        if let Some(session) = session {
            sources.push(self.direct_entity_source_id("sessions", session)?);
        }
        if let Some(run) = run {
            sources.push(self.stored_run_effective_source_id(run)?);
        }
        if let Some(artifact) = artifact {
            sources.push(self.direct_entity_source_id("artifacts", artifact)?);
        }
        if let Some(record) = record {
            sources.push(self.direct_entity_source_id("history_records", record)?);
        }
        Ok(sources)
    }

    fn run_reference_source_ids(
        &self,
        session: Option<Uuid>,
        input: Option<Uuid>,
        output: Option<Uuid>,
        record: Option<Uuid>,
    ) -> Result<Vec<Option<Uuid>>> {
        let mut sources = Vec::new();
        if let Some(session) = session {
            sources.push(self.direct_entity_source_id("sessions", session)?);
        }
        for artifact in [input, output].into_iter().flatten() {
            sources.push(self.direct_entity_source_id("artifacts", artifact)?);
        }
        if let Some(record) = record {
            sources.push(self.direct_entity_source_id("history_records", record)?);
        }
        Ok(sources)
    }

    fn file_reference_source_ids(
        &self,
        event: Option<Uuid>,
        run: Option<Uuid>,
        workspace: Option<Uuid>,
        record: Option<Uuid>,
    ) -> Result<Vec<Option<Uuid>>> {
        let mut sources = Vec::new();
        if let Some(event) = event {
            sources.push(self.stored_event_effective_source_id(event)?);
        }
        if let Some(run) = run {
            sources.push(self.stored_run_effective_source_id(run)?);
        }
        if let Some(workspace) = workspace {
            sources.push(self.direct_entity_source_id("vcs_workspaces", workspace)?);
        }
        if let Some(record) = record {
            sources.push(self.direct_entity_source_id("history_records", record)?);
        }
        Ok(sources)
    }

    fn link_target_source_id(&self, target_type: &str, target_id: Uuid) -> Result<Option<Uuid>> {
        match target_type {
            "session" => self.direct_entity_source_id("sessions", target_id),
            "run" => self.stored_run_effective_source_id(target_id),
            "event" => self.stored_event_effective_source_id(target_id),
            "artifact" => self.direct_entity_source_id("artifacts", target_id),
            "vcs_workspace" => self.direct_entity_source_id("vcs_workspaces", target_id),
            "vcs_change" => self.direct_entity_source_id("vcs_changes", target_id),
            _ => Err(StoreError::ProviderFileReconciliationInconsistent {
                entity: "history record link target",
            }),
        }
    }

    fn ensure_provider_file_source_ids_write_allowed(
        &self,
        existing_source_ids: &[Option<Uuid>],
        incoming_source_ids: &[Option<Uuid>],
    ) -> Result<()> {
        if let Some(active) = self.provider_file_publication.borrow().as_ref() {
            if self.provider_file_write_scope.get() != Some(active.scope_id)
                || incoming_source_ids.iter().any(Option::is_none)
            {
                return Err(active_owner_mismatch(active));
            }
            for source_id in existing_source_ids
                .iter()
                .chain(incoming_source_ids)
                .flatten()
            {
                if !self.capture_source_matches_active_scope(*source_id)? {
                    return Err(active_owner_mismatch(active));
                }
            }
            return Ok(());
        }
        for source_id in existing_source_ids
            .iter()
            .chain(incoming_source_ids)
            .flatten()
        {
            let marker = self
                .conn
                .query_row(
                    &format!(
                        r#"
                        SELECT replacement.replacement_id, replacement.provider,
                               replacement.source_path
                        FROM capture_sources AS source
                        JOIN provider_file_publications AS replacement
                          ON {}
                        WHERE source.id = ?1
                        LIMIT 1
                        "#,
                        material_source_matches_replacement_predicate("source", "replacement",)
                    ),
                    params![source_id.to_string()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            self.ensure_provider_file_marker_write_allowed(marker)?;
        }
        Ok(())
    }

    fn ensure_provider_file_marker_write_allowed(
        &self,
        marker: Option<(String, String, String)>,
    ) -> Result<()> {
        let Some((replacement_id, provider, _source_path)) = marker else {
            return Ok(());
        };
        let active_matches = self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| active.scope_id.to_string() == replacement_id);
        if active_matches {
            let current: bool = self.conn.query_row(
                &format!(
                    "SELECT EXISTS (
                        SELECT 1 FROM provider_file_publications AS replacement
                        WHERE replacement.replacement_id = ?1
                          AND ({})
                    )",
                    replacement_observation_current_predicate("replacement")
                ),
                params![replacement_id],
                |row| row.get(0),
            )?;
            if current {
                return Ok(());
            }
            let active = self.provider_file_publication.borrow();
            let active = active
                .as_ref()
                .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
            return Err(StoreError::ProviderFileObservationChanged {
                provider: active.provider.as_str().to_owned(),
                owner_id: opaque_provider_file_owner_id(
                    active.provider,
                    &active.material_source_format,
                    &active.material_source_root,
                    &active.source_path,
                ),
            });
        }
        Err(StoreError::ProviderFileReplacementBusy {
            provider,
            owner_id: replacement_id,
        })
    }

    fn track_provider_file_publication_entity(
        &self,
        entity_kind: &'static str,
        table: &'static str,
        entity_id: Uuid,
        effective_source_predicate: &'static str,
    ) -> Result<()> {
        self.cleanup_abandoned_provider_file_publication()?;
        if !self
            .provider_file_publication
            .borrow()
            .as_ref()
            .is_some_and(|active| active.attached)
        {
            return Ok(());
        }
        let sql = format!(
            r#"
            INSERT OR IGNORE INTO {STAGING_SCHEMA}.seen (entity_kind, entity_id)
            SELECT ?1, entity.id
            FROM {table} AS entity
            JOIN capture_sources AS source ON ({effective_source_predicate})
            JOIN {STAGING_SCHEMA}.scope AS scope
              ON scope.provider = source.provider
             AND scope.material_source_format = source.source_format
             AND (
                (source.raw_source_path = scope.source_path AND (
                    source.source_root = scope.material_source_root
                    OR source.source_root = source.raw_source_path
                    OR source.source_root IS NULL
                ))
                OR (source.raw_source_path IS NULL AND source.source_root = scope.source_path)
             )
            WHERE entity.id = ?2
            "#
        );
        self.conn
            .execute(&sql, params![entity_kind, entity_id.to_string()])?;
        Ok(())
    }

    pub(crate) fn replacement_event_conflict_id(
        &self,
        dedupe_key: &str,
        incoming: &Event,
    ) -> Result<Option<Uuid>> {
        self.cleanup_abandoned_provider_file_publication()?;
        if self.provider_file_publication.borrow().is_none() {
            return Ok(None);
        }
        let conflicts = provider_event_hash_conflict_rows(&self.conn, dedupe_key)?;
        if conflicts.is_empty() {
            return Ok(None);
        }
        let incoming_source = self.event_effective_source_id(
            incoming.capture_source_id,
            incoming.session_id,
            incoming.run_id,
        )?;
        let incoming_owned = incoming_source
            .map(|source_id| self.capture_source_matches_active_scope(source_id))
            .transpose()?
            .unwrap_or(false);
        let mut owned_conflict = None;
        for conflict in conflicts {
            let owned = self
                .stored_event_effective_source_id(conflict.event_id)?
                .map(|source_id| self.capture_source_matches_active_scope(source_id))
                .transpose()?
                .unwrap_or(false);
            if !owned || !incoming_owned {
                return Err(conflict.into_store_error());
            }
            owned_conflict.get_or_insert(conflict.event_id);
        }
        Ok(owned_conflict)
    }

    fn stored_event_effective_source_id(&self, event_id: Uuid) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    event.capture_source_id,
                    (SELECT session.capture_source_id
                     FROM sessions AS session WHERE session.id = event.session_id),
                    (SELECT run_session.capture_source_id
                     FROM runs AS run
                     JOIN sessions AS run_session ON run_session.id = run.session_id
                     WHERE run.id = event.run_id),
                    (SELECT run.source_id FROM runs AS run WHERE run.id = event.run_id)
                )
                FROM events AS event
                WHERE event.id = ?1
                "#,
                params![event_id.to_string()],
                |row| {
                    row.get::<_, Option<String>>(0)?
                        .map(|value| Uuid::parse_str(&value))
                        .transpose()
                        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
                },
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(StoreError::from)
    }

    fn event_effective_source_id(
        &self,
        capture_source_id: Option<Uuid>,
        session_id: Option<Uuid>,
        run_id: Option<Uuid>,
    ) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    ?1,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = ?2),
                    (SELECT run_session.capture_source_id
                     FROM runs AS run
                     JOIN sessions AS run_session ON run_session.id = run.session_id
                     WHERE run.id = ?3),
                    (SELECT run.source_id FROM runs AS run WHERE run.id = ?3)
                )
                "#,
                params![
                    capture_source_id.map(|id| id.to_string()),
                    session_id.map(|id| id.to_string()),
                    run_id.map(|id| id.to_string()),
                ],
                |row| {
                    row.get::<_, Option<String>>(0)?
                        .map(|value| Uuid::parse_str(&value))
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::ToSqlConversionFailure(Box::new(error))
                        })
                },
            )
            .map_err(StoreError::from)
    }

    fn stored_session_effective_source_id(&self, session_id: Uuid) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                "SELECT capture_source_id FROM sessions WHERE id = ?1",
                params![session_id.to_string()],
                optional_uuid_from_first_column,
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(StoreError::from)
    }

    fn run_effective_source_id(
        &self,
        source_id: Option<Uuid>,
        session_id: Option<Uuid>,
    ) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    ?1,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = ?2)
                )
                "#,
                params![
                    source_id.map(|id| id.to_string()),
                    session_id.map(|id| id.to_string()),
                ],
                optional_uuid_from_first_column,
            )
            .map_err(StoreError::from)
    }

    fn stored_run_effective_source_id(&self, run_id: Uuid) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    run.source_id,
                    (SELECT session.capture_source_id
                     FROM sessions AS session WHERE session.id = run.session_id)
                )
                FROM runs AS run WHERE run.id = ?1
                "#,
                params![run_id.to_string()],
                optional_uuid_from_first_column,
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(StoreError::from)
    }

    fn file_touched_effective_source_id(
        &self,
        source_id: Option<Uuid>,
        event_id: Option<Uuid>,
        run_id: Option<Uuid>,
    ) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    ?1,
                    (SELECT event.capture_source_id FROM events AS event WHERE event.id = ?2),
                    (SELECT session.capture_source_id
                     FROM events AS event
                     JOIN sessions AS session ON session.id = event.session_id
                     WHERE event.id = ?2),
                    (SELECT run.source_id
                     FROM events AS event JOIN runs AS run ON run.id = event.run_id
                     WHERE event.id = ?2),
                    (SELECT run.source_id FROM runs AS run WHERE run.id = ?3),
                    (SELECT session.capture_source_id
                     FROM runs AS run
                     JOIN sessions AS session ON session.id = run.session_id
                     WHERE run.id = ?3)
                )
                "#,
                params![
                    source_id.map(|id| id.to_string()),
                    event_id.map(|id| id.to_string()),
                    run_id.map(|id| id.to_string()),
                ],
                optional_uuid_from_first_column,
            )
            .map_err(StoreError::from)
    }

    fn stored_file_touched_effective_source_id(&self, id: Uuid) -> Result<Option<Uuid>> {
        let values = self
            .conn
            .query_row(
                "SELECT source_id, event_id, run_id FROM files_touched WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((source_id, event_id, run_id)) = values else {
            return Ok(None);
        };
        self.file_touched_effective_source_id(
            source_id.map(|value| Uuid::parse_str(&value)).transpose()?,
            event_id.map(|value| Uuid::parse_str(&value)).transpose()?,
            run_id.map(|value| Uuid::parse_str(&value)).transpose()?,
        )
    }

    fn session_edge_effective_source_id(
        &self,
        source_id: Option<Uuid>,
        from_session_id: Uuid,
        to_session_id: Uuid,
    ) -> Result<Option<Uuid>> {
        self.conn
            .query_row(
                r#"
                SELECT COALESCE(
                    ?1,
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = ?2),
                    (SELECT session.capture_source_id FROM sessions AS session WHERE session.id = ?3)
                )
                "#,
                params![
                    source_id.map(|id| id.to_string()),
                    from_session_id.to_string(),
                    to_session_id.to_string(),
                ],
                optional_uuid_from_first_column,
            )
            .map_err(StoreError::from)
    }

    fn stored_session_edge_effective_source_id(&self, id: Uuid) -> Result<Option<Uuid>> {
        let values = self
            .conn
            .query_row(
                "SELECT source_id, from_session_id, to_session_id FROM session_edges WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((source_id, from_session_id, to_session_id)) = values else {
            return Ok(None);
        };
        self.session_edge_effective_source_id(
            source_id.map(|value| Uuid::parse_str(&value)).transpose()?,
            Uuid::parse_str(&from_session_id)?,
            Uuid::parse_str(&to_session_id)?,
        )
    }

    fn capture_source_matches_active_scope(&self, source_id: Uuid) -> Result<bool> {
        let active = self.provider_file_publication.borrow();
        let active = active
            .as_ref()
            .ok_or(StoreError::InvalidProviderFilePublicationScope)?;
        self.conn
            .query_row(
                &format!(
                    r#"
                    SELECT EXISTS (
                        SELECT 1 FROM capture_sources AS source
                        WHERE source.id = ?1 AND {}
                    )
                    "#,
                    material_owner_predicate("source", "?2", "?3", "?4", "?5")
                ),
                params![
                    source_id.to_string(),
                    active.provider.as_str(),
                    &active.material_source_format,
                    &active.material_source_root,
                    &active.source_path,
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }
}
