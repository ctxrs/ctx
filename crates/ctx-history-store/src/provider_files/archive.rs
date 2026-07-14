pub(crate) fn ensure_no_provider_file_publications(conn: &Connection) -> Result<()> {
    let effective = effective_provider_file_publication_predicate("publication");
    let marker = conn
        .query_row(
            &format!(
                r#"
                SELECT replacement_id, provider, source_path
                FROM provider_file_publications AS publication
                WHERE {effective}
                ORDER BY started_at_ms, replacement_id
                LIMIT 1
                "#
            ),
            [],
            provider_file_marker_from_row,
        )
        .optional()?;
    if let Some((replacement_id, provider, _)) = marker {
        return Err(StoreError::ProviderFileReplacementBusy {
            provider,
            owner_id: replacement_id,
        });
    }
    Ok(())
}

fn provider_file_owner_has_prior_material(
    conn: &Connection,
    provider: CaptureProvider,
    material_source_format: &str,
    material_source_root: &str,
    source_path: &str,
) -> Result<bool> {
    let sql = format!(
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM capture_sources AS source
            WHERE {}
              AND (
                EXISTS (SELECT 1 FROM events WHERE capture_source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM runs WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM files_touched WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM session_edges WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM sessions WHERE capture_source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM artifacts WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM summaries WHERE source_id = source.id LIMIT 1)
                OR EXISTS (
                    SELECT 1 FROM history_record_links WHERE source_id = source.id LIMIT 1
                )
                OR EXISTS (SELECT 1 FROM vcs_workspaces WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM vcs_changes WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM history_records WHERE source_id = source.id LIMIT 1)
                OR EXISTS (
                    SELECT 1 FROM history_record_tags WHERE source_id = source.id LIMIT 1
                )
                OR EXISTS (SELECT 1 FROM record_edges WHERE source_id = source.id LIMIT 1)
                OR EXISTS (SELECT 1 FROM audit_log WHERE source_id = source.id LIMIT 1)
              )
            LIMIT 1
        )
        "#,
        material_owner_predicate("source", "?1", "?2", "?3", "?4")
    );
    conn.query_row(
        &sql,
        params![
            provider.as_str(),
            material_source_format,
            material_source_root,
            source_path,
        ],
        |row| row.get(0),
    )
    .map_err(StoreError::from)
}

struct EffectiveArchivePublication {
    replacement_id: String,
    provider: CaptureProvider,
    material_source_format: String,
    material_source_root: String,
    source_path: String,
    mutation_started: bool,
}

pub(crate) fn ensure_archive_provider_file_writes_allowed(
    conn: &Connection,
    archive: &SessionHistoryArchive,
    forced_source: Option<(Uuid, &CaptureSourceDescriptor)>,
) -> Result<()> {
    let effective = effective_provider_file_publication_predicate("publication");
    let mut stmt = conn.prepare(&format!(
        r#"
        SELECT replacement_id, provider, material_source_format,
               material_source_root, source_path, mutation_started
        FROM provider_file_publications AS publication
        WHERE {effective}
        ORDER BY started_at_ms, replacement_id
        "#
    ))?;
    let publications = stmt
        .query_map([], |row| {
            Ok(EffectiveArchivePublication {
                replacement_id: row.get(0)?,
                provider: row
                    .get::<_, String>(1)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                material_source_format: row.get(2)?,
                material_source_root: row.get(3)?,
                source_path: row.get(4)?,
                mutation_started: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if publications.is_empty() {
        return Ok(());
    }

    let mut source_ids = BTreeSet::new();
    source_ids.extend(archive.capture_sources.iter().map(|source| source.id));
    source_ids.extend(
        archive
            .sessions
            .iter()
            .filter_map(|session| session.capture_source_id),
    );
    source_ids.extend(archive.runs.iter().filter_map(|run| run.source_id));
    source_ids.extend(
        archive
            .events
            .iter()
            .filter_map(|event| event.capture_source_id),
    );
    source_ids.extend(
        archive
            .artifact_records
            .iter()
            .filter_map(|artifact| artifact.source_id),
    );
    source_ids.extend(
        archive
            .vcs_workspaces
            .iter()
            .filter_map(|workspace| workspace.source_id),
    );
    source_ids.extend(
        archive
            .vcs_changes
            .iter()
            .filter_map(|change| change.source_id),
    );
    source_ids.extend(
        archive
            .history_record_links
            .iter()
            .filter_map(|link| link.source_id),
    );
    source_ids.extend(
        archive
            .summaries
            .iter()
            .filter_map(|summary| summary.source_id),
    );
    source_ids.extend(
        archive
            .files_touched
            .iter()
            .filter_map(|file| file.source_id),
    );
    if let Some((source_id, _)) = forced_source {
        source_ids.insert(source_id);
    }

    for publication in publications {
        let owner_has_material = provider_file_owner_has_prior_material(
            conn,
            publication.provider,
            &publication.material_source_format,
            &publication.material_source_root,
            &publication.source_path,
        )?;
        if publication.mutation_started || owner_has_material {
            return Err(StoreError::ProviderFileReplacementBusy {
                provider: publication.provider.as_str().to_owned(),
                owner_id: publication.replacement_id,
            });
        }
        let touches_owner_descriptor = archive.capture_sources.iter().any(|source| {
            capture_source_descriptor_matches_owner(
                &source.descriptor,
                publication.provider,
                &publication.material_source_format,
                &publication.material_source_root,
                &publication.source_path,
            )
        }) || forced_source.is_some_and(|(_, source)| {
            capture_source_descriptor_matches_owner(
                source,
                publication.provider,
                &publication.material_source_format,
                &publication.material_source_root,
                &publication.source_path,
            )
        });
        if touches_owner_descriptor {
            return Err(StoreError::ProviderFileReplacementBusy {
                provider: publication.provider.as_str().to_owned(),
                owner_id: publication.replacement_id,
            });
        }
        let owner_predicate = material_owner_predicate("source", "?2", "?3", "?4", "?5");
        let mut source_stmt = conn.prepare(&format!(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM capture_sources AS source
                WHERE source.id = ?1 AND {owner_predicate}
            )
            "#
        ))?;
        for source_id in &source_ids {
            let touches_existing_owner: bool = source_stmt.query_row(
                params![
                    source_id.to_string(),
                    publication.provider.as_str(),
                    &publication.material_source_format,
                    &publication.material_source_root,
                    &publication.source_path,
                ],
                |row| row.get(0),
            )?;
            if touches_existing_owner {
                return Err(StoreError::ProviderFileReplacementBusy {
                    provider: publication.provider.as_str().to_owned(),
                    owner_id: publication.replacement_id,
                });
            }
        }
    }
    Ok(())
}

fn provider_file_marker_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}
