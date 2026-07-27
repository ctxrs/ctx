use super::*;

pub(super) fn canonical_source_key(
    canonical_source_identity: &str,
    provider_session_id: &str,
) -> String {
    serde_json::to_string(&(
        "shelley-nativepath-canonical-source-v1",
        canonical_source_identity,
        provider_session_id,
    ))
    .expect("Shelley canonical source identity tuple must serialize")
}

pub(super) fn retained_or_planned_event_index(
    committed_store: &Store,
    source_id: Uuid,
    value: &ShelleyMessage,
) -> Result<u64> {
    let released_index = shelley_event_index(&value.message);
    let released_identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Shelley,
        &value.message.conversation_id,
        source_id,
        released_index,
        released_index,
        &value.message.message_id,
        None,
        None,
        false,
    )?;
    let occupant = match committed_store.get_event(released_identity.id) {
        Ok(event) => Some(event),
        Err(ctx_history_store::StoreError::NotFound(_)) => None,
        Err(error) => return Err(error.into()),
    };
    let Some(occupant) = occupant else {
        return Ok(value.provider_event_index);
    };
    if event_has_native_message_identity(&occupant, &value.message) {
        return Ok(released_index);
    }
    if value.provider_event_index == released_index {
        return Ok(shelley_collision_event_index(
            &value.message,
            released_index,
        ));
    }
    Ok(value.provider_event_index)
}

fn event_has_native_message_identity(event: &Event, message: &ShelleyMessageRow) -> bool {
    let metadata = &event.sync.metadata;
    metadata
        .pointer("/metadata/message_id")
        .and_then(Value::as_str)
        == Some(message.message_id.as_str())
        && metadata
            .pointer("/metadata/conversation_id")
            .and_then(Value::as_str)
            == Some(message.conversation_id.as_str())
        && metadata
            .pointer("/metadata/sequence_id")
            .and_then(Value::as_i64)
            == Some(message.sequence_id)
}

pub(super) fn page_publication_id(
    page: &ShelleyCorePage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-publication-v1\0");
    hash_bytes(&mut digest, transition.next().stream.as_bytes());
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    digest.update((page.logical_units as u64).to_le_bytes());
    digest.update((page.retained_bytes as u64).to_le_bytes());
    format!("shelley-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Shelley.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

// The decoded native cursor is short-lived migration state and remains inline for direct validation.
#[allow(clippy::large_enum_variant)]
pub(super) enum DecodedCursor {
    Native(ShelleyNativeCursor),
    Legacy,
}

pub(super) fn decode_store_cursor(cursor: &SyncCursor) -> Result<Option<DecodedCursor>> {
    if let Ok(committed) = decode_native_path_committed_cursor(&cursor.cursor) {
        let decoded: ShelleyNativeCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Shelley NativePath committed cursor is malformed: {error}"
                ))
            })?;
        return Ok(Some(DecodedCursor::Native(decoded)));
    }
    let Some(legacy) = CertifiedProviderCursor::decode_if_certified(&cursor.cursor)? else {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor is neither NativePath nor a released migration cursor".to_owned(),
        ));
    };
    if legacy.parser_revision() != LEGACY_SHELLEY_CAPTURE_REVISION
        || legacy.policy_revision() != LEGACY_SHELLEY_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Shelley migration cursor has unreleased parser or policy revisions".to_owned(),
        ));
    }
    let _: () = legacy.parser_checkpoint().deserialize()?;
    let position = legacy.native_position();
    if !valid_legacy_shelley_position(position.kind(), position.value()) {
        return Err(CaptureError::InvalidPayload(
            "Shelley released cursor has an invalid native position".to_owned(),
        ));
    }
    Ok(Some(DecodedCursor::Legacy))
}

fn valid_legacy_shelley_position(kind: &str, value: &[u8]) -> bool {
    if kind != LEGACY_SHELLEY_POSITION_KIND {
        return false;
    }
    if value == [0] {
        return true;
    }
    value.len() == LEGACY_SHELLEY_POSITION_BYTES
        && matches!(value[0], 1..=3)
        && value[17..].iter().all(|flag| matches!(flag, 0 | 1))
}

pub(super) fn decode_native_provider_cursor(encoded: &str) -> Result<ShelleyNativeCursor> {
    let committed = decode_native_path_committed_cursor(encoded)?;
    serde_json::from_str(committed.provider_cursor()).map_err(CaptureError::from)
}

pub(super) fn observed_source_revision(
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
    inventory_token: Option<&str>,
) -> String {
    let base = shelley_source_revision(snapshot, user_version, schema_fingerprint);
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-source-revision-v1\0");
    hash_bytes(&mut digest, base.as_bytes());
    if let Some(token) = inventory_token {
        hash_bytes(&mut digest, token.as_bytes());
    }
    format!("shelley-nativepath-source-v1:{:x}", digest.finalize())
}

pub(super) fn locator_identity(path_identity: &str, route_epoch: u64) -> String {
    format!("{path_identity}:shelley-route-epoch:{route_epoch}")
}

pub(super) fn handle_missing_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    sink: Option<&dyn ProOutputSink>,
) -> Result<ProviderImportSummary> {
    let path_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Shelley shelley.db does not exist",
        });
    };
    let decoded = decode_store_cursor(&stored)?;
    if import_options.import_profile.is_replay_only() {
        match decoded {
            Some(DecodedCursor::Native(cursor)) => {
                cursor.validate(path, &path_identity)?;
                retire_output_or_mark_behind(path, context, &cursor, sink);
            }
            Some(DecodedCursor::Legacy) | None => {
                if let Some(sink) = sink {
                    sink.mark_behind(ProOutputSinkError::new(
                        "shelley_nativepath_output_retirement",
                        "Shelley Core has no committed NativePath frontier",
                    ));
                }
            }
        }
        return Ok(ProviderImportSummary::default());
    }
    let Some(DecodedCursor::Native(mut cursor)) = decoded else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Shelley source disappeared before its NativePath cursor migration",
        });
    };
    cursor.validate(path, &path_identity)?;
    if cursor.route_retired {
        retire_output_or_mark_behind(path, context, &cursor, sink);
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    cursor.route_retired = true;
    cursor.phase = ShelleyPhase::Complete;
    cursor.terminal = true;
    let next = provider_sync_cursor(
        &context.machine_id,
        stream.clone(),
        serde_json::to_string(&cursor)?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let publication_id = missing_publication_id(&transition);
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
                    let disposition =
                        group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                            provider: CaptureProvider::Shelley,
                            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
                            machine_id: context.machine_id.clone(),
                            locator_identity: cursor.locator_identity.clone(),
                            cursor_stream: stream.clone(),
                            expected_canonical_source_identity: cursor
                                .canonical_source_identity
                                .clone(),
                            expected_source_revision: cursor.source_revision.clone(),
                            retired_at_ms: context.imported_at.timestamp_millis(),
                            reason: if path.parent().is_some_and(Path::exists) {
                                ProviderSourceRouteRetirementReason::SourceMissing
                            } else {
                                ProviderSourceRouteRetirementReason::RootMissing
                            },
                        })?;
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
    let summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    retire_output_or_mark_behind(path, context, &cursor, sink);
    Ok(summary)
}

fn missing_publication_id(transition: &NativePathCursorTransition) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-missing-v1\0");
    hash_bytes(&mut digest, transition.next().stream.as_bytes());
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    format!("shelley-nativepath-missing-v1:{:x}", digest.finalize())
}
