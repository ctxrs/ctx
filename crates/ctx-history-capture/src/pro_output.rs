use std::{fmt, sync::Arc};

/// Once-selected output policy for one import operation.
#[derive(Clone, Default)]
pub enum ImportProfile {
    #[default]
    CoreOnly,
    CoreAndPro(Arc<dyn ProOutputSink>),
    ProReplayOnly(Arc<dyn ProOutputSink>),
}

impl ImportProfile {
    pub(crate) fn sink(&self) -> Option<&Arc<dyn ProOutputSink>> {
        match self {
            Self::CoreOnly => None,
            Self::CoreAndPro(sink) | Self::ProReplayOnly(sink) => Some(sink),
        }
    }

    pub(crate) fn is_replay_only(&self) -> bool {
        matches!(self, Self::ProReplayOnly(_))
    }
}

impl fmt::Debug for ImportProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CoreOnly => "CoreOnly",
            Self::CoreAndPro(_) => "CoreAndPro(<output-sink>)",
            Self::ProReplayOnly(_) => "ProReplayOnly(<output-sink>)",
        })
    }
}

pub trait ProOutputSink: Send + Sync {
    fn inventory_generation(&self) -> u64;

    fn materializer_revision(&self) -> &str;

    fn observe_source(
        &self,
        source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError>;

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError>;

    fn mark_behind(&self, _error: ProOutputSinkError) {}
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProOutputSinkError {
    pub code: &'static str,
    pub message: String,
}

impl ProOutputSinkError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Debug for ProOutputSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputSinkError")
            .field("code", &self.code)
            .field("message", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for ProOutputSinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProOutputSinkError {}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OutputSourceIdentity {
    pub provider: String,
    pub namespace_id: String,
    pub source_id: String,
}

impl fmt::Debug for OutputSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSourceIdentity")
            .field("provider", &self.provider)
            .field("namespace_bytes", &self.namespace_id.len())
            .field("source_id_bytes", &self.source_id.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputNativeCursor {
    pub version: u32,
    pub payload: Vec<u8>,
}

impl fmt::Debug for OutputNativeCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputNativeCursor")
            .field("version", &self.version)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProOutputProgress {
    pub source_epoch: u64,
    pub observed_revision: String,
    pub cursor: Option<OutputNativeCursor>,
    pub parser_revision: String,
    pub materializer_revision: String,
    pub terminal: bool,
}

impl fmt::Debug for ProOutputProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputProgress")
            .field("source_epoch", &self.source_epoch)
            .field("observed_revision_bytes", &self.observed_revision.len())
            .field("has_cursor", &self.cursor.is_some())
            .field("parser_revision_bytes", &self.parser_revision.len())
            .field(
                "materializer_revision_bytes",
                &self.materializer_revision.len(),
            )
            .field("terminal", &self.terminal)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProOutputSourceDisposition {
    AppendOrResume,
    NewSource,
    Rewrite,
}

pub struct ProOutputMaterializationPage {
    pub inventory_generation: u64,
    pub source: OutputSourceIdentity,
    pub source_epoch: u64,
    pub observed_revision: String,
    pub parser_revision: String,
    pub materializer_revision: String,
    pub disposition: ProOutputSourceDisposition,
    pub expected_prior_source_epoch: Option<u64>,
    pub expected_prior_cursor: Option<OutputNativeCursor>,
    pub next_safe_cursor: OutputNativeCursor,
    pub terminal: bool,
    pub observations: Vec<ProOutputObservation>,
}

impl fmt::Debug for ProOutputMaterializationPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputMaterializationPage")
            .field("inventory_generation", &self.inventory_generation)
            .field("source", &self.source)
            .field("source_epoch", &self.source_epoch)
            .field("observed_revision_bytes", &self.observed_revision.len())
            .field("parser_revision_bytes", &self.parser_revision.len())
            .field(
                "materializer_revision_bytes",
                &self.materializer_revision.len(),
            )
            .field("disposition", &self.disposition)
            .field(
                "expected_prior_source_epoch",
                &self.expected_prior_source_epoch,
            )
            .field("expected_prior_cursor", &self.expected_prior_cursor)
            .field("next_safe_cursor", &self.next_safe_cursor)
            .field("terminal", &self.terminal)
            .field("observation_count", &self.observations.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProOutputPageResult {
    pub source_epoch: u64,
    pub committed_cursor: OutputNativeCursor,
    pub accepted_outputs: u32,
    pub materialized_facts: u32,
    pub replayed: bool,
}

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
