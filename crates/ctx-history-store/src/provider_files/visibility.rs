fn event_material_source_id_sql(event_alias: &str) -> String {
    format!(
        r#"
        COALESCE(
               {event_alias}.capture_source_id,
               (
                    SELECT event_session.capture_source_id
                    FROM sessions AS event_session
                    WHERE event_session.id = {event_alias}.session_id
               ),
               (
                    SELECT run_session.capture_source_id
                    FROM runs AS event_run
                    JOIN sessions AS run_session ON run_session.id = event_run.session_id
                    WHERE event_run.id = {event_alias}.run_id
               ),
               (
                    SELECT event_run.source_id
                    FROM runs AS event_run
                    WHERE event_run.id = {event_alias}.run_id
               )
            )
        "#
    )
}

pub(crate) fn event_material_visible_predicate(event_alias: &str) -> String {
    material_source_id_not_replacing_predicate(&event_material_source_id_sql(event_alias))
}

impl Store {
    pub(crate) fn importer_event_material_visible_predicate(&self, event_alias: &str) -> String {
        self.importer_material_visible_predicate(&event_material_source_id_sql(event_alias))
    }

    pub(crate) fn importer_session_material_visible_predicate(
        &self,
        session_alias: &str,
    ) -> String {
        self.importer_material_visible_predicate(&format!("{session_alias}.capture_source_id"))
    }

    pub(crate) fn importer_capture_source_material_visible_predicate(
        &self,
        source_alias: &str,
    ) -> String {
        self.importer_material_visible_predicate(&format!("{source_alias}.id"))
    }

    fn importer_material_visible_predicate(&self, source_id_sql: &str) -> String {
        let public = material_source_id_not_replacing_predicate(source_id_sql);
        let active = self.provider_file_publication.borrow();
        let Some(active) = active.as_ref().filter(|active| {
            active.lifecycle.load(Ordering::Acquire)
                && !active.retires_observation
                && self.provider_file_write_scope.get() == Some(active.scope_id)
        }) else {
            return public;
        };
        let owner = material_source_matches_replacement_owner_predicate(
            "active_publication_source",
            "active_publication",
        );
        format!(
            r#"
            (({public}) OR EXISTS (
                SELECT 1
                FROM capture_sources AS active_publication_source
                JOIN provider_file_publications AS active_publication ON ({owner})
                WHERE active_publication.replacement_id = '{}'
                  AND active_publication_source.id = {source_id_sql}
            ))
            "#,
            active.scope_id
        )
    }
}

pub(crate) fn session_material_visible_predicate(session_alias: &str) -> String {
    material_source_id_not_replacing_predicate(&format!("{session_alias}.capture_source_id"))
}

pub(crate) fn capture_source_material_visible_predicate(source_alias: &str) -> String {
    material_source_id_not_replacing_predicate(&format!("{source_alias}.id"))
}

pub(crate) fn direct_source_material_visible_predicate(
    table_alias: &str,
    source_column: &str,
) -> String {
    material_source_id_not_replacing_predicate(&format!("{table_alias}.{source_column}"))
}

pub(crate) fn vcs_change_material_visible_predicate(change_alias: &str) -> String {
    material_source_id_not_replacing_predicate(&format!(
        "COALESCE({change_alias}.source_id, (SELECT workspace.source_id FROM vcs_workspaces AS workspace WHERE workspace.id = {change_alias}.vcs_workspace_id))"
    ))
}

pub(crate) fn history_record_material_visible_predicate(record_alias: &str) -> String {
    let direct = direct_source_material_visible_predicate(record_alias, "source_id");
    let session_visible = session_material_visible_predicate("record_session");
    let run_visible = run_material_visible_predicate("record_run");
    let event_visible = event_material_visible_predicate("record_event");
    format!(
        r#"
        ({direct}) AND (
            NOT EXISTS (SELECT 1 FROM sessions WHERE history_record_id = {record_alias}.id)
            AND NOT EXISTS (SELECT 1 FROM runs WHERE history_record_id = {record_alias}.id)
            AND NOT EXISTS (SELECT 1 FROM events WHERE history_record_id = {record_alias}.id)
            OR EXISTS (
                SELECT 1 FROM sessions AS record_session
                WHERE record_session.history_record_id = {record_alias}.id AND {session_visible}
            )
            OR EXISTS (
                SELECT 1 FROM runs AS record_run
                WHERE record_run.history_record_id = {record_alias}.id AND {run_visible}
            )
            OR EXISTS (
                SELECT 1 FROM events AS record_event
                WHERE record_event.history_record_id = {record_alias}.id AND {event_visible}
            )
        )
        "#
    )
}

pub(crate) fn summary_material_visible_predicate(summary_alias: &str) -> String {
    let direct = direct_source_material_visible_predicate(summary_alias, "source_id");
    let session_visible = session_material_visible_predicate("summary_session");
    let record_visible = history_record_material_visible_predicate("summary_record");
    format!(
        r#"
        ({direct})
        AND (
            {summary_alias}.session_id IS NULL OR EXISTS (
                SELECT 1 FROM sessions AS summary_session
                WHERE summary_session.id = {summary_alias}.session_id AND {session_visible}
            )
        )
        AND (
            {summary_alias}.history_record_id IS NULL OR EXISTS (
                SELECT 1 FROM history_records AS summary_record
                WHERE summary_record.id = {summary_alias}.history_record_id AND {record_visible}
            )
        )
        "#
    )
}

pub(crate) fn history_record_link_material_visible_predicate(link_alias: &str) -> String {
    let direct = direct_source_material_visible_predicate(link_alias, "source_id");
    let record_visible = history_record_material_visible_predicate("link_record");
    let session_visible = session_material_visible_predicate("link_session");
    let run_visible = run_material_visible_predicate("link_run");
    let event_visible = event_material_visible_predicate("link_event");
    let artifact_visible = direct_source_material_visible_predicate("link_artifact", "source_id");
    let workspace_visible = direct_source_material_visible_predicate("link_workspace", "source_id");
    let change_visible = vcs_change_material_visible_predicate("link_change");
    format!(
        r#"
        ({direct})
        AND EXISTS (
            SELECT 1 FROM history_records AS link_record
            WHERE link_record.id = {link_alias}.history_record_id AND {record_visible}
        )
        AND CASE {link_alias}.target_type
            WHEN 'session' THEN EXISTS (SELECT 1 FROM sessions AS link_session WHERE link_session.id = {link_alias}.target_id AND {session_visible})
            WHEN 'run' THEN EXISTS (SELECT 1 FROM runs AS link_run WHERE link_run.id = {link_alias}.target_id AND {run_visible})
            WHEN 'event' THEN EXISTS (SELECT 1 FROM events AS link_event WHERE link_event.id = {link_alias}.target_id AND {event_visible})
            WHEN 'artifact' THEN EXISTS (SELECT 1 FROM artifacts AS link_artifact WHERE link_artifact.id = {link_alias}.target_id AND {artifact_visible})
            WHEN 'vcs_workspace' THEN EXISTS (SELECT 1 FROM vcs_workspaces AS link_workspace WHERE link_workspace.id = {link_alias}.target_id AND {workspace_visible})
            WHEN 'vcs_change' THEN EXISTS (SELECT 1 FROM vcs_changes AS link_change WHERE link_change.id = {link_alias}.target_id AND {change_visible})
            ELSE 0
        END
        "#
    )
}

pub(crate) fn run_material_visible_predicate(run_alias: &str) -> String {
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM capture_sources AS replacement_source
            JOIN provider_file_publications AS replacement
              ON {}
            WHERE replacement_source.id = COALESCE(
               {run_alias}.source_id,
               (
                    SELECT run_session.capture_source_id
                    FROM sessions AS run_session
                    WHERE run_session.id = {run_alias}.session_id
               )
            )
        )
        "#,
        material_source_matches_replacement_predicate("replacement_source", "replacement")
    )
}

pub(crate) fn session_edge_material_visible_predicate(edge_alias: &str) -> String {
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM capture_sources AS replacement_source
            JOIN provider_file_publications AS replacement
              ON {}
            WHERE replacement_source.id = COALESCE(
                {edge_alias}.source_id,
                (SELECT edge_session.capture_source_id
                 FROM sessions AS edge_session
                 WHERE edge_session.id = {edge_alias}.from_session_id),
                (SELECT edge_session.capture_source_id
                 FROM sessions AS edge_session
                 WHERE edge_session.id = {edge_alias}.to_session_id)
            )
        )
        "#,
        material_source_matches_replacement_predicate("replacement_source", "replacement")
    )
}

pub(crate) fn file_touched_material_visible_predicate(file_alias: &str) -> String {
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM capture_sources AS replacement_source
            JOIN provider_file_publications AS replacement
              ON {}
            WHERE replacement_source.id = COALESCE(
               {file_alias}.source_id,
               (
                    SELECT file_event.capture_source_id
                    FROM events AS file_event
                    WHERE file_event.id = {file_alias}.event_id
               ),
               (
                    SELECT file_session.capture_source_id
                    FROM events AS file_event
                    JOIN sessions AS file_session ON file_session.id = file_event.session_id
                    WHERE file_event.id = {file_alias}.event_id
               ),
               (
                    SELECT file_session.capture_source_id
                    FROM events AS file_event
                    JOIN runs AS file_run ON file_run.id = file_event.run_id
                    JOIN sessions AS file_session ON file_session.id = file_run.session_id
                    WHERE file_event.id = {file_alias}.event_id
               ),
               (
                    SELECT file_run.source_id
                    FROM events AS file_event
                    JOIN runs AS file_run ON file_run.id = file_event.run_id
                    WHERE file_event.id = {file_alias}.event_id
               ),
               (
                    SELECT file_run.source_id
                    FROM runs AS file_run
                    WHERE file_run.id = {file_alias}.run_id
               ),
               (
                    SELECT file_session.capture_source_id
                    FROM runs AS file_run
                    JOIN sessions AS file_session ON file_session.id = file_run.session_id
                    WHERE file_run.id = {file_alias}.run_id
               )
            )
        )
        "#,
        material_source_matches_replacement_predicate("replacement_source", "replacement")
    )
}

fn material_source_id_not_replacing_predicate(source_id_sql: &str) -> String {
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM capture_sources AS replacement_source
            JOIN provider_file_publications AS replacement
              ON {}
            WHERE replacement_source.id = {source_id_sql}
        )
        "#,
        material_source_matches_replacement_predicate("replacement_source", "replacement")
    )
}

fn material_source_matches_replacement_predicate(
    source_alias: &str,
    replacement_alias: &str,
) -> String {
    let owner =
        material_source_matches_replacement_owner_predicate(source_alias, replacement_alias);
    let effective = effective_provider_file_publication_predicate(replacement_alias);
    format!("({owner}) AND ({effective})")
}

fn material_source_matches_replacement_owner_predicate(
    source_alias: &str,
    replacement_alias: &str,
) -> String {
    format!(
        r#"
        {replacement_alias}.provider = {source_alias}.provider
        AND {replacement_alias}.material_source_format = {source_alias}.source_format
        AND (
            ({source_alias}.raw_source_path = {replacement_alias}.source_path AND (
                {source_alias}.source_root = {replacement_alias}.material_source_root
                OR {source_alias}.source_root = {source_alias}.raw_source_path
                OR {source_alias}.source_root IS NULL
            ))
            OR ({source_alias}.raw_source_path IS NULL
                AND {source_alias}.source_root = {replacement_alias}.source_path)
        )
        "#
    )
}

pub(crate) fn catalog_material_visible_predicate(catalog_alias: &str) -> String {
    let effective = effective_provider_file_publication_predicate("replacement");
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM provider_file_publications AS replacement
            WHERE replacement.inventory_family = '{CATALOG_INVENTORY_FAMILY}'
              AND replacement.provider = {catalog_alias}.provider
              AND replacement.inventory_source_format = {catalog_alias}.source_format
              AND replacement.inventory_source_root = {catalog_alias}.source_root
              AND replacement.source_path = {catalog_alias}.source_path
              AND ({effective})
        )
        "#
    )
}

pub(crate) fn source_import_file_material_visible_predicate(file_alias: &str) -> String {
    let effective = effective_provider_file_publication_predicate("publication");
    format!(
        r#"
        NOT EXISTS (
            SELECT 1 FROM provider_file_publications AS publication
            WHERE publication.inventory_family = '{SOURCE_IMPORT_INVENTORY_FAMILY}'
              AND publication.provider = {file_alias}.provider
              AND publication.inventory_source_format = {file_alias}.source_format
              AND publication.inventory_source_root = {file_alias}.source_root
              AND publication.source_path = {file_alias}.source_path
              AND ({effective})
        )
        "#
    )
}

pub(crate) fn effective_provider_file_publication_predicate(publication_alias: &str) -> String {
    let current = replacement_observation_current_predicate(publication_alias);
    format!("{publication_alias}.mutation_started = 1 OR ({current})")
}

fn provider_file_retirement_observation_current_predicate(publication_alias: &str) -> String {
    format!(
        r#"
        (
            {publication_alias}.inventory_family = '{CATALOG_INVENTORY_FAMILY}'
            AND EXISTS (
                SELECT 1 FROM catalog_sessions AS retirement_catalog
                WHERE retirement_catalog.provider = {publication_alias}.provider
                  AND retirement_catalog.source_format = {publication_alias}.inventory_source_format
                  AND retirement_catalog.source_root = {publication_alias}.inventory_source_root
                  AND retirement_catalog.source_path = {publication_alias}.source_path
                  AND retirement_catalog.is_stale = 0
            )
        )
        OR (
            {publication_alias}.inventory_family = '{SOURCE_IMPORT_INVENTORY_FAMILY}'
            AND EXISTS (
                SELECT 1 FROM source_import_files AS retirement_file
                WHERE retirement_file.provider = {publication_alias}.provider
                  AND retirement_file.source_format = {publication_alias}.inventory_source_format
                  AND retirement_file.source_root = {publication_alias}.inventory_source_root
                  AND retirement_file.source_path = {publication_alias}.source_path
                  AND retirement_file.is_stale = 0
            )
        )
        "#
    )
}

pub(crate) fn replacement_observation_current_predicate(replacement_alias: &str) -> String {
    format!(
        r#"
        EXISTS (
            SELECT 1 FROM import_inventory_generations AS replacement_inventory
            WHERE replacement_inventory.provider = {replacement_alias}.provider
              AND replacement_inventory.source_root = {replacement_alias}.inventory_source_root
              AND replacement_inventory.inventory_family = {replacement_alias}.inventory_family
              AND replacement_inventory.current_generation = {replacement_alias}.inventory_generation
        )
        AND (
            (
                {replacement_alias}.inventory_family = '{CATALOG_INVENTORY_FAMILY}'
                AND EXISTS (
                    SELECT 1 FROM catalog_sessions AS replacement_catalog
                    WHERE replacement_catalog.provider = {replacement_alias}.provider
                      AND replacement_catalog.source_format = {replacement_alias}.inventory_source_format
                      AND replacement_catalog.source_root = {replacement_alias}.inventory_source_root
                      AND replacement_catalog.source_path = {replacement_alias}.source_path
                      AND replacement_catalog.is_stale = 0
                      AND replacement_catalog.file_size_bytes = {replacement_alias}.file_size_bytes
                      AND replacement_catalog.file_modified_at_ms = {replacement_alias}.file_modified_at_ms
                      AND replacement_catalog.import_revision = {replacement_alias}.import_revision
                )
            )
            OR (
                {replacement_alias}.inventory_family = '{SOURCE_IMPORT_INVENTORY_FAMILY}'
                AND EXISTS (
                    SELECT 1 FROM source_import_files AS replacement_file
                    WHERE replacement_file.provider = {replacement_alias}.provider
                      AND replacement_file.source_format = {replacement_alias}.inventory_source_format
                      AND replacement_file.source_root = {replacement_alias}.inventory_source_root
                      AND replacement_file.source_path = {replacement_alias}.source_path
                      AND replacement_file.is_stale = 0
                      AND replacement_file.file_size_bytes = {replacement_alias}.file_size_bytes
                      AND replacement_file.file_modified_at_ms = {replacement_alias}.file_modified_at_ms
                      AND replacement_file.import_revision = {replacement_alias}.import_revision
                      AND replacement_file.metadata_json IS {replacement_alias}.metadata_json
                )
            )
        )
        "#
    )
}

pub(crate) fn has_fenced_provider_file_publications(conn: &Connection) -> Result<bool> {
    let effective = effective_provider_file_publication_predicate("replacement");
    conn.query_row(
        &format!(
            "SELECT EXISTS (
                SELECT 1 FROM provider_file_publications AS replacement
                WHERE {effective}
                LIMIT 1
            )"
        ),
        [],
        |row| row.get(0),
    )
    .map_err(StoreError::from)
}

pub(crate) fn material_owner_predicate(
    source_alias: &str,
    provider: &str,
    source_format: &str,
    source_root: &str,
    source_path: &str,
) -> String {
    format!(
        r#"
        {source_alias}.provider = {provider}
        AND {source_alias}.source_format = {source_format}
        AND (
            ({source_alias}.raw_source_path = {source_path} AND (
                {source_alias}.source_root = {source_root}
                OR {source_alias}.source_root = {source_alias}.raw_source_path
                OR {source_alias}.source_root IS NULL
            ))
            OR ({source_alias}.raw_source_path IS NULL
                AND {source_alias}.source_root = {source_path})
        )
        "#
    )
}

fn capture_source_matches_owner(
    source: &CaptureSource,
    active: &ActiveProviderFilePublication,
) -> bool {
    capture_source_descriptor_matches_owner(
        &source.descriptor,
        active.provider,
        &active.material_source_format,
        &active.material_source_root,
        &active.source_path,
    )
}

fn capture_source_descriptor_matches_owner(
    descriptor: &CaptureSourceDescriptor,
    provider: CaptureProvider,
    material_source_format: &str,
    material_source_root: &str,
    source_path: &str,
) -> bool {
    if descriptor.provider != provider
        || descriptor.source_format.as_deref() != Some(material_source_format)
    {
        return false;
    }
    match (
        descriptor.raw_source_path.as_deref(),
        descriptor.source_root.as_deref(),
    ) {
        (Some(raw_path), source_root_value) if raw_path == source_path => {
            source_root_value == Some(material_source_root)
                || source_root_value == Some(raw_path)
                || source_root_value.is_none()
        }
        (None, Some(source_root_value)) => source_root_value == source_path,
        _ => false,
    }
}
