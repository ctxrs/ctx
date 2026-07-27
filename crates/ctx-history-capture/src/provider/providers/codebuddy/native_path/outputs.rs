use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    sources: &[CodeBuddySource],
    store: &Store,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) {
    let Some(sink) = profile.sink() else {
        return;
    };
    for source in sources {
        if let Err(error) = replay_source_outputs(source, store, context, sink.as_ref()) {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

pub(super) fn replay_source_outputs(
    source: &CodeBuddySource,
    store: &Store,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let core_cursor = match load_stored_cursor(store, &context.machine_id, &source.cursor_stream)? {
        StoredCursor::Native { cursor, .. }
            if cursor.source_revision == source.source_revision && cursor.terminal =>
        {
            cursor
        }
        _ => {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_core_not_committed",
                "CodeBuddy output replay requires terminal matching NativePath Core",
            ));
            return Ok(());
        }
    };
    let output_source = source.output_identity();
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == CODEBUDDY_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<CodeBuddyNativeCursor>(&cursor.payload).ok())
        .filter(|cursor| {
            cursor.version == CODEBUDDY_NATIVE_CURSOR_VERSION
                && cursor.shape == source.shape
                && cursor.canonical_path == source.canonical_path
                && cursor.source_identity == source.proposed_source_identity
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == CODEBUDDY_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == source.source_revision
            && progress_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.generation == core_cursor.generation)
    });
    if can_resume && progress.as_ref().is_some_and(|progress| progress.terminal) {
        return Ok(());
    }
    let mut scan_cursor = if can_resume {
        progress_cursor
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy resumable output progress lost its cursor",
            ))?
    } else {
        let mut cursor = initial_cursor(source, context)?;
        cursor.generation = core_cursor.generation;
        cursor
    };
    let prior_epoch = progress.as_ref().map(|progress| progress.source_epoch);
    let source_epoch = if can_resume {
        prior_epoch.unwrap_or(1)
    } else {
        prior_epoch
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy output source epoch overflowed",
            ))?
    };
    let mut disposition = if can_resume {
        ProOutputSourceDisposition::AppendOrResume
    } else if progress.is_some() {
        ProOutputSourceDisposition::Rewrite
    } else {
        ProOutputSourceDisposition::NewSource
    };
    let mut expected_prior_epoch = prior_epoch;
    let mut expected_prior_frontier = progress_cursor.as_ref().map(output_frontier).transpose()?;

    while !scan_cursor.terminal {
        let page = next_source_page(source, &scan_cursor, context)?.ok_or(
            CaptureError::SystemInvariant("CodeBuddy output scanner stopped before terminal"),
        )?;
        if page.next_cursor.next_native_offset > core_cursor.next_native_offset
            || page.next_cursor.next_native_ordinal > core_cursor.next_native_ordinal
        {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy output replay exceeded committed Core authority".to_owned(),
            ));
        }
        if !source.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_frontier = output_frontier(&page.expected_cursor)?;
        let next_frontier = output_frontier(&page.next_cursor)?;
        let observations = page
            .records
            .iter()
            .filter_map(|record| {
                record.output.as_ref().map(|output| {
                    output_observation(source, &page.next_cursor.session, record, output)
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: source.source_revision.clone(),
            parser_revision: CODEBUDDY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_prior_epoch,
            expected_prior_frontier: expected_prior_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::CodeBuddy.as_str(),
                &source.locator_identity,
            ),
            expected_frontier,
            next_frontier.clone(),
            page.next_cursor.terminal,
            NativePageAccounting {
                logical_units: page.logical_units(),
                conservative_serialized_bytes: page.retained_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_nativepath_output_page",
                "CodeBuddy output sink did not commit the requested replay page",
            ));
            return Ok(());
        }
        scan_cursor = page.next_cursor;
        expected_prior_epoch = Some(source_epoch);
        expected_prior_frontier = Some(next_frontier);
        disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

pub(super) fn output_frontier(cursor: &CodeBuddyNativeCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CODEBUDDY_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(cursor)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn output_observation(
    source: &CodeBuddySource,
    session: &CodeBuddySessionCheckpoint,
    record: &CodeBuddyRecord,
    output: &CodeBuddyOutputDraft,
) -> Result<ProOutputObservation> {
    let provider_session_id = session.provider_session_id();
    Ok(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "codebuddy:{}:{}",
                source.shape.cursor_tag(),
                record.native_ordinal
            ),
            native_sequence: record.native_ordinal,
            native_record_id: Some(output.native_record_id.clone()),
            source_record_ordinal: Some(record.native_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: record.byte_start,
            byte_end_exclusive: record.byte_end_exclusive,
        },
        occurred_at_unix_ms: Some(output.occurred_at_unix_ms),
        associations: OutputAssociations {
            direct_session_id: provider_session_id.clone(),
            root_session_id: provider_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id.clone(),
        command: None,
        outcome: output.outcome.clone(),
        locator: OutputSourceLocator {
            version: 1,
            kind: format!("codebuddy-{}-native-v1", source.shape.cursor_tag()),
            payload: serde_json::to_vec(&json!({
                "path": source.canonical_path,
                "source_revision": source.source_revision,
                "native_ordinal": record.native_ordinal,
                "byte_start": record.byte_start,
                "byte_end_exclusive": record.byte_end_exclusive,
            }))?,
        },
        content: output.content.clone(),
    })
}

pub(crate) fn codebuddy_cli_complete_content_record(
    value: &Value,
    physical_line: usize,
) -> Option<(String, String)> {
    let text = cli_message_text(value);
    if !codebuddy_is_message_record(
        value.get("role").and_then(Value::as_str),
        value.get("type").and_then(Value::as_str),
    ) || text.trim().is_empty()
    {
        return None;
    }
    let native_record_id = codebuddy_cli_explicit_native_message_id(value)
        .unwrap_or_else(|| format!("line-{physical_line}"));
    Some((text, native_record_id))
}

pub(crate) fn codebuddy_cli_complete_content_source_from_admitted(
    metadata: &Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    let frozen = CodeBuddyFrozenFile::from_metadata(metadata)?;
    Ok((
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        path_identity,
    ))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}
