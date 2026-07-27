use super::*;

pub(super) const ROVODEV_NATIVE_CURSOR_VERSION: u32 = 1;
pub(super) const ROVODEV_NATIVE_FRONTIER_VERSION: u32 = 1;
pub(super) const ROVODEV_NATIVE_PARSER_REVISION: &str = "rovodev-nativepath-v1";
pub(super) const ROVODEV_NATIVE_POLICY_REVISION: u32 = 8;
pub(super) const ROVODEV_OUTPUT_PARSER_REVISION: &str = "rovodev-output-nativepath-v1";
pub(super) const ROVODEV_ROOT_CURSOR_FORMAT: &str = "rovodev-nativepath-root-v1";
pub(super) const ROVODEV_NATIVE_LOCATOR_KIND: &str = "rovodev-session-context-message-v1";
pub(super) const ROVODEV_PUBLICATION_DOMAIN: &[u8] = b"ctx-rovodev-nativepath-publication-v1\0";
pub(super) const ROVODEV_ROOT_PUBLICATION_DOMAIN: &[u8] = b"ctx-rovodev-nativepath-root-v1\0";
pub(super) const ROVODEV_RETIREMENT_PUBLICATION_DOMAIN: &[u8] =
    b"ctx-rovodev-nativepath-retirement-v1\0";
pub(super) const ROVODEV_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-rovodev-native-source-v1\0";
pub(super) const ROVODEV_PREFIX_DOMAIN: &[u8] = b"ctx-rovodev-message-prefix-v1\0";
pub(super) const ROVODEV_PAGE_MAX_UNITS: usize = 64;
pub(super) const ROVODEV_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
pub(super) const ROVODEV_MAX_FAILURES: usize = 4;
pub(super) const ROVODEV_MAX_FAILURE_BYTES: usize = 4 * 1024;
pub(super) const ROVODEV_MAX_JSON_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevFrontier {
    pub(super) version: u32,
    pub(super) next_message_index: u64,
    pub(super) prefix_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevOutputFrontier {
    pub(super) version: u32,
    pub(super) generation: u64,
    pub(super) physical_identity: String,
    pub(super) next_message_index: u64,
    pub(super) prefix_sha256: [u8; 32],
}

impl RovoDevFrontier {
    pub(super) fn start() -> Self {
        Self {
            version: ROVODEV_NATIVE_FRONTIER_VERSION,
            next_message_index: 0,
            prefix_sha256: prefix_sha256(&[]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevFailure {
    pub(super) line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevNativeCursor {
    pub(super) version: u32,
    pub(super) provider: String,
    pub(super) source_identity: String,
    pub(super) source_revision: String,
    pub(super) physical_identity: String,
    pub(super) locator_identity: String,
    pub(super) source_id: Option<Uuid>,
    pub(super) frontier: RovoDevFrontier,
    pub(super) terminal: bool,
    pub(super) missing: bool,
    pub(super) generation: u64,
    pub(super) accepted_sessions: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    pub(super) failures: Vec<RovoDevFailure>,
}

impl RovoDevNativeCursor {
    pub(super) fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }

    pub(super) fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if cursor.version != ROVODEV_NATIVE_CURSOR_VERSION
            || cursor.provider != CaptureProvider::RovoDev.as_str()
            || cursor.frontier.version != ROVODEV_NATIVE_FRONTIER_VERSION
            || cursor.source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.physical_identity.is_empty()
            || cursor.locator_identity.is_empty()
            || cursor.failures.len() > ROVODEV_MAX_FAILURES
        {
            return Err(CaptureError::InvalidPayload(
                "RovoDev NativePath cursor is inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevManifestEntry {
    pub(super) source_identity: String,
    pub(super) cursor_stream: String,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: Option<String>,
    pub(super) source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RovoDevRootManifest {
    pub(super) version: u32,
    pub(super) root_identity: String,
    pub(super) sources: Vec<RovoDevManifestEntry>,
}

#[derive(Debug)]
pub(super) struct PreparedDocument {
    pub(super) context_record: Vec<u8>,
    pub(super) context_metadata: Value,
    pub(super) metadata: Value,
    pub(super) metadata_preview: Value,
    pub(super) messages: Vec<Value>,
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) initial_failures: Vec<RovoDevFailure>,
}

#[derive(Debug)]
pub(super) struct PreparedMessage {
    pub(super) line: usize,
    pub(super) event: Option<RovoDevCoreEvent>,
    pub(super) touches: Vec<RovoDevFileTouch>,
    pub(super) rejection: Option<RovoDevFailure>,
    pub(super) estimated_bytes: usize,
}

#[derive(Debug)]
pub(super) struct RovoDevFileTouch {
    pub(super) provider_touch_index: u64,
    pub(super) provider_event_index: Option<u64>,
    pub(super) raw_source_path: Option<String>,
    pub(super) source_root: Option<String>,
    pub(super) path: String,
    pub(super) change_kind: Option<FileChangeKind>,
    pub(super) old_path: Option<String>,
    pub(super) line_count_delta: Option<i64>,
    pub(super) confidence: Confidence,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) metadata: Value,
}

impl RovoDevFileTouch {
    pub(super) fn estimated_bytes(&self) -> usize {
        self.path
            .len()
            .saturating_add(self.old_path.as_ref().map_or(0, String::len))
            .saturating_add(self.raw_source_path.as_ref().map_or(0, String::len))
            .saturating_add(self.source_root.as_ref().map_or(0, String::len))
            .saturating_add(serde_json::to_vec(&self.metadata).map_or(0, |metadata| metadata.len()))
            .saturating_add(512)
    }
}

#[derive(Debug)]
pub(super) struct PreparedPage {
    pub(super) expected_frontier: RovoDevFrontier,
    pub(super) next_frontier: RovoDevFrontier,
    pub(super) terminal: bool,
    pub(super) messages: Vec<PreparedMessage>,
    pub(super) retained_bytes: usize,
}

#[derive(Debug)]
pub(super) enum CursorPlan {
    AlreadyCommitted(RovoDevNativeCursor),
    Publish {
        expected: Option<String>,
        prior: Option<RovoDevNativeCursor>,
        generation: u64,
        start: usize,
        replacement: bool,
    },
}

#[derive(Debug)]
pub(super) struct PublishedSource {
    pub(super) cursor: RovoDevNativeCursor,
    pub(super) summary: ProviderImportSummary,
    pub(super) groups_changed: usize,
}

#[derive(Debug)]
pub(super) struct ResolvedSource {
    pub(super) source_id: Uuid,
    pub(super) session: Session,
}

#[derive(Debug)]
pub(super) struct OutputState {
    pub(super) source: OutputSourceIdentity,
    pub(super) source_epoch: u64,
    pub(super) expected_source_epoch: Option<u64>,
    pub(super) expected_frontier: Option<NativeSafeFrontier>,
    pub(super) source_start: usize,
    pub(super) disposition: ProOutputSourceDisposition,
    pub(super) requires_checkpoint: bool,
}
