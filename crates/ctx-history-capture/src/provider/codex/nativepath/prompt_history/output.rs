use super::*;

pub(super) fn replay_no_outputs(
    store: &Store,
    authority: &SourceAuthority,
    options: &CodexHistoryImportOptions,
) -> Result<()> {
    let StoredCursor::Native { cursor } =
        load_cursor(store, &options.machine_id, &authority.cursor_stream)?
    else {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay requires committed NativePath Core".to_owned(),
        ));
    };
    if !cursor.terminal() {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay requires terminal Core authority".to_owned(),
        ));
    }
    let digest = digest_source(
        authority,
        None,
        options.inventory_observation_token.as_deref(),
    )?;
    if digest.revision != cursor.source_revision {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history Pro replay source changed after Core commit".to_owned(),
        ));
    }
    if let ImportProfile::ProReplayOnly(sink) = &options.import_profile {
        replay_empty_output_or_mark_behind(
            store,
            authority,
            &digest.revision,
            &cursor,
            sink.as_ref(),
        );
    }
    Ok(())
}

pub(super) fn replay_empty_output_or_mark_behind(
    store: &Store,
    authority: &SourceAuthority,
    revision: &str,
    cursor: &PromptHistoryCursor,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) = replay_empty_output(store, authority, revision, cursor, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "codex_prompt_history_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_empty_output(
    _store: &Store,
    authority: &SourceAuthority,
    revision: &str,
    cursor: &PromptHistoryCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Codex.as_str().to_owned(),
        namespace_id: authority.cursor_stream.clone(),
        source_id: cursor.canonical_source_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let frontier = output_frontier(revision)?;
    if progress.as_ref().is_some_and(|progress| {
        progress.terminal
            && progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == revision
            && progress.cursor.as_ref().is_some_and(|committed| {
                committed.version == frontier.version && committed.payload == frontier.bytes
            })
    }) {
        return Ok(());
    }
    let state = output_state(progress, revision, sink.materializer_revision())?;
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source,
        source_epoch: state.source_epoch,
        observed_revision: revision.to_owned(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_frontier,
        observations: Vec::new(),
    };
    let page = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::Codex.as_str(),
            &cursor.canonical_source_identity,
        ),
        frontier.clone(),
        frontier,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: 1024,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(page, sink).is_err() {
        // The NativePath output coordinator already marked the sink behind.
        return Ok(());
    }
    Ok(())
}

struct OutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    revision: &str,
    materializer_revision: &str,
) -> Result<OutputState> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
        });
    };
    let can_resume = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && progress.observed_revision == revision;
    let expected_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(OutputState {
        source_epoch: if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Codex prompt-history output epoch exhausted",
                ))?
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier,
        disposition: if can_resume {
            ProOutputSourceDisposition::AppendOrResume
        } else {
            ProOutputSourceDisposition::Rewrite
        },
    })
}

fn output_frontier(revision: &str) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&json!({
            "version": OUTPUT_FRONTIER_VERSION,
            "source_revision": revision,
            "next_output": 0,
        }))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
