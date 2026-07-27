use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    path: &Path,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(path, conn, snapshot, context, core, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "shelley_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    path: &Path,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    if core.route_retired {
        retire_output_or_mark_behind(path, context, core, Some(sink));
        return Ok(());
    }
    let conversation_select =
        shelley_conversation_select_expressions(&shelley_conversation_columns(conn)?, "c");
    let message_columns = shelley_message_columns(conn)?;
    let has_message_sequence_id = message_columns.contains("sequence_id");
    let message_select = shelley_message_select_expressions(&message_columns, "m");
    if !verify_message_prefix(conn, &message_select, &conversation_select, &core.messages)? {
        return Err(CaptureError::InvalidPayload(
            "Shelley output replay no longer matches committed Core authority".to_owned(),
        ));
    }
    let source = output_source(path, context)?;
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == SHELLEY_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<ShelleyOutputFrontier>(&cursor.payload).ok())
        .filter(|frontier| {
            frontier.version == SHELLEY_OUTPUT_FRONTIER_VERSION
                && frontier.generation == core.generation
                && !frontier.retired
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == SHELLEY_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.as_ref().is_some_and(|frontier| {
                verify_message_prefix(
                    conn,
                    &message_select,
                    &conversation_select,
                    &frontier.messages,
                )
                .unwrap_or(false)
                    && frontier.messages.count <= core.messages.count
            })
    });
    let mut frontier = if can_resume {
        progress_frontier
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "Shelley output resume lost its frontier",
            ))?
    } else {
        ShelleyOutputFrontier::initial(core.generation)
    };
    let mut output_state =
        ShelleyOutputState::new(source, progress, can_resume, sink.materializer_revision())?;
    let mut emitted = false;
    loop {
        let expected = frontier.clone();
        let mut observations = Vec::new();
        let mut logical_units = 0_usize;
        let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
        while logical_units < SHELLEY_PAGE_MAX_UNITS
            && frontier.messages.count < core.messages.count
        {
            let Some((unit, row_digest)) = next_message_unit(
                conn,
                &message_select,
                &conversation_select,
                has_message_sequence_id,
                frontier.messages.after_rowid,
                core.messages.after_rowid,
            )?
            else {
                return Err(CaptureError::InvalidPayload(
                    "Shelley output replay frontier ended before committed Core".to_owned(),
                ));
            };
            let bytes = unit.retained_bytes();
            if logical_units != 0 && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES {
                break;
            }
            frontier.messages.advance(unit.rowid(), row_digest)?;
            retained_bytes = retained_bytes.saturating_add(bytes);
            logical_units = logical_units.saturating_add(1);
            if let ShelleyUnit::Accepted { value, .. } = unit {
                if let Some(classification) = shelley_output_classification(&value.message) {
                    observations.push(shelley_output_observation(
                        &value.message,
                        &value.conversation,
                        value.parent_bearing,
                        value.provider_event_index,
                        context,
                        &classification,
                    )?);
                }
            }
        }
        frontier.terminal = core.terminal && frontier.messages == core.messages;
        if logical_units == 0 && emitted {
            break;
        }
        let output_bytes = observations.iter().fold(0_usize, |bytes, observation| {
            bytes
                .saturating_add(observation.content.len())
                .saturating_add(512)
        });
        let accounting = NativePageAccounting {
            logical_units: logical_units.max(1),
            conservative_serialized_bytes: retained_bytes
                .saturating_add(output_bytes)
                .min(crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES),
        };
        let expected_frontier = output_safe_frontier(&expected)?;
        let next_safe_frontier = output_safe_frontier(&frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: format!("{}:generation={}", core.source_revision, core.generation),
            parser_revision: SHELLEY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::Shelley.as_str(), &core.locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            frontier.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(());
        }
        emitted = true;
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
        if frontier.terminal || frontier.messages == core.messages {
            break;
        }
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    Ok(())
}

struct ShelleyOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl ShelleyOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let prior = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn output_source(path: &Path, context: &ProviderAdapterContext) -> Result<OutputSourceIdentity> {
    Ok(OutputSourceIdentity {
        provider: CaptureProvider::Shelley.as_str().to_owned(),
        namespace_id: context
            .source_root_display()
            .unwrap_or_else(|| path.display().to_string()),
        source_id: provider_path_identity(path)?,
    })
}

fn output_safe_frontier(frontier: &ShelleyOutputFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        SHELLEY_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn retire_output_or_mark_behind(
    path: &Path,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = retire_output(path, context, core, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "shelley_nativepath_output_retirement",
            error.to_string(),
        ));
    }
}

fn retire_output(
    path: &Path,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let source = output_source(path, context)?;
    let Some(progress) = sink
        .observe_source(&source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
    else {
        return Ok(());
    };
    if progress.terminal
        && progress
            .cursor
            .as_ref()
            .and_then(|cursor| {
                serde_json::from_slice::<ShelleyOutputFrontier>(&cursor.payload).ok()
            })
            .is_some_and(|frontier| frontier.retired)
    {
        return Ok(());
    }
    let expected = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let retired = ShelleyOutputFrontier {
        version: SHELLEY_OUTPUT_FRONTIER_VERSION,
        generation: core.generation,
        messages: core.messages.clone(),
        terminal: true,
        retired: true,
    };
    let next = output_safe_frontier(&retired)?;
    let output =
        NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source,
            source_epoch: progress.source_epoch.checked_add(1).ok_or(
                CaptureError::SystemInvariant("Shelley output source epoch exhausted"),
            )?,
            observed_revision: "shelley-source-missing".to_owned(),
            parser_revision: SHELLEY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: ProOutputSourceDisposition::Rewrite,
            expected_prior_source_epoch: Some(progress.source_epoch),
            expected_prior_frontier: expected.clone(),
            observations: Vec::new(),
        };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Shelley.as_str(), &core.locator_identity),
        NativeSafeFrontier::new(
            SHELLEY_OUTPUT_FRONTIER_VERSION,
            serde_json::to_vec(&ShelleyOutputFrontier::initial(core.generation))?,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        next,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: SHELLEY_PAGE_FIXED_OVERHEAD,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let _ = process_pro_replay_only(replay, sink);
    Ok(())
}
