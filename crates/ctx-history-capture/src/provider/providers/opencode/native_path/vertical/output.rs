use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    context: &OpenCodePublicationContext<'_>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(context, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "opencode_family_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn replay_outputs(
    context: &OpenCodePublicationContext<'_>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let reader = OpenCodeNativePathReader::acquire_for_dialect(
        OpenCodeNativeSourceSelection::exact(context.selected_path)
            .with_inventory_observation_token(context.options.inventory_observation_token.clone()),
        context.dialect,
    )?;
    let mut verification = reader.scanner_with_profile_and_prior(
        OpenCodeNativeProfile::CoreOnly,
        OpenCodeNativePageLimits::default(),
        &context.current_state,
    )?;
    while verification.next_page()?.is_some() {}
    let verified = verification.finish()?;
    if !same_generation(&verified.persisted_state(), &context.current_state) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let source = output_source_identity(context);
    let progress = sink.observe_source(&source).map_err(output_sink_error)?;
    let output_plan = output_replay_plan(context, sink, progress.as_ref())?;
    if output_plan.already_terminal {
        return Ok(());
    }
    let mut scanner = reader.scanner_with_profile_and_prior(
        OpenCodeNativeProfile::CoreAndPro,
        OpenCodeNativePageLimits::default(),
        &context.current_state,
    )?;
    scanner.resume_pro_from(output_plan.frontier)?;
    let mut expected_prior_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.clone());
    while let Some(mut page) = scanner.next_pro_output_page()? {
        if !page.rejections.is_empty() {
            return Err(CaptureError::InvalidPayload(format!(
                "{} output replay rejected {} malformed output records",
                context.dialect.display_name,
                page.rejections.len()
            )));
        }
        for observation in &mut page.observations {
            observation.coordinate.unit_key = output_unit_key(context, observation);
        }
        let next_safe_cursor = encode_output_frontier(page.next_frontier)?;
        let materialized = sink
            .materialize_page(ProOutputMaterializationPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: output_plan.source_epoch,
                observed_revision: context.source_revision.clone(),
                parser_revision: OPENCODE_NATIVE_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: output_plan.disposition,
                expected_prior_source_epoch: output_plan.expected_prior_source_epoch,
                expected_prior_cursor: expected_prior_cursor.clone(),
                next_safe_cursor: next_safe_cursor.clone(),
                terminal: page.terminal,
                observations: page.observations,
            })
            .map_err(output_sink_error)?;
        if materialized.source_epoch != output_plan.source_epoch
            || materialized.committed_cursor != next_safe_cursor
        {
            return Err(CaptureError::InvalidPayload(format!(
                "{} output sink acknowledged the wrong NativePath frontier",
                context.dialect.display_name
            )));
        }
        expected_prior_cursor = Some(next_safe_cursor);
    }
    let finished = scanner.finish_pro_replay()?;
    if !finished.complete
        || finished.source_generation_digest != context.current_state.source_generation_digest
        || finished.capability_digest != context.current_state.capability_digest
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

struct OutputReplayPlan {
    source_epoch: u64,
    disposition: ProOutputSourceDisposition,
    expected_prior_source_epoch: Option<u64>,
    frontier: OpenCodeNativeProFrontier,
    already_terminal: bool,
}

fn output_replay_plan(
    context: &OpenCodePublicationContext<'_>,
    sink: &dyn ProOutputSink,
    progress: Option<&ProOutputProgress>,
) -> Result<OutputReplayPlan> {
    let Some(progress) = progress else {
        return Ok(OutputReplayPlan {
            source_epoch: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            expected_prior_source_epoch: None,
            frontier: OpenCodeNativeProFrontier::default(),
            already_terminal: false,
        });
    };
    let same_revision = progress.observed_revision == context.source_revision
        && progress.parser_revision == OPENCODE_NATIVE_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision();
    if same_revision {
        let frontier = progress
            .cursor
            .as_ref()
            .map(decode_output_frontier)
            .transpose()?
            .unwrap_or_default();
        return Ok(OutputReplayPlan {
            source_epoch: progress.source_epoch,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            expected_prior_source_epoch: Some(progress.source_epoch),
            frontier,
            already_terminal: progress.terminal && frontier.terminal,
        });
    }
    Ok(OutputReplayPlan {
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode NativePath output epoch overflowed",
            ))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        expected_prior_source_epoch: Some(progress.source_epoch),
        frontier: OpenCodeNativeProFrontier::default(),
        already_terminal: false,
    })
}

pub(super) fn output_source_identity(
    context: &OpenCodePublicationContext<'_>,
) -> OutputSourceIdentity {
    OutputSourceIdentity {
        provider: context.dialect.provider.as_str().to_owned(),
        namespace_id: context.cursor_stream.clone(),
        source_id: format!("opencode-sqlite:{}", context.cursor_path_identity),
    }
}

pub(super) fn output_unit_key(
    context: &OpenCodePublicationContext<'_>,
    observation: &crate::ProOutputObservation,
) -> String {
    let session = &observation.associations.direct_session_id;
    let native = observation
        .coordinate
        .native_record_id
        .as_deref()
        .unwrap_or("unknown-native-record");
    match observation.coordinate.source_record_subrecord_index {
        Some(0) | None => format!(
            "{}:{session}:{native}:output",
            context.dialect.source_format
        ),
        Some(index) => format!(
            "{}:{session}:{native}:output:subrecord:{index}",
            context.dialect.source_format
        ),
    }
}

pub(super) fn encode_output_frontier(
    frontier: OpenCodeNativeProFrontier,
) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION,
        payload: serde_json::to_vec(&frontier)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    })
}

pub(super) fn decode_output_frontier(
    cursor: &OutputNativeCursor,
) -> Result<OpenCodeNativeProFrontier> {
    if cursor.version != OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "OpenCode NativePath output cursor has an unsupported version".to_owned(),
        ));
    }
    serde_json::from_slice(&cursor.payload)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn output_sink_error(error: ProOutputSinkError) -> CaptureError {
    CaptureError::InvalidPayload(format!("OpenCode-family output sink failed: {error}"))
}
