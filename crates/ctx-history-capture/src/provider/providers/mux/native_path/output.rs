use super::*;

pub(super) fn replay_source_outputs(
    plan: &MuxSourcePlan,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Mux.as_str().to_owned(),
        namespace_id: plan.canonical_source_identity.clone(),
        source_id: plan.path_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(_) => {
            sink.mark_behind(crate::ProOutputSinkError::new(
                "mux_output_progress",
                "Mux Pro output progress is unavailable",
            ));
            return Ok(true);
        }
    };
    let output_plan = match MuxOutputPlan::new(plan, sink, progress.as_ref()) {
        Ok(plan) => plan,
        Err(_) => {
            sink.mark_behind(crate::ProOutputSinkError::new(
                "mux_output_progress",
                "Mux Pro output progress is invalid",
            ));
            return Ok(true);
        }
    };
    if output_plan.noop {
        revalidate_source(plan)?;
        return Ok(false);
    }
    let mut session =
        mux_bounded_session_metadata(&plan.source, &plan.metadata_revision, context.imported_at)?;
    let (mut reader, mut hasher) =
        open_reader_at_frontier(&plan.path, &output_plan.start_frontier)?;
    let mut frontier = output_plan.start_frontier.clone();
    let mut expected_sink_frontier = output_plan.expected_sink_frontier.clone();
    let mut expected_source_epoch = output_plan.expected_source_epoch;
    let mut disposition = output_plan.disposition;
    loop {
        let Some(page) = read_output_page(
            &mut reader,
            &mut hasher,
            &mut session,
            plan,
            frontier.clone(),
        )?
        else {
            break;
        };
        let next_safe_frontier = match safe_frontier(&page.next) {
            Ok(frontier) => frontier,
            Err(_) => {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "mux_output_page",
                    "Mux Pro output page is invalid",
                ));
                return Ok(true);
            }
        };
        let expected_frontier = match safe_frontier(&page.expected) {
            Ok(frontier) => frontier,
            Err(_) => {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "mux_output_page",
                    "Mux Pro output page is invalid",
                ));
                return Ok(true);
            }
        };
        let estimated_output_bytes =
            page.observations
                .iter()
                .fold(16 * 1024_usize, |bytes, observation| {
                    bytes
                        .saturating_add(observation.content.len())
                        .saturating_add(observation.coordinate.unit_key.len())
                        .saturating_add(observation.locator.payload.len())
                        .saturating_add(512)
                });
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: output_plan.source_epoch,
            observed_revision: plan.source_revision.clone(),
            parser_revision: MUX_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_source_epoch,
            expected_prior_frontier: expected_sink_frontier.clone(),
            observations: page.observations,
        };
        let replay = match NativeProReplayPage::new(
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: page.physical_records,
                conservative_serialized_bytes: estimated_output_bytes,
            },
            output,
        ) {
            Ok(replay) => replay,
            Err(_) => {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "mux_output_page",
                    "Mux Pro output page is invalid",
                ));
                return Ok(true);
            }
        };
        revalidate_source(plan)?;
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(true);
        }
        frontier = page.next;
        expected_sink_frontier = Some(next_safe_frontier);
        expected_source_epoch = Some(output_plan.source_epoch);
        disposition = ProOutputSourceDisposition::AppendOrResume;
        if page.terminal {
            break;
        }
    }
    revalidate_source(plan)?;
    Ok(false)
}

struct MuxOutputPlan {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    start_frontier: MuxFrontier,
    noop: bool,
}

impl MuxOutputPlan {
    fn new(
        plan: &MuxSourcePlan,
        sink: &dyn ProOutputSink,
        progress: Option<&crate::ProOutputProgress>,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                start_frontier: MuxFrontier::initial(),
                noop: false,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(decode_output_frontier)
            .transpose()?;
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let revisions_match = progress.parser_revision == MUX_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision();
        if revisions_match
            && progress.observed_revision == plan.source_revision
            && progress.terminal
        {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                start_frontier: prior_frontier.unwrap_or_else(MuxFrontier::initial),
                noop: true,
            });
        }
        let append = if revisions_match {
            match prior_frontier.as_ref() {
                Some(frontier) => prefix_matches(&plan.path, &plan.observation, frontier)?,
                None => false,
            }
        } else {
            false
        };
        if append {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                start_frontier: prior_frontier.unwrap_or_else(MuxFrontier::initial),
                noop: false,
            });
        }
        Ok(Self {
            source_epoch: progress.source_epoch.checked_add(1).ok_or(
                CaptureError::InvalidPayload("Mux output source epoch is exhausted".to_owned()),
            )?,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: ProOutputSourceDisposition::Rewrite,
            start_frontier: MuxFrontier::initial(),
            noop: false,
        })
    }
}

struct MuxPreparedOutputPage {
    observations: Vec<ProOutputObservation>,
    expected: MuxFrontier,
    next: MuxFrontier,
    terminal: bool,
    physical_records: usize,
}

fn read_output_page(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    session: &mut MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    expected: MuxFrontier,
) -> Result<Option<MuxPreparedOutputPage>> {
    let mut observations = Vec::new();
    let mut source_bytes = 0_usize;
    let mut physical_records = 0_usize;
    let mut offset = expected.next_offset;
    let mut ordinal = expected.next_ordinal;
    let max_records = if plan.kind == MuxStreamKind::Partial {
        1
    } else {
        MUX_PAGE_MAX_RECORDS
    };
    while physical_records < max_records && source_bytes < MUX_PAGE_MAX_BYTES {
        let record = if plan.kind == MuxStreamKind::Partial {
            read_bounded_whole_record(reader, hasher, offset)?
        } else {
            read_bounded_record(reader, hasher, offset)?
        };
        let Some(record) = record else {
            break;
        };
        offset = record.end;
        source_bytes = source_bytes.saturating_add(record.observed_bytes);
        physical_records = physical_records.saturating_add(1);
        if !record.oversized && !record.payload.iter().all(u8::is_ascii_whitespace) {
            if let Ok(value) = serde_json::from_slice::<Value>(&record.payload) {
                if value.is_object() {
                    if let Some(provider_session_id) = value
                        .get("workspaceId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                    {
                        session.provider_session_id = bounded_mux_id(
                            provider_session_id.to_owned(),
                            &plan.path,
                            "workspace id",
                        )?;
                    }
                    if let Some(observation) =
                        prepare_output_observation(&value, &record, ordinal, session, plan)?
                    {
                        observations.push(observation);
                    }
                }
            }
        }
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mux output ordinal overflowed",
        ))?;
        if plan.kind == MuxStreamKind::Partial {
            break;
        }
    }
    let terminal = reader.fill_buf()?.is_empty();
    let next = MuxFrontier {
        version: MUX_FRONTIER_VERSION,
        next_offset: offset,
        next_ordinal: ordinal,
        prefix_sha256: hasher.clone().finalize().into(),
        file_identity: Some(plan.observation.content_identity()),
        legacy_valid_rows: expected.legacy_valid_rows,
    };
    Ok(Some(MuxPreparedOutputPage {
        observations,
        expected,
        next,
        terminal,
        physical_records,
    }))
}

pub(super) fn prepare_output_observation(
    value: &Value,
    record: &MuxRawRecord,
    ordinal: u64,
    session: &MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
) -> Result<Option<ProOutputObservation>> {
    let Some(projection) = mux_output_projection(value).filter(|output| output.body_available)
    else {
        return Ok(None);
    };
    let Some(content) = mux_result_content(value) else {
        return Ok(None);
    };
    let started_at = session
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let occurred_at = mux_message_timestamp_opt(value).unwrap_or(started_at);
    let native_sequence = if plan.kind.is_partial() {
        mux_partial_event_index(&record.payload).max(MUX_PARTIAL_NATIVE_ORDINAL)
    } else {
        ordinal
    };
    let outcome = match projection.outcome {
        MuxOutputOutcome::Success => OutputOutcome::Success,
        MuxOutputOutcome::Failure => OutputOutcome::Failure,
        MuxOutputOutcome::Timeout => OutputOutcome::Timeout,
        MuxOutputOutcome::Unknown => OutputOutcome::Unknown,
    };
    let native_record_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty() && id.len() <= 4 * 1024)
        .map(str::to_owned);
    let locator_payload = serde_json::to_vec(&json!({
        "path": plan.observation.canonical_path,
        "byte_start": record.start,
        "byte_end_exclusive": record.end,
        "kind": plan.kind.label(),
    }))
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(Some(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("mux:{}:{native_sequence}:output", plan.kind.label()),
            native_sequence,
            native_record_id,
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(record.start),
            byte_end_exclusive: Some(record.end),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: session.provider_session_id.clone(),
            root_session_id: session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| session.provider_session_id.clone()),
            parent_session_id: session.parent_provider_session_id.clone(),
            provider_session_id: Some(session.provider_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: match projection.call_ids.as_slice() {
            [call_id] => Some(call_id.clone()),
            _ => None,
        },
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: projection.exit_code,
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "mux-native-source-range-v1".to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    }))
}

pub(super) fn safe_frontier(frontier: &MuxFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        MUX_FRONTIER_VERSION,
        serde_json::to_vec(frontier)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

pub(super) fn decode_output_frontier(cursor: &crate::OutputNativeCursor) -> Result<MuxFrontier> {
    if cursor.version != MUX_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Mux output cursor version is unsupported".to_owned(),
        ));
    }
    let frontier: MuxFrontier = serde_json::from_slice(&cursor.payload)
        .map_err(|_| CaptureError::InvalidPayload("Mux output cursor is corrupt".to_owned()))?;
    if frontier.version != MUX_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Mux output frontier is inconsistent".to_owned(),
        ));
    }
    Ok(frontier)
}
