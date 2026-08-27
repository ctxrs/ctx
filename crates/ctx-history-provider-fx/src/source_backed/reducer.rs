use crate::{
    replacement::ReplacementAccumulator,
    replay::{
        apply_cold_replay_event, apply_suffix_ordinary, checkpoint_for, decode_event_envelope,
        parse_event, suffix_event_requires_replacement, validate_authority_pair,
    },
    AppendReplay, CanonicalReplay, CanonicalState, FxAuthority, FxId, FxProviderError,
    FxProviderResult, FxWatermark, LogicalTurn, ReplacementScratch, ReplayCheckpoint, ReplayLimits,
    SessionPreferences, SuffixDisposition, EVENT_FRAME_MAX_BYTES,
};

pub(super) struct CommittedReplayReducer {
    authority: FxAuthority,
    watermark: FxWatermark,
    frames: FrameValidator,
    state: Option<CanonicalState>,
    replacement: Option<ReplacementAccumulator>,
}

impl CommittedReplayReducer {
    pub(super) fn new(
        authority: FxAuthority,
        watermark: FxWatermark,
        limits: ReplayLimits,
    ) -> FxProviderResult<Self> {
        validate_authority_pair(&authority, &watermark, limits)?;
        Ok(Self {
            frames: FrameValidator::cold(&watermark, limits),
            authority,
            watermark,
            state: None,
            replacement: None,
        })
    }

    pub(super) fn consume(
        &mut self,
        bytes: &[u8],
        byte_start: u64,
        byte_end_exclusive: u64,
        scratch: &dyn ReplacementScratch,
    ) -> FxProviderResult<()> {
        let envelope = self.frames.consume(bytes, byte_start, byte_end_exclusive)?;
        let event = parse_event(&envelope.kind, envelope.payload.get())?;
        apply_cold_replay_event(
            &mut self.state,
            &mut self.replacement,
            event,
            &envelope,
            scratch,
            self.frames.limits,
        )
    }

    pub(super) fn finish(self) -> FxProviderResult<(CanonicalReplay, u64)> {
        if self.replacement.is_some() {
            return Err(FxProviderError::InvalidReplacement(
                "committed replacement is incomplete",
            ));
        }
        self.frames.validate_watermark(&self.watermark)?;
        let state = self.state.ok_or(FxProviderError::InvalidState(
            "session_started event is missing",
        ))?;
        if state.id != self.authority.session_id {
            return Err(FxProviderError::InvalidAuthority(
                "authority session does not match replayed state",
            ));
        }
        crate::validate_canonical_state(&state, self.frames.limits)?;
        let checkpoint = checkpoint_for(&state, &self.watermark)?;
        Ok((CanonicalReplay { state, checkpoint }, self.frames.events))
    }
}

pub(super) struct SuffixReplayReducer {
    watermark: FxWatermark,
    frames: FrameValidator,
    checkpoint: ReplayCheckpoint,
    workspace_root: String,
    preferences: SessionPreferences,
    new_turns: Vec<LogicalTurn>,
    next_ordinal: u64,
    replacement_required: bool,
}

impl SuffixReplayReducer {
    pub(super) fn new(
        authority: &FxAuthority,
        prior: ReplayCheckpoint,
        watermark: FxWatermark,
        limits: ReplayLimits,
    ) -> FxProviderResult<Self> {
        validate_authority_pair(authority, &watermark, limits)?;
        if prior.native_session_id != authority.session_id
            || prior.log_generation != watermark.log_generation
            || watermark.through_event_log_bytes < prior.through_event_log_bytes
            || watermark.through_seq.saturating_add(1) < prior.next_seq
        {
            return Err(FxProviderError::WatermarkMismatch);
        }
        let frames = FrameValidator::suffix(&prior, &watermark, limits)?;
        Ok(Self {
            workspace_root: prior.current_workspace_root.clone(),
            preferences: prior.preferences.clone(),
            next_ordinal: prior.absolute_turn_slots,
            checkpoint: prior,
            watermark,
            frames,
            new_turns: Vec::new(),
            replacement_required: false,
        })
    }

    pub(super) fn consume(
        &mut self,
        bytes: &[u8],
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> FxProviderResult<()> {
        let envelope = self.frames.consume(bytes, byte_start, byte_end_exclusive)?;
        let event = parse_event(&envelope.kind, envelope.payload.get())?;
        if suffix_event_requires_replacement(&event) {
            self.replacement_required = true;
        } else if !self.replacement_required {
            apply_suffix_ordinary(
                &mut self.workspace_root,
                &mut self.preferences,
                &mut self.new_turns,
                &mut self.next_ordinal,
                event,
            )?;
        }
        Ok(())
    }

    pub(super) fn finish(self) -> FxProviderResult<(SuffixDisposition, u64)> {
        self.frames.validate_watermark(&self.watermark)?;
        if self.replacement_required {
            return Ok((SuffixDisposition::ReplaceCanonicalState, self.frames.events));
        }
        let checkpoint = ReplayCheckpoint {
            native_session_id: self.checkpoint.native_session_id,
            log_generation: self.watermark.log_generation,
            next_seq: self
                .watermark
                .through_seq
                .checked_add(1)
                .ok_or(FxProviderError::InvalidWatermark("sequence overflow"))?,
            through_event_id: self.watermark.through_event_id,
            through_event_log_bytes: self.watermark.through_event_log_bytes,
            absolute_turn_slots: self.next_ordinal,
            current_workspace_root: self.workspace_root,
            preferences: self.preferences,
        };
        Ok((
            SuffixDisposition::AppendNewTurns(AppendReplay {
                new_turns: self.new_turns,
                checkpoint,
            }),
            self.frames.events,
        ))
    }
}

struct FrameValidator {
    target_bytes: u64,
    consumed: u64,
    absolute_base: u64,
    generation: Option<FxId>,
    next_seq: u64,
    events: u64,
    last: Option<(u64, FxId)>,
    limits: ReplayLimits,
}

impl FrameValidator {
    fn cold(watermark: &FxWatermark, limits: ReplayLimits) -> Self {
        Self {
            target_bytes: watermark.through_event_log_bytes,
            consumed: 0,
            absolute_base: 0,
            generation: None,
            next_seq: 1,
            events: 0,
            last: None,
            limits,
        }
    }

    fn suffix(
        prior: &ReplayCheckpoint,
        watermark: &FxWatermark,
        limits: ReplayLimits,
    ) -> FxProviderResult<Self> {
        let target_bytes = watermark
            .through_event_log_bytes
            .checked_sub(prior.through_event_log_bytes)
            .ok_or(FxProviderError::WatermarkMismatch)?;
        crate::limits::check_limit(
            "committed event bytes",
            target_bytes,
            limits.max_committed_bytes,
        )?;
        Ok(Self {
            target_bytes,
            consumed: 0,
            absolute_base: prior.through_event_log_bytes,
            generation: Some(prior.log_generation),
            next_seq: prior.next_seq,
            events: 0,
            last: Some((prior.next_seq.saturating_sub(1), prior.through_event_id)),
            limits,
        })
    }

    fn consume(
        &mut self,
        bytes: &[u8],
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> FxProviderResult<crate::replay::EventEnvelope> {
        let expected_start = self
            .absolute_base
            .checked_add(self.consumed)
            .ok_or(FxProviderError::WatermarkMismatch)?;
        let physical_bytes = byte_end_exclusive
            .checked_sub(byte_start)
            .ok_or(FxProviderError::WatermarkMismatch)?;
        if byte_start != expected_start
            || physical_bytes != bytes.len() as u64 + 1
            || physical_bytes > EVENT_FRAME_MAX_BYTES as u64
        {
            return Err(FxProviderError::WatermarkMismatch);
        }
        self.consumed = self
            .consumed
            .checked_add(physical_bytes)
            .ok_or(FxProviderError::WatermarkMismatch)?;
        if self.consumed > self.target_bytes {
            return Err(FxProviderError::WatermarkMismatch);
        }
        self.events = self.events.saturating_add(1);
        crate::limits::check_limit("committed events", self.events, self.limits.max_events)?;
        let envelope = decode_event_envelope(bytes, self.limits)?;
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
        Ok(envelope)
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
