use super::*;

pub(super) fn replay_output(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    replay_output_inner(conn, snapshot, authority, sink)
}

fn replay_output_inner(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Firebender.as_str().to_owned(),
        namespace_id: authority.canonical_source_identity.clone(),
        source_id: authority.route_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "firebender_output_progress",
                "Firebender Pro output progress is unavailable",
            ));
            return Ok(true);
        }
    };
    let mut state = match output_state(progress, authority, sink) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "firebender_output_progress",
                "Firebender Pro output progress is invalid",
            ));
            return Ok(true);
        }
    };
    if state.complete {
        return Ok(false);
    }
    loop {
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let page = with_sqlite_read_snapshot(conn, || build_page(conn, &state.frontier, true))?;
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let output_page = (|| {
            let observations = output_observations(&page)?;
            let expected = safe_frontier(&page.expected)?;
            let next = safe_frontier(&page.next)?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: authority.source_revision.clone(),
                parser_revision: FIREBENDER_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                NativeSourceIdentity::new(
                    CaptureProvider::Firebender.as_str(),
                    authority.route_identity.clone(),
                ),
                expected,
                next.clone(),
                page.next.terminal,
                NativePageAccounting {
                    logical_units: output.observations.len(),
                    conservative_serialized_bytes: NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                },
                output,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            Ok::<_, CaptureError>((replay, next))
        })();
        let (replay, next) = match output_page {
            Ok(page) => page,
            Err(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "firebender_output_page",
                    "Firebender Pro output page is invalid",
                ));
                return Ok(true);
            }
        };
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(true);
        }
        state.frontier = page.next;
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        if state.frontier.terminal {
            return Ok(false);
        }
    }
}

struct FirebenderOutputState {
    frontier: FirebenderFrontier,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    complete: bool,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    authority: &FirebenderSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<FirebenderOutputState> {
    let Some(progress) = progress else {
        return Ok(FirebenderOutputState {
            frontier: FirebenderFrontier::initial(),
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            complete: false,
        });
    };
    let prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            if cursor.version != FIREBENDER_NATIVE_FRONTIER_VERSION {
                return Err(CaptureError::InvalidPayload(
                    "Firebender output cursor has an unsupported version".to_owned(),
                ));
            }
            serde_json::from_slice::<FirebenderFrontier>(&cursor.payload)
                .map_err(CaptureError::from)
        })
        .transpose()?;
    let matching = progress.observed_revision == authority.source_revision
        && progress.parser_revision == FIREBENDER_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && prior_frontier.is_some();
    if matching {
        let frontier = prior_frontier.unwrap_or_else(FirebenderFrontier::initial);
        frontier.validate()?;
        return Ok(FirebenderOutputState {
            complete: progress.terminal && frontier.terminal,
            frontier,
            source_epoch: progress.source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: progress
                .cursor
                .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
                .transpose()
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
            disposition: ProOutputSourceDisposition::AppendOrResume,
        });
    }
    Ok(FirebenderOutputState {
        frontier: FirebenderFrontier::initial(),
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Firebender output source epoch is exhausted".to_owned(),
            ))?,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: progress
            .cursor
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        complete: false,
    })
}

fn output_observations(page: &FirebenderPage) -> Result<Vec<ProOutputObservation>> {
    let Some(row) = page.row.as_ref() else {
        return Ok(Vec::new());
    };
    let mut observations = Vec::new();
    for (offset, message) in row.messages[page.message_start..page.message_end]
        .iter()
        .enumerate()
    {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let index = page.message_start.saturating_add(offset);
        let provider_event_index = u64::try_from(index).map_err(|_| {
            CaptureError::InvalidPayload("Firebender output index exceeds u64".to_owned())
        })?;
        let subrecord = u32::try_from(index).map_err(|_| {
            CaptureError::InvalidPayload(
                "Firebender output index exceeds native coordinates".to_owned(),
            )
        })?;
        let fallback = provider_timestamp_millis(Some(row.created_at), DateTime::<Utc>::UNIX_EPOCH);
        let occurred_at = firebender_message_time(message, fallback);
        let evidence = firebender_output_evidence(message);
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let native_record_id = message
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| call_id.clone())
            .unwrap_or_else(|| format!("message:{provider_event_index}"));
        observations.push(ProOutputObservation {
            kind: OutputObservationKind::Tool,
            coordinate: OutputNativeCoordinate {
                unit_key: format!("firebender:{}:message:{index:010}:output", row.id),
                native_sequence: provider_event_index,
                native_record_id: Some(native_record_id),
                source_record_ordinal: Some(row.row_ordinal),
                source_record_subrecord_index: Some(subrecord),
                byte_start: None,
                byte_end_exclusive: None,
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: row.id.clone(),
                root_session_id: row.id.clone(),
                parent_session_id: None,
                provider_session_id: Some(row.id.clone()),
                agent_id: None,
                repository: None,
            },
            call_id,
            command: None,
            outcome: OutputOutcomeMetadata {
                outcome: if evidence.timeout {
                    OutputOutcome::Timeout
                } else if evidence.failure {
                    OutputOutcome::Failure
                } else if evidence.success {
                    OutputOutcome::Success
                } else {
                    OutputOutcome::Unknown
                },
                exit_code: evidence.exit_code,
                duration_ms: evidence.duration_ms,
            },
            locator: OutputSourceLocator {
                version: 1,
                kind: FIREBENDER_LOCATOR_KIND.to_owned(),
                payload: row.rowid.to_be_bytes().to_vec(),
            },
            content: firebender_result_content(message)
                .unwrap_or_default()
                .into_bytes(),
        });
    }
    Ok(observations)
}

fn safe_frontier(frontier: &FirebenderFrontier) -> Result<NativeSafeFrontier> {
    let encoded = serde_json::to_vec(frontier)?;
    NativeSafeFrontier::new(FIREBENDER_NATIVE_FRONTIER_VERSION, encoded)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
