use super::*;

pub(super) fn replay_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, machine_id, paths, source_root, imported_at, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openclaw_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

pub(super) fn replay_outputs(
    store: &Store,
    machine_id: &str,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let authority = committed_replay_authority(store, machine_id, path)?;
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::OpenClaw.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                continue;
            }
        };
        replay_source(
            path,
            imported_at,
            sink,
            source,
            locator_identity,
            progress,
            &authority,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn replay_source(
    path: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
    authority: &Checkpoint,
) -> Result<()> {
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<Checkpoint>(&cursor.payload).ok())
        .filter(Checkpoint::supported);
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = can_resume.then_some(progress_cursor.as_ref()).flatten();
    let mut reader = open_pages(path, imported_at, true, None, false, previous)?;
    if !authority
        .source_observation
        .matches_live(&reader.observation)?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let source_change = reader.source_change;
    let observed_revision = reader.source_revision.clone();
    let mut output_state = OutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
        if !replay_checkpoint_is_covered_by(authority, &page.next_checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "OpenClaw output replay advanced beyond committed Core authority".to_owned(),
            ));
        }
        let expected_frontier = safe_frontier(&page.expected_checkpoint)?;
        let next_safe_frontier = safe_frontier(&page.next_checkpoint)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output_observation(&page.session, &locator_identity, output))
            .collect::<Result<Vec<_>>>()?;
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            conservative_serialized_bytes: page.conservative_serialized_bytes,
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::OpenClaw.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(());
        }
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    let outcome = reader
        .outcome
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw output replay reader completed without an outcome",
        ))?;
    if !outcome.checkpoint.terminal
        || !replay_checkpoint_is_covered_by(authority, &outcome.checkpoint)
    {
        return Err(CaptureError::InvalidPayload(
            "OpenClaw output replay outcome exceeded committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

pub(super) struct OutputState {
    pub(super) source: OutputSourceIdentity,
    pub(super) source_epoch: u64,
    pub(super) expected_source_epoch: Option<u64>,
    pub(super) expected_sink_frontier: Option<NativeSafeFrontier>,
    pub(super) disposition: ProOutputSourceDisposition,
}

impl OutputState {
    pub(super) fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        source_change: SourceChange,
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
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
            || progress.materializer_revision != materializer_revision
            || matches!(
                source_change,
                SourceChange::Fresh
                    | SourceChange::Rewrite
                    | SourceChange::Truncation
                    | SourceChange::Replacement
            );
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenClaw output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

pub(super) fn safe_frontier(checkpoint: &Checkpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(checkpoint)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn output_observation(
    session: &SessionFact,
    locator_identity: &str,
    output: OutputFact,
) -> Result<ProOutputObservation> {
    let locator = output_locator(
        locator_identity,
        output.byte_start,
        output.byte_end_exclusive,
    )?;
    let direct_session_id = session.cursor.provider_session_id.clone();
    Ok(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: output.native_record_id.clone(),
            native_sequence: output.raw_ordinal,
            native_record_id: Some(output.native_record_id),
            source_record_ordinal: Some(output.raw_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: session
                .cursor
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: session.cursor.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: session.cursor.agent_id.clone(),
            repository: None,
        },
        call_id: output.call_id,
        command: output.command,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "jsonl-source-item-byte-range-v1".to_owned(),
            payload: locator,
        },
        content: output.content,
    })
}

pub(super) fn output_locator(
    locator_identity: &str,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<Vec<u8>> {
    if byte_start >= byte_end_exclusive {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw output locator range is empty",
        ));
    }
    let identity = locator_identity.as_bytes();
    let length = u32::try_from(identity.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw output path identity exceeds locator limits".to_owned(),
        )
    })?;
    let mut locator = Vec::with_capacity(4 + identity.len() + 16);
    locator.extend_from_slice(&length.to_be_bytes());
    locator.extend_from_slice(identity);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(locator)
}
