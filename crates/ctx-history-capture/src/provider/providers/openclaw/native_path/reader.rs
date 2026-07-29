use super::*;

pub(super) fn open_pages_from_admitted(
    canonical_path: PathBuf,
    imported_at: DateTime<Utc>,
    previous: Option<&Checkpoint>,
    observation: OpenClawSessionObservation,
    transcript: OpenedProviderSourceFile,
) -> Result<PageReader> {
    let file = transcript.file().try_clone()?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    open_pages_from_file(
        canonical_path,
        imported_at,
        previous,
        observation,
        file,
        transcript,
    )
}

fn open_pages_from_file(
    canonical_path: PathBuf,
    imported_at: DateTime<Utc>,
    previous: Option<&Checkpoint>,
    observation: OpenClawSessionObservation,
    mut file: File,
    admitted_transcript: OpenedProviderSourceFile,
) -> Result<PageReader> {
    let source_revision = source_revision(&observation);
    let mut prefix_hasher = new_prefix_hasher();
    let mut complete_prefix_end = 0_u64;
    let mut next_raw_ordinal = 0_u64;
    let mut accepted_events = 0_u64;
    let mut accepted_file_touches = 0_u64;
    let mut rejected_records = 0_u64;
    let mut session = fresh_session(&canonical_path, imported_at, &observation.index);
    let mut source_change = SourceChange::Fresh;
    let mut generation = 0_u64;
    let mut skip_scan = false;

    if let Some(previous) = previous.filter(|checkpoint| checkpoint.supported()) {
        let same_path = previous.source_path == canonical_path;
        let continuity = file_continuity(
            &previous.source_observation.transcript,
            &observation.transcript,
        )?;
        let enough_bytes = observation.transcript.length >= previous.complete_prefix_end;
        if same_path && continuity != FileContinuity::Replacement && enough_bytes {
            let observed_prefix =
                hash_prefix(&mut file, previous.complete_prefix_end, new_prefix_hasher())?;
            if prefix_digest(&observed_prefix) == previous.complete_prefix_sha256
                && previous
                    .source_observation
                    .auxiliary_matches_live(&observation)?
            {
                prefix_hasher = observed_prefix;
                complete_prefix_end = previous.complete_prefix_end;
                next_raw_ordinal = previous.next_raw_ordinal;
                accepted_events = previous.accepted_events;
                accepted_file_touches = previous.accepted_file_touches;
                rejected_records = previous.rejected_records;
                session = resume_session(previous, &observation)?;
                generation = previous.generation;
                source_change = if observation.transcript.length == previous.complete_prefix_end
                    && previous.terminal
                {
                    skip_scan = true;
                    SourceChange::Unchanged
                } else {
                    SourceChange::Append
                };
            } else {
                source_change = match continuity {
                    FileContinuity::SamePhysicalFile => SourceChange::Rewrite,
                    FileContinuity::ExactPathPrefixProof | FileContinuity::Replacement => {
                        SourceChange::Replacement
                    }
                };
                generation = next_generation(previous)?;
            }
        } else if same_path && observation.transcript.length < previous.complete_prefix_end {
            source_change = match continuity {
                FileContinuity::SamePhysicalFile => SourceChange::Truncation,
                FileContinuity::ExactPathPrefixProof | FileContinuity::Replacement => {
                    SourceChange::Replacement
                }
            };
            generation = next_generation(previous)?;
        } else if same_path {
            source_change = SourceChange::Replacement;
            generation = next_generation(previous)?;
        }
    }

    file.seek(SeekFrom::Start(complete_prefix_end))?;
    Ok(PageReader {
        path: canonical_path,
        imported_at,
        observation,
        source_revision,
        generation,
        reader: BufReader::new(file),
        admitted_transcript,
        prefix_hasher,
        complete_prefix_end,
        next_raw_ordinal,
        accepted_events,
        accepted_file_touches,
        rejected_records,
        session,
        source_change,
        skip_scan,
        finished: false,
        outcome: None,
    })
}

pub(super) fn next_generation(previous: &Checkpoint) -> Result<u64> {
    previous
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw NativePath source generation exhausted",
        ))
}

impl PageReader {
    pub(super) fn next_page(&mut self) -> Result<Option<Page>> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true)?;
            return Ok(None);
        }

        let expected_checkpoint = self.checkpoint(false);
        let mut events = Vec::new();
        let mut touches = Vec::new();
        let mut rejections = Vec::new();
        let mut physical_records = 0_usize;
        let mut logical_units = 0_usize;
        let mut serialized_bytes = PAGE_ENVELOPE_BYTES;

        while physical_records < PAGE_MAX_RECORDS {
            let start = self.complete_prefix_end;
            let ordinal = self.next_raw_ordinal;
            let hasher_before = self.prefix_hasher.clone();
            let line = read_bounded_line(
                &mut self.reader,
                &mut self.prefix_hasher,
                self.observation.transcript.length,
                start,
            )?;
            let (bytes, end) = match line {
                Line::EndOfFile => {
                    self.finish(true)?;
                    break;
                }
                Line::IncompleteTail => {
                    self.prefix_hasher = hasher_before;
                    self.reader.seek(SeekFrom::Start(start))?;
                    self.finish(false)?;
                    break;
                }
                Line::Oversized { end } => {
                    let rejection = Rejection {
                        reason: format!(
                            "{}:{} exceeds the {} byte JSONL record limit",
                            self.path.display(),
                            ordinal.saturating_add(1),
                            MAX_PROVIDER_JSONL_LINE_BYTES
                        ),
                    };
                    let bytes = rejection_wire_bytes(&rejection);
                    if physical_records != 0
                        && serialized_bytes.saturating_add(bytes) > PAGE_MAX_BYTES
                    {
                        self.prefix_hasher = hasher_before;
                        self.reader.seek(SeekFrom::Start(start))?;
                        break;
                    }
                    self.complete_prefix_end = end;
                    self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
                    self.rejected_records = self.rejected_records.saturating_add(1);
                    physical_records = physical_records.saturating_add(1);
                    logical_units = logical_units.saturating_add(1);
                    serialized_bytes = serialized_bytes.saturating_add(bytes);
                    rejections.push(rejection);
                    continue;
                }
                Line::Complete { bytes, end } => (bytes, end),
            };

            let projected = self.project_line(&bytes, ordinal, start, end)?;
            if projected.logical_units > PAGE_MAX_RECORDS
                || projected.serialized_bytes > PAGE_MAX_BYTES
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                return Err(CaptureError::InvalidPayload(format!(
                    "{}:{} expands past the OpenClaw NativePath page boundary",
                    self.path.display(),
                    ordinal.saturating_add(1)
                )));
            }
            if physical_records != 0
                && (logical_units.saturating_add(projected.logical_units) > PAGE_MAX_RECORDS
                    || serialized_bytes.saturating_add(projected.serialized_bytes) > PAGE_MAX_BYTES)
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                break;
            }

            self.complete_prefix_end = end;
            self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
            self.accepted_events = self
                .accepted_events
                .saturating_add(projected.events.len() as u64);
            self.accepted_file_touches = self
                .accepted_file_touches
                .saturating_add(projected.touches.len() as u64);
            self.rejected_records = self
                .rejected_records
                .saturating_add(projected.rejections.len() as u64);
            physical_records = physical_records.saturating_add(1);
            logical_units = logical_units.saturating_add(projected.logical_units.max(1));
            serialized_bytes = serialized_bytes.saturating_add(projected.serialized_bytes);
            events.extend(projected.events);
            touches.extend(projected.touches);
            rejections.extend(projected.rejections);
        }

        if physical_records == 0 {
            return Ok(None);
        }
        let terminal = self.finished
            && self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.checkpoint.terminal);
        Ok(Some(Page {
            expected_checkpoint,
            next_checkpoint: self.checkpoint(terminal),
            session: self.session.clone(),
            events,
            touches,
            rejections,
            terminal,
        }))
    }

    pub(super) fn project_line(
        &mut self,
        bytes: &[u8],
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> Result<ProjectedLine> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(ProjectedLine::default());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Ok(ProjectedLine::rejection(Rejection {
                    reason: format!(
                        "{}:{} malformed OpenClaw JSONL: {error}",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
        };
        if !value.is_object() {
            return Ok(ProjectedLine::rejection(Rejection {
                reason: format!(
                    "{}:{} OpenClaw JSONL record must be a JSON object",
                    self.path.display(),
                    ordinal.saturating_add(1)
                ),
            }));
        }
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw NativePath line number exceeds platform limits",
            ))?;
        if value.get("type").and_then(Value::as_str) == Some("session") {
            self.update_header(&value, byte_start, byte_end_exclusive, bytes);
            return Ok(ProjectedLine {
                logical_units: 1,
                serialized_bytes: session_wire_bytes(&self.session),
                ..ProjectedLine::default()
            });
        }

        let occurred_at =
            provider_timestamp_value(value.get("timestamp"), self.session.state.started_at);
        let mut touches = Vec::new();
        visit_all_file_touch_drafts(&value, |draft| {
            touches.push(CoreTouch {
                event_ordinal: None,
                path: draft.path,
            });
            Ok::<(), CaptureError>(())
        })?;

        if let Some(output_metadata) = openclaw_output_metadata(&value) {
            let retained_failure = matches!(
                output_metadata.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            );
            let mut projected = ProjectedLine {
                touches,
                ..ProjectedLine::default()
            };
            if retained_failure {
                let mut event =
                    normalization::event_fact(ordinal, line_number, &value, occurred_at);
                if output_metadata.kind == OutputObservationKind::Command {
                    event.event_type = EventType::CommandOutput;
                }
                projected.events.push(core_event(
                    ordinal,
                    event,
                    byte_start,
                    byte_end_exclusive,
                    bytes,
                ));
                for touch in &mut projected.touches {
                    touch.event_ordinal = Some(ordinal);
                }
            }
            projected.recompute();
            return Ok(projected);
        }

        let event = normalization::event_fact(ordinal, line_number, &value, occurred_at);
        for touch in &mut touches {
            touch.event_ordinal = Some(ordinal);
        }
        let mut projected = ProjectedLine {
            events: vec![core_event(
                ordinal,
                event,
                byte_start,
                byte_end_exclusive,
                bytes,
            )],
            touches,
            ..ProjectedLine::default()
        };
        projected.recompute();
        Ok(projected)
    }

    pub(super) fn update_header(&mut self, value: &Value, start: u64, end: u64, bytes: &[u8]) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.session.state.provider_session_id =
                qualify_session_id(self.session.state.agent_id.as_deref(), id);
        }
        self.session.state.started_at =
            provider_timestamp_value(value.get("timestamp"), self.imported_at);
        self.session.state.cwd = value.get("cwd").and_then(Value::as_str).map(capped_text);
        self.session.state.header_anchor = Some(HeaderAnchor {
            start,
            end,
            digest: header_digest(bytes),
        });
        self.session.header = provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS);
    }

    pub(super) fn checkpoint(&self, terminal: bool) -> Checkpoint {
        Checkpoint {
            version: CHECKPOINT_VERSION,
            parser_revision: PARSER_REVISION,
            policy_revision: POLICY_REVISION,
            generation: self.generation,
            source_path: self.path.clone(),
            source_observation: SourceObservation::from_live(&self.observation),
            route_source_revision: self.source_revision.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            next_raw_ordinal: self.next_raw_ordinal,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
            session: self.session.state.clone(),
            terminal,
        }
    }

    pub(super) fn finish(&mut self, terminal: bool) -> Result<()> {
        self.admitted_transcript.revalidate()?;
        self.outcome = Some(ScanOutcome {
            checkpoint: self.checkpoint(terminal),
        });
        self.finished = true;
        Ok(())
    }

    pub(super) fn revalidate_admitted_transcript(&self) -> Result<()> {
        self.admitted_transcript.revalidate()
    }
}

#[derive(Default)]
pub(super) struct ProjectedLine {
    pub(super) events: Vec<CoreEvent>,
    pub(super) touches: Vec<CoreTouch>,
    pub(super) rejections: Vec<Rejection>,
    pub(super) logical_units: usize,
    pub(super) serialized_bytes: usize,
}

impl ProjectedLine {
    pub(super) fn rejection(rejection: Rejection) -> Self {
        Self {
            serialized_bytes: rejection_wire_bytes(&rejection),
            logical_units: 1,
            rejections: vec![rejection],
            ..Self::default()
        }
    }

    pub(super) fn recompute(&mut self) {
        self.logical_units = self
            .events
            .len()
            .saturating_add(self.touches.len())
            .saturating_add(self.rejections.len())
            .max(1);
        self.serialized_bytes = self
            .events
            .iter()
            .map(event_wire_bytes)
            .chain(self.touches.iter().map(touch_wire_bytes))
            .chain(self.rejections.iter().map(rejection_wire_bytes))
            .fold(0_usize, usize::saturating_add);
    }
}

pub(super) fn core_event(
    raw_ordinal: u64,
    event: normalization::OpenClawEventFact,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_bytes: &[u8],
) -> CoreEvent {
    let native_record_id = event.provider_event_hash.clone();
    CoreEvent {
        raw_ordinal,
        native_record_id,
        byte_start,
        byte_end_exclusive,
        record_digest: Sha256::digest(record_bytes).into(),
        provider_event_sequence_index: event.provider_event_index,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        lexical_text: event.lexical_text,
    }
}

pub(super) fn fresh_session(path: &Path, imported_at: DateTime<Utc>, index: &Value) -> SessionFact {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    let agent_id = openclaw_agent_id(path).map(|value| capped_text(&value));
    let provider_session_id = qualify_session_id(agent_id.as_deref(), fallback_id);
    let parent_provider_session_id = related_session_id(
        index,
        agent_id.as_deref(),
        &["parentSessionId", "parent_session_id"],
    );
    let root_provider_session_id = related_session_id(
        index,
        agent_id.as_deref(),
        &["rootSessionId", "root_session_id"],
    )
    .or_else(|| parent_provider_session_id.clone());
    SessionFact {
        state: SessionState {
            provider_session_id,
            agent_id,
            parent_provider_session_id,
            root_provider_session_id,
            started_at: imported_at,
            cwd: None,
            header_anchor: None,
        },
        index: index.clone(),
        header: Value::Null,
    }
}

pub(super) fn resume_session(
    checkpoint: &Checkpoint,
    observation: &OpenClawSessionObservation,
) -> Result<SessionFact> {
    let header = bootstrap_header(
        &checkpoint.source_path,
        checkpoint.session.header_anchor,
        observation,
    )?;
    Ok(SessionFact {
        state: checkpoint.session.clone(),
        index: observation.index.clone(),
        header,
    })
}

pub(super) fn bootstrap_header(
    path: &Path,
    anchor: Option<HeaderAnchor>,
    observation: &OpenClawSessionObservation,
) -> Result<Value> {
    let Some(anchor) = anchor else {
        return Ok(Value::Null);
    };
    let length = anchor
        .end
        .checked_sub(anchor.start)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw checkpoint header range is invalid",
        ))?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    if length > maximum || anchor.end > observation.transcript.length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let length = usize::try_from(length).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw checkpoint header range exceeds platform limits".to_owned(),
        )
    })?;
    let mut file = File::open(path)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if header_digest(&bytes) != anchor.digest {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let header: Value = serde_json::from_slice(&bytes)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(provider_capped_json(&header, PROVIDER_MAX_PREVIEW_CHARS))
}

pub(super) fn related_session_id(
    index: &Value,
    agent_id: Option<&str>,
    fields: &[&str],
) -> Option<String> {
    fields
        .iter()
        .find_map(|field| index.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| qualify_session_id(agent_id, value))
}

pub(super) fn qualify_session_id(agent_id: Option<&str>, session_id: &str) -> String {
    let session_id = capped_text(session_id);
    match agent_id {
        Some(agent_id) if !session_id.contains('/') => format!("{agent_id}/{session_id}"),
        _ => session_id,
    }
}

pub(super) fn capped_text(value: &str) -> String {
    provider_local_preview(value, crate::PROVIDER_MAX_TEXT_CHARS).0
}

pub(super) fn header_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-openclaw-header-anchor-sha256-v1\0");
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

pub(super) enum Line {
    EndOfFile,
    IncompleteTail,
    Oversized { end: u64 },
    Complete { bytes: Vec<u8>, end: u64 },
}

pub(super) fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<Line> {
    if start >= frozen_length {
        return Ok(Line::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                Line::EndOfFile
            } else {
                Line::IncompleteTail
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        total = total.saturating_add(chunk.len() as u64);
        if !oversized {
            if bytes.len().saturating_add(chunk.len())
                > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let complete = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if complete {
            let end = start.saturating_add(total);
            if oversized {
                return Ok(Line::Oversized { end });
            }
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Line::Complete { bytes, end });
        }
    }
}

pub(super) fn file_continuity(
    previous: &FrozenMetadata,
    current: &OpenClawFrozenFileMetadata,
) -> Result<FileContinuity> {
    let previous = previous.to_live()?;
    Ok(
        match (
            previous.device,
            previous.inode,
            current.device,
            current.inode,
        ) {
            (Some(previous_device), Some(previous_inode), Some(device), Some(inode))
                if previous_device == device && previous_inode == inode =>
            {
                FileContinuity::SamePhysicalFile
            }
            (Some(_), Some(_), Some(_), Some(_)) => FileContinuity::Replacement,
            _ => FileContinuity::ExactPathPrefixProof,
        },
    )
}

pub(super) fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX_HASH_DOMAIN);
    hasher
}

pub(super) fn hash_prefix(file: &mut File, length: u64, mut hasher: Sha256) -> Result<Sha256> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("OpenClaw prefix read length exceeds usize")
        })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}

pub(super) fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

pub(super) fn session_wire_bytes(session: &SessionFact) -> usize {
    512_usize
        .saturating_add(session.state.provider_session_id.len())
        .saturating_add(session.state.agent_id.as_deref().map_or(0, str::len))
        .saturating_add(
            session
                .state
                .parent_provider_session_id
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(
            session
                .state
                .root_provider_session_id
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(session.state.cwd.as_deref().map_or(0, str::len))
        .saturating_add(serde_json::to_vec(&session.index).map_or(usize::MAX, |v| v.len()))
        .saturating_add(serde_json::to_vec(&session.header).map_or(usize::MAX, |v| v.len()))
}

pub(super) fn event_wire_bytes(event: &CoreEvent) -> usize {
    EVENT_ENVELOPE_BYTES
        .saturating_add(64)
        .saturating_add(event.lexical_text.len())
}

pub(super) fn touch_wire_bytes(touch: &CoreTouch) -> usize {
    256_usize.saturating_add(touch.path.len())
}

pub(super) fn rejection_wire_bytes(rejection: &Rejection) -> usize {
    128_usize.saturating_add(rejection.reason.len())
}
