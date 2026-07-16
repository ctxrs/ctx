fn active_owner_mismatch(active: &ActiveProviderFilePublication) -> StoreError {
    StoreError::ProviderFilePublicationOwnerMismatch {
        provider: active.provider.as_str().to_owned(),
        owner_id: active.owner_id.clone(),
    }
}

fn validate_successful_outcome(outcome: ProviderFileImportOutcome<'_>) -> Result<()> {
    if !matches!(
        outcome.status,
        CatalogIndexedStatus::Indexed | CatalogIndexedStatus::CompletedWithRejections
    ) {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "checkpoint finalization requires a completed import outcome",
        ));
    }
    Ok(())
}

fn validate_observation_identity(observation: ProviderFileInventoryObservation<'_>) -> Result<()> {
    if observation.source_format().is_empty()
        || observation.source_root().is_empty()
        || observation.source_path().is_empty()
    {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "inventory observation identity fields must not be empty",
        ));
    }
    Ok(())
}

fn validate_scope_matches_outcome(
    scope: &ProviderFilePublicationScope,
    outcome: ProviderFileImportOutcome<'_>,
) -> Result<()> {
    let observation = outcome.observation;
    if scope.provider != outcome.provider
        || scope.inventory_source_format != observation.source_format()
        || scope.inventory_source_root != observation.source_root()
        || scope.source_path != observation.source_path()
        || scope.inventory_family != observation.inventory_family()
        || scope.inventory_generation != observation.inventory_generation()
        || scope.file_size_bytes != observation.file_size_bytes()
        || scope.file_modified_at_ms != observation.file_modified_at_ms()
        || scope.import_revision != observation.import_revision()
        || scope.metadata_json != observation.metadata_json()?
    {
        return Err(StoreError::InvalidProviderFilePublicationScope);
    }
    Ok(())
}

fn validate_checkpoint_for_outcome(
    outcome: ProviderFileImportOutcome<'_>,
    checkpoint: &ProviderFileCheckpoint,
) -> Result<()> {
    validate_successful_outcome(outcome)?;
    validate_checkpoint(checkpoint)?;
    let observation = outcome.observation;
    if checkpoint.provider != outcome.provider
        || checkpoint.source_format != observation.source_format()
        || checkpoint.source_root != observation.source_root()
        || checkpoint.source_path != observation.source_path()
        || checkpoint.import_revision != observation.import_revision()
    {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "checkpoint identity does not match the inventory observation",
        ));
    }
    if checkpoint.committed_byte_offset > observation.file_size_bytes() {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "committed offset exceeds the observed file size",
        ));
    }
    Ok(())
}

fn validate_checkpoint(checkpoint: &ProviderFileCheckpoint) -> Result<()> {
    if checkpoint.source_format.is_empty()
        || checkpoint.source_root.is_empty()
        || checkpoint.source_path.is_empty()
        || checkpoint.stable_file_identity.is_empty()
    {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "identity fields must not be empty",
        ));
    }
    if checkpoint.import_revision == 0 {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "import revision must be positive",
        ));
    }
    if checkpoint.checkpoint_version == 0 {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "checkpoint version must be positive",
        ));
    }
    if checkpoint.committed_complete_line_count > checkpoint.committed_byte_offset {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "complete line count exceeds committed bytes",
        ));
    }
    if !is_sha256_hex(&checkpoint.head_sha256) || !is_sha256_hex(&checkpoint.boundary_sha256) {
        return Err(StoreError::InvalidProviderFileCheckpoint(
            "head and boundary hashes must be lowercase SHA-256 hex",
        ));
    }
    if let Some(resume_state) = checkpoint.resume_state.as_deref() {
        if resume_state.is_empty() {
            return Err(StoreError::InvalidProviderFileCheckpoint(
                "resume state must not be empty",
            ));
        }
        if resume_state.len() > PROVIDER_FILE_CHECKPOINT_RESUME_STATE_MAX_BYTES {
            return Err(StoreError::InvalidProviderFileCheckpoint(
                "resume state exceeds the maximum encoded size",
            ));
        }
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn provider_file_observation_changed(
    provider: CaptureProvider,
    observation: ProviderFileInventoryObservation<'_>,
) -> StoreError {
    StoreError::ProviderFileObservationChanged {
        provider: provider.as_str().to_owned(),
        owner_id: opaque_provider_file_owner_id(
            provider,
            observation.source_format(),
            observation.source_root(),
            observation.source_path(),
        ),
    }
}

fn maintenance_warning_as_error(_warning: ProviderFileMaintenanceWarning) -> StoreError {
    StoreError::ProviderFileStaging
}

fn nonnegative_i64_to_usize(value: i64) -> rusqlite::Result<usize> {
    usize::try_from(value).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn optional_uuid_from_first_column(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<String>>(0)?
        .map(|value| Uuid::parse_str(&value))
        .transpose()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn parse_provider_file_publication_kind(value: &str) -> Result<ProviderFilePublicationKind> {
    match value {
        "incremental" => Ok(ProviderFilePublicationKind::Incremental),
        "replacement" => Ok(ProviderFilePublicationKind::Replacement),
        _ => Err(StoreError::InvalidProviderFilePublicationScope),
    }
}

fn parse_provider_file_publication_kind_sql(
    value: &str,
) -> rusqlite::Result<ProviderFilePublicationKind> {
    match value {
        "incremental" => Ok(ProviderFilePublicationKind::Incremental),
        "replacement" => Ok(ProviderFilePublicationKind::Replacement),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_provider_file_inventory_family_sql(value: &str) -> rusqlite::Result<&'static str> {
    match value {
        CATALOG_INVENTORY_FAMILY => Ok(CATALOG_INVENTORY_FAMILY),
        SOURCE_IMPORT_INVENTORY_FAMILY => Ok(SOURCE_IMPORT_INVENTORY_FAMILY),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_uuid_text(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn optional_uuid_at(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<Uuid>> {
    row.get::<_, Option<String>>(index)?
        .map(parse_uuid_text)
        .transpose()
}

fn two_optional_uuids_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(Option<Uuid>, Option<Uuid>)> {
    Ok((optional_uuid_at(row, 0)?, optional_uuid_at(row, 1)?))
}

type OptionalUuidQuad = (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>);

fn four_optional_uuids_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OptionalUuidQuad> {
    Ok((
        optional_uuid_at(row, 0)?,
        optional_uuid_at(row, 1)?,
        optional_uuid_at(row, 2)?,
        optional_uuid_at(row, 3)?,
    ))
}

fn two_uuids_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Uuid, Uuid)> {
    Ok((parse_uuid_text(row.get(0)?)?, parse_uuid_text(row.get(1)?)?))
}
