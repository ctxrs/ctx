use super::*;

impl CursorFrozenSource {
    pub(crate) fn replay_outputs(
        &self,
        source_root: &Path,
        canonical_source_identity: &str,
        core_checkpoint: &CursorCheckpoint,
        observed_revision: &str,
        sink: &dyn ProOutputSink,
    ) -> Result<()> {
        let output_source = OutputSourceIdentity {
            provider: ctx_history_core::CaptureProvider::Cursor
                .as_str()
                .to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: self.observation.locator_identity.clone(),
        };
        let progress = match sink.observe_source(&output_source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        };
        let same_physical_source = progress.as_ref().is_none_or(|progress| {
            cursor_output_revision_allows_resume(&progress.observed_revision, observed_revision)
        });
        let resume = same_physical_source
            .then(|| resumable_cursor_output_checkpoint(progress.as_ref(), sink))
            .flatten();
        if progress.as_ref().is_some_and(|progress| {
            progress.observed_revision == observed_revision
                && progress.terminal == core_checkpoint.terminal
                && resume.as_ref() == Some(core_checkpoint)
        }) {
            return Ok(());
        }

        let append_or_resume = progress.is_some() && resume.is_some();
        let mut state =
            CursorOutputState::new(output_source.clone(), progress.as_ref(), append_or_resume)?;
        let outcome = self.scan_and_replay_outputs(
            canonical_source_identity,
            core_checkpoint,
            observed_revision,
            resume.as_ref(),
            sink,
            &mut state,
        )?;
        if outcome == CursorOutputScanOutcome::PrefixMismatch && append_or_resume {
            state = CursorOutputState::new(output_source, progress.as_ref(), false)?;
            let retry = self.scan_and_replay_outputs(
                canonical_source_identity,
                core_checkpoint,
                observed_revision,
                None,
                sink,
                &mut state,
            )?;
            if retry == CursorOutputScanOutcome::PrefixMismatch {
                return Err(CaptureError::InvalidPayload(
                    "Cursor full output replay did not reproduce committed Core".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn scan_and_replay_outputs(
        &self,
        canonical_source_identity: &str,
        core_checkpoint: &CursorCheckpoint,
        observed_revision: &str,
        resume: Option<&CursorCheckpoint>,
        sink: &dyn ProOutputSink,
        state: &mut CursorOutputState,
    ) -> Result<CursorOutputScanOutcome> {
        let file = self.open()?;
        let mut reader = BufReader::new(file);
        let source_identity = NativeSourceIdentity::new(
            ctx_history_core::CaptureProvider::Cursor.as_str(),
            canonical_source_identity,
        );
        let mut emit = |page: CursorOutputPage| {
            let expected_frontier = cursor_output_frontier(&page.expected_checkpoint)?;
            let next_frontier = cursor_output_frontier(&page.next_checkpoint)?;
            let observations = page
                .outputs
                .into_iter()
                .map(|output| cursor_output_observation(&self.observation, output))
                .collect::<Result<Vec<_>>>()?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: state.source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: observed_revision.to_owned(),
                parser_revision: CURSOR_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                source_identity.clone(),
                expected_frontier,
                next_frontier.clone(),
                page.terminal,
                NativePageAccounting {
                    logical_units: page.logical_units,
                    conservative_serialized_bytes: page.conservative_serialized_bytes,
                },
                output,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            self.revalidate()?;
            if process_pro_replay_only(replay, sink).is_err() {
                return Ok(false);
            }
            state.advance(next_frontier);
            Ok(true)
        };
        let outcome = scan_cursor_output_pages(&mut reader, resume, core_checkpoint, &mut emit)?;
        self.revalidate()?;
        Ok(outcome)
    }
}

struct CursorOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl CursorOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<&ProOutputProgress>,
        append_or_resume: bool,
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
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok(Self {
            source,
            source_epoch: if append_or_resume {
                progress.source_epoch
            } else {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Cursor output source epoch exhausted",
                    ))?
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if append_or_resume {
                ProOutputSourceDisposition::AppendOrResume
            } else {
                ProOutputSourceDisposition::Rewrite
            },
        })
    }

    fn advance(&mut self, next_frontier: NativeSafeFrontier) {
        self.expected_source_epoch = Some(self.source_epoch);
        self.expected_sink_frontier = Some(next_frontier);
        self.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
}

fn resumable_cursor_output_checkpoint(
    progress: Option<&ProOutputProgress>,
    sink: &dyn ProOutputSink,
) -> Option<CursorCheckpoint> {
    let progress = progress?;
    if progress.parser_revision != CURSOR_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != sink.materializer_revision()
    {
        return None;
    }
    let cursor = progress.cursor.as_ref()?;
    if cursor.version != CURSOR_OUTPUT_FRONTIER_VERSION {
        return None;
    }
    serde_json::from_slice::<CursorCheckpoint>(&cursor.payload)
        .ok()
        .filter(CursorCheckpoint::is_supported)
}

fn cursor_output_frontier(checkpoint: &CursorCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CURSOR_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(checkpoint)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn cursor_output_revision_allows_resume(previous: &str, current: &str) -> bool {
    match (
        cursor_output_revision_file_identity(previous),
        cursor_output_revision_file_identity(current),
    ) {
        (Some(previous), Some(current)) => previous == current,
        _ => true,
    }
}

fn cursor_output_revision_file_identity(revision: &str) -> Option<(&str, &str)> {
    let (_, identity) = revision.rsplit_once(";device=")?;
    let (device, inode) = identity.split_once(";inode=")?;
    (device != "none" && inode != "none").then_some((device, inode))
}

fn cursor_output_observation(
    source: &CursorSourceObservation,
    output: CursorOutputFact,
) -> Result<ProOutputObservation> {
    let locator = serde_json::to_vec(&serde_json::json!({
        "path": source.path,
        "locator_identity": source.locator_identity,
        "semantic_ordinal": output.semantic_ordinal,
        "subrecord_index": output.subrecord_index,
        "byte_start": output.byte_start,
        "byte_end_exclusive": output.byte_end_exclusive,
    }))?;
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "cursor-semantic-v1:{}:{}",
                output.semantic_ordinal, output.subrecord_index
            ),
            native_sequence: output.semantic_ordinal,
            native_record_id: output.call_id.clone(),
            source_record_ordinal: Some(output.semantic_ordinal),
            source_record_subrecord_index: Some(output.subrecord_index),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: output.occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: source.native_session_id.clone(),
            root_session_id: source.native_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(source.native_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "cursor/nativepath/jsonl-result".to_owned(),
            payload: locator,
        },
        content: output.content,
    })
}
