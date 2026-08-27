use serde::{de::IgnoredAny, Deserialize, Deserializer, Serialize};

use crate::{
    history::{CompactedSummaryTurn, HistoryTurn, HistoryTurnKind, TurnStats},
    limits::{check_limit, inspect_json},
    CanonicalState, FxAuthority, FxId, FxProviderError, FxProviderResult, PermissionState,
    ReplayLimits, SessionPreferences,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySnapshotVersion {
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyDefaults {
    pub source_root: String,
    pub preferences: SessionPreferences,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyReduction {
    pub version: LegacySnapshotVersion,
    pub state: CanonicalState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectedSessionProtocol {
    V3(FxAuthority),
    Legacy(Box<LegacyReduction>),
}

/// Selects authority without ever inspecting legacy bytes after an authority
/// marker exists. A malformed marker is fatal; it never falls back to legacy.
pub fn select_session_protocol(
    authority_marker: Option<&[u8]>,
    legacy_snapshot: Option<&[u8]>,
    defaults: &LegacyDefaults,
    limits: ReplayLimits,
) -> FxProviderResult<SelectedSessionProtocol> {
    if let Some(marker) = authority_marker {
        return Ok(SelectedSessionProtocol::V3(crate::decode_authority(
            marker, limits,
        )?));
    }
    let bytes = legacy_snapshot.ok_or(FxProviderError::InvalidLegacy(
        "marker-less session has no legacy snapshot",
    ))?;
    Ok(SelectedSessionProtocol::Legacy(Box::new(
        replay_legacy_snapshot(bytes, defaults, limits)?,
    )))
}

pub fn replay_legacy_snapshot(
    bytes: &[u8],
    defaults: &LegacyDefaults,
    limits: ReplayLimits,
) -> FxProviderResult<LegacyReduction> {
    check_limit(
        "legacy snapshot bytes",
        bytes.len() as u64,
        limits.max_legacy_snapshot_bytes,
    )?;
    inspect_json(bytes, limits)?;
    let wire: LegacySnapshot = serde_json::from_slice(bytes)?;
    let version = match wire.schema_version {
        1 => LegacySnapshotVersion::V1,
        2 => LegacySnapshotVersion::V2,
        other => return Err(FxProviderError::UnsupportedLegacySchema(other)),
    };
    if wire.history_len != wire.history.len() as u64 {
        return Err(FxProviderError::InvalidLegacy(
            "history_len does not match history",
        ));
    }
    let root = wire
        .workspace_root
        .unwrap_or_else(|| defaults.source_root.clone());
    if root.is_empty() {
        return Err(FxProviderError::InvalidLegacy(
            "legacy workspace root is unavailable",
        ));
    }
    let mut history = Vec::with_capacity(wire.history.len());
    for turn in wire.history {
        history.push(normalize_turn(turn)?);
    }
    let state = CanonicalState {
        id: wire.id,
        origin_workspace_root: root.clone(),
        workspace_root: root,
        created_at_ms: wire.created_at_ms,
        updated_at_ms: wire.updated_at_ms,
        conversation_language: wire.conversation_language,
        preferences: defaults.preferences.clone(),
        history,
        total_input_tokens: wire.total_input_tokens.0,
        total_output_tokens: wire.total_output_tokens.0,
        context_history_start: 0,
        permission_state: PermissionState::default(),
        last_subagent_work_id: None,
        usage: None,
        recovery_checkpoint: None,
    };
    crate::limits::validate_canonical_state(&state, limits)?;
    Ok(LegacyReduction { version, state })
}

#[derive(Debug, Clone, Copy, Default)]
struct LegacyCounter(u64);

impl<'de> Deserialize<'de> for LegacyCounter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = LegacyCounter;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a legacy optional counter")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(LegacyCounter(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(LegacyCounter(u64::try_from(value).unwrap_or(0)))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LegacyCounter(0))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LegacyCounter(0))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(LegacyCounter(0))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(LegacyCounter(0))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LegacyCounter(0))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LegacyCounter(0))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Deserialize)]
struct LegacySnapshot {
    schema_version: u64,
    id: String,
    #[serde(default)]
    workspace_root: Option<String>,
    created_at_ms: i64,
    updated_at_ms: i64,
    conversation_language: String,
    history_len: u64,
    history: Vec<LegacyTurn>,
    #[serde(default)]
    total_input_tokens: LegacyCounter,
    #[serde(default)]
    total_output_tokens: LegacyCounter,
    #[serde(default, rename = "total_web_search_requests")]
    _total_web_search_requests: LegacyCounter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LegacyTurn {
    CompactedSummary {
        summary: String,
        removed_turn_count: u64,
        compaction_count: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_user_messages: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_user_messages_complete: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_feedback: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_feedback_complete: Option<bool>,
    },
    Assistant {
        user: LegacyUser,
        assistant: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<LegacyExecution>,
    },
    BackgroundCommand {
        user: LegacyUser,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<LegacyExecution>,
        log_path: String,
        expect_url: bool,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        background_record_id: Option<FxId>,
    },
    Interrupted {
        user: LegacyUser,
        #[serde(default)]
        assistant: Option<String>,
        #[serde(default)]
        tool_call: Option<LegacyToolCall>,
        #[serde(default)]
        completed_tool_names: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_reason: Option<LegacyTerminalReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<LegacyExecution>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyUser {
    text: String,
    #[serde(default)]
    images: Vec<LegacyImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyImage {
    #[serde(default)]
    id: u64,
    path: String,
    media_type: String,
    #[serde(default)]
    snapshot_path: Option<String>,
    #[serde(default)]
    snapshot_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyToolCall {
    id: String,
    name: String,
    arguments_json: String,
    #[serde(default)]
    provider_result: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyToolStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyToolResult {
    tool_call_id: String,
    tool_name: String,
    status: LegacyToolStatus,
    output: String,
    #[serde(default)]
    output_handle: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    output_bytes: u64,
    stored_output_bytes: u64,
    truncated: bool,
    provider_native: bool,
    created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_feedback: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    committed_file_presentation: Option<LegacyCommittedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyToolStep {
    #[serde(default)]
    assistant: Option<String>,
    #[serde(default)]
    tool_calls: Vec<LegacyToolCall>,
    #[serde(default)]
    tool_results: Vec<LegacyToolResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyExecution {
    #[serde(default = "legacy_execution_schema")]
    schema_version: u64,
    #[serde(default)]
    tool_steps: Vec<LegacyToolStep>,
    #[serde(default)]
    files: Vec<LegacyFileEvidence>,
}

fn legacy_execution_schema() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyFileAction {
    Read,
    Write,
    Edit,
    Delete,
    Rename,
    Copy,
    Search,
    List,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyFileEvidence {
    path: String,
    #[serde(default)]
    new_path: Option<String>,
    tool_call_id: String,
    tool_name: String,
    action: LegacyFileAction,
    status: LegacyToolStatus,
    model_view_covers_full_file: bool,
    stale: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyCommittedKind {
    Added,
    Edited,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyLineKind {
    Context,
    Addition,
    Deletion,
    Elision,
    Notice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyCommittedLine {
    kind: LegacyLineKind,
    #[serde(default)]
    old_line: Option<u32>,
    #[serde(default)]
    new_line: Option<u32>,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyLifecycleId {
    turn_id: u64,
    call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyCommittedFile {
    path: String,
    kind: LegacyCommittedKind,
    #[serde(default)]
    lines: Vec<LegacyCommittedLine>,
    additions: u64,
    deletions: u64,
    truncated: bool,
    #[serde(default)]
    previous_content: Option<String>,
    #[serde(default)]
    after_content: Option<String>,
    #[serde(default)]
    lifecycle_id: Option<LegacyLifecycleId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyTerminalReason {
    Cancelled,
    Failed,
}

fn normalize_turn(turn: LegacyTurn) -> FxProviderResult<HistoryTurn> {
    let (kind, summary, stats) = legacy_metadata(&turn)?;
    let encoded = serde_json::to_string(&turn)?;
    HistoryTurn::from_legacy_normalized(encoded, kind, summary, stats)
}

fn legacy_metadata(
    turn: &LegacyTurn,
) -> FxProviderResult<(HistoryTurnKind, Option<CompactedSummaryTurn>, TurnStats)> {
    let mut stats = TurnStats::default();
    match turn {
        LegacyTurn::CompactedSummary {
            summary,
            removed_turn_count,
            compaction_count,
            root_user_messages,
            root_user_messages_complete,
            permission_feedback,
            permission_feedback_complete,
        } => {
            stats.nested_items = root_user_messages
                .as_ref()
                .map_or(0, |items| items.len() as u64)
                .saturating_add(
                    permission_feedback
                        .as_ref()
                        .map_or(0, |items| items.len() as u64),
                );
            Ok((
                HistoryTurnKind::CompactedSummary,
                Some(CompactedSummaryTurn {
                    summary: crate::DurableBytes::Utf8(summary.clone()),
                    removed_turn_count: *removed_turn_count,
                    compaction_count: *compaction_count,
                    root_user_messages: root_user_messages.as_ref().map(|items| {
                        items
                            .iter()
                            .cloned()
                            .map(crate::DurableBytes::Utf8)
                            .collect()
                    }),
                    root_user_messages_complete: *root_user_messages_complete,
                    permission_feedback: permission_feedback.as_ref().map(|items| {
                        items
                            .iter()
                            .cloned()
                            .map(crate::DurableBytes::Utf8)
                            .collect()
                    }),
                    permission_feedback_complete: *permission_feedback_complete,
                }),
                stats,
            ))
        }
        LegacyTurn::Assistant {
            user, execution, ..
        } => {
            validate_legacy_user(user)?;
            stats.image_count = user.images.len() as u64;
            add_legacy_execution(&mut stats, execution.as_ref())?;
            Ok((HistoryTurnKind::Assistant, None, stats))
        }
        LegacyTurn::BackgroundCommand {
            user, execution, ..
        } => {
            validate_legacy_user(user)?;
            stats.image_count = user.images.len() as u64;
            add_legacy_execution(&mut stats, execution.as_ref())?;
            Ok((HistoryTurnKind::BackgroundCommand, None, stats))
        }
        LegacyTurn::Interrupted {
            user,
            execution,
            completed_tool_names,
            tool_call,
            ..
        } => {
            validate_legacy_user(user)?;
            stats.image_count = user.images.len() as u64;
            add_legacy_execution(&mut stats, execution.as_ref())?;
            stats.nested_items = stats
                .nested_items
                .saturating_add(completed_tool_names.len() as u64)
                .saturating_add(u64::from(tool_call.is_some()));
            Ok((HistoryTurnKind::Interrupted, None, stats))
        }
    }
}

fn validate_legacy_user(user: &LegacyUser) -> FxProviderResult<()> {
    for image in &user.images {
        if image.snapshot_path.is_some() != image.snapshot_sha256.is_some() {
            return Err(FxProviderError::InvalidLegacy(
                "legacy image snapshot fields are inconsistent",
            ));
        }
    }
    Ok(())
}

fn add_legacy_execution(
    stats: &mut TurnStats,
    execution: Option<&LegacyExecution>,
) -> FxProviderResult<()> {
    let Some(execution) = execution else {
        stats.nested_items = stats.nested_items.saturating_add(stats.image_count);
        return Ok(());
    };
    if !(1..=2).contains(&execution.schema_version) {
        return Err(FxProviderError::InvalidLegacy(
            "legacy execution schema is unsupported",
        ));
    }
    stats.file_count = stats
        .file_count
        .saturating_add(execution.files.len() as u64);
    for step in &execution.tool_steps {
        stats.tool_count = stats
            .tool_count
            .saturating_add(1)
            .saturating_add(step.tool_calls.len() as u64)
            .saturating_add(step.tool_results.len() as u64);
        for result in &step.tool_results {
            if result.created_at_ms < 0 {
                return Err(FxProviderError::InvalidLegacy(
                    "legacy tool result timestamp is negative",
                ));
            }
            stats.nested_items = stats.nested_items.saturating_add(
                result
                    .permission_feedback
                    .as_ref()
                    .map_or(0, |items| items.len() as u64),
            );
            if let Some(presentation) = &result.committed_file_presentation {
                stats.nested_items = stats
                    .nested_items
                    .saturating_add(presentation.lines.len() as u64);
            }
        }
    }
    stats.nested_items = stats
        .nested_items
        .saturating_add(stats.image_count)
        .saturating_add(stats.tool_count)
        .saturating_add(stats.file_count);
    Ok(())
}
