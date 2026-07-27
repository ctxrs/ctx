use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeState {
    pub(super) started_at_ms: i64,
    pub(super) last_ts_ms: i64,
    pub(super) ended_at_ms: Option<i64>,
    pub(super) title: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) saw_supported_event: bool,
}

impl RuntimeState {
    pub(super) fn fresh(meta: &JunieIndexMeta, imported_at: DateTime<Utc>) -> Self {
        let started_at = provider_timestamp_millis(meta.created_at, imported_at);
        Self {
            started_at_ms: started_at.timestamp_millis(),
            last_ts_ms: started_at.timestamp_millis(),
            ended_at_ms: meta
                .updated_at
                .map(|value| provider_timestamp_millis(Some(value), started_at).timestamp_millis()),
            title: meta.task_name.clone(),
            cwd: meta.project_dir.clone(),
            saw_supported_event: false,
        }
    }

    pub(super) fn started_at(&self) -> DateTime<Utc> {
        timestamp(self.started_at_ms)
    }

    pub(super) fn last_ts(&self) -> DateTime<Utc> {
        timestamp(self.last_ts_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingTurn {
    pub(super) start_offset: u64,
    pub(super) end_offset: u64,
    pub(super) start_ordinal: u64,
    pub(super) end_ordinal: u64,
    pub(super) base_event_index: u64,
    pub(super) next_event_index: u64,
    pub(super) next_row: u32,
    pub(super) row_count: u32,
    pub(super) turn_sha256: [u8; 32],
    pub(super) terminal: bool,
    pub(super) after_state: RuntimeState,
    pub(super) after_prefix_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Frontier {
    pub(super) offset: u64,
    pub(super) next_ordinal: u64,
    pub(super) next_event_index: u64,
    pub(super) prefix_sha256: [u8; 32],
    pub(super) state: RuntimeState,
    pub(super) pending: Option<PendingTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JunieStoreCursor {
    pub(super) version: u32,
    pub(super) provider: String,
    pub(super) source_identity: String,
    pub(super) source_revision: String,
    pub(super) observed_length: u64,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    pub(super) generation: u64,
    pub(super) terminal: bool,
    pub(super) retired: bool,
    pub(super) rejected_records: u64,
    pub(super) frontier: Frontier,
}

impl JunieStoreCursor {
    pub(super) fn encode(&self) -> Result<String> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie NativePath cursor exceeds its provider-local bound".to_owned(),
            ));
        }
        Ok(encoded)
    }

    pub(super) fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != CURSOR_VERSION
            || cursor.provider != CaptureProvider::Junie.as_str()
            || cursor.source_identity.is_empty()
            || !valid_junie_source_revision(&cursor.source_revision)
            || cursor.frontier.offset > cursor.observed_length
            || cursor.frontier.next_event_index > GENERATION_EVENT_STRIDE
            || (cursor.terminal
                && (cursor.frontier.pending.is_some()
                    || cursor.frontier.offset != cursor.observed_length))
            || cursor.frontier.pending.as_ref().is_some_and(|pending| {
                pending.start_offset != cursor.frontier.offset
                    || pending.start_ordinal != cursor.frontier.next_ordinal
                    || pending.base_event_index != cursor.frontier.next_event_index
                    || pending.next_event_index < pending.base_event_index
                    || pending.start_offset >= pending.end_offset
                    || pending.next_row > pending.row_count
            })
        {
            return Err(CaptureError::InvalidPayload(
                "Junie NativePath cursor is malformed or inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedJunieCheckpointFailure {
    #[serde(rename = "line")]
    pub(super) _line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedJunieMetadataAnchor {
    pub(super) start: u64,
    pub(super) end: u64,
    pub(super) sha256: [u8; 32],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedJunieParserCheckpoint {
    pub(super) next_ordinal: u64,
    #[serde(rename = "next_line_number")]
    pub(super) _next_line_number: u64,
    pub(super) provider_event_index: u64,
    pub(super) started_at: DateTime<Utc>,
    pub(super) last_ts: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) title_anchor: Option<ReleasedJunieMetadataAnchor>,
    pub(super) cwd_anchor: Option<ReleasedJunieMetadataAnchor>,
    pub(super) saw_supported_event: bool,
    #[serde(rename = "metadata_dirty")]
    pub(super) _metadata_dirty: bool,
    pub(super) source_ended: bool,
    #[serde(rename = "auxiliary_revision")]
    pub(super) _auxiliary_revision: u64,
    #[serde(rename = "accepted_captures")]
    pub(super) _accepted_captures: u64,
    pub(super) accepted_events: u64,
    #[serde(rename = "accepted_file_touches")]
    pub(super) _accepted_file_touches: u64,
    pub(super) structural_rejections: u64,
    pub(super) rejected_records: u64,
    pub(super) failures: Vec<ReleasedJunieCheckpointFailure>,
}

#[derive(Clone)]
pub(super) struct ReleasedJunieCursor {
    pub(super) source_revision: String,
    pub(super) native_position: crate::native_source::NativePosition,
    pub(super) checkpoint: ReleasedJunieParserCheckpoint,
    pub(super) rejected_records: u64,
}

// Keep the decoded native cursor inline with its stored sync cursor: this short-lived
// planning value mirrors the persisted cursor variants and is consumed immediately.
#[allow(clippy::large_enum_variant)]
pub(super) enum CursorOrigin {
    Fresh,
    Native {
        stored: SyncCursor,
        cursor: JunieStoreCursor,
    },
    Legacy {
        stored: SyncCursor,
        cursor: ReleasedJunieCursor,
    },
}

pub(super) fn load_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
    source_identity: &str,
) -> Result<CursorOrigin> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(CursorOrigin::Fresh);
    };
    match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => {
            let cursor = JunieStoreCursor::decode(committed.provider_cursor()).map_err(|_| {
                CaptureError::SystemInvariant("Junie persisted NativePath cursor is corrupt")
            })?;
            if cursor.source_identity != source_identity {
                return Err(CaptureError::SystemInvariant(
                    "Junie persisted NativePath cursor belongs to another source",
                ));
            }
            return Ok(CursorOrigin::Native { stored, cursor });
        }
        Err(error) if looks_like_native_path_cursor(&stored.cursor) => {
            return Err(CaptureError::Store(error));
        }
        Err(_) => {}
    }
    let legacy = CertifiedProviderCursor::decode_if_certified(&stored.cursor)
        .map_err(|_| CaptureError::SystemInvariant("Junie persisted released cursor is corrupt"))?
        .ok_or(CaptureError::SystemInvariant(
            "Junie persisted cursor has an unknown encoding",
        ))?;
    if legacy.parser_revision() != 2 || legacy.policy_revision() != 5 {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor has unsupported revisions",
        ));
    }
    if !valid_junie_source_revision(legacy.source_revision()) {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor has an invalid source revision",
        ));
    }
    let checkpoint = legacy
        .parser_checkpoint()
        .deserialize::<ReleasedJunieParserCheckpoint>()
        .map_err(|_| {
            CaptureError::SystemInvariant("Junie persisted released cursor checkpoint is corrupt")
        })?;
    let offset = released_jsonl_position_offset(legacy.native_position()).ok_or(
        CaptureError::SystemInvariant("Junie persisted released cursor position is corrupt"),
    )?;
    validate_released_checkpoint(&checkpoint, offset)?;
    Ok(CursorOrigin::Legacy {
        stored,
        cursor: ReleasedJunieCursor {
            source_revision: legacy.source_revision().to_owned(),
            native_position: legacy.native_position().clone(),
            rejected_records: legacy.rejected_records(),
            checkpoint,
        },
    })
}

pub(super) fn looks_like_native_path_cursor(encoded: &str) -> bool {
    serde_json::from_str::<Value>(encoded)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .is_some_and(|object| {
            object.contains_key("publication_id") || object.contains_key("provider_cursor")
        })
}

pub(super) fn valid_junie_source_revision(revision: &str) -> bool {
    let Some(revision) = revision.strip_prefix(super::super::JUNIE_SOURCE_REVISION_SCHEMA) else {
        return false;
    };
    let Some(digest) = revision.strip_prefix(":fnv1a64:") else {
        return false;
    };
    digest.len() == 16
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn released_jsonl_position_offset(
    position: &crate::native_source::NativePosition,
) -> Option<u64> {
    // Exact released `jsonl-byte-boundary-v1` wire layout: fixed header,
    // offset, canonical suffix-proof length, and SHA-256 boundary proof.
    let value = position.value();
    if position.kind() != "jsonl-byte-boundary-v1"
        || value.len() != 56
        || value.get(..8) != Some(b"CTXJLBP\0")
        || value.get(8..12) != Some(&[1, 1, 0, 0])
    {
        return None;
    }
    let offset = u64::from_be_bytes(value.get(12..20)?.try_into().ok()?);
    let proof_len = u32::from_be_bytes(value.get(20..24)?.try_into().ok()?);
    (u64::from(proof_len) == offset.min(64 * 1024)).then_some(offset)
}

pub(super) fn released_jsonl_prefix_is_proven(
    path: &Path,
    position: &crate::native_source::NativePosition,
) -> Result<bool> {
    let Some(offset) = released_jsonl_position_offset(position) else {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor position is corrupt",
        ));
    };
    let proof_len = offset.min(64 * 1024);
    if fs::metadata(path)?.len() < offset {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset.saturating_sub(proof_len)))?;
    let proof_len_usize = usize::try_from(proof_len)
        .map_err(|_| CaptureError::SystemInvariant("Junie released cursor proof exceeds usize"))?;
    let mut proof = vec![0_u8; proof_len_usize];
    file.read_exact(&mut proof)?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-jsonl-append-boundary-sha256-v1\0");
    digest.update(offset.to_be_bytes());
    let proof_len = u32::try_from(proof_len).map_err(|_| {
        CaptureError::SystemInvariant("Junie released cursor proof length exceeds u32")
    })?;
    digest.update(proof_len.to_be_bytes());
    digest.update(&proof);
    let expected: [u8; 32] = digest.finalize().into();
    Ok(position.value().get(24..56) == Some(expected.as_slice()))
}

pub(super) fn validate_released_checkpoint(
    checkpoint: &ReleasedJunieParserCheckpoint,
    offset: u64,
) -> Result<()> {
    let anchors_valid = [&checkpoint.title_anchor, &checkpoint.cwd_anchor]
        .into_iter()
        .flatten()
        .all(|anchor| {
            anchor.start < anchor.end
                && anchor.end <= offset
                && anchor.end.saturating_sub(anchor.start)
                    <= crate::MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) as u64
        });
    let bounded_failures = checkpoint.failures.len() <= MAX_JUNIE_FAILURES
        && checkpoint
            .failures
            .iter()
            .all(|failure| failure.error.len() <= MAX_JUNIE_FAILURE_BYTES)
        && u64::try_from(checkpoint.failures.len()).unwrap_or(u64::MAX)
            <= checkpoint.rejected_records;
    let counters_valid = checkpoint.provider_event_index <= GENERATION_EVENT_STRIDE
        && checkpoint.accepted_events <= checkpoint.provider_event_index
        && checkpoint.structural_rejections <= checkpoint.rejected_records;
    if !anchors_valid || !bounded_failures || !counters_valid {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor checkpoint violates bounded state",
        ));
    }
    Ok(())
}

pub(super) fn released_cursor_for_retirement(
    source_identity: &str,
    legacy: CertifiedProviderCursor,
) -> Result<JunieStoreCursor> {
    if legacy.parser_revision() != 2 || legacy.policy_revision() != 5 {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor has unsupported revisions",
        ));
    }
    if !valid_junie_source_revision(legacy.source_revision()) {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted released cursor has an invalid source revision",
        ));
    }
    let checkpoint = legacy
        .parser_checkpoint()
        .deserialize::<ReleasedJunieParserCheckpoint>()
        .map_err(|_| {
            CaptureError::SystemInvariant("Junie persisted released cursor checkpoint is corrupt")
        })?;
    let offset = released_jsonl_position_offset(legacy.native_position()).ok_or(
        CaptureError::SystemInvariant("Junie persisted released cursor position is corrupt"),
    )?;
    validate_released_checkpoint(&checkpoint, offset)?;
    Ok(JunieStoreCursor {
        version: CURSOR_VERSION,
        provider: CaptureProvider::Junie.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: legacy.source_revision().to_owned(),
        observed_length: offset,
        device: None,
        inode: None,
        generation: 0,
        terminal: true,
        retired: false,
        rejected_records: legacy.rejected_records().max(checkpoint.rejected_records),
        frontier: Frontier {
            offset,
            next_ordinal: checkpoint.next_ordinal,
            next_event_index: checkpoint.provider_event_index,
            prefix_sha256: Sha256::digest([]).into(),
            state: RuntimeState {
                started_at_ms: checkpoint.started_at.timestamp_millis(),
                last_ts_ms: checkpoint.last_ts.timestamp_millis(),
                ended_at_ms: checkpoint.ended_at.map(|value| value.timestamp_millis()),
                title: None,
                cwd: None,
                saw_supported_event: checkpoint.saw_supported_event,
            },
            pending: None,
        },
    })
}

pub(super) fn released_anchor_value(
    path: &Path,
    anchor: Option<&ReleasedJunieMetadataAnchor>,
    expected_kind: &'static str,
    field: &'static str,
) -> Result<Option<String>> {
    let Some(anchor) = anchor else {
        return Ok(None);
    };
    let length = usize::try_from(anchor.end.saturating_sub(anchor.start)).map_err(|_| {
        CaptureError::SystemInvariant("Junie released metadata anchor exceeds usize")
    })?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if <[u8; 32]>::from(Sha256::digest(&bytes)) != anchor.sha256 {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(agent_event) = value
        .get("event")
        .and_then(|event| event.get("agentEvent"))
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some(expected_kind))
    else {
        return Ok(None);
    };
    Ok(agent_event
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned))
}

pub(super) struct CursorPlan {
    pub(super) expected: Option<String>,
    pub(super) cursor: JunieStoreCursor,
    // Even a terminal released cursor must cross the Store-owned publication
    // boundary once so the durable cursor is upgraded to its native envelope.
    pub(super) force_publication: bool,
}

pub(super) fn plan_cursor(
    path: &JunieSessionPath,
    observation: &JunieSessionObservation,
    source_identity: &str,
    imported_at: DateTime<Utc>,
    origin: CursorOrigin,
) -> Result<CursorPlan> {
    let fresh = || JunieStoreCursor {
        version: CURSOR_VERSION,
        provider: CaptureProvider::Junie.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: observation.source_revision(),
        observed_length: observation.events_file.length,
        device: observation.events_file.device,
        inode: observation.events_file.inode,
        generation: 0,
        terminal: false,
        retired: false,
        rejected_records: 0,
        frontier: Frontier {
            offset: 0,
            next_ordinal: 0,
            next_event_index: 0,
            prefix_sha256: Sha256::digest([]).into(),
            state: RuntimeState::fresh(&bounded_junie_index_meta(&path.index_meta), imported_at),
            pending: None,
        },
    };
    match origin {
        CursorOrigin::Fresh => Ok(CursorPlan {
            expected: None,
            cursor: fresh(),
            force_publication: false,
        }),
        CursorOrigin::Legacy { stored, cursor } => {
            let offset = released_jsonl_position_offset(&cursor.native_position).ok_or(
                CaptureError::SystemInvariant(
                    "Junie persisted released cursor position is corrupt",
                ),
            )?;
            let revision_matches = cursor.source_revision == observation.source_revision();
            let prefix_is_proven =
                released_jsonl_prefix_is_proven(&path.events_path, &cursor.native_position)?;
            // The empty boundary proves no consumed bytes. A changed source
            // revision therefore needs a replacement generation.
            if !prefix_is_proven || (offset == 0 && !revision_matches) {
                let mut reset = fresh();
                reset.generation = 1;
                return Ok(CursorPlan {
                    expected: Some(stored.cursor),
                    cursor: reset,
                    force_publication: false,
                });
            }
            let meta = bounded_junie_index_meta(&path.index_meta);
            let checkpoint = cursor.checkpoint;
            let title = match checkpoint.title_anchor.as_ref() {
                Some(anchor) => released_anchor_value(
                    &path.events_path,
                    Some(anchor),
                    "AgentTaskNameUpdatedEvent",
                    "name",
                )?,
                None => meta.task_name,
            };
            let cwd = match checkpoint.cwd_anchor.as_ref() {
                Some(anchor) => released_anchor_value(
                    &path.events_path,
                    Some(anchor),
                    "CurrentDirectoryUpdatedEvent",
                    "currentDirectory",
                )?,
                None => meta.project_dir,
            };
            if (checkpoint.title_anchor.is_some() && title.is_none())
                || (checkpoint.cwd_anchor.is_some() && cwd.is_none())
            {
                let mut reset = fresh();
                reset.generation = 1;
                return Ok(CursorPlan {
                    expected: Some(stored.cursor),
                    cursor: reset,
                    force_publication: false,
                });
            }
            let terminal = checkpoint.source_ended && offset == observation.events_file.length;
            Ok(CursorPlan {
                expected: Some(stored.cursor),
                cursor: JunieStoreCursor {
                    version: CURSOR_VERSION,
                    provider: CaptureProvider::Junie.as_str().to_owned(),
                    source_identity: source_identity.to_owned(),
                    source_revision: observation.source_revision(),
                    observed_length: observation.events_file.length,
                    device: observation.events_file.device,
                    inode: observation.events_file.inode,
                    generation: 0,
                    terminal,
                    retired: false,
                    rejected_records: cursor.rejected_records.max(checkpoint.rejected_records),
                    frontier: Frontier {
                        offset,
                        next_ordinal: checkpoint.next_ordinal,
                        next_event_index: checkpoint.provider_event_index,
                        prefix_sha256: hash_prefix(&path.events_path, offset)?,
                        state: RuntimeState {
                            started_at_ms: checkpoint.started_at.timestamp_millis(),
                            last_ts_ms: checkpoint.last_ts.timestamp_millis(),
                            ended_at_ms: checkpoint.ended_at.map(|value| value.timestamp_millis()),
                            title,
                            cwd,
                            saw_supported_event: checkpoint.saw_supported_event,
                        },
                        pending: None,
                    },
                },
                force_publication: true,
            })
        }
        CursorOrigin::Native { stored, mut cursor } => {
            let same_physical = cursor.device == observation.events_file.device
                && cursor.inode == observation.events_file.inode;
            let (prefix_boundary, expected_prefix) = cursor.frontier.pending.as_ref().map_or(
                (cursor.frontier.offset, cursor.frontier.prefix_sha256),
                |pending| (pending.end_offset, pending.after_prefix_sha256),
            );
            let prefix_matches = observation.events_file.length >= prefix_boundary
                && hash_prefix(&path.events_path, prefix_boundary)? == expected_prefix;
            if cursor.retired || !same_physical || !prefix_matches {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Junie source generation exhausted",
                        ))?;
                let mut reset = fresh();
                reset.generation = generation;
                return Ok(CursorPlan {
                    expected: Some(stored.cursor),
                    cursor: reset,
                    force_publication: false,
                });
            }
            cursor.retired = false;
            if cursor.frontier.pending.is_none() {
                let meta = bounded_junie_index_meta(&path.index_meta);
                cursor.frontier.state.title = meta.task_name.or(cursor.frontier.state.title);
                cursor.frontier.state.cwd = meta.project_dir.or(cursor.frontier.state.cwd);
                cursor.frontier.state.ended_at_ms =
                    meta.updated_at.or(cursor.frontier.state.ended_at_ms);
            }
            Ok(CursorPlan {
                expected: Some(stored.cursor),
                cursor,
                force_publication: false,
            })
        }
    }
}

pub(super) fn hash_prefix(path: &Path, length: u64) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut remaining = length;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Junie prefix length exceeds usize"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(digest.finalize().into())
}
