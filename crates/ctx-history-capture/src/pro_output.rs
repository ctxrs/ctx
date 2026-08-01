use std::fmt;

/// A provider-native command or tool result retained during acquisition for
/// exact private projection and linked outcome extraction.
pub struct ProOutputObservation {
    pub kind: OutputObservationKind,
    pub coordinate: OutputNativeCoordinate,
    pub occurred_at_unix_ms: Option<i64>,
    pub associations: OutputAssociations,
    pub call_id: Option<String>,
    pub command: Option<OutputCommandContext>,
    pub outcome: OutputOutcomeMetadata,
    pub locator: OutputSourceLocator,
    pub content: Vec<u8>,
}

impl fmt::Debug for ProOutputObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputObservation")
            .field("kind", &self.kind)
            .field("coordinate", &self.coordinate)
            .field("has_occurred_at", &self.occurred_at_unix_ms.is_some())
            .field("associations", &self.associations)
            .field("has_call_id", &self.call_id.is_some())
            .field("command", &self.command)
            .field("outcome", &self.outcome)
            .field("locator", &self.locator)
            .field("content_bytes", &self.content.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputNativeCoordinate {
    pub unit_key: String,
    pub native_sequence: u64,
    pub native_record_id: Option<String>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_start: Option<u64>,
    pub byte_end_exclusive: Option<u64>,
}

impl fmt::Debug for OutputNativeCoordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputNativeCoordinate")
            .field("native_sequence", &self.native_sequence)
            .field("has_native_record_id", &self.native_record_id.is_some())
            .field("source_record_ordinal", &self.source_record_ordinal)
            .field(
                "source_record_subrecord_index",
                &self.source_record_subrecord_index,
            )
            .field("byte_start", &self.byte_start)
            .field("byte_end_exclusive", &self.byte_end_exclusive)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputRepositoryContext {
    pub repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub object_format: Option<String>,
}

impl fmt::Debug for OutputRepositoryContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputRepositoryContext")
            .field("has_repository_id", &!self.repository_id.is_empty())
            .field("has_checkout_id", &self.checkout_id.is_some())
            .field("has_worktree_id", &self.worktree_id.is_some())
            .field("has_object_format", &self.object_format.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputObservationKind {
    Command,
    Tool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputAssociations {
    pub direct_session_id: String,
    pub root_session_id: String,
    pub parent_session_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub repository: Option<OutputRepositoryContext>,
}

impl fmt::Debug for OutputAssociations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputAssociations")
            .field("has_direct_session", &!self.direct_session_id.is_empty())
            .field("has_root_session", &!self.root_session_id.is_empty())
            .field("has_parent_session", &self.parent_session_id.is_some())
            .field("has_provider_session", &self.provider_session_id.is_some())
            .field("has_agent", &self.agent_id.is_some())
            .field("repository", &self.repository)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputCommandContext {
    pub tool_name: String,
    pub command: String,
    pub working_directory: Option<String>,
}

impl fmt::Debug for OutputCommandContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputCommandContext")
            .field("tool_name", &self.tool_name)
            .field("command_bytes", &self.command.len())
            .field(
                "has_working_directory",
                &self.working_directory.as_ref().is_some(),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputOutcomeMetadata {
    pub outcome: OutputOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, PartialEq)]
pub struct OutputSourceLocator {
    pub version: u32,
    pub kind: String,
    pub payload: Vec<u8>,
}

impl fmt::Debug for OutputSourceLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSourceLocator")
            .field("version", &self.version)
            .field("kind_bytes", &self.kind.len())
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}
