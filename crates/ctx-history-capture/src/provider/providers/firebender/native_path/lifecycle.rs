use super::*;

pub(super) fn retire_missing_firebender_source(
    original_path: &Path,
    path_identity: &FirebenderPathIdentity,
    missing_error: io::Error,
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if options.inventory_observation_token.is_none() {
        return Err(CaptureError::Io(missing_error));
    }
    let route_identity = path_identity.route_identity.clone();
    let stream = path_identity.cursor_stream.clone();
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Err(CaptureError::Io(missing_error));
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = FirebenderNativeCursor::decode(committed.provider_cursor())?;
    let direct_database_path =
        original_path.file_name().and_then(|name| name.to_str()) == Some("chat_history.db");
    let reason = if direct_database_path || fs::symlink_metadata(original_path).is_ok() {
        ProviderSourceRouteRetirementReason::SourceMissing
    } else {
        ProviderSourceRouteRetirementReason::RootMissing
    };
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: prior.canonical_source_identity.clone(),
        expected_source_revision: prior.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream,
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        if matches!(
            group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
            NativePathCursorSetClassification::AllNextSameGroup { .. }
        ) {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        let disposition = group.retire_provider_source_route(&retirement)?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(match disposition {
            ProviderSourceRouteRetirementDisposition::Retired => ProviderImportWorkResult::Changed,
            ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
                ProviderImportWorkResult::NoOp
            }
        });
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn firebender_path_identity(path: &Path) -> Result<FirebenderPathIdentity> {
    let database_path = absolute_path(&firebender_chat_history_db_path(path)?)?;
    let canonical_database_path = database_path.clone();
    let route_identity = provider_path_identity(&canonical_database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Firebender,
        FIREBENDER_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    Ok(FirebenderPathIdentity {
        database_path,
        canonical_database_path,
        route_identity,
        cursor_stream,
    })
}

pub(super) fn publication_id(
    authority: &FirebenderSourceAuthority,
    page: &FirebenderPage,
    next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(FIREBENDER_PUBLICATION_DOMAIN);
    digest.update(authority.route_identity.as_bytes());
    digest.update(authority.source_revision.as_bytes());
    digest.update(page.expected.prefix_sha256);
    digest.update(page.next.prefix_sha256);
    digest.update(next_cursor.as_bytes());
    format!("firebender-native:{}", hex(&digest.finalize()))
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(FIREBENDER_RETIREMENT_DOMAIN);
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("firebender-retirement:{}", hex(&digest.finalize()))
}

pub(super) fn firebender_source_revision(
    evidence: &SqliteSourceEvidence,
    schema_fingerprint: &str,
) -> String {
    format!(
        "firebender-native-sqlite-v2:parser={FIREBENDER_NATIVE_PARSER_REVISION};policy={FIREBENDER_NATIVE_POLICY_REVISION};schema={schema_fingerprint};identity={};length={};revision={}",
        hex(evidence.identity()),
        evidence.length(),
        hex(evidence.revision()),
    )
}

pub(super) fn validate_schema(conn: &Connection, _path: &Path) -> Result<()> {
    if !sqlite_table_exists(conn, "chat_sessions")? {
        return Err(CaptureError::UnsupportedSchemaVersion(
            FIREBENDER_NATIVE_PARSER_REVISION,
        ));
    }
    let columns = sqlite_table_columns(conn, "chat_sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Firebender chat_sessions table",
        &[
            "id",
            "name",
            "created_at",
            "updated_at",
            "messages_json",
            "metadata_json",
        ],
    )
    .map_err(|_| CaptureError::UnsupportedSchemaVersion(FIREBENDER_NATIVE_PARSER_REVISION))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
