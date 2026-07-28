use super::*;

pub(super) const AUGGIE_NATIVE_CURSOR_VERSION: u32 = 1;
pub(super) const AUGGIE_OUTPUT_FRONTIER_VERSION: u32 = 1;
pub(super) const AUGGIE_PARSER_REVISION: &str = "auggie-nativepath-json-v1";
pub(super) const AUGGIE_POLICY_REVISION: &str = "auggie-core-private-output-v1";
pub(super) const AUGGIE_CORE_EVENTS_PER_PAGE: usize = 60;
pub(super) const AUGGIE_OUTPUTS_PER_PAGE: usize = 32;
pub(super) const AUGGIE_OUTPUT_PAGE_CONTENT_BYTES: usize = 6 * 1024 * 1024;
pub(super) const AUGGIE_GENERATION_EVENT_STRIDE: u64 = 1 << 32;
pub(super) const AUGGIE_MAX_DISCOVERED_FILES: usize = 4_096;
pub(super) const AUGGIE_MAX_DISCOVERED_DIRECTORIES: usize = 4_096;
pub(super) const AUGGIE_MAX_DISCOVERY_DEPTH: usize = 64;
pub(super) const PAGE_ACCOUNTING_OVERHEAD_BYTES: usize = 256 * 1024;

pub(super) struct ParsedAuggieSource {
    pub(super) stamp: AuggieFileStamp,
    pub(super) source_revision: String,
    pub(super) content_digest: [u8; 32],
    pub(super) session: ParsedAuggieSession,
    pub(super) events: Vec<ParsedAuggieEvent>,
    pub(super) outputs: Vec<ParsedAuggieOutput>,
}

pub(super) struct ParsedAuggieSession {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) external_agent_id: Option<String>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) raw_source_path: String,
    pub(super) source_metadata: Value,
    pub(super) session_metadata: Value,
}

pub(super) struct ParsedAuggieEvent {
    pub(super) event: AuggieEvent,
    pub(super) chat_index: usize,
    pub(super) sub_index: u32,
    pub(super) message_kind: &'static str,
    pub(super) native_event_id: Option<String>,
    pub(super) json_pointer: String,
}

pub(super) struct ParsedAuggieOutput {
    pub(super) output_sequence: u32,
    pub(super) chat_index: usize,
    pub(super) node_collection: &'static str,
    pub(super) node_index: usize,
    pub(super) occurred_at: Option<DateTime<Utc>>,
    pub(super) call_id: Option<String>,
    pub(super) outcome: OutputOutcomeMetadata,
    pub(super) content: Vec<u8>,
    pub(super) content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuggieNativeCursor {
    pub(super) version: u32,
    pub(super) parser_revision: String,
    pub(super) policy_revision: String,
    pub(super) source_path: PathBuf,
    pub(super) source_revision: String,
    pub(super) generation: u64,
    pub(super) next_event: u64,
    pub(super) prefix_sha256: String,
    pub(super) terminal: bool,
    pub(super) event_count: u64,
    pub(super) provider_session_id: String,
    pub(super) rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuggieOutputFrontier {
    pub(super) version: u32,
    pub(super) source_revision: String,
    pub(super) next_output: u64,
}

#[derive(Clone)]
pub(super) struct KnownAuggieRoute {
    pub(super) path: PathBuf,
    pub(super) locator_identity: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
    pub(super) session_id: Uuid,
    pub(super) provider_session_id: String,
    pub(super) current_cursor: SyncCursor,
    pub(super) provider_cursor: AuggieNativeCursor,
}

pub(super) struct SourceCompletion {
    pub(super) changed_groups: usize,
    pub(super) terminal: bool,
    pub(super) session_id: Uuid,
}

#[derive(Clone)]
pub(super) struct RelationshipFact {
    pub(super) path: PathBuf,
    pub(super) stamp: AuggieFileStamp,
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) session_id: Uuid,
}

pub(super) enum CursorPlan {
    AlreadyCommitted(AuggieNativeCursor),
    Publish {
        expected_cursor: Option<String>,
        generation: u64,
        next_event: usize,
        rejected_records: u64,
    },
}

pub(super) fn source_revision(
    stamp: &AuggieFileStamp,
    bytes: &[u8],
    inventory_token: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-source-v1\0");
    stamp.revision_material(&mut digest);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    if let Some(token) = inventory_token {
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    }
    format!("auggie-nativepath-sha256-v1:{:x}", digest.finalize())
}

pub(super) fn event_prefix_digest(events: &[ParsedAuggieEvent]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-event-prefix-v1\0");
    for event in events {
        let encoded = released_auggie_event_encoding(&event.event)?;
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn empty_digest() -> String {
    format!("{:x}", Sha256::digest([]))
}

pub(super) fn encode_cursor(cursor: &AuggieNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

pub(super) fn decode_cursor(encoded: &str) -> Result<AuggieNativeCursor> {
    serde_json::from_str(encoded).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Auggie NativePath cursor: {error}"))
    })
}

pub(super) fn validate_native_cursor(cursor: &AuggieNativeCursor, path: &Path) -> Result<()> {
    if cursor.version != AUGGIE_NATIVE_CURSOR_VERSION
        || cursor.parser_revision != AUGGIE_PARSER_REVISION
        || cursor.policy_revision != AUGGIE_POLICY_REVISION
        || cursor.source_path != path
    {
        return Err(CaptureError::InvalidPayload(
            "Auggie NativePath cursor is incompatible with this source".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
struct ReleasedAuggieEventEncoding<'a> {
    provider_event_index: u64,
    provider_event_hash: &'a str,
    cursor: &'a str,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    fidelity: Fidelity,
    idempotency_key: String,
    payload: &'a Value,
    metadata: &'a Value,
}

pub(super) fn released_auggie_event_encoding(event: &AuggieEvent) -> Result<Vec<u8>> {
    serde_json::to_vec(&ReleasedAuggieEventEncoding {
        provider_event_index: event.provider_event_index,
        provider_event_hash: &event.provider_event_hash,
        cursor: &event.cursor,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Auggie.as_str(),
            event.provider_session_id,
            event.provider_event_index,
        ),
        payload: &event.payload,
        metadata: &event.metadata,
    })
    .map_err(CaptureError::from)
}

pub(super) fn provider_text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
