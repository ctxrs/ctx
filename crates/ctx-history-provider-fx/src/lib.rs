//! Pure, path-independent replay and projection for fx event-log v3 sessions.

mod dto;
mod error;
mod history;
mod legacy;
mod limits;
mod projection;
mod replacement;
mod replay;
mod scratch;
mod source_backed;

pub use dto::{
    CanonicalState, CredentialSource, DurableBytes, FxAuthority, FxAuthoritySource, FxDigest, FxId,
    FxWatermark, LogicalTurn, PermissionDecision, PermissionRule, PermissionRuleKind,
    PermissionState, ProviderId, RecoveryCheckpoint, SessionPreferences, UsageSnapshot,
};
pub use error::{FxProviderError, FxProviderResult};
pub use history::{
    CompactedSummaryTurn, ExecutionMemory, HistoryTurn, HistoryTurnKind, ImageAttachment, ToolCall,
    ToolExecutionStep, ToolResult, UserTurn, MAX_HISTORY_TURN_BYTES,
};
pub use legacy::{
    replay_legacy_snapshot, select_session_protocol, LegacyDefaults, LegacyReduction,
    LegacySnapshotVersion, SelectedSessionProtocol,
};
pub use limits::{validate_canonical_state, ReplayLimits, MAX_LEGACY_SNAPSHOT_BYTES};
pub use projection::{
    project_canonical_state, project_logical_turns, ProjectionBinding, FX_PARSER_REVISION,
};
pub use replacement::RAW_STATE_CHUNK_BYTES;
pub use replay::{
    decode_authority, decode_first_event_binding, decode_replay_checkpoint, decode_watermark,
    encode_replay_checkpoint, replay_committed, replay_suffix, AppendReplay, BoundaryIntent,
    CanonicalReplay, ColdReplayDisposition, FxFirstEventBinding, PendingIntent, ReplayCheckpoint,
    SuffixDisposition, EVENT_FRAME_MAX_BYTES, REPLAY_CHECKPOINT_MAX_BYTES,
};
pub use scratch::{ReplacementScratch, ScratchFile, TempFileScratch};
pub use source_backed::{
    fx_sessions_tree_adapter, FX_SESSIONS_TREE_SCHEMA_VARIANT, FX_SESSIONS_TREE_SOURCE_FORMAT,
};

#[cfg(test)]
mod tests;
