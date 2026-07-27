use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};

use super::{ErrorClass, ProtocolError, MAX_FRAME_PAYLOAD_BYTES};

pub const OUTPUT_MATERIALIZATION_CONTRACT_VERSION: u32 = 1;
pub const MAX_OUTPUT_OBSERVATIONS_PER_PAGE: usize = 512;
pub const MAX_OUTPUT_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_OUTPUT_CONTENT_BYTES_PER_PAGE: usize = MAX_OUTPUT_CONTENT_BYTES;
/// Certified provider cursors may carry a 256 KiB parser checkpoint plus
/// native position, structured metadata, and nested encoding overhead.
pub const MAX_OUTPUT_CURSOR_BYTES: usize = 704 * 1024;
pub const MAX_OUTPUT_LOCATOR_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_IDENTITY_BYTES: usize = 4 * 1024;
pub const MAX_OUTPUT_COMMAND_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_PROGRESS_SOURCES: usize = 512;

const MAX_OUTPUT_ENCODED_CONTENT_BYTES: usize = MAX_OUTPUT_CONTENT_BYTES.div_ceil(3) * 4;
// Covers the largest serialized HostEnvelope fields around a materialization
// page (u64 sequence, UUID request ID, message tag/body, punctuation). Keeping
// this separate from the exact page byte count makes page validation sufficient
// before a request ID and sequence are assigned.
const OUTPUT_PAGE_FRAME_ENVELOPE_BYTES: usize = 256;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSourceIdentity {
    pub provider: String,
    pub namespace_id: String,
    pub source_id: String,
}

impl OutputSourceIdentity {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.provider, "output provider")?;
        validate_identity(&self.namespace_id, "output source namespace")?;
        validate_identity(&self.source_id, "output source identity")
    }
}

impl fmt::Debug for OutputSourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSourceIdentity")
            .field("provider_bytes", &self.provider.len())
            .field("namespace_id_bytes", &self.namespace_id.len())
            .field("source_id_bytes", &self.source_id.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputNativeCursor {
    pub version: u32,
    pub payload_base64: String,
}

impl OutputNativeCursor {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.version == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output cursor version must be positive",
            ));
        }
        decode_bounded_base64(
            &self.payload_base64,
            MAX_OUTPUT_CURSOR_BYTES,
            "output cursor",
        )
        .map(|_| ())
    }
}

impl fmt::Debug for OutputNativeCursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputNativeCursor")
            .field("version", &self.version)
            .field("encoded_payload_bytes", &self.payload_base64.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSourceDisposition {
    AppendOrResume,
    NewSource,
    Rewrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSourceAvailability {
    Available,
    Unavailable,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputObservationKind {
    Command,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOutcome {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputOutcomeMetadata {
    pub outcome: OutputOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputNativeCoordinate {
    pub unit_key: String,
    pub native_sequence: u64,
    pub native_record_id: Option<String>,
    pub source_record_ordinal: Option<u64>,
    pub source_record_subrecord_index: Option<u32>,
    pub byte_start: Option<u64>,
    pub byte_end_exclusive: Option<u64>,
}

impl OutputNativeCoordinate {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.unit_key, "output unit key")?;
        validate_optional_identity(
            self.native_record_id.as_deref(),
            "output native record identity",
        )?;
        if self.source_record_subrecord_index.is_some() && self.source_record_ordinal.is_none() {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output subrecord requires a record ordinal",
            ));
        }
        match (self.byte_start, self.byte_end_exclusive) {
            (Some(start), Some(end)) if start <= end => Ok(()),
            (None, None) => Ok(()),
            _ => Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output byte range is incomplete or reversed",
            )),
        }
    }
}

impl fmt::Debug for OutputNativeCoordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputNativeCoordinate")
            .field("unit_key_bytes", &self.unit_key.len())
            .field("has_native_record_id", &self.native_record_id.is_some())
            .field(
                "has_source_record_ordinal",
                &self.source_record_ordinal.is_some(),
            )
            .field(
                "has_source_record_subrecord_index",
                &self.source_record_subrecord_index.is_some(),
            )
            .field(
                "has_byte_range",
                &(self.byte_start.is_some() && self.byte_end_exclusive.is_some()),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSourceLocator {
    pub version: u32,
    pub kind: String,
    pub payload_base64: String,
}

impl OutputSourceLocator {
    fn validate_and_decode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.version == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output locator version must be positive",
            ));
        }
        validate_identity(&self.kind, "output locator kind")?;
        decode_bounded_base64(
            &self.payload_base64,
            MAX_OUTPUT_LOCATOR_BYTES,
            "output locator",
        )
    }
}

impl fmt::Debug for OutputSourceLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputSourceLocator")
            .field("version", &self.version)
            .field("kind_bytes", &self.kind.len())
            .field("encoded_payload_bytes", &self.payload_base64.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOutputEvidence {
    pub source_id: String,
    pub source_epoch: u64,
    pub locator: OutputSourceLocator,
    pub coordinate: OutputNativeCoordinate,
    pub availability: OutputSourceAvailability,
}

impl ProviderOutputEvidence {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        validate_identity(&self.source_id, "provider output source identity").is_ok()
            && self.locator.validate_and_decode().is_ok()
            && self.coordinate.validate().is_ok()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRepositoryContext {
    pub repository_id: String,
    pub checkout_id: Option<String>,
    pub worktree_id: Option<String>,
    pub object_format: Option<String>,
}

impl OutputRepositoryContext {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.repository_id, "output repository identity")?;
        validate_optional_identity(self.checkout_id.as_deref(), "output checkout identity")?;
        validate_optional_identity(self.worktree_id.as_deref(), "output worktree identity")?;
        validate_optional_identity(self.object_format.as_deref(), "output object format")
    }
}

impl fmt::Debug for OutputRepositoryContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputRepositoryContext")
            .field("repository_id_bytes", &self.repository_id.len())
            .field("has_checkout_id", &self.checkout_id.is_some())
            .field("has_worktree_id", &self.worktree_id.is_some())
            .field("has_object_format", &self.object_format.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputAssociations {
    pub direct_session_id: String,
    pub root_session_id: String,
    pub parent_session_id: Option<String>,
    pub provider_session_id: Option<String>,
    pub agent_id: Option<String>,
    pub repository: Option<OutputRepositoryContext>,
}

impl OutputAssociations {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_identity(&self.direct_session_id, "output direct session identity")?;
        validate_identity(&self.root_session_id, "output root session identity")?;
        validate_optional_identity(
            self.parent_session_id.as_deref(),
            "output parent session identity",
        )?;
        validate_optional_identity(
            self.provider_session_id.as_deref(),
            "output provider session identity",
        )?;
        validate_optional_identity(self.agent_id.as_deref(), "output agent identity")?;
        if let Some(repository) = &self.repository {
            repository.validate()?;
        }
        Ok(())
    }
}

impl fmt::Debug for OutputAssociations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputAssociations")
            .field("has_parent_session_id", &self.parent_session_id.is_some())
            .field(
                "has_provider_session_id",
                &self.provider_session_id.is_some(),
            )
            .field("has_agent_id", &self.agent_id.is_some())
            .field("has_repository", &self.repository.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCommandContext {
    pub tool_name: String,
    pub command: String,
    pub working_directory: Option<String>,
}

impl OutputCommandContext {
    fn validate(&self) -> Result<(), ProtocolError> {
        if self.tool_name.is_empty()
            || self.tool_name.len() > MAX_OUTPUT_IDENTITY_BYTES
            || self.tool_name.chars().any(char::is_control)
        {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "output tool name is empty, unsafe, or exceeds its byte bound",
            ));
        }
        if self.command.len() > MAX_OUTPUT_COMMAND_BYTES || self.command.contains('\0') {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "output command is unsafe or exceeds its byte bound",
            ));
        }
        validate_optional_identity(
            self.working_directory.as_deref(),
            "output working directory",
        )
    }
}

impl fmt::Debug for OutputCommandContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutputCommandContext")
            .field("tool_name_bytes", &self.tool_name.len())
            .field("command_bytes", &self.command.len())
            .field("has_working_directory", &self.working_directory.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransientOutputContent(String);

impl TransientOutputContent {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        (bytes.len() <= MAX_OUTPUT_CONTENT_BYTES).then(|| Self(STANDARD.encode(bytes)))
    }

    pub fn decode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.0.len() > MAX_OUTPUT_ENCODED_CONTENT_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "transient output exceeds its encoded byte bound",
            ));
        }
        decode_bounded_base64(
            &self.0,
            MAX_OUTPUT_CONTENT_BYTES,
            "transient output content",
        )
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for TransientOutputContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransientOutputContent")
            .field("encoded_bytes", &self.0.len())
            .field("content", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProOutputObservation {
    pub kind: OutputObservationKind,
    pub coordinate: OutputNativeCoordinate,
    pub occurred_at_unix_ms: Option<i64>,
    pub associations: OutputAssociations,
    pub call_id: Option<String>,
    pub command: Option<OutputCommandContext>,
    pub outcome: OutputOutcomeMetadata,
    pub locator: OutputSourceLocator,
    pub content: TransientOutputContent,
}

/// Validated transient bytes passed directly from protocol validation to the
/// detector adapter. This type intentionally has no `Debug` implementation.
pub(crate) struct DecodedOutputObservation {
    pub(crate) content: Vec<u8>,
}

impl ProOutputObservation {
    fn validate_and_decode(&self) -> Result<DecodedOutputObservation, ProtocolError> {
        self.coordinate.validate()?;
        self.associations.validate()?;
        validate_optional_identity(self.call_id.as_deref(), "output call identity")?;
        if let Some(command) = &self.command {
            command.validate()?;
        }
        self.locator.validate_and_decode()?;
        let content = self.content.decode()?;
        Ok(DecodedOutputObservation { content })
    }
}

impl fmt::Debug for ProOutputObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputObservation")
            .field("coordinate", &self.coordinate)
            .field("associations", &self.associations)
            .field("has_occurred_at", &self.occurred_at_unix_ms.is_some())
            .field("has_call_id", &self.call_id.is_some())
            .field("has_command", &self.command.is_some())
            .field("locator", &self.locator)
            .field("content", &self.content)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProOutputMaterializationPage {
    pub contract_version: u32,
    pub inventory_generation: u64,
    pub source: OutputSourceIdentity,
    pub source_epoch: u64,
    pub observed_revision: String,
    pub parser_revision: String,
    pub materializer_revision: String,
    pub disposition: OutputSourceDisposition,
    pub expected_prior_source_epoch: Option<u64>,
    pub expected_prior_cursor: Option<OutputNativeCursor>,
    pub next_safe_cursor: OutputNativeCursor,
    pub terminal: bool,
    pub observations: Vec<ProOutputObservation>,
}

impl ProOutputMaterializationPage {
    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.validate_and_decode().map(|_| ())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn validate_and_decode(
        &self,
    ) -> Result<Vec<DecodedOutputObservation>, ProtocolError> {
        if self.estimated_frame_payload_bytes()? > MAX_FRAME_PAYLOAD_BYTES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "output page exceeds the maximum serialized frame payload",
            ));
        }
        if self.contract_version != OUTPUT_MATERIALIZATION_CONTRACT_VERSION {
            return Err(ProtocolError::new(
                ErrorClass::ProtocolMismatch,
                "output page does not match the materialization contract",
            ));
        }
        if self.inventory_generation == 0 {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output inventory generation must be positive",
            ));
        }
        self.source.validate()?;
        validate_identity(&self.observed_revision, "output source revision")?;
        validate_identity(&self.parser_revision, "output parser revision")?;
        validate_identity(&self.materializer_revision, "output materializer revision")?;
        if self.expected_prior_source_epoch.is_some() != self.expected_prior_cursor.is_some() {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "output expected epoch and cursor must be present together",
            ));
        }
        if let Some(cursor) = &self.expected_prior_cursor {
            cursor.validate()?;
        }
        self.next_safe_cursor.validate()?;
        match self.disposition {
            OutputSourceDisposition::NewSource
                if self.expected_prior_cursor.is_some()
                    || self.expected_prior_source_epoch.is_some() =>
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "new output source must not declare prior progress",
                ));
            }
            OutputSourceDisposition::AppendOrResume
                if self.expected_prior_source_epoch != Some(self.source_epoch) =>
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "resumed output page must compare against the current source epoch",
                ));
            }
            OutputSourceDisposition::Rewrite
                if !self
                    .expected_prior_source_epoch
                    .is_some_and(|prior| prior < self.source_epoch) =>
            {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "rewritten output source must advance its epoch",
                ));
            }
            _ => {}
        }
        if self.observations.len() > MAX_OUTPUT_OBSERVATIONS_PER_PAGE {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "output page exceeds its observation count bound",
            ));
        }
        let mut prior: Option<(u64, &str)> = None;
        let mut unit_keys = BTreeSet::new();
        let mut content_bytes = 0_usize;
        let mut decoded = Vec::with_capacity(self.observations.len());
        for observation in &self.observations {
            let current = (
                observation.coordinate.native_sequence,
                observation.coordinate.unit_key.as_str(),
            );
            if prior.is_some_and(|value| current <= value) {
                return Err(ProtocolError::new(
                    ErrorClass::Sequence,
                    "output observations must be in strict native order",
                ));
            }
            if !unit_keys.insert(observation.coordinate.unit_key.as_str()) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "output unit keys must be unique within a page",
                ));
            }
            let output = observation.validate_and_decode()?;
            content_bytes = content_bytes
                .checked_add(output.content.len())
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorClass::Bounds,
                        "output page content byte total overflowed",
                    )
                })?;
            if content_bytes > MAX_OUTPUT_CONTENT_BYTES_PER_PAGE {
                return Err(ProtocolError::new(
                    ErrorClass::Bounds,
                    "output page exceeds its complete-content byte bound",
                ));
            }
            decoded.push(output);
            prior = Some(current);
        }
        Ok(decoded)
    }

    fn estimated_frame_payload_bytes(&self) -> Result<usize, ProtocolError> {
        let mut counter = SerializedByteCounter {
            bytes: OUTPUT_PAGE_FRAME_ENVELOPE_BYTES,
        };
        serde_json::to_writer(&mut counter, self).map_err(|_| {
            ProtocolError::new(
                ErrorClass::Internal,
                "output page serialized-size estimation failed",
            )
        })?;
        Ok(counter.bytes)
    }
}

impl fmt::Debug for ProOutputMaterializationPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProOutputMaterializationPage")
            .field(
                "has_expected_prior_progress",
                &(self.expected_prior_source_epoch.is_some()
                    && self.expected_prior_cursor.is_some()),
            )
            .field("terminal", &self.terminal)
            .field("observation_count", &self.observations.len())
            .finish()
    }
}

struct SerializedByteCounter {
    bytes: usize,
}

impl Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginOutputInventoryRequest {
    pub generation: u64,
}

impl BeginOutputInventoryRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_generation(self.generation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputInventoryBegan {
    pub generation: u64,
    /// Opaque private detector/materializer bundle revision negotiated once per inventory.
    pub materializer_revision: String,
}

impl OutputInventoryBegan {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_generation(self.generation)?;
        validate_identity(&self.materializer_revision, "output materializer revision")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserveOutputSourceRequest {
    pub generation: u64,
    pub source: OutputSourceIdentity,
    pub availability: OutputSourceAvailability,
}

impl ObserveOutputSourceRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_generation(self.generation)?;
        self.source.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSourceObserved {
    pub generation: u64,
    pub source: OutputSourceIdentity,
    pub availability: OutputSourceAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishOutputInventoryRequest {
    pub generation: u64,
}

impl FinishOutputInventoryRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_generation(self.generation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputInventoryFinished {
    pub generation: u64,
    pub observed_sources: u32,
    pub unavailable_sources: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputProgressRequest {
    pub sources: Vec<OutputSourceIdentity>,
}

impl OutputProgressRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.sources.len() > MAX_OUTPUT_PROGRESS_SOURCES {
            return Err(ProtocolError::new(
                ErrorClass::Bounds,
                "output progress request exceeds its source count bound",
            ));
        }
        let mut unique = BTreeSet::new();
        for source in &self.sources {
            source.validate()?;
            if !unique.insert(source) {
                return Err(ProtocolError::new(
                    ErrorClass::InvalidRequest,
                    "output progress sources must be unique",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputSourceProgress {
    pub source: OutputSourceIdentity,
    pub source_epoch: u64,
    pub observed_revision: String,
    pub cursor: Option<OutputNativeCursor>,
    pub parser_revision: String,
    pub materializer_revision: String,
    pub terminal: bool,
    pub availability: OutputSourceAvailability,
    pub last_seen_inventory: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputProgressResult {
    pub inventory_generation: u64,
    pub inventory_complete: bool,
    pub sources: Vec<OutputSourceProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputPageMaterialized {
    pub inventory_generation: u64,
    pub source: OutputSourceIdentity,
    pub source_epoch: u64,
    pub committed_cursor: OutputNativeCursor,
    pub accepted_outputs: u32,
    pub materialized_facts: u32,
    pub materialized_evidence: u32,
    pub replayed: bool,
}

fn validate_generation(generation: u64) -> Result<(), ProtocolError> {
    if generation == 0 {
        Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            "output inventory generation must be positive",
        ))
    } else {
        Ok(())
    }
}

fn validate_identity(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty()
        || value.len() > MAX_OUTPUT_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ))
    } else {
        Ok(())
    }
}

fn validate_optional_identity(value: Option<&str>, name: &str) -> Result<(), ProtocolError> {
    value.map_or(Ok(()), |value| validate_identity(value, name))
}

fn decode_bounded_base64(
    encoded: &str,
    maximum: usize,
    name: &str,
) -> Result<Vec<u8>, ProtocolError> {
    let decoded = STANDARD.decode(encoded).map_err(|_| {
        ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{name} is not canonical base64"),
        )
    })?;
    if STANDARD.encode(&decoded) != encoded {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{name} is not canonical base64"),
        ));
    }
    if decoded.len() > maximum {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} exceeds its decoded byte bound"),
        ));
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostEnvelope, HostMessage, MAX_FRAME_PAYLOAD_BYTES};
    use uuid::Uuid;

    fn page_with_observations(
        observations: Vec<ProOutputObservation>,
    ) -> ProOutputMaterializationPage {
        ProOutputMaterializationPage {
            contract_version: OUTPUT_MATERIALIZATION_CONTRACT_VERSION,
            inventory_generation: 1,
            source: OutputSourceIdentity {
                provider: "test-provider".to_owned(),
                namespace_id: "test-namespace".to_owned(),
                source_id: "test-source".to_owned(),
            },
            source_epoch: 0,
            observed_revision: "revision-1".to_owned(),
            parser_revision: "parser-1".to_owned(),
            materializer_revision: "materializer-1".to_owned(),
            disposition: OutputSourceDisposition::NewSource,
            expected_prior_source_epoch: None,
            expected_prior_cursor: None,
            next_safe_cursor: OutputNativeCursor {
                version: 1,
                payload_base64: STANDARD.encode(b"cursor-1"),
            },
            terminal: false,
            observations,
        }
    }

    fn observation(sequence: u64, content: TransientOutputContent) -> ProOutputObservation {
        ProOutputObservation {
            kind: OutputObservationKind::Tool,
            coordinate: OutputNativeCoordinate {
                unit_key: format!("unit-{sequence}"),
                native_sequence: sequence,
                native_record_id: Some(format!("record-{sequence}")),
                source_record_ordinal: Some(sequence),
                source_record_subrecord_index: None,
                byte_start: None,
                byte_end_exclusive: None,
            },
            occurred_at_unix_ms: None,
            associations: OutputAssociations {
                direct_session_id: "direct-session".to_owned(),
                root_session_id: "root-session".to_owned(),
                parent_session_id: None,
                provider_session_id: None,
                agent_id: None,
                repository: None,
            },
            call_id: None,
            command: None,
            outcome: OutputOutcomeMetadata {
                outcome: OutputOutcome::Success,
                exit_code: Some(0),
                duration_ms: None,
            },
            locator: OutputSourceLocator {
                version: 1,
                kind: "provider-record".to_owned(),
                payload_base64: STANDARD.encode(b"locator-1"),
            },
            content,
        }
    }

    #[test]
    fn accepted_sixteen_mibibyte_content_fits_the_self_contained_frame() {
        let content = vec![b'"'; MAX_OUTPUT_CONTENT_BYTES];
        let transient = TransientOutputContent::from_bytes(&content)
            .unwrap_or_else(|| panic!("contract maximum must be accepted"));
        let page = page_with_observations(vec![observation(1, transient)]);
        page.validate()
            .unwrap_or_else(|error| panic!("maximum page must validate: {error:?}"));
        let envelope = HostEnvelope {
            sequence: 1,
            request_id: Uuid::from_u128(1),
            message: HostMessage::MaterializeOutputPage(page),
        };
        let payload_len = serde_json::to_vec(&envelope)
            .map(|payload| payload.len())
            .unwrap_or_else(|error| panic!("output page must encode: {error}"));
        assert!(payload_len < MAX_FRAME_PAYLOAD_BYTES);
        assert!(
            TransientOutputContent::from_bytes(&vec![0; MAX_OUTPUT_CONTENT_BYTES + 1]).is_none()
        );
    }

    #[test]
    fn aggregate_metadata_that_exceeds_the_frame_is_rejected() {
        let empty = TransientOutputContent::from_bytes(b"")
            .unwrap_or_else(|| panic!("empty content must be accepted"));
        let observations = (1..=MAX_OUTPUT_OBSERVATIONS_PER_PAGE)
            .map(|sequence| {
                let mut observation = observation(sequence as u64, empty.clone());
                observation.command = Some(OutputCommandContext {
                    tool_name: "exec_command".to_owned(),
                    command: "x".repeat(MAX_OUTPUT_COMMAND_BYTES),
                    working_directory: Some("/workspace".to_owned()),
                });
                observation
            })
            .collect();
        let page = page_with_observations(observations);

        assert_eq!(
            page.validate().map_err(|error| error.class),
            Err(ErrorClass::Bounds)
        );
    }

    #[test]
    fn metadata_heavy_aggregate_requires_multiple_self_contained_pages() {
        let empty = TransientOutputContent::from_bytes(b"")
            .unwrap_or_else(|| panic!("empty content must be accepted"));
        let observations = (1..=64)
            .map(|sequence| {
                let mut observation = observation(sequence, empty.clone());
                observation.command = Some(OutputCommandContext {
                    tool_name: "exec_command".to_owned(),
                    command: "x".repeat(MAX_OUTPUT_COMMAND_BYTES),
                    working_directory: Some("/workspace".to_owned()),
                });
                observation.locator.payload_base64 =
                    STANDARD.encode(vec![b'l'; MAX_OUTPUT_LOCATOR_BYTES]);
                observation
            })
            .collect();
        let page = page_with_observations(observations);
        page.validate()
            .unwrap_or_else(|error| panic!("partitioned metadata page must validate: {error:?}"));
        let envelope = HostEnvelope {
            sequence: u64::MAX,
            request_id: Uuid::from_u128(1),
            message: HostMessage::MaterializeOutputPage(page),
        };
        let payload_len = serde_json::to_vec(&envelope)
            .map(|payload| payload.len())
            .unwrap_or_else(|error| panic!("metadata page must encode: {error}"));

        assert!(payload_len < MAX_FRAME_PAYLOAD_BYTES);
        assert!(payload_len.saturating_mul(4) > MAX_FRAME_PAYLOAD_BYTES);
    }

    #[test]
    fn debug_redacts_complete_output_content() {
        let canary = b"TRANSIENT_OUTPUT_DEBUG_CANARY";
        let content = TransientOutputContent::from_bytes(canary)
            .unwrap_or_else(|| panic!("small content must be accepted"));
        let debug = format!("{content:?}");
        assert!(!debug.contains(std::str::from_utf8(canary).unwrap_or_default()));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn output_debug_redacts_paths_and_native_identities() {
        let canary = "OUTPUT_DEBUG_PRIVACY_CANARY";
        let mut observation = observation(
            1,
            TransientOutputContent::from_bytes(b"output")
                .unwrap_or_else(|| panic!("small content must be accepted")),
        );
        observation.coordinate.unit_key = format!("{canary}-unit");
        observation.coordinate.native_record_id = Some(format!("{canary}-record"));
        observation.associations.direct_session_id = format!("{canary}-direct");
        observation.associations.root_session_id = format!("{canary}-root");
        observation.associations.repository = Some(OutputRepositoryContext {
            repository_id: format!("{canary}-repository"),
            checkout_id: Some(format!("{canary}-checkout")),
            worktree_id: Some(format!("{canary}-worktree")),
            object_format: Some("sha256".to_owned()),
        });
        observation.command = Some(OutputCommandContext {
            tool_name: format!("{canary}-tool"),
            command: format!("read {canary}/secret"),
            working_directory: Some(format!("/{canary}/workspace")),
        });
        let mut page = page_with_observations(vec![observation]);
        page.source = OutputSourceIdentity {
            provider: format!("{canary}-provider"),
            namespace_id: format!("{canary}-namespace"),
            source_id: format!("{canary}-source"),
        };

        let debug = format!(
            "{page:?} {:?} {:?} {:?}",
            page.source, page.observations[0], page.observations[0].command
        );
        assert!(!debug.contains(canary));
        assert!(debug.contains("observation_count"));
        assert!(debug.contains("has_working_directory"));
    }

    #[test]
    fn inventory_begin_materializer_revision_is_required_and_bounded() {
        let missing = OutputInventoryBegan {
            generation: 1,
            materializer_revision: String::new(),
        };
        assert!(missing.validate().is_err());

        let oversized = OutputInventoryBegan {
            generation: 1,
            materializer_revision: "r".repeat(MAX_OUTPUT_IDENTITY_BYTES + 1),
        };
        assert_eq!(
            oversized.validate().map_err(|error| error.class),
            Err(ErrorClass::Bounds)
        );
    }

    #[test]
    fn certified_provider_cursor_bound_is_accepted_exactly() {
        let exact = OutputNativeCursor {
            version: 1,
            payload_base64: STANDARD.encode(vec![0_u8; MAX_OUTPUT_CURSOR_BYTES]),
        };
        exact
            .validate()
            .unwrap_or_else(|error| panic!("exact cursor bound must validate: {error:?}"));

        let oversized = OutputNativeCursor {
            version: 1,
            payload_base64: STANDARD.encode(vec![0_u8; MAX_OUTPUT_CURSOR_BYTES + 1]),
        };
        assert_eq!(
            oversized.validate().map_err(|error| error.class),
            Err(ErrorClass::Bounds)
        );
    }
}
