use super::*;

#[derive(Debug)]
struct DeepAgentsOutputPage {
    expected: DeepAgentsOutputFrontier,
    next: DeepAgentsOutputFrontier,
    key: Option<DeepAgentsWriteKey>,
    rowid: Option<i64>,
    messages: Vec<(usize, DeepAgentsMessage)>,
    occurred_at: Option<DateTime<Utc>>,
    retained_bytes: usize,
}

pub(super) fn replay_outputs(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    replay_outputs_inner(conn, snapshot, authority, context, sink)
}

pub(super) fn replay_outputs_inner(
    conn: &Connection,
    snapshot: &ProviderSqliteSourceSnapshot,
    authority: &DeepAgentsSourceAuthority,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::DeepAgents.as_str().to_owned(),
        namespace_id: authority.canonical_source_identity.clone(),
        source_id: authority.route_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "deepagents_output_progress",
                "Deep Agents Pro output progress is unavailable",
            ));
            return Ok(true);
        }
    };
    let mut state = match output_state(progress, authority, sink) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "deepagents_output_progress",
                "Deep Agents Pro output progress is invalid",
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
        let page =
            with_sqlite_read_snapshot(conn, || build_output_page(conn, context, &state.frontier))?;
        if !snapshot.revalidate(&authority.database_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let output_page = (|| {
            let observations = output_observations(&page)?;
            let expected = output_safe_frontier(&page.expected)?;
            let next = output_safe_frontier(&page.next)?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: authority.source_revision.clone(),
                parser_revision: DEEPAGENTS_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                NativeSourceIdentity::new(
                    CaptureProvider::DeepAgents.as_str(),
                    authority.route_identity.clone(),
                ),
                expected,
                next.clone(),
                page.next.terminal,
                NativePageAccounting {
                    logical_units: output.observations.len().max(1),
                    conservative_serialized_bytes: page.retained_bytes,
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
                    "deepagents_output_page",
                    "Deep Agents Pro output page is invalid",
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

pub(super) fn record_output_behind(summary: &mut ProviderImportSummary) {
    summary.record_failure(ProviderImportFailure {
        line: 0,
        error: "Deep Agents Pro output is behind committed Core".to_owned(),
    });
    summary.work_remaining = true;
}

struct DeepAgentsOutputState {
    frontier: DeepAgentsOutputFrontier,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    complete: bool,
}

fn output_state(
    progress: Option<ProOutputProgress>,
    authority: &DeepAgentsSourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<DeepAgentsOutputState> {
    let Some(progress) = progress else {
        return Ok(DeepAgentsOutputState {
            frontier: DeepAgentsOutputFrontier::initial(),
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
            if cursor.version != DEEPAGENTS_OUTPUT_FRONTIER_VERSION {
                return Err(CaptureError::InvalidPayload(
                    "Deep Agents output cursor has an unsupported version".to_owned(),
                ));
            }
            serde_json::from_slice::<DeepAgentsOutputFrontier>(&cursor.payload)
                .map_err(CaptureError::from)
        })
        .transpose()?;
    let matching = progress.observed_revision == authority.source_revision
        && progress.parser_revision == DEEPAGENTS_OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && prior_frontier.is_some();
    if matching {
        let frontier = prior_frontier.unwrap_or_else(DeepAgentsOutputFrontier::initial);
        return Ok(DeepAgentsOutputState {
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
    Ok(DeepAgentsOutputState {
        frontier: DeepAgentsOutputFrontier::initial(),
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Deep Agents output source epoch is exhausted".to_owned(),
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

fn build_output_page(
    conn: &Connection,
    context: &ProviderAdapterContext,
    expected: &DeepAgentsOutputFrontier,
) -> Result<DeepAgentsOutputPage> {
    if expected.terminal {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents output replay advanced a terminal frontier".to_owned(),
        ));
    }
    let candidate = match expected.active_rowid {
        Some(rowid) => deepagents_write_candidate_at(conn, rowid)?
            .ok_or(CaptureError::SourceChangedDuringCapture)?,
        None => match deepagents_next_write_candidate(conn, expected.after_rowid)? {
            Some(candidate) => candidate,
            None => {
                let mut next = expected.clone();
                next.terminal = true;
                return Ok(DeepAgentsOutputPage {
                    expected: expected.clone(),
                    next,
                    key: None,
                    rowid: None,
                    messages: Vec::new(),
                    occurred_at: None,
                    retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
                });
            }
        },
    };
    let rowid = candidate.rowid;
    let Some(key) = candidate.key else {
        let mut next = expected.clone();
        next.after_rowid = Some(rowid);
        next.active_rowid = None;
        next.next_message_offset = 0;
        return Ok(DeepAgentsOutputPage {
            expected: expected.clone(),
            next,
            key: None,
            rowid: Some(rowid),
            messages: Vec::new(),
            occurred_at: None,
            retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
        });
    };
    let (value_type, value) = deepagents_hydrate_write(conn, rowid)?;
    let messages = match deepagents_messages_from_blob(value_type.as_deref(), &value) {
        Ok(messages) => messages.messages,
        Err(_) => {
            let mut next = expected.clone();
            next.after_rowid = Some(rowid);
            next.active_rowid = None;
            next.next_message_offset = 0;
            return Ok(DeepAgentsOutputPage {
                expected: expected.clone(),
                next,
                key: None,
                rowid: Some(rowid),
                messages: Vec::new(),
                occurred_at: None,
                retained_bytes: DEEPAGENTS_PAGE_OVERHEAD_BYTES,
            });
        }
    };
    let start = usize::try_from(expected.next_message_offset).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents output message frontier exceeds platform limits".to_owned(),
        )
    })?;
    if start > messages.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let end = start
        .saturating_add(NATIVE_INGESTION_PAGE_MAX_UNITS)
        .min(messages.len());
    let selected = messages[start..end]
        .iter()
        .cloned()
        .enumerate()
        .map(|(offset, message)| (start.saturating_add(offset), message))
        .collect::<Vec<_>>();
    let mut next = expected.clone();
    if end == messages.len() {
        next.after_rowid = Some(rowid);
        next.active_rowid = None;
        next.next_message_offset = 0;
    } else {
        next.active_rowid = Some(rowid);
        next.next_message_offset = u32::try_from(end).map_err(|_| {
            CaptureError::InvalidPayload(
                "Deep Agents output row contains too many messages".to_owned(),
            )
        })?;
    }
    let retained_bytes = DEEPAGENTS_PAGE_OVERHEAD_BYTES.saturating_add(value.len());
    ensure_retained_bound(retained_bytes)?;
    let occurred_at =
        deepagents_checkpoint_time(conn, context, &key.thread_id, &key.checkpoint_id)?;
    Ok(DeepAgentsOutputPage {
        expected: expected.clone(),
        next,
        key: Some(key),
        rowid: Some(rowid),
        messages: selected,
        occurred_at,
        retained_bytes,
    })
}

fn output_observations(page: &DeepAgentsOutputPage) -> Result<Vec<ProOutputObservation>> {
    let Some(key) = page.key.as_ref() else {
        return Ok(Vec::new());
    };
    let occurred_at = page.occurred_at.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    let row_ordinal = page.rowid.and_then(|rowid| u64::try_from(rowid).ok());
    let mut observations = Vec::new();
    for (offset, message) in &page.messages {
        if message.role != EventRole::Tool {
            continue;
        }
        let subrecord = u32::try_from(*offset).map_err(|_| {
            CaptureError::InvalidPayload(
                "Deep Agents output offset exceeds native coordinates".to_owned(),
            )
        })?;
        let address = DeepAgentsContentAddress::from_write(key, *offset).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents output offset exceeds locator bounds".to_owned(),
            )
        })?;
        let locator_payload = address.encode().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Deep Agents output locator exceeds coordinate bounds".to_owned(),
            )
        })?;
        let native_sequence = message.message_id.as_deref().map_or_else(
            || coordinate_hash(key, *offset),
            |message_id| deepagents_message_identity(&key.thread_id, message_id).provider_index,
        );
        let stable_record_id = message.message_id.clone().unwrap_or_else(|| {
            format!(
                "{}:{}:{}:{}:{offset}",
                key.thread_id, key.checkpoint_id, key.task_id, key.idx
            )
        });
        observations.push(ProOutputObservation {
            kind: OutputObservationKind::Tool,
            coordinate: OutputNativeCoordinate {
                unit_key: format!("deepagents:{}:output:{stable_record_id}", key.thread_id),
                native_sequence,
                native_record_id: Some(stable_record_id),
                source_record_ordinal: row_ordinal,
                source_record_subrecord_index: Some(subrecord),
                byte_start: None,
                byte_end_exclusive: None,
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: key.thread_id.clone(),
                root_session_id: key.thread_id.clone(),
                parent_session_id: None,
                provider_session_id: Some(key.thread_id.clone()),
                agent_id: None,
                repository: None,
            },
            call_id: message.tool_call_id.clone(),
            command: None,
            outcome: deepagents_output_outcome(message),
            locator: OutputSourceLocator {
                version: 1,
                kind: DEEPAGENTS_CONTENT_LOCATOR_KIND.to_owned(),
                payload: locator_payload,
            },
            content: message.text.as_bytes().to_vec(),
        });
    }
    Ok(observations)
}

pub(super) fn coordinate_hash(key: &DeepAgentsWriteKey, offset: usize) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        key.thread_id.as_bytes(),
        key.checkpoint_id.as_bytes(),
        key.task_id.as_bytes(),
        &key.idx.to_be_bytes(),
        &u64::try_from(offset).unwrap_or(u64::MAX).to_be_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub(super) fn output_safe_frontier(
    frontier: &DeepAgentsOutputFrontier,
) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        DEEPAGENTS_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}
