use std::fmt;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{value::RawValue, Value};

use crate::{DurableBytes, FxId, FxProviderError, FxProviderResult};

pub const MAX_HISTORY_TURN_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Nullable<T>(pub Option<T>);

fn deserialize_present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Nullable<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Nullable(Option::<T>::deserialize(deserializer)?)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserTurn {
    pub text: DurableBytes,
    pub images: Vec<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageAttachment {
    pub id: u64,
    pub path: DurableBytes,
    pub media_type: DurableBytes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_path: Option<DurableBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_sha256: Option<DurableBytes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: DurableBytes,
    pub name: DurableBytes,
    pub arguments_json: DurableBytes,
    pub provider_result: Nullable<DurableBytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Failure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEvidence {
    pub path: DurableBytes,
    pub new_path: Nullable<DurableBytes>,
    pub tool_call_id: DurableBytes,
    pub tool_name: DurableBytes,
    pub action: FileAction,
    pub status: ToolStatus,
    pub model_view_covers_full_file: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommittedFileKind {
    Added,
    Edited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommittedLineKind {
    Context,
    Addition,
    Deletion,
    Elision,
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedLine {
    pub kind: CommittedLineKind,
    pub old_line: Nullable<u32>,
    pub new_line: Nullable<u32>,
    pub text: DurableBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolLifecycleId {
    pub turn_id: u64,
    pub call_id: DurableBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedFilePresentation {
    pub path: DurableBytes,
    pub kind: CommittedFileKind,
    pub lines: Vec<CommittedLine>,
    pub additions: u64,
    pub deletions: u64,
    pub truncated: bool,
    pub previous_content: Nullable<DurableBytes>,
    pub after_content: Nullable<DurableBytes>,
    pub lifecycle_id: Nullable<ToolLifecycleId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandOutputReplay {
    Available {
        handle: DurableBytes,
        framed_bytes: u64,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CommandProcessPresentation {
    ExitCode { value: i64 },
    Signal { value: u32 },
    TimedOut { value: () },
    OutputCaptureFailed { value: () },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalOutcome {
    Started { value: () },
    ConditionMet { value: () },
    SafetyCeiling { value: () },
    Cancelled { value: () },
    Exited { value: i32 },
    Signal { value: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalActionPresentation {
    Returned { outcome: TerminalOutcome },
    Failed { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    pub tool_call_id: DurableBytes,
    pub tool_name: DurableBytes,
    pub status: ToolStatus,
    pub output: DurableBytes,
    pub output_handle: Nullable<DurableBytes>,
    pub preview: Nullable<DurableBytes>,
    pub output_bytes: u64,
    pub stored_output_bytes: u64,
    pub truncated: bool,
    pub provider_native: bool,
    pub created_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_feedback: Option<Vec<DurableBytes>>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub committed_file_presentation: Option<Nullable<CommittedFilePresentation>>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub command_output_replay: Option<Nullable<CommandOutputReplay>>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub command_process_presentation: Option<Nullable<CommandProcessPresentation>>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub terminal_action_presentation: Option<Nullable<TerminalActionPresentation>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolExecutionStep {
    pub assistant: Nullable<DurableBytes>,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionMemory {
    pub schema_version: u64,
    pub tool_steps: Vec<ToolExecutionStep>,
    pub files: Vec<FileEvidence>,
}

impl Default for ExecutionMemory {
    fn default() -> Self {
        Self {
            schema_version: 1,
            tool_steps: Vec::new(),
            files: Vec::new(),
        }
    }
}

impl ExecutionMemory {
    pub(crate) fn validate(&self) -> FxProviderResult<()> {
        if !(1..=4).contains(&self.schema_version) {
            return Err(FxProviderError::InvalidState(
                "unsupported execution schema version",
            ));
        }
        for step in &self.tool_steps {
            for result in &step.tool_results {
                if result.created_at_ms < 0 {
                    return Err(FxProviderError::InvalidState(
                        "tool result timestamp is negative",
                    ));
                }
                let has_feedback = result.permission_feedback.is_some();
                let has_committed = result.committed_file_presentation.is_some();
                let has_replay = result.command_output_replay.is_some();
                let has_process = result.command_process_presentation.is_some();
                let has_terminal = result.terminal_action_presentation.is_some();
                let valid = match self.schema_version {
                    1 => {
                        !has_feedback
                            && !has_committed
                            && !has_replay
                            && !has_process
                            && !has_terminal
                    }
                    2 => has_feedback && !has_replay && !has_process && !has_terminal,
                    3 => {
                        has_feedback && has_committed && has_replay && has_process && !has_terminal
                    }
                    4 => has_feedback && has_committed && has_replay && has_process && has_terminal,
                    _ => false,
                };
                if !valid {
                    return Err(FxProviderError::InvalidState(
                        "tool result fields do not match execution schema",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn aggregate_items(&self) -> u64 {
        let mut items = self.files.len() as u64;
        for step in &self.tool_steps {
            items = items
                .saturating_add(1)
                .saturating_add(step.tool_calls.len() as u64)
                .saturating_add(step.tool_results.len() as u64);
            for result in &step.tool_results {
                items = items.saturating_add(
                    result
                        .permission_feedback
                        .as_ref()
                        .map_or(0, |values| values.len() as u64),
                );
                if let Some(Nullable(Some(presentation))) = &result.committed_file_presentation {
                    items = items.saturating_add(presentation.lines.len() as u64);
                }
            }
        }
        items
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelledCommandPresentation {
    pub output_replay: Nullable<CommandOutputReplay>,
    pub command_artifact_handle: Nullable<DurableBytes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptedTerminalReason {
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactedSummaryTurn {
    pub summary: DurableBytes,
    pub removed_turn_count: u64,
    pub compaction_count: u64,
    pub root_user_messages: Option<Vec<DurableBytes>>,
    pub root_user_messages_complete: Option<bool>,
    pub permission_feedback: Option<Vec<DurableBytes>>,
    pub permission_feedback_complete: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HistoryWire {
    CompactedSummary {
        summary: DurableBytes,
        removed_turn_count: u64,
        compaction_count: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_user_messages: Option<Vec<DurableBytes>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_user_messages_complete: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_feedback: Option<Vec<DurableBytes>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_feedback_complete: Option<bool>,
    },
    Assistant {
        user: UserTurn,
        assistant: DurableBytes,
        execution: ExecutionMemory,
    },
    BackgroundCommand {
        user: UserTurn,
        log_path: DurableBytes,
        expect_url: bool,
        url: Nullable<DurableBytes>,
        background_record_id: Nullable<FxId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assistant: Option<Nullable<DurableBytes>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<ExecutionMemory>,
    },
    Interrupted {
        user: UserTurn,
        assistant: Nullable<DurableBytes>,
        tool_call: Nullable<ToolCall>,
        completed_tool_names: Vec<DurableBytes>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_reason: Option<InterruptedTerminalReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution: Option<ExecutionMemory>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cancelled_command: Option<CancelledCommandPresentation>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTurnKind {
    CompactedSummary,
    Assistant,
    BackgroundCommand,
    Interrupted,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TurnStats {
    pub nested_items: u64,
    pub image_count: u64,
    pub tool_count: u64,
    pub file_count: u64,
    pub string_bytes: u64,
}

#[derive(Clone)]
pub struct HistoryTurn {
    raw: Box<RawValue>,
    kind: HistoryTurnKind,
    summary: Option<CompactedSummaryTurn>,
    stats: TurnStats,
}

impl fmt::Debug for HistoryTurn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HistoryTurn")
            .field("kind", &self.kind)
            .field("encoded_bytes", &self.raw.get().len())
            .finish()
    }
}

impl PartialEq for HistoryTurn {
    fn eq(&self, other: &Self) -> bool {
        self.raw.get() == other.raw.get()
    }
}

impl HistoryTurn {
    pub(crate) fn from_wire(wire: HistoryWire) -> FxProviderResult<Self> {
        let encoded = serde_json::to_string(&wire)?;
        Self::from_json(encoded)
    }

    pub(crate) fn from_json(encoded: String) -> FxProviderResult<Self> {
        if encoded.len() > MAX_HISTORY_TURN_BYTES {
            return Err(FxProviderError::LimitExceeded {
                resource: "history turn bytes",
                actual: encoded.len() as u64,
                maximum: MAX_HISTORY_TURN_BYTES as u64,
            });
        }
        let raw = RawValue::from_string(encoded)?;
        Self::from_raw(raw)
    }

    pub(crate) fn from_legacy_normalized(
        encoded: String,
        kind: HistoryTurnKind,
        summary: Option<CompactedSummaryTurn>,
        mut stats: TurnStats,
    ) -> FxProviderResult<Self> {
        if encoded.len() > MAX_HISTORY_TURN_BYTES {
            return Err(FxProviderError::LimitExceeded {
                resource: "history turn bytes",
                actual: encoded.len() as u64,
                maximum: MAX_HISTORY_TURN_BYTES as u64,
            });
        }
        stats.string_bytes = encoded.len() as u64;
        Ok(Self {
            raw: RawValue::from_string(encoded)?,
            kind,
            summary,
            stats,
        })
    }

    fn from_raw(raw: Box<RawValue>) -> FxProviderResult<Self> {
        if raw.get().len() > MAX_HISTORY_TURN_BYTES {
            return Err(FxProviderError::LimitExceeded {
                resource: "history turn bytes",
                actual: raw.get().len() as u64,
                maximum: MAX_HISTORY_TURN_BYTES as u64,
            });
        }
        let wire: HistoryWire = serde_json::from_str(raw.get())?;
        let (kind, summary, mut stats) = validate_wire(&wire)?;
        stats.string_bytes = raw.get().len() as u64;
        Ok(Self {
            raw,
            kind,
            summary,
            stats,
        })
    }

    pub fn kind(&self) -> HistoryTurnKind {
        self.kind
    }

    pub fn raw_json(&self) -> &str {
        self.raw.get()
    }

    pub fn structured_value(&self) -> FxProviderResult<Value> {
        Ok(serde_json::from_str(self.raw.get())?)
    }

    pub fn compacted_summary(&self) -> Option<&CompactedSummaryTurn> {
        self.summary.as_ref()
    }

    pub(crate) fn stats(&self) -> TurnStats {
        self.stats
    }

    pub(crate) fn associate_event_work_id(
        &mut self,
        event_work_id: Option<&str>,
    ) -> FxProviderResult<()> {
        let mut wire: HistoryWire = serde_json::from_str(self.raw.get())?;
        let turn_work_id = wire.work_id();
        if let Some(value) = turn_work_id {
            validate_work_id(value)?;
        }
        if let Some(value) = event_work_id {
            validate_work_id(value)?;
        }
        match (event_work_id, turn_work_id) {
            (None, None) => Ok(()),
            (None, Some(_)) => Err(FxProviderError::InvalidFrame(
                "turn work_id has no authoritative event association",
            )),
            (Some(_), _) if self.kind == HistoryTurnKind::CompactedSummary => Err(
                FxProviderError::InvalidFrame("compacted summary cannot carry work association"),
            ),
            (Some(event), Some(turn)) if event == turn => Ok(()),
            (Some(_), Some(_)) => Err(FxProviderError::InvalidFrame(
                "event and turn work_id conflict",
            )),
            (Some(event), None) => {
                wire.set_work_id(event.to_owned())?;
                *self = Self::from_wire(wire)?;
                Ok(())
            }
        }
    }
}

impl Serialize for HistoryTurn {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HistoryTurn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::from_raw(raw).map_err(D::Error::custom)
    }
}

fn validate_user(user: &UserTurn) -> FxProviderResult<u64> {
    if let Some(work_id) = &user.work_id {
        validate_work_id(work_id)?;
    }
    for image in &user.images {
        if image.snapshot_path.is_some() != image.snapshot_sha256.is_some() {
            return Err(FxProviderError::InvalidState(
                "image snapshot path and digest must appear together",
            ));
        }
    }
    Ok(user.images.len() as u64)
}

impl HistoryWire {
    fn work_id(&self) -> Option<&str> {
        match self {
            Self::Assistant { user, .. }
            | Self::BackgroundCommand { user, .. }
            | Self::Interrupted { user, .. } => user.work_id.as_deref(),
            Self::CompactedSummary { .. } => None,
        }
    }

    fn set_work_id(&mut self, work_id: String) -> FxProviderResult<()> {
        match self {
            Self::Assistant { user, .. }
            | Self::BackgroundCommand { user, .. }
            | Self::Interrupted { user, .. } => {
                user.work_id = Some(work_id);
                Ok(())
            }
            Self::CompactedSummary { .. } => Err(FxProviderError::InvalidFrame(
                "compacted summary cannot carry work association",
            )),
        }
    }
}

pub(crate) fn validate_work_id(work_id: &str) -> FxProviderResult<()> {
    if work_id.is_empty() || work_id.len() > 128 || work_id.as_bytes().contains(&0) {
        return Err(FxProviderError::InvalidFrame("invalid work_id"));
    }
    Ok(())
}

fn validate_wire(
    wire: &HistoryWire,
) -> FxProviderResult<(HistoryTurnKind, Option<CompactedSummaryTurn>, TurnStats)> {
    let mut stats = TurnStats::default();
    match wire {
        HistoryWire::CompactedSummary {
            summary,
            removed_turn_count,
            compaction_count,
            root_user_messages,
            root_user_messages_complete,
            permission_feedback,
            permission_feedback_complete,
        } => {
            if root_user_messages_complete.is_some() && root_user_messages.is_none() {
                return Err(FxProviderError::InvalidState(
                    "summary root message completeness has no messages",
                ));
            }
            if permission_feedback.is_some() != permission_feedback_complete.is_some()
                || (permission_feedback.is_some()
                    && (root_user_messages.is_none() || root_user_messages_complete.is_none()))
            {
                return Err(FxProviderError::InvalidState(
                    "summary permission feedback completeness mismatch",
                ));
            }
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
                    summary: summary.clone(),
                    removed_turn_count: *removed_turn_count,
                    compaction_count: *compaction_count,
                    root_user_messages: root_user_messages.clone(),
                    root_user_messages_complete: *root_user_messages_complete,
                    permission_feedback: permission_feedback.clone(),
                    permission_feedback_complete: *permission_feedback_complete,
                }),
                stats,
            ))
        }
        HistoryWire::Assistant {
            user, execution, ..
        } => {
            stats.image_count = validate_user(user)?;
            execution.validate()?;
            stats.tool_count = execution.aggregate_items();
            stats.file_count = execution.files.len() as u64;
            stats.nested_items = stats.image_count.saturating_add(stats.tool_count);
            Ok((HistoryTurnKind::Assistant, None, stats))
        }
        HistoryWire::BackgroundCommand {
            user,
            assistant,
            execution,
            ..
        } => {
            if assistant.is_some() != execution.is_some() {
                return Err(FxProviderError::InvalidState(
                    "background extended fields must appear together",
                ));
            }
            stats.image_count = validate_user(user)?;
            if let Some(memory) = execution {
                memory.validate()?;
                stats.tool_count = memory.aggregate_items();
                stats.file_count = memory.files.len() as u64;
            }
            stats.nested_items = stats.image_count.saturating_add(stats.tool_count);
            Ok((HistoryTurnKind::BackgroundCommand, None, stats))
        }
        HistoryWire::Interrupted {
            user,
            execution,
            completed_tool_names,
            terminal_reason,
            cancelled_command,
            tool_call,
            ..
        } => {
            stats.image_count = validate_user(user)?;
            if let Some(memory) = execution {
                memory.validate()?;
                stats.tool_count = memory.aggregate_items();
                stats.file_count = memory.files.len() as u64;
            }
            if cancelled_command.is_some()
                && (terminal_reason != &Some(InterruptedTerminalReason::Cancelled)
                    || tool_call.0.is_none())
            {
                return Err(FxProviderError::InvalidState(
                    "cancelled command presentation is inconsistent",
                ));
            }
            stats.nested_items = stats
                .image_count
                .saturating_add(stats.tool_count)
                .saturating_add(completed_tool_names.len() as u64)
                .saturating_add(u64::from(tool_call.0.is_some()));
            Ok((HistoryTurnKind::Interrupted, None, stats))
        }
    }
}
