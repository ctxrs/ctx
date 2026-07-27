use super::*;

pub(super) struct PrefixVerification {
    pub(super) core_valid: bool,
    pub(super) output_valid: bool,
    pub(super) states: Vec<(u64, Sha256)>,
    pub(super) headers: Vec<(u64, PiNativeSessionHeader)>,
}

pub(super) fn verify_planned_prefixes(
    reader: &mut BufReader<fs::File>,
    source: &PiFrozenSource,
    core: Option<&LanePlan>,
    output: Option<&LanePlan>,
    stats: &mut PiNativeScanStats,
) -> Result<PrefixVerification, PiNativePathError> {
    let targets = [core, output]
        .into_iter()
        .flatten()
        .filter(|plan| plan.verify_prefix)
        .map(|plan| plan.checkpoint.complete_offset)
        .collect::<Vec<_>>();
    let max_target = targets.iter().copied().max().unwrap_or(0);
    let mut hasher = initial_prefix_hasher();
    let mut states = vec![(0, hasher.clone())];
    let mut headers = Vec::new();
    let mut offset = 0_u64;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| PiNativePathError::Io {
            path: source.path.clone(),
            source: error,
        })?;
    while offset < max_target {
        let line = read_bounded_line(reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES).map_err(
            |error| PiNativePathError::Io {
                path: source.path.clone(),
                source: error,
            },
        )?;
        if line.observed_bytes == 0 || !line.terminated {
            break;
        }
        offset = offset
            .checked_add(line.observed_bytes)
            .ok_or(PiNativePathError::PositionOverflow)?;
        stats.prefix_bytes_hashed = stats
            .prefix_bytes_hashed
            .saturating_add(line.observed_bytes);
        if !line.oversized && might_be_session_header(&line.bytes) {
            if let Ok(value) = serde_json::from_slice::<Value>(json_record_bytes(&line.bytes)) {
                if value.get("type").and_then(Value::as_str) == Some("session") {
                    if let Ok(header) = parse_pi_session_header(value) {
                        stats.prefix_header_records_parsed =
                            stats.prefix_header_records_parsed.saturating_add(1);
                        headers.push((offset, header));
                    }
                }
            }
        }
        if targets.contains(&offset) {
            states.push((offset, hasher.clone()));
        }
        if offset > max_target {
            break;
        }
    }
    let valid = |plan: Option<&LanePlan>| {
        plan.is_some_and(|plan| {
            !plan.verify_prefix
                || states.iter().any(|(offset, hasher)| {
                    *offset == plan.checkpoint.complete_offset
                        && prefix_digest(hasher) == plan.checkpoint.committed_prefix_sha256
                })
        })
    };
    Ok(PrefixVerification {
        core_valid: core.is_none() || valid(core),
        output_valid: output.is_none() || valid(output),
        states,
        headers,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_complete_message_locator(
    event: &mut PiNativeEventRow,
    entry: &Value,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    line_number: usize,
) -> Result<(), CaptureError> {
    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            CaptureProvider::Pi,
            PI_SOURCE_FORMAT,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let Some((text, native_record_id)) = pi_complete_content_message_record(entry, line_number)
    else {
        return Ok(());
    };
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
        || byte_start >= byte_end_exclusive
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported Pi JSONL route has no complete-content profile",
        ));
    };
    let mut range = [0_u8; 16];
    range[..8].copy_from_slice(&byte_start.to_be_bytes());
    range[8..].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Pi verified-content locator collection is malformed"),
    )
}

pub(super) fn plan_lane(
    previous: Option<&PiNativeCheckpoint>,
    source: &PiFrozenSource,
) -> LanePlan {
    let initial =
        || PiNativeCheckpoint::initial(source.route_sha256, source.physical_file_id, source.len);
    let Some(previous) = previous else {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Fresh,
            verify_prefix: false,
        };
    };
    if !previous.revisions_match() {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Rewrite,
            verify_prefix: false,
        };
    }
    let same_route = previous.route_sha256 == source.route_sha256;
    let same_physical = previous.physical_file_id == source.physical_file_id
        && (previous.physical_file_id.is_some() || same_route);
    if source.len < previous.complete_offset {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: PiSourceLifecycle::Truncate,
            verify_prefix: false,
        };
    }
    if !same_physical {
        return LanePlan {
            checkpoint: initial(),
            lifecycle: if same_route {
                PiSourceLifecycle::Replace
            } else {
                PiSourceLifecycle::Copy
            },
            verify_prefix: false,
        };
    }
    let lifecycle = if !same_route {
        PiSourceLifecycle::Relocate
    } else if source.len == previous.complete_offset && previous.terminal {
        PiSourceLifecycle::NoOp
    } else {
        PiSourceLifecycle::Append
    };
    LanePlan {
        checkpoint: previous.clone(),
        lifecycle,
        verify_prefix: true,
    }
}

pub(super) fn apply_prefix_verification(
    plan: Option<&mut LanePlan>,
    valid: bool,
    source: &PiFrozenSource,
) {
    let Some(plan) = plan else {
        return;
    };
    if valid {
        return;
    }
    plan.checkpoint =
        PiNativeCheckpoint::initial(source.route_sha256, source.physical_file_id, source.len);
    plan.lifecycle = PiSourceLifecycle::Rewrite;
    plan.verify_prefix = false;
}

pub(super) fn current_checkpoint_for_plan(
    plan: &LanePlan,
    source: &PiFrozenSource,
) -> PiNativeCheckpoint {
    let mut checkpoint = plan.checkpoint.clone();
    if plan.lifecycle == PiSourceLifecycle::Relocate {
        checkpoint.route_sha256 = source.route_sha256;
        checkpoint.physical_file_id = source.physical_file_id;
        checkpoint.observed_file_len = source.len;
    }
    checkpoint
}

pub(super) fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    stream_hasher: &mut Sha256,
    max_retained_bytes: usize,
) -> io::Result<RawLine> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        stream_hasher.update(chunk);
        observed_bytes = observed_bytes
            .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| io::Error::other("Pi JSONL line position overflowed"))?;
        if !oversized {
            let remaining = max_retained_bytes.saturating_sub(bytes.len());
            if chunk.len() <= remaining {
                bytes.extend_from_slice(chunk);
            } else {
                bytes.clear();
                oversized = true;
            }
        }
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(RawLine {
        bytes,
        observed_bytes,
        terminated,
        oversized,
    })
}

pub(super) fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

pub(super) fn might_be_session_header(bytes: &[u8]) -> bool {
    bytes
        .windows(b"session".len())
        .any(|window| window == b"session")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn output_observation(
    header: &PiNativeSessionHeader,
    entry: &Value,
    event_type: EventType,
    ordinal: u64,
    line_number: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    locator_source_item: &[u8],
    occurred_at: chrono::DateTime<Utc>,
    command: Option<crate::provider::importer::OutputCommandContext>,
    outcome: crate::provider::importer::OutputOutcomeMetadata,
    content: &str,
) -> Result<ProOutputObservation, PiNativePathError> {
    let line_number_usize =
        usize::try_from(line_number).map_err(|_| PiNativePathError::PositionOverflow)?;
    let source_item_len = u32::try_from(locator_source_item.len())
        .map_err(|_| PiNativePathError::PositionOverflow)?;
    let mut locator = Vec::with_capacity(20_usize.saturating_add(locator_source_item.len()));
    locator.extend_from_slice(&source_item_len.to_be_bytes());
    locator.extend_from_slice(locator_source_item);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(ProOutputObservation {
        kind: if event_type == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: format!("line-{line_number}:output"),
            native_sequence: ordinal,
            native_record_id: Some(pi_output_native_record_id(entry, line_number_usize)),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: header.id.clone(),
            root_session_id: header.id.clone(),
            parent_session_id: None,
            provider_session_id: Some(header.id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: pi_output_call_id(entry),
        command,
        outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: PI_OUTPUT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content: content.as_bytes().to_vec(),
    })
}

pub(super) fn output_estimated_bytes_for(output: &ProOutputObservation) -> usize {
    let associations = &output.associations;
    let repository_bytes = associations.repository.as_ref().map_or(0, |repository| {
        repository
            .repository_id
            .len()
            .saturating_add(repository.checkout_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.worktree_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.object_format.as_ref().map_or(0, String::len))
    });
    let command_bytes = output.command.as_ref().map_or(0, |command| {
        command
            .tool_name
            .len()
            .saturating_add(command.command.len())
            .saturating_add(command.working_directory.as_ref().map_or(0, String::len))
    });
    let text_bytes = output
        .coordinate
        .unit_key
        .len()
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.direct_session_id.len())
        .saturating_add(associations.root_session_id.len())
        .saturating_add(
            associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(
            associations
                .provider_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.agent_id.as_ref().map_or(0, String::len))
        .saturating_add(repository_bytes)
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(command_bytes)
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len());
    text_bytes
        .saturating_mul(6)
        .saturating_add(1_024)
        .saturating_add(output.content.len())
}

pub(super) fn event_row_occurred_at(
    units: &[PiNativeCoreUnit],
) -> Result<chrono::DateTime<Utc>, PiNativePathError> {
    units
        .iter()
        .find_map(|unit| match unit {
            PiNativeCoreUnit::Event(event) => Some(event.occurred_at),
            _ => None,
        })
        .ok_or_else(|| {
            PiNativePathError::Normalization(CaptureError::SystemInvariant(
                "Pi NativePath event unit is missing",
            ))
        })
}

pub(super) fn page_error(error: impl std::fmt::Display) -> PiNativePathError {
    PiNativePathError::Page(error.to_string())
}
