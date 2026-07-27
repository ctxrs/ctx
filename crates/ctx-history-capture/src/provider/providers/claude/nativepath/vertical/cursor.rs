use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClaudeRetirementFrontier {
    kind: String,
    id: Uuid,
}

impl ClaudeRetirementFrontier {
    pub(super) fn from_store(value: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: value.kind.as_str().to_owned(),
            id: value.id,
        }
    }

    pub(super) fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "Claude NativePath retirement frontier is invalid".to_owned(),
                ))
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ClaudeGenerationPhase {
    #[default]
    Live,
    Staging,
    Retiring {
        after: Option<ClaudeRetirementFrontier>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClaudeStoreCursor {
    pub(super) version: u32,
    pub(super) source_generation: u64,
    pub(super) source_id: Uuid,
    pub(super) checkpoint: ParseCheckpoint,
    pub(super) session: ClaudeSessionMetadata,
    pub(super) accepted_rows: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    #[serde(default)]
    pub(super) generation_phase: ClaudeGenerationPhase,
    #[serde(default)]
    pub(super) generation_source_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedClaudeParserCheckpoint {
    pub(super) session: Option<ReleasedClaudeSessionCheckpoint>,
    pub(super) next_ordinal: u64,
    pub(super) accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReleasedClaudeSessionCheckpoint {
    native_session_id: String,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    pub(super) version: Option<String>,
    git_branch: Option<String>,
}

// Both variants are persisted compatibility shapes; boxing would add allocation
// to every cursor decode for a release-time size-only concern.
#[allow(clippy::large_enum_variant)]
pub(super) enum ClaudeStoredCursor {
    Native(ClaudeStoreCursor),
    Released(String),
}

pub(super) fn next_cursor_state(
    source: &DiscoveredClaudeSession,
    previous: Option<&ClaudeStoreCursor>,
    page: &ClaudeNativePage,
    checkpoint: ParseCheckpoint,
    source_revision: &str,
) -> ClaudeStoreCursor {
    let reset = page.expected_frontier.complete_offset == 0;
    let source_generation = if reset {
        previous.map_or(0, |cursor| cursor.source_generation.saturating_add(1))
    } else {
        previous.map_or(0, |cursor| cursor.source_generation)
    };
    let source_id =
        previous.map_or_else(|| stable_route_source_id(source), |cursor| cursor.source_id);
    let generation_in_progress = (reset && previous.is_some())
        || previous.is_some_and(|cursor| {
            matches!(cursor.generation_phase, ClaudeGenerationPhase::Staging)
        });
    let generation_phase = if generation_in_progress {
        if page.terminal {
            ClaudeGenerationPhase::Retiring { after: None }
        } else {
            ClaudeGenerationPhase::Staging
        }
    } else {
        ClaudeGenerationPhase::Live
    };
    let generation_source_revision = match &generation_phase {
        ClaudeGenerationPhase::Live => None,
        ClaudeGenerationPhase::Staging | ClaudeGenerationPhase::Retiring { .. } => previous
            .filter(|_| !reset)
            .and_then(|cursor| cursor.generation_source_revision.clone())
            .or_else(|| Some(source_revision.to_owned())),
    };
    let accepted_rows = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.accepted_rows)
    }
    .saturating_add(u64::try_from(page.rows.len()).unwrap_or(u64::MAX));
    let accepted_file_touches = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.accepted_file_touches)
    }
    .saturating_add(
        page.rows
            .iter()
            .filter_map(|row| row.tool_call.as_ref())
            .map(|call| u64::try_from(call.file_touches.len()).unwrap_or(u64::MAX))
            .sum::<u64>(),
    );
    let rejected_records = if reset {
        0
    } else {
        previous.map_or(0, |cursor| cursor.rejected_records)
    }
    .saturating_add(page.rejected_records);
    ClaudeStoreCursor {
        version: CLAUDE_STORE_CURSOR_VERSION,
        source_generation,
        source_id,
        checkpoint,
        session: page.session.clone(),
        accepted_rows,
        accepted_file_touches,
        rejected_records,
        generation_phase,
        generation_source_revision,
    }
}

pub(super) fn encode_store_cursor(cursor: &ClaudeStoreCursor) -> Result<String> {
    Ok(serde_json::to_string(cursor)?)
}

pub(super) fn decode_store_cursor(cursor: &str) -> Result<ClaudeStoredCursor> {
    let provider = decode_native_path_committed_cursor(cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| cursor.to_owned());
    if let Ok(decoded) = serde_json::from_str::<ClaudeStoreCursor>(&provider) {
        if decoded.version != CLAUDE_STORE_CURSOR_VERSION {
            return Err(CaptureError::InvalidPayload(
                "unsupported Claude NativePath Store cursor".to_owned(),
            ));
        }
        return Ok(ClaudeStoredCursor::Native(decoded));
    }
    validate_released_cursor(&provider)?;
    Ok(ClaudeStoredCursor::Released(provider))
}

fn validate_released_cursor(encoded: &str) -> Result<()> {
    let cursor = CertifiedProviderCursor::decode_if_certified(encoded)?.ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Claude cursor is neither NativePath nor a released certified cursor".to_owned(),
        )
    })?;
    if cursor.parser_revision() != CLAUDE_RELEASED_CAPTURE_REVISION
        || cursor.policy_revision() != CLAUDE_RELEASED_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Claude released cursor has unsupported revisions".to_owned(),
        ));
    }
    crate::released_jsonl_cursor::released_jsonl_position_offset(cursor.native_position())
        .map_err(|_| {
            CaptureError::InvalidPayload("Claude released cursor position is malformed".to_owned())
        })?;
    let checkpoint: ReleasedClaudeParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
    validate_released_checkpoint(&checkpoint)
}

fn validate_released_checkpoint(checkpoint: &ReleasedClaudeParserCheckpoint) -> Result<()> {
    if checkpoint.accepted_captures > checkpoint.next_ordinal
        || checkpoint.accepted_events > checkpoint.accepted_captures
        || checkpoint.rejected_records > checkpoint.next_ordinal
    {
        return Err(CaptureError::InvalidPayload(
            "Claude released cursor checkpoint counters are inconsistent".to_owned(),
        ));
    }
    if let Some(session) = &checkpoint.session {
        if session.native_session_id.trim().is_empty()
            || session.provider_session_id.trim().is_empty()
        {
            return Err(CaptureError::InvalidPayload(
                "Claude released cursor session identity is empty".to_owned(),
            ));
        }
        let _ = (
            &session.parent_provider_session_id,
            &session.external_agent_id,
            session.is_subagent,
            session.started_at,
            &session.cwd,
            &session.version,
            &session.git_branch,
        );
    }
    let _ = checkpoint.accepted_file_touches;
    Ok(())
}

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Claude.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

pub(super) fn source_revision(source: &DiscoveredClaudeSession, token: Option<&str>) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-claude-nativepath-source-revision-v1\0");
    digest.update(source.fingerprint.observation_sha256());
    if let Some(token) = token {
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    }
    format!("claude-nativepath-sha256-v1:{:x}", digest.finalize())
}
