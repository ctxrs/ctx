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

pub(super) enum CursorOrigin {
    Fresh,
    Native {
        stored: SyncCursor,
        cursor: JunieStoreCursor,
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
    let committed =
        decode_native_path_committed_cursor(&stored.cursor).map_err(CaptureError::Store)?;
    let cursor = JunieStoreCursor::decode(committed.provider_cursor()).map_err(|_| {
        CaptureError::SystemInvariant("Junie persisted NativePath cursor is corrupt")
    })?;
    if cursor.source_identity != source_identity {
        return Err(CaptureError::SystemInvariant(
            "Junie persisted NativePath cursor belongs to another source",
        ));
    }
    Ok(CursorOrigin::Native { stored, cursor })
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

pub(super) struct CursorPlan {
    pub(super) expected: Option<String>,
    pub(super) cursor: JunieStoreCursor,
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
        }),
        CursorOrigin::Native { stored, mut cursor } => {
            let same_physical = cursor.device == observation.events_file.device
                && cursor.inode == observation.events_file.inode;
            let (prefix_boundary, expected_prefix) = cursor.frontier.pending.as_ref().map_or(
                (cursor.frontier.offset, cursor.frontier.prefix_sha256),
                |pending| (pending.end_offset, pending.after_prefix_sha256),
            );
            let prefix_matches = observation.events_file.length >= prefix_boundary
                && hash_prefix(path, prefix_boundary)? == expected_prefix;
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
            })
        }
    }
}

pub(super) fn hash_prefix(session_path: &JunieSessionPath, length: u64) -> Result<[u8; 32]> {
    let opened = session_path.open_events()?;
    if opened.len() < length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut file = opened.file().try_clone()?;
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
    opened.revalidate()?;
    Ok(digest.finalize().into())
}
