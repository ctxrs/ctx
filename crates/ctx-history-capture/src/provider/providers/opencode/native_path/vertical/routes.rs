use super::*;

pub(super) fn import_missing_source(
    path: &Path,
    store: &mut Store,
    adapter: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    let known = known_route_for_path(path, store, adapter, dialect)?;
    let Some(known) = known else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenCode-family SQLite history database does not exist",
        });
    };
    if options.import_profile.is_replay_only() {
        if let Some(sink) = options.import_profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                "opencode_family_source_missing",
                format!("{} source is unavailable", dialect.display_name),
            ));
        }
        return Ok(ProviderImportSummary::default());
    }
    if known.cursor.route_retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let mut next = known.cursor.clone();
    next.route_retired = true;
    let next_sync = SyncCursor {
        id: known.stored.id,
        team_id: known.stored.team_id.clone(),
        device_id: known.stored.device_id.clone(),
        stream: known.stored.stream.clone(),
        cursor: serde_json::to_string(&next)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        last_synced_at: Some(adapter.imported_at),
        timestamps: timestamps(adapter.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(known.stored.cursor.clone()), next_sync);
    let retirement = ProviderSourceRouteRetirement {
        provider: dialect.provider,
        source_format: dialect.source_format.to_owned(),
        machine_id: adapter.machine_id.clone(),
        locator_identity: known.cursor.locator_identity.clone(),
        cursor_stream: known.stored.stream.clone(),
        expected_canonical_source_identity: known.cursor.canonical_source_identity.clone(),
        expected_source_revision: known.cursor.source_revision.clone(),
        retired_at_ms: adapter.imported_at.timestamp_millis(),
        reason: if adapter
            .source_root
            .as_ref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_PUBLICATION_DOMAIN);
    digest.update(b"retire\0");
    hash_field(&mut digest, known.cursor.locator_identity.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    let publication_id = format!("opencode-nativepath-retire-v1:{:x}", digest.finalize());
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllExpected => {
                    let disposition = group.retire_provider_source_route(&retirement)?;
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    matches!(
                        disposition,
                        ProviderSourceRouteRetirementDisposition::Retired
                    )
                }
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
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

struct KnownRoute {
    stored: SyncCursor,
    cursor: OpenCodeNativeStoreCursor,
}

fn known_route_for_path(
    path: &Path,
    store: &Store,
    adapter: &ProviderAdapterContext,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<KnownRoute>> {
    let requested = absolute_lexical_path(path)?;
    let requested_text = requested.display().to_string();
    let direct_path_identity = provider_path_identity(&requested)?;
    let direct_stream = provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.source_format,
        &direct_path_identity,
    );
    if let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &direct_stream)? {
        if let Ok(cursor) = decode_current_cursor(&stored.cursor) {
            if cursor.provider == dialect.provider.as_str()
                && cursor.source_format == dialect.source_format
                && cursor.selected_path == requested
            {
                return Ok(Some(KnownRoute { stored, cursor }));
            }
        }
    }
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != dialect.provider
            || source.descriptor.machine_id != adapter.machine_id
            || source.descriptor.source_format.as_deref() != Some(dialect.source_format)
            || source.descriptor.raw_source_path.as_deref() != Some(requested_text.as_str())
        {
            continue;
        }
        let stream = direct_stream.clone();
        let Some(stored) = store.get_sync_cursor(None, &adapter.machine_id, &stream)? else {
            continue;
        };
        let Ok(cursor) = decode_current_cursor(&stored.cursor) else {
            continue;
        };
        if cursor.selected_path != requested {
            continue;
        }
        routes.insert(stream, KnownRoute { stored, cursor });
    }
    match routes.len() {
        0 => Ok(None),
        1 => Ok(routes.into_values().next()),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode NativePath found duplicate current routes",
        )),
    }
}

pub(super) fn absolute_lexical_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(super) fn source_revision(
    dialect: &OpenCodeSqliteDialect,
    summary: &OpenCodeNativeScanSummary,
) -> String {
    let mut digest = Sha256::new();
    digest.update(OPENCODE_NATIVE_SOURCE_REVISION_DOMAIN);
    hash_field(&mut digest, dialect.provider.as_str().as_bytes());
    hash_field(&mut digest, dialect.source_format.as_bytes());
    hash_field(&mut digest, summary.source_generation_digest.as_bytes());
    hash_field(&mut digest, summary.capability_digest.as_bytes());
    hash_field(&mut digest, summary.semantic_digest.as_bytes());
    hash_field(&mut digest, summary.schema_family.label().as_bytes());
    format!("opencode-family-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn prior_relocation_identity(
    store: &Store,
    dialect: &OpenCodeSqliteDialect,
    machine_id: &str,
    source_revision: &str,
    current_path: &str,
) -> Result<Option<String>> {
    let mut candidates = BTreeSet::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != dialect.provider
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(dialect.source_format)
            || source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str)
                != Some(source_revision)
        {
            continue;
        }
        let Some(prior_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        if prior_path == current_path || Path::new(prior_path).exists() {
            continue;
        }
        if let Some(identity) = source.descriptor.source_identity.as_deref() {
            candidates.insert(identity.to_owned());
        }
    }
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(candidates.into_iter().next()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "{} relocation matches multiple canonical sources",
            dialect.display_name
        ))),
    }
}

pub(super) fn sqlite_locator_identity(
    path_identity: &str,
    physical: &OpenCodeNativePhysicalSourceIdentity,
) -> Result<String> {
    let encoded =
        serde_json::to_string(&("opencode-family-sqlite-locator-v1", path_identity, physical))?;
    Ok(encoded)
}

pub(super) fn sqlite_generation_locator_identity(
    physical_locator_identity: &str,
    source_revision: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-opencode-family-nativepath-generation-locator-v1\0");
    hash_field(&mut digest, physical_locator_identity.as_bytes());
    hash_field(&mut digest, source_revision.as_bytes());
    format!(
        "opencode-family-generation-locator-v1:{:x}",
        digest.finalize()
    )
}

pub(super) fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}
