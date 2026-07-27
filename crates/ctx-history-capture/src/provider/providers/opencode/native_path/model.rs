use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ProOutputObservation;

pub(super) const OPENCODE_NATIVE_PAGE_MAX_UNITS: usize = 64;
pub(super) const OPENCODE_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const OPENCODE_NATIVE_STATE_SCHEMA_VERSION: u32 = 2;
pub(super) const OPENCODE_NATIVE_PARSER_REVISION: u32 = 2;
pub(super) const OPENCODE_NATIVE_PRIOR_POLICY_REVISION: u32 = 2;
pub(super) const OPENCODE_NATIVE_POLICY_REVISION: u32 = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::provider::providers::opencode) enum OpenCodeNativeProfile {
    #[default]
    CoreOnly,
    CoreAndPro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::provider::providers::opencode) enum OpenCodeNativeSchemaFamily {
    SessionMessageSeq,
    SessionMessageSynthesizedSeq,
    SessionEntry,
    LegacyMessage,
    MessagePart,
}

impl OpenCodeNativeSchemaFamily {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::SessionMessageSeq => "session_message_seq",
            Self::SessionMessageSynthesizedSeq => "session_message_synthesized_seq",
            Self::SessionEntry => "session_entry",
            Self::LegacyMessage => "legacy_message",
            Self::MessagePart => "message_part",
        }
    }

    pub(super) const fn identity_semantics(self) -> &'static str {
        match self {
            Self::MessagePart => "opencode-native-part-id-v1",
            Self::SessionMessageSeq
            | Self::SessionMessageSynthesizedSeq
            | Self::SessionEntry
            | Self::LegacyMessage => "opencode-native-message-id-v1",
        }
    }

    pub(super) const fn ordering_semantics(self) -> &'static str {
        match self {
            Self::SessionMessageSeq => "session-id,explicit-seq,message-id",
            Self::SessionMessageSynthesizedSeq | Self::SessionEntry | Self::LegacyMessage => {
                "session-id,time-created,message-id"
            }
            Self::MessagePart => "session-id,message-time,message-id,part-time,part-id",
        }
    }

    pub(super) const fn event_table(self) -> &'static str {
        match self {
            Self::SessionMessageSeq | Self::SessionMessageSynthesizedSeq => "session_message",
            Self::SessionEntry => "session_entry",
            Self::LegacyMessage => "message",
            Self::MessagePart => "part",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OpenCodeNativeSourceAuthority {
    ExactDispatchedDatabase {
        path: PathBuf,
        inventory_observation_token: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum OpenCodeNativePhysicalSourceIdentity {
    Unix { device: u64, inode: u64 },
    UnsupportedPlatform,
}

impl OpenCodeNativeSourceAuthority {
    pub(super) fn selected_path(&self) -> &std::path::Path {
        match self {
            Self::ExactDispatchedDatabase { path, .. } => path,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativeSourceSelection {
    pub(super) selected_path: PathBuf,
    pub(super) inventory_observation_token: Option<String>,
}

impl OpenCodeNativeSourceSelection {
    pub(super) fn exact(selected_path: impl Into<PathBuf>) -> Self {
        Self {
            selected_path: selected_path.into(),
            inventory_observation_token: None,
        }
    }

    pub(super) fn with_inventory_observation_token(
        mut self,
        inventory_observation_token: Option<String>,
    ) -> Self {
        self.inventory_observation_token = inventory_observation_token;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OpenCodeNativeOrder {
    ExplicitSequence {
        session_id: String,
        sequence: i64,
        message_id: String,
    },
    SynthesizedSequence {
        session_id: String,
        time_created: i64,
        message_id: String,
    },
    MessagePart {
        session_id: String,
        message_time_created: i64,
        message_id: String,
        part_time_created: i64,
        part_id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OpenCodeNativeEventKind {
    Message,
    Summary,
    Notice,
    ToolCall,
    ToolOutput,
    CommandOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeFileTouch {
    pub(super) path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeSession {
    pub(super) native_identity: String,
    pub(super) parent_identity: Option<String>,
    pub(super) root_identity: String,
    pub(super) title: Option<String>,
    pub(super) directory: Option<String>,
    pub(super) model_identity: Option<String>,
    pub(super) agent_identity: Option<String>,
    pub(super) time_created: i64,
    pub(super) time_updated: i64,
    pub(super) content_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeEvent {
    pub(super) native_identity: String,
    pub(super) message_identity: String,
    pub(super) session_identity: String,
    pub(super) native_order: OpenCodeNativeOrder,
    pub(super) kind: OpenCodeNativeEventKind,
    pub(super) role: String,
    pub(super) provider_event_index: u64,
    pub(super) legacy_provider_event_index: u64,
    pub(super) source_record_ordinal: u64,
    pub(super) time_created: i64,
    pub(super) time_updated: i64,
    pub(super) searchable_text: String,
    pub(super) body: Value,
    pub(super) content_digest: String,
    pub(super) file_touches: Vec<OpenCodeNativeFileTouch>,
    pub(super) locator: OpenCodeNativeLocator,
}

/// Retained only as a source inventory counter shape. Core pages never carry these markers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeExcludedOutput {
    pub(super) native_identity: String,
    pub(super) message_identity: String,
    pub(super) session_identity: String,
    pub(super) native_order: OpenCodeNativeOrder,
    pub(super) content_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeLocator {
    pub(super) version: u32,
    pub(super) kind: String,
    pub(super) payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OpenCodeNativeRejectionKind {
    MalformedJson,
    MalformedResultJson,
    UnsupportedStorageClass,
    OversizedRetainedContent,
    MissingSession,
    MissingMessage,
    SessionRelationshipMismatch,
    UnknownRecordType,
    InvalidTimestamp,
    RetainedParseMismatch,
}

impl OpenCodeNativeRejectionKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::MalformedJson => "malformed_json",
            Self::MalformedResultJson => "malformed_result_json",
            Self::UnsupportedStorageClass => "unsupported_storage_class",
            Self::OversizedRetainedContent => "oversized_retained_content",
            Self::MissingSession => "missing_session",
            Self::MissingMessage => "missing_message",
            Self::SessionRelationshipMismatch => "session_relationship_mismatch",
            Self::UnknownRecordType => "unknown_record_type",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::RetainedParseMismatch => "retained_parse_mismatch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeRejection {
    pub(super) native_identity: String,
    pub(super) session_identity: Option<String>,
    pub(super) native_order: Option<OpenCodeNativeOrder>,
    pub(super) kind: OpenCodeNativeRejectionKind,
    pub(super) reason: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OpenCodeNativeScanPhase {
    #[default]
    Sessions,
    Events,
    Complete,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenCodeNativeFrontier {
    pub(super) phase: OpenCodeNativeScanPhase,
    pub(super) scan_ordinal: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativeProFrontier {
    pub(super) source_event_ordinal: u64,
    pub(super) subrecord_index: u32,
    pub(super) terminal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativePageIdentity(pub(super) [u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeProPageIdentity(pub(super) [u8; 32]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodeNativePageAccounting {
    pub(super) logical_units: usize,
    pub(super) conservative_serialized_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodeNativeScanPosition {
    pub(super) phase: OpenCodeNativeScanPhase,
    pub(super) native_sessions_seen: u64,
    pub(super) native_events_seen: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodeNativeMetrics {
    pub(super) snapshot_attempts: u64,
    pub(super) source_session_rows_scanned: u64,
    pub(super) source_event_rows_scanned: u64,
    pub(super) snapshot_session_rows_indexed: u64,
    pub(super) snapshot_event_rows_indexed: u64,
    pub(super) snapshot_ordering_passes: u64,
    pub(super) prefix_session_rows_read: u64,
    pub(super) prefix_event_rows_read: u64,
    pub(super) prefix_pro_rows_read: u64,
    pub(super) indexed_session_rows_read: u64,
    pub(super) indexed_event_rows_read: u64,
    pub(super) json_records_visited: u64,
    pub(super) json_bytes_visited: u64,
    pub(super) session_page_queries: u64,
    pub(super) event_metadata_page_queries: u64,
    pub(super) retained_hydration_queries: u64,
    pub(super) native_sessions: u64,
    pub(super) native_events: u64,
    pub(super) retained_events: u64,
    pub(super) excluded_outputs: u64,
    pub(super) rejected_records: u64,
    pub(super) retained_content_cells_transferred: u64,
    pub(super) retained_content_bytes_transferred: u64,
    pub(super) output_content_cells_transferred: u64,
    pub(super) output_content_bytes_transferred: u64,
    pub(super) output_hashes_built: u64,
    pub(super) output_previews_built: u64,
    pub(super) output_touches_built: u64,
    pub(super) output_fts_documents_built: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenCodeNativeSequencePrefixEvidence {
    pub(super) count: u64,
    pub(super) max_key_digest: String,
    pub(super) rolling_digest: String,
}

impl OpenCodeNativeSequencePrefixEvidence {
    pub(super) fn is_supported(&self) -> bool {
        is_sha256_hex(&self.max_key_digest) && is_sha256_hex(&self.rolling_digest)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenCodeNativeOrderedPrefixEvidence {
    pub(super) sessions: OpenCodeNativeSequencePrefixEvidence,
    pub(super) core_events: OpenCodeNativeSequencePrefixEvidence,
    pub(super) pro_units: OpenCodeNativeSequencePrefixEvidence,
}

impl OpenCodeNativeOrderedPrefixEvidence {
    pub(super) fn is_supported(&self) -> bool {
        self.sessions.is_supported()
            && self.core_events.is_supported()
            && self.pro_units.is_supported()
    }

    pub(super) fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"ctx-opencode-nativepath-prefix-evidence-v1\0");
        for sequence in [&self.sessions, &self.core_events, &self.pro_units] {
            hasher.update(sequence.count.to_le_bytes());
            hash_evidence_str(&mut hasher, &sequence.max_key_digest);
            hash_evidence_str(&mut hasher, &sequence.rolling_digest);
        }
        hex_digest(hasher.finalize().into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeRestartPrefixComparison {
    pub(super) prior_evidence_fingerprint: String,
    pub(super) sessions_prefix_matches: bool,
    pub(super) core_events_prefix_matches: bool,
    pub(super) pro_units_prefix_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativePage {
    pub(super) identity: OpenCodeNativePageIdentity,
    pub(super) source_authority: OpenCodeNativeSourceAuthority,
    pub(super) expected_frontier: OpenCodeNativeFrontier,
    pub(super) next_frontier: OpenCodeNativeFrontier,
    pub(super) terminal: bool,
    pub(super) accounting: OpenCodeNativePageAccounting,
    pub(super) position: OpenCodeNativeScanPosition,
    pub(super) sessions: Vec<OpenCodeNativeSession>,
    pub(super) events: Vec<OpenCodeNativeEvent>,
    pub(super) excluded_outputs: Vec<OpenCodeNativeExcludedOutput>,
    pub(super) rejections: Vec<OpenCodeNativeRejection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OpenCodeNativeProRejectionKind {
    MalformedOutput,
    OversizedOutput,
    TooManySubrecords,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpenCodeNativeProRejection {
    pub(super) source_event_ordinal: u64,
    pub(super) native_identity: String,
    pub(super) subrecord_index: Option<u32>,
    pub(super) kind: OpenCodeNativeProRejectionKind,
    pub(super) reason: String,
    pub(super) locator: OpenCodeNativeLocator,
}

pub(in crate::provider::providers::opencode) struct OpenCodeNativeProOutputPage {
    pub(super) identity: OpenCodeNativeProPageIdentity,
    pub(super) source_authority: OpenCodeNativeSourceAuthority,
    pub(super) expected_frontier: OpenCodeNativeProFrontier,
    pub(super) next_frontier: OpenCodeNativeProFrontier,
    pub(super) terminal: bool,
    pub(super) accounting: OpenCodeNativePageAccounting,
    pub(super) observations: Vec<ProOutputObservation>,
    pub(super) rejections: Vec<OpenCodeNativeProRejection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativeProReplaySummary {
    pub(super) source_authority: OpenCodeNativeSourceAuthority,
    pub(super) source_generation_digest: String,
    pub(super) capability_digest: String,
    pub(super) frontier: OpenCodeNativeProFrontier,
    pub(super) complete: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenCodeNativeCompletedInventory {
    pub(super) native_sessions: u64,
    pub(super) native_events: u64,
    pub(super) retained_events: u64,
    pub(super) excluded_outputs: u64,
    pub(super) rejected_records: u64,
}

#[derive(Debug)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativeScanSummary {
    pub(super) source_authority: OpenCodeNativeSourceAuthority,
    pub(super) source_generation_digest: String,
    pub(super) physical_source_identity: OpenCodeNativePhysicalSourceIdentity,
    pub(super) capability_digest: String,
    pub(super) semantic_digest: String,
    pub(super) schema_family: OpenCodeNativeSchemaFamily,
    pub(super) identity_semantics: &'static str,
    pub(super) ordering_semantics: &'static str,
    pub(super) complete: bool,
    pub(super) profile: OpenCodeNativeProfile,
    pub(super) core_frontier: OpenCodeNativeFrontier,
    pub(super) pro_frontier: OpenCodeNativeProFrontier,
    pub(super) ordered_prefix_evidence: Box<OpenCodeNativeOrderedPrefixEvidence>,
    pub(super) restart_prefix_comparison: Option<Box<OpenCodeNativeRestartPrefixComparison>>,
    pub(super) metrics: OpenCodeNativeMetrics,
}

impl OpenCodeNativeScanSummary {
    pub(super) fn persisted_state(&self) -> OpenCodeNativePersistedState {
        OpenCodeNativePersistedState {
            schema_version: OPENCODE_NATIVE_STATE_SCHEMA_VERSION,
            parser_revision: OPENCODE_NATIVE_PARSER_REVISION,
            policy_revision: OPENCODE_NATIVE_POLICY_REVISION,
            selected_path: self.source_authority.selected_path().to_path_buf(),
            physical_source_identity: self.physical_source_identity.clone(),
            source_generation_digest: self.source_generation_digest.clone(),
            capability_digest: self.capability_digest.clone(),
            semantic_digest: self.semantic_digest.clone(),
            schema_family: self.schema_family,
            identity_semantics: self.identity_semantics.to_owned(),
            ordering_semantics: self.ordering_semantics.to_owned(),
            complete: self.complete,
            profile: self.profile,
            completed_inventory: OpenCodeNativeCompletedInventory {
                native_sessions: self.metrics.native_sessions,
                native_events: self.metrics.native_events,
                retained_events: self.metrics.retained_events,
                excluded_outputs: self.metrics.excluded_outputs,
                rejected_records: self.metrics.rejected_records,
            },
            core_frontier: self.core_frontier,
            pro_frontier: self.pro_frontier,
            ordered_prefix_evidence: (*self.ordered_prefix_evidence).clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativePersistedState {
    pub(super) schema_version: u32,
    pub(super) parser_revision: u32,
    pub(super) policy_revision: u32,
    pub(super) selected_path: PathBuf,
    pub(super) physical_source_identity: OpenCodeNativePhysicalSourceIdentity,
    pub(super) source_generation_digest: String,
    pub(super) capability_digest: String,
    pub(super) semantic_digest: String,
    pub(super) schema_family: OpenCodeNativeSchemaFamily,
    pub(super) identity_semantics: String,
    pub(super) ordering_semantics: String,
    pub(super) complete: bool,
    pub(super) profile: OpenCodeNativeProfile,
    pub(super) completed_inventory: OpenCodeNativeCompletedInventory,
    pub(super) core_frontier: OpenCodeNativeFrontier,
    pub(super) pro_frontier: OpenCodeNativeProFrontier,
    pub(super) ordered_prefix_evidence: OpenCodeNativeOrderedPrefixEvidence,
}

impl OpenCodeNativePersistedState {
    pub(super) fn is_supported(&self) -> bool {
        self.schema_version == OPENCODE_NATIVE_STATE_SCHEMA_VERSION
            && self.parser_revision == OPENCODE_NATIVE_PARSER_REVISION
            && self.policy_revision == OPENCODE_NATIVE_POLICY_REVISION
            && self.selected_path.is_absolute()
            && is_sha256_hex(&self.source_generation_digest)
            && is_sha256_hex(&self.capability_digest)
            && is_sha256_hex(&self.semantic_digest)
            && self.ordered_prefix_evidence.is_supported()
            && self.complete
            && self.core_frontier.phase == OpenCodeNativeScanPhase::Complete
    }

    pub(super) fn is_supported_cursor_migration_source(&self) -> bool {
        if self.is_supported() {
            return true;
        }
        let mut current = self.clone();
        if current.policy_revision != OPENCODE_NATIVE_PRIOR_POLICY_REVISION {
            return false;
        }
        current.policy_revision = OPENCODE_NATIVE_POLICY_REVISION;
        current.is_supported()
    }
}

fn hash_evidence_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn hex_digest(bytes: [u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::provider::providers::opencode) struct OpenCodeNativePageLimits {
    pub(super) rows: usize,
    pub(super) retained_bytes: usize,
}

impl OpenCodeNativePageLimits {
    pub(super) fn new(rows: usize, retained_bytes: usize) -> crate::Result<Self> {
        if rows == 0 || rows > OPENCODE_NATIVE_PAGE_MAX_UNITS {
            return Err(crate::CaptureError::InvalidPayload(
                "OpenCode NativePath page rows must be in 1..=64".to_owned(),
            ));
        }
        if retained_bytes == 0 || retained_bytes > OPENCODE_NATIVE_PAGE_MAX_BYTES {
            return Err(crate::CaptureError::InvalidPayload(
                "OpenCode NativePath retained page bytes must be in 1..=8 MiB".to_owned(),
            ));
        }
        Ok(Self {
            rows,
            retained_bytes,
        })
    }
}

impl Default for OpenCodeNativePageLimits {
    fn default() -> Self {
        Self {
            rows: OPENCODE_NATIVE_PAGE_MAX_UNITS,
            retained_bytes: OPENCODE_NATIVE_PAGE_MAX_BYTES,
        }
    }
}
