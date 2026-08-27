use std::io::{BufRead, Read};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{
    history::HistoryTurn,
    limits::{check_limit, inspect_json},
    replacement::{
        ReplacementAccumulator, ReplacementChunk, ReplacementCommitted, ReplacementStarted,
    },
    CanonicalState, FxAuthority, FxId, FxProviderError, FxProviderResult, FxWatermark, LogicalTurn,
    PermissionState, RecoveryCheckpoint, ReplacementScratch, ReplayLimits, SessionPreferences,
    UsageSnapshot,
};

pub const EVENT_FRAME_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const AUTHORITY_MAX_BYTES: usize = 16 * 1024;
pub const WATERMARK_MAX_BYTES: usize = 16 * 1024;
pub const REPLAY_CHECKPOINT_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingIntent {
    AuthorityTransition,
    StateReplacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryIntent {
    Stable,
    ProvisionalTail { bytes_after_watermark: u64 },
    TerminalPending(PendingIntent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayCheckpoint {
    pub native_session_id: String,
    pub log_generation: FxId,
    pub next_seq: u64,
    pub through_event_id: FxId,
    pub through_event_log_bytes: u64,
    pub absolute_turn_slots: u64,
    pub current_workspace_root: String,
    pub preferences: SessionPreferences,
}

pub fn encode_replay_checkpoint(checkpoint: &ReplayCheckpoint) -> FxProviderResult<Vec<u8>> {
    validate_checkpoint_metadata(checkpoint)?;
    let encoded = serde_json::to_vec(checkpoint)?;
    check_limit(
        "replay checkpoint bytes",
        encoded.len() as u64,
        REPLAY_CHECKPOINT_MAX_BYTES as u64,
    )?;
    Ok(encoded)
}

pub fn decode_replay_checkpoint(
    bytes: &[u8],
    limits: ReplayLimits,
) -> FxProviderResult<ReplayCheckpoint> {
    check_limit(
        "replay checkpoint bytes",
        bytes.len() as u64,
        REPLAY_CHECKPOINT_MAX_BYTES as u64,
    )?;
    inspect_json(bytes, limits)?;
    let checkpoint: ReplayCheckpoint = serde_json::from_slice(bytes)?;
    validate_checkpoint_metadata(&checkpoint)?;
    Ok(checkpoint)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalReplay {
    pub state: CanonicalState,
    pub checkpoint: ReplayCheckpoint,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendReplay {
    pub new_turns: Vec<LogicalTurn>,
    pub checkpoint: ReplayCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FxFirstEventBinding {
    pub native_session_id: String,
    pub log_generation: FxId,
    pub event_id: FxId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColdReplayDisposition {
    Canonical(Box<CanonicalReplay>),
    UnsafePending(PendingIntent),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SuffixDisposition {
    AppendNewTurns(AppendReplay),
    ReplaceCanonicalState,
    UnsafePending(PendingIntent),
}

pub fn decode_authority(bytes: &[u8], limits: ReplayLimits) -> FxProviderResult<FxAuthority> {
    check_limit(
        "authority bytes",
        bytes.len() as u64,
        AUTHORITY_MAX_BYTES as u64,
    )?;
    inspect_json(bytes, limits)?;
    let authority: FxAuthority = serde_json::from_slice(bytes)?;
    validate_authority(&authority)?;
    Ok(authority)
}

pub fn decode_watermark(bytes: &[u8], limits: ReplayLimits) -> FxProviderResult<FxWatermark> {
    check_limit(
        "watermark bytes",
        bytes.len() as u64,
        WATERMARK_MAX_BYTES as u64,
    )?;
    inspect_json(bytes, limits)?;
    let watermark: FxWatermark = serde_json::from_slice(bytes)?;
    validate_watermark(&watermark)?;
    Ok(watermark)
}

pub fn decode_first_event_binding(
    bytes: &[u8],
    limits: ReplayLimits,
) -> FxProviderResult<FxFirstEventBinding> {
    let envelope = decode_event_envelope(bytes, limits)?;
    if envelope.seq != 1 {
        return Err(FxProviderError::NonContiguousSequence {
            expected: 1,
            actual: envelope.seq,
        });
    }
    let Event::SessionStarted(started) = parse_event(&envelope.kind, envelope.payload.get())?
    else {
        return Err(FxProviderError::InvalidFrame(
            "event log does not begin with session_started",
        ));
    };
    if !crate::dto::validate_session_id(&started.id) {
        return Err(FxProviderError::InvalidState("invalid native session id"));
    }
    Ok(FxFirstEventBinding {
        native_session_id: started.id,
        log_generation: envelope.log_generation,
        event_id: envelope.event_id,
    })
}

pub fn replay_committed<R: BufRead>(
    authority: &FxAuthority,
    watermark: &FxWatermark,
    events: &mut R,
    boundary: BoundaryIntent,
    scratch: &dyn ReplacementScratch,
    limits: ReplayLimits,
) -> FxProviderResult<ColdReplayDisposition> {
    if let BoundaryIntent::TerminalPending(intent) = boundary {
        return Ok(ColdReplayDisposition::UnsafePending(intent));
    }
    validate_authority_pair(authority, watermark, limits)?;
    let mut stream = FrameStream::cold(events, watermark.through_event_log_bytes, limits)?;
    let mut state = None;
    let mut replacement = None;
    while let Some(envelope) = stream.next()? {
        let event = parse_event(&envelope.kind, envelope.payload.get())?;
        apply_cold_replay_event(
            &mut state,
            &mut replacement,
            event,
            &envelope,
            scratch,
            limits,
        )?;
    }
    if replacement.is_some() {
        return Err(FxProviderError::InvalidReplacement(
            "committed replacement is incomplete",
        ));
    }
    stream.validate_watermark(watermark)?;
    let state = state.ok_or(FxProviderError::InvalidState(
        "session_started event is missing",
    ))?;
    if state.id != authority.session_id {
        return Err(FxProviderError::InvalidAuthority(
            "authority session does not match replayed state",
        ));
    }
    crate::limits::validate_canonical_state(&state, limits)?;
    let checkpoint = checkpoint_for(&state, watermark)?;
    Ok(ColdReplayDisposition::Canonical(Box::new(
        CanonicalReplay { state, checkpoint },
    )))
}

pub fn replay_suffix<R: BufRead>(
    authority: &FxAuthority,
    prior: &ReplayCheckpoint,
    watermark: &FxWatermark,
    suffix: &mut R,
    boundary: BoundaryIntent,
    limits: ReplayLimits,
) -> FxProviderResult<SuffixDisposition> {
    if let BoundaryIntent::TerminalPending(intent) = boundary {
        return Ok(SuffixDisposition::UnsafePending(intent));
    }
    validate_authority_pair(authority, watermark, limits)?;
    if prior.native_session_id != authority.session_id
        || prior.log_generation != watermark.log_generation
        || watermark.through_event_log_bytes < prior.through_event_log_bytes
        || watermark.through_seq.saturating_add(1) < prior.next_seq
    {
        return Err(FxProviderError::WatermarkMismatch);
    }
    let suffix_bytes = watermark
        .through_event_log_bytes
        .checked_sub(prior.through_event_log_bytes)
        .ok_or(FxProviderError::WatermarkMismatch)?;
    let mut stream = FrameStream::suffix(
        suffix,
        suffix_bytes,
        prior.log_generation,
        prior.next_seq,
        prior.through_event_log_bytes,
        limits,
    )?;
    let mut workspace_root = prior.current_workspace_root.clone();
    let mut preferences = prior.preferences.clone();
    let mut new_turns = Vec::new();
    let mut next_ordinal = prior.absolute_turn_slots;
    let mut saw_replacement = false;
    while let Some(envelope) = stream.next()? {
        let event = parse_event(&envelope.kind, envelope.payload.get())?;
        if suffix_event_requires_replacement(&event) {
            saw_replacement = true;
            continue;
        }
        if saw_replacement {
            continue;
        }
        apply_suffix_ordinary(
            &mut workspace_root,
            &mut preferences,
            &mut new_turns,
            &mut next_ordinal,
            event,
        )?;
    }
    stream.validate_watermark(watermark)?;
    if saw_replacement {
        return Ok(SuffixDisposition::ReplaceCanonicalState);
    }
    let checkpoint = ReplayCheckpoint {
        native_session_id: prior.native_session_id.clone(),
        log_generation: watermark.log_generation,
        next_seq: watermark
            .through_seq
            .checked_add(1)
            .ok_or(FxProviderError::InvalidWatermark("sequence overflow"))?,
        through_event_id: watermark.through_event_id,
        through_event_log_bytes: watermark.through_event_log_bytes,
        absolute_turn_slots: next_ordinal,
        current_workspace_root: workspace_root,
        preferences,
    };
    Ok(SuffixDisposition::AppendNewTurns(AppendReplay {
        new_turns,
        checkpoint,
    }))
}

pub(crate) fn apply_cold_replay_event(
    state: &mut Option<CanonicalState>,
    replacement: &mut Option<ReplacementAccumulator>,
    event: Event,
    envelope: &EventEnvelope,
    scratch: &dyn ReplacementScratch,
    limits: ReplayLimits,
) -> FxProviderResult<()> {
    match event {
        Event::StateReplacementStarted(start) => {
            if state.is_none() || replacement.is_some() {
                return Err(FxProviderError::InvalidReplacement(
                    "replacement start is out of order",
                ));
            }
            *replacement = Some(ReplacementAccumulator::new(start, scratch, limits)?);
        }
        Event::StateReplacementChunk(chunk) => {
            replacement
                .as_mut()
                .ok_or(FxProviderError::InvalidReplacement(
                    "replacement chunk has no start",
                ))?
                .push_chunk(chunk, limits)?;
        }
        Event::StateReplacementCommitted(commit) => {
            let pending = replacement
                .take()
                .ok_or(FxProviderError::InvalidReplacement(
                    "replacement commit has no start",
                ))?;
            let prior = state.as_ref().ok_or(FxProviderError::InvalidReplacement(
                "replacement has no prior state",
            ))?;
            *state = Some(pending.commit(commit, prior, envelope.timestamp_ms, limits)?);
        }
        ordinary => {
            if replacement.is_some() {
                return Err(FxProviderError::InvalidReplacement(
                    "ordinary event interrupts replacement",
                ));
            }
            apply_ordinary(state, ordinary, envelope)?;
        }
    }
    Ok(())
}

pub(crate) fn suffix_event_requires_replacement(event: &Event) -> bool {
    event.is_replacement()
        || matches!(event, Event::SessionStarted(_))
        || matches!(
            event,
            Event::HistoryTurnCommitted(payload)
                if payload.turn.kind() == crate::HistoryTurnKind::CompactedSummary
        )
}

fn validate_authority(authority: &FxAuthority) -> FxProviderResult<()> {
    if authority.schema_version != 1 {
        return Err(FxProviderError::InvalidAuthority(
            "unsupported authority schema",
        ));
    }
    if !crate::dto::validate_session_id(&authority.session_id)
        || authority.storage_format != "event_log_v1"
    {
        return Err(FxProviderError::InvalidAuthority(
            "invalid authority marker",
        ));
    }
    Ok(())
}

fn validate_checkpoint_metadata(checkpoint: &ReplayCheckpoint) -> FxProviderResult<()> {
    if !crate::dto::validate_session_id(&checkpoint.native_session_id) || checkpoint.next_seq == 0 {
        return Err(FxProviderError::InvalidState(
            "invalid replay checkpoint metadata",
        ));
    }
    crate::limits::validate_workspace_root(&checkpoint.current_workspace_root)?;
    crate::limits::validate_preferences(&checkpoint.preferences)
}

fn validate_watermark(watermark: &FxWatermark) -> FxProviderResult<()> {
    if watermark.schema_version != 1
        || !crate::dto::validate_session_id(&watermark.session_id)
        || watermark.through_seq == 0
        || watermark.through_event_log_bytes == 0
    {
        return Err(FxProviderError::InvalidWatermark(
            "invalid commit watermark",
        ));
    }
    Ok(())
}

pub(crate) fn validate_authority_pair(
    authority: &FxAuthority,
    watermark: &FxWatermark,
    limits: ReplayLimits,
) -> FxProviderResult<()> {
    validate_authority(authority)?;
    validate_watermark(watermark)?;
    if authority.session_id != watermark.session_id {
        return Err(FxProviderError::WatermarkMismatch);
    }
    check_limit(
        "committed event bytes",
        watermark.through_event_log_bytes,
        limits.max_committed_bytes,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventEnvelope {
    pub(crate) schema_version: u64,
    pub(crate) log_generation: FxId,
    pub(crate) seq: u64,
    pub(crate) event_id: FxId,
    pub(crate) timestamp_ms: i64,
    pub(crate) kind: String,
    pub(crate) payload: Box<RawValue>,
}

pub(crate) fn decode_event_envelope(
    bytes: &[u8],
    limits: ReplayLimits,
) -> FxProviderResult<EventEnvelope> {
    check_limit(
        "event frame bytes",
        bytes.len() as u64 + 1,
        EVENT_FRAME_MAX_BYTES as u64,
    )?;
    inspect_json(bytes, limits)?;
    let envelope: EventEnvelope = serde_json::from_slice(bytes)?;
    if envelope.schema_version != 1 {
        return Err(FxProviderError::UnsupportedEventSchema(
            envelope.schema_version,
        ));
    }
    if envelope.seq == 0 || envelope.timestamp_ms < 0 {
        return Err(FxProviderError::InvalidFrame(
            "sequence or timestamp is invalid",
        ));
    }
    Ok(envelope)
}

struct FrameStream<'a, R> {
    reader: &'a mut R,
    target_bytes: u64,
    consumed: u64,
    absolute_base: u64,
    generation: Option<FxId>,
    next_seq: u64,
    events: u64,
    last: Option<(u64, FxId)>,
    limits: ReplayLimits,
}

impl<'a, R: BufRead> FrameStream<'a, R> {
    fn cold(reader: &'a mut R, target_bytes: u64, limits: ReplayLimits) -> FxProviderResult<Self> {
        check_limit(
            "committed event bytes",
            target_bytes,
            limits.max_committed_bytes,
        )?;
        Ok(Self {
            reader,
            target_bytes,
            consumed: 0,
            absolute_base: 0,
            generation: None,
            next_seq: 1,
            events: 0,
            last: None,
            limits,
        })
    }

    fn suffix(
        reader: &'a mut R,
        target_bytes: u64,
        generation: FxId,
        next_seq: u64,
        absolute_base: u64,
        limits: ReplayLimits,
    ) -> FxProviderResult<Self> {
        check_limit(
            "committed event bytes",
            target_bytes,
            limits.max_committed_bytes,
        )?;
        Ok(Self {
            reader,
            target_bytes,
            consumed: 0,
            absolute_base,
            generation: Some(generation),
            next_seq,
            events: 0,
            last: None,
            limits,
        })
    }

    fn next(&mut self) -> FxProviderResult<Option<EventEnvelope>> {
        if self.consumed == self.target_bytes {
            return Ok(None);
        }
        let mut line = Vec::new();
        let mut limited = self.reader.by_ref().take(EVENT_FRAME_MAX_BYTES as u64 + 1);
        let read = limited.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            return Err(FxProviderError::WatermarkMismatch);
        }
        if line.len() > EVENT_FRAME_MAX_BYTES {
            return Err(FxProviderError::LimitExceeded {
                resource: "event frame bytes",
                actual: line.len() as u64,
                maximum: EVENT_FRAME_MAX_BYTES as u64,
            });
        }
        self.consumed = self
            .consumed
            .checked_add(line.len() as u64)
            .ok_or(FxProviderError::WatermarkMismatch)?;
        if self.consumed > self.target_bytes {
            return Err(FxProviderError::WatermarkMismatch);
        }
        self.events = self.events.saturating_add(1);
        check_limit("committed events", self.events, self.limits.max_events)?;
        inspect_json(&line[..line.len() - 1], self.limits)?;
        let envelope: EventEnvelope = serde_json::from_slice(&line[..line.len() - 1])?;
        if envelope.schema_version != 1 {
            return Err(FxProviderError::UnsupportedEventSchema(
                envelope.schema_version,
            ));
        }
        if envelope.seq == 0 || envelope.timestamp_ms < 0 {
            return Err(FxProviderError::InvalidFrame(
                "sequence or timestamp is invalid",
            ));
        }
        if envelope.seq != self.next_seq {
            return Err(FxProviderError::NonContiguousSequence {
                expected: self.next_seq,
                actual: envelope.seq,
            });
        }
        if let Some(generation) = self.generation {
            if generation != envelope.log_generation {
                return Err(FxProviderError::GenerationChanged);
            }
        } else {
            self.generation = Some(envelope.log_generation);
        }
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(FxProviderError::InvalidFrame("sequence overflow"))?;
        self.last = Some((envelope.seq, envelope.event_id));
        Ok(Some(envelope))
    }

    fn validate_watermark(&self, watermark: &FxWatermark) -> FxProviderResult<()> {
        let (last_seq, last_event_id) = self.last.ok_or(FxProviderError::WatermarkMismatch)?;
        if self.absolute_base.saturating_add(self.consumed) != watermark.through_event_log_bytes
            || self.generation != Some(watermark.log_generation)
            || last_seq != watermark.through_seq
            || last_event_id != watermark.through_event_id
        {
            return Err(FxProviderError::WatermarkMismatch);
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionStarted {
    id: String,
    created_at_ms: i64,
    origin_workspace_root: String,
    workspace_root: String,
    conversation_language: String,
    preferences: SessionPreferences,
    #[serde(default)]
    usage: Option<UsageSnapshot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreferencesChanged {
    #[serde(default)]
    provider: Option<crate::dto::ProviderId>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    fast_mode: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceRebound {
    previous_workspace_root: String,
    workspace_root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryTurnCommitted {
    conversation_language: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
    turn: HistoryTurn,
    #[serde(default)]
    work_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageCheckpointed {
    usage: UsageSnapshot,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryCheckpointSet {
    checkpoint: RecoveryCheckpoint,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyPayload {}

pub(crate) enum Event {
    SessionStarted(SessionStarted),
    PreferencesChanged(PreferencesChanged),
    WorkspaceRebound(WorkspaceRebound),
    HistoryTurnCommitted(HistoryTurnCommitted),
    UsageCheckpointed(UsageCheckpointed),
    RecoveryCheckpointSet(RecoveryCheckpointSet),
    RecoveryCheckpointCleared,
    StateReplacementStarted(ReplacementStarted),
    StateReplacementChunk(ReplacementChunk),
    StateReplacementCommitted(ReplacementCommitted),
}

impl Event {
    pub(crate) fn is_replacement(&self) -> bool {
        matches!(
            self,
            Self::StateReplacementStarted(_)
                | Self::StateReplacementChunk(_)
                | Self::StateReplacementCommitted(_)
        )
    }
}

pub(crate) fn parse_event(kind: &str, raw: &str) -> FxProviderResult<Event> {
    Ok(match kind {
        "session_started" => Event::SessionStarted(serde_json::from_str(raw)?),
        "preferences_changed" => Event::PreferencesChanged(serde_json::from_str(raw)?),
        "workspace_rebound" => Event::WorkspaceRebound(serde_json::from_str(raw)?),
        "history_turn_committed" => Event::HistoryTurnCommitted(serde_json::from_str(raw)?),
        "usage_checkpointed" => Event::UsageCheckpointed(serde_json::from_str(raw)?),
        "recovery_checkpoint_set" => Event::RecoveryCheckpointSet(serde_json::from_str(raw)?),
        "recovery_checkpoint_cleared" => {
            let _: EmptyPayload = serde_json::from_str(raw)?;
            Event::RecoveryCheckpointCleared
        }
        "state_replacement_started" => Event::StateReplacementStarted(serde_json::from_str(raw)?),
        "state_replacement_chunk" => Event::StateReplacementChunk(serde_json::from_str(raw)?),
        "state_replacement_committed" => {
            Event::StateReplacementCommitted(serde_json::from_str(raw)?)
        }
        _ => return Err(FxProviderError::UnknownEventKind(kind.to_owned())),
    })
}

pub(crate) fn apply_ordinary(
    state: &mut Option<CanonicalState>,
    event: Event,
    envelope: &EventEnvelope,
) -> FxProviderResult<()> {
    match event {
        Event::SessionStarted(payload) => {
            if state.is_some() || payload.created_at_ms < 0 {
                return Err(FxProviderError::InvalidState(
                    "session_started is duplicated or invalid",
                ));
            }
            if !crate::dto::validate_session_id(&payload.id) {
                return Err(FxProviderError::InvalidState("invalid native session id"));
            }
            crate::limits::validate_workspace_root(&payload.origin_workspace_root)?;
            crate::limits::validate_workspace_root(&payload.workspace_root)?;
            crate::limits::validate_conversation_language(&payload.conversation_language)?;
            crate::limits::validate_preferences(&payload.preferences)?;
            if let Some(usage) = &payload.usage {
                crate::limits::validate_usage_snapshot(usage)?;
            }
            *state = Some(CanonicalState {
                id: payload.id,
                origin_workspace_root: payload.origin_workspace_root,
                workspace_root: payload.workspace_root,
                created_at_ms: payload.created_at_ms,
                updated_at_ms: envelope.timestamp_ms,
                conversation_language: payload.conversation_language,
                preferences: payload.preferences,
                history: Vec::new(),
                total_input_tokens: 0,
                total_output_tokens: 0,
                context_history_start: 0,
                permission_state: PermissionState::default(),
                last_subagent_work_id: None,
                usage: payload.usage,
                recovery_checkpoint: None,
            });
        }
        Event::PreferencesChanged(payload) => {
            if payload.provider.is_none()
                && payload.model.is_none()
                && payload.effort.is_none()
                && payload.fast_mode.is_none()
            {
                return Err(FxProviderError::InvalidFrame("empty preferences change"));
            }
            let current = require_state(state)?;
            if let Some(provider) = payload.provider {
                current.preferences.provider = provider;
            }
            if let Some(model) = payload.model {
                current.preferences.model = model;
            }
            if let Some(effort) = payload.effort {
                current.preferences.effort = effort;
            }
            if let Some(fast_mode) = payload.fast_mode {
                current.preferences.fast_mode = fast_mode;
            }
            crate::limits::validate_preferences(&current.preferences)?;
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::WorkspaceRebound(payload) => {
            let current = require_state(state)?;
            if current.workspace_root != payload.previous_workspace_root
                || current.workspace_root == payload.workspace_root
                || payload.workspace_root.is_empty()
            {
                return Err(FxProviderError::InvalidState(
                    "workspace rebound does not match prior state",
                ));
            }
            current.workspace_root = payload.workspace_root;
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::HistoryTurnCommitted(payload) => {
            let current = require_state(state)?;
            let mut turn = payload.turn;
            turn.associate_event_work_id(payload.work_id.as_deref())?;
            crate::limits::validate_conversation_language(&payload.conversation_language)?;
            current.history.push(turn);
            current.conversation_language = payload.conversation_language;
            current.total_input_tokens = payload.total_input_tokens;
            current.total_output_tokens = payload.total_output_tokens;
            if payload.work_id.is_some() {
                current.last_subagent_work_id = payload.work_id;
            }
            current.recovery_checkpoint = None;
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::UsageCheckpointed(payload) => {
            crate::limits::validate_usage_snapshot(&payload.usage)?;
            let current = require_state(state)?;
            current.usage = Some(payload.usage);
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::RecoveryCheckpointSet(payload) => {
            crate::limits::validate_recovery_checkpoint(&payload.checkpoint)?;
            let current = require_state(state)?;
            current.recovery_checkpoint = Some(payload.checkpoint);
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::RecoveryCheckpointCleared => {
            let current = require_state(state)?;
            current.recovery_checkpoint = None;
            current.updated_at_ms = envelope.timestamp_ms;
        }
        Event::StateReplacementStarted(_)
        | Event::StateReplacementChunk(_)
        | Event::StateReplacementCommitted(_) => {
            return Err(FxProviderError::InvalidReplacement(
                "replacement event reached ordinary reducer",
            ));
        }
    }
    Ok(())
}

pub(crate) fn apply_suffix_ordinary(
    workspace_root: &mut String,
    preferences: &mut SessionPreferences,
    new_turns: &mut Vec<LogicalTurn>,
    next_ordinal: &mut u64,
    event: Event,
) -> FxProviderResult<()> {
    match event {
        Event::PreferencesChanged(payload) => {
            if payload.provider.is_none()
                && payload.model.is_none()
                && payload.effort.is_none()
                && payload.fast_mode.is_none()
            {
                return Err(FxProviderError::InvalidFrame("empty preferences change"));
            }
            if let Some(provider) = payload.provider {
                preferences.provider = provider;
            }
            if let Some(model) = payload.model {
                preferences.model = model;
            }
            if let Some(effort) = payload.effort {
                preferences.effort = effort;
            }
            if let Some(fast_mode) = payload.fast_mode {
                preferences.fast_mode = fast_mode;
            }
            crate::limits::validate_preferences(preferences)?;
        }
        Event::WorkspaceRebound(payload) => {
            if *workspace_root != payload.previous_workspace_root
                || *workspace_root == payload.workspace_root
            {
                return Err(FxProviderError::InvalidState(
                    "workspace rebound does not match continuation",
                ));
            }
            crate::limits::validate_workspace_root(&payload.workspace_root)?;
            *workspace_root = payload.workspace_root;
        }
        Event::HistoryTurnCommitted(mut payload) => {
            payload
                .turn
                .associate_event_work_id(payload.work_id.as_deref())?;
            new_turns.push(LogicalTurn {
                absolute_ordinal: *next_ordinal,
                turn: payload.turn,
            });
            *next_ordinal = next_ordinal
                .checked_add(1)
                .ok_or(FxProviderError::InvalidState(
                    "logical turn ordinal overflow",
                ))?;
        }
        Event::UsageCheckpointed(payload) => {
            crate::limits::validate_usage_snapshot(&payload.usage)?;
        }
        Event::RecoveryCheckpointSet(payload) => {
            crate::limits::validate_recovery_checkpoint(&payload.checkpoint)?;
        }
        Event::RecoveryCheckpointCleared => {}
        Event::SessionStarted(_)
        | Event::StateReplacementStarted(_)
        | Event::StateReplacementChunk(_)
        | Event::StateReplacementCommitted(_) => {
            return Err(FxProviderError::InvalidFrame(
                "event cannot be validated from append continuation",
            ));
        }
    }
    Ok(())
}

fn require_state(state: &mut Option<CanonicalState>) -> FxProviderResult<&mut CanonicalState> {
    state.as_mut().ok_or(FxProviderError::InvalidState(
        "event precedes session_started",
    ))
}

pub(crate) fn checkpoint_for(
    state: &CanonicalState,
    watermark: &FxWatermark,
) -> FxProviderResult<ReplayCheckpoint> {
    let surviving = state
        .history
        .iter()
        .filter(|turn| turn.kind() != crate::HistoryTurnKind::CompactedSummary)
        .count() as u64;
    let absolute_turn_slots = state
        .removed_turn_count()
        .checked_add(surviving)
        .ok_or(FxProviderError::InvalidState("logical turn count overflow"))?;
    Ok(ReplayCheckpoint {
        native_session_id: state.id.clone(),
        log_generation: watermark.log_generation,
        next_seq: watermark
            .through_seq
            .checked_add(1)
            .ok_or(FxProviderError::InvalidWatermark("sequence overflow"))?,
        through_event_id: watermark.through_event_id,
        through_event_log_bytes: watermark.through_event_log_bytes,
        absolute_turn_slots,
        current_workspace_root: state.workspace_root.clone(),
        preferences: state.preferences.clone(),
    })
}
