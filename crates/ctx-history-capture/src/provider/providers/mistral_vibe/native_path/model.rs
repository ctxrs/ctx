use super::*;

pub(super) const CURSOR_VERSION: u32 = 1;
pub(super) const OUTPUT_FRONTIER_VERSION: u32 = 1;
pub(super) const PAGE_MAX_UNITS: usize = 64;
pub(super) const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const PAGE_BASE_BYTES: usize = 4 * 1024;
pub(super) const EVENT_BASE_BYTES: usize = 1024;
pub(super) const OUTPUT_BASE_BYTES: usize = 1024;
pub(super) const MAX_TOUCHES_PER_RECORD: usize = PAGE_MAX_UNITS - 4;
pub(super) const MAX_REJECTION_DETAIL_BYTES: usize = 4 * 1024;
pub(super) const CURSOR_KIND: &str = "mistral-vibe-nativepath";
pub(super) const LOCATOR_REPAIR_PREVIOUS_POLICY_REVISION: u32 = 7;
pub(super) const OUTPUT_PARSER_REVISION: &str = "mistral-vibe-nativepath-output-v1";
pub(super) const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-prefix-v1\0";
pub(super) const PUBLICATION_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-publication-v1\0";
pub(super) const RETIREMENT_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-retirement-v1\0";
pub(super) const SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-source-v1\0";
pub(super) const EXACT_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-source-revision-v1\0";
pub(super) const EXACT_PATH_IDENTITY_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-path-identity-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SessionFact {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) external_agent_id: Option<String>,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) metadata: Value,
}

impl SessionFact {
    pub(super) fn from_source(
        source: &MistralVibeSessionSource,
        imported_at: DateTime<Utc>,
    ) -> Result<(Self, Option<String>)> {
        let (metadata, failure) = mistral_vibe_bounded_metadata(source, imported_at)?;
        let provider_session_id = mistral_vibe_metadata_string(&metadata, "session_id").ok_or(
            CaptureError::SystemInvariant("Mistral Vibe bounded metadata lost its session id"),
        )?;
        Ok((
            Self {
                provider_session_id,
                parent_provider_session_id: mistral_vibe_metadata_string(
                    &metadata,
                    "parent_session_id",
                ),
                external_agent_id: mistral_vibe_metadata_pointer_string(
                    &metadata,
                    &["/agent_profile/name"],
                ),
                started_at: mistral_vibe_metadata_timestamp(&metadata, "start_time")
                    .unwrap_or(imported_at),
                ended_at: mistral_vibe_metadata_timestamp(&metadata, "end_time"),
                cwd: mistral_vibe_metadata_pointer_string(
                    &metadata,
                    &["/environment/working_directory"],
                ),
                metadata,
            },
            failure,
        ))
    }

    pub(super) fn is_primary(&self) -> bool {
        self.parent_provider_session_id.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointFailure {
    pub(super) line: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub(super) version: u32,
    pub(super) capture_revision: u32,
    pub(super) policy_revision: u32,
    pub(super) provider: String,
    pub(super) machine_id: String,
    pub(super) source_format: String,
    pub(super) canonical_metadata_path: PathBuf,
    pub(super) canonical_messages_path: PathBuf,
    pub(super) metadata_stamp: FileStamp,
    pub(super) messages_stamp: FileStamp,
    pub(super) metadata_sha256: [u8; 32],
    pub(super) source_revision: String,
    pub(super) generation_identity: [u8; 32],
    pub(super) canonical_source_identity: String,
    pub(super) complete_prefix_end: u64,
    pub(super) complete_prefix_sha256: [u8; 32],
    pub(super) next_ordinal: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
    #[serde(default)]
    pub(super) rejection_details: Vec<CheckpointFailure>,
    pub(super) metadata_failure_reported: bool,
    pub(super) generation: u64,
    pub(super) session: SessionFact,
    pub(super) terminal: bool,
}

impl Checkpoint {
    pub(super) fn fresh(
        observation: &SourceObservation,
        machine_id: &str,
        source_revision: String,
        canonical_source_identity: String,
        session: SessionFact,
        generation: u64,
    ) -> Self {
        Self {
            version: CURSOR_VERSION,
            capture_revision: MISTRAL_VIBE_CAPTURE_REVISION,
            policy_revision: MISTRAL_VIBE_POLICY_REVISION,
            provider: CaptureProvider::MistralVibe.as_str().to_owned(),
            machine_id: machine_id.to_owned(),
            source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
            canonical_metadata_path: observation.canonical_metadata_path.clone(),
            canonical_messages_path: observation.canonical_messages_path.clone(),
            metadata_stamp: observation.metadata.clone(),
            messages_stamp: observation.messages.clone(),
            metadata_sha256: observation.metadata_sha256,
            source_revision,
            generation_identity: observation.generation_identity(),
            canonical_source_identity,
            complete_prefix_end: 0,
            complete_prefix_sha256: initial_prefix_digest(),
            next_ordinal: 0,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            rejection_details: Vec::new(),
            metadata_failure_reported: false,
            generation,
            session,
            terminal: false,
        }
    }

    pub(super) fn supported_at_policy(&self, policy_revision: u32) -> bool {
        self.version == CURSOR_VERSION
            && self.capture_revision == MISTRAL_VIBE_CAPTURE_REVISION
            && self.policy_revision == policy_revision
            && self.provider == CaptureProvider::MistralVibe.as_str()
            && self.source_format == MISTRAL_VIBE_SOURCE_FORMAT
            && self.rejection_details.len() <= MAX_RETAINED_PROVIDER_FAILURES
            && self
                .rejection_details
                .iter()
                .all(|failure| failure.error.len() <= MAX_REJECTION_DETAIL_BYTES)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CursorWire {
    pub(super) version: u32,
    pub(super) kind: String,
    pub(super) checkpoint: Checkpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KnownRoute {
    pub(super) locator_identity: String,
    pub(super) cursor_stream: String,
    pub(super) canonical_source_identity: String,
    pub(super) source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutputFrontier {
    pub(super) version: u32,
    pub(super) complete_prefix_end: u64,
    pub(super) next_ordinal: u64,
    pub(super) complete_prefix_sha256: [u8; 32],
    pub(super) generation_identity: [u8; 32],
}

impl OutputFrontier {
    pub(super) fn safe_frontier(&self) -> Result<NativeSafeFrontier> {
        NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(self)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }

    pub(super) fn decode(cursor: &OutputNativeCursor) -> Option<Self> {
        if cursor.version != OUTPUT_FRONTIER_VERSION {
            return None;
        }
        let frontier = serde_json::from_slice::<Self>(&cursor.payload).ok()?;
        (frontier.version == OUTPUT_FRONTIER_VERSION).then_some(frontier)
    }
}

#[derive(Debug, Clone)]
pub(super) struct TouchFact {
    pub(super) path: String,
    pub(super) old_path: Option<String>,
    pub(super) change_kind: Option<FileChangeKind>,
    pub(super) confidence: Confidence,
}

#[derive(Debug)]
pub(super) struct EventFact {
    pub(super) ordinal: u64,
    pub(super) line_number: usize,
    pub(super) byte_start: u64,
    pub(super) byte_end_exclusive: u64,
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) provider_event_hash: String,
    pub(super) text: String,
    pub(super) body: Value,
    pub(super) metadata: Value,
    pub(super) touches: Vec<TouchFact>,
}

#[derive(Debug)]
pub(super) struct Page {
    pub(super) expected: Checkpoint,
    pub(super) next: Checkpoint,
    pub(super) events: Vec<EventFact>,
    pub(super) detached_touches: Vec<DetachedTouches>,
    pub(super) rejections: Vec<ProviderImportFailure>,
    pub(super) physical_records: usize,
    pub(super) conservative_serialized_bytes: usize,
}

#[derive(Debug)]
pub(super) struct DetachedTouches {
    pub(super) ordinal: u64,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) touches: Vec<TouchFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceLifecycle {
    Fresh,
    NoOp,
    Append,
    Rewrite,
    Truncate,
    Replace,
    Migrated,
}

pub(super) struct OpenedSource {
    pub(super) source: MistralVibeSessionSource,
    pub(super) observation: SourceObservation,
    pub(super) lifecycle: SourceLifecycle,
    pub(super) checkpoint: Checkpoint,
    pub(super) target_source_revision: String,
    pub(super) target_source_identity: String,
    pub(super) target_session: SessionFact,
    pub(super) force_publication: bool,
    pub(super) metadata_failure: Option<String>,
    pub(super) reader: BufReader<File>,
    pub(super) hasher: Sha256,
}

pub(super) struct PreparedSource {
    pub(super) source: MistralVibeSessionSource,
    pub(super) observation: SourceObservation,
    pub(super) file_context: ProviderAdapterContext,
    pub(super) stream: String,
    pub(super) source_revision: String,
    pub(super) canonical_source_identity: String,
    pub(super) session: SessionFact,
    pub(super) metadata_failure: Option<String>,
}

pub(super) fn proposed_source_identity(
    context: &ProviderAdapterContext,
    messages_path: &Path,
) -> Result<String> {
    let raw_source_path = messages_path.display().to_string();
    provider_source_identity(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        context.source_root_display().as_deref(),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Mistral Vibe source has no canonical identity",
    ))
}

pub(crate) fn source_cursor_stream(path: &Path) -> Result<String> {
    let canonical_path = fs::canonicalize(path)?;
    let identity = provider_path_identity(&canonical_path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &identity,
    ))
}

pub(super) fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::MistralVibe.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

pub(super) fn encode_checkpoint(checkpoint: &Checkpoint) -> Result<String> {
    Ok(serde_json::to_string(&CursorWire {
        version: CURSOR_VERSION,
        kind: CURSOR_KIND.to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

pub(super) fn decode_native_checkpoint(encoded_store_cursor: &str) -> Result<Option<Checkpoint>> {
    decode_native_checkpoint_at_policy(encoded_store_cursor, MISTRAL_VIBE_POLICY_REVISION)
}

pub(super) fn decode_native_checkpoint_at_policy(
    encoded_store_cursor: &str,
    policy_revision: u32,
) -> Result<Option<Checkpoint>> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<CursorWire>(&encoded) else {
        return Ok(None);
    };
    if wire.version != CURSOR_VERSION
        || wire.kind != CURSOR_KIND
        || !wire.checkpoint.supported_at_policy(policy_revision)
    {
        return Ok(None);
    }
    Ok(Some(wire.checkpoint))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyParserCheckpoint {
    pub(super) metadata_revision: String,
    pub(super) metadata_failure_reported: bool,
    pub(super) next_ordinal: u64,
    #[serde(rename = "accepted_captures")]
    pub(super) _accepted_captures: u64,
    pub(super) accepted_events: u64,
    pub(super) accepted_file_touches: u64,
    pub(super) rejected_records: u64,
}

pub(super) fn migrate_released_cursor(
    encoded_store_cursor: &str,
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    session: &SessionFact,
    machine_id: &str,
    canonical_source_identity: &str,
    source_revision: &str,
) -> Result<Option<Checkpoint>> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let legacy = match CertifiedProviderCursor::decode_if_certified(&encoded) {
        Ok(Some(legacy)) => legacy,
        Ok(None) | Err(_) => return Ok(None),
    };
    if legacy.parser_revision() != 3 || legacy.policy_revision() != 6 {
        return Ok(None);
    }
    let old_observation = super::super::source::MistralVibeSessionObservation::read(source)?;
    if legacy.source_revision() != old_observation.source_revision_for_revisions(3, 6) {
        return Ok(None);
    }
    let old: LegacyParserCheckpoint = legacy.parser_checkpoint().deserialize()?;
    if old.metadata_revision != old_observation.metadata_revision() {
        return Ok(None);
    }
    let complete_prefix_end =
        crate::released_jsonl_cursor::released_jsonl_position_offset(legacy.native_position())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if complete_prefix_end > observation.messages.length {
        return Ok(None);
    }
    Ok(Some(Checkpoint {
        version: CURSOR_VERSION,
        capture_revision: MISTRAL_VIBE_CAPTURE_REVISION,
        policy_revision: MISTRAL_VIBE_POLICY_REVISION,
        provider: CaptureProvider::MistralVibe.as_str().to_owned(),
        machine_id: machine_id.to_owned(),
        source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
        canonical_metadata_path: observation.canonical_metadata_path.clone(),
        canonical_messages_path: observation.canonical_messages_path.clone(),
        metadata_stamp: observation.metadata.clone(),
        messages_stamp: observation.messages.clone(),
        metadata_sha256: observation.metadata_sha256,
        source_revision: source_revision.to_owned(),
        generation_identity: observation.generation_identity(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        complete_prefix_end,
        complete_prefix_sha256: hash_file_prefix(
            &observation.canonical_messages_path,
            complete_prefix_end,
        )?,
        next_ordinal: old.next_ordinal,
        accepted_events: old.accepted_events,
        accepted_file_touches: old.accepted_file_touches,
        rejected_records: old.rejected_records.max(legacy.rejected_records()),
        rejection_details: Vec::new(),
        metadata_failure_reported: old.metadata_failure_reported,
        generation: 0,
        session: session.clone(),
        terminal: complete_prefix_end == observation.messages.length,
    }))
}

pub(super) fn summary_from_checkpoint(checkpoint: &Checkpoint) -> ProviderImportSummary {
    let skipped_events = usize::try_from(checkpoint.accepted_events).unwrap_or(usize::MAX);
    let skipped_touches = usize::try_from(checkpoint.accepted_file_touches).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: 1_usize
            .saturating_add(skipped_events)
            .saturating_add(skipped_touches),
        failed: usize::try_from(checkpoint.rejected_records).unwrap_or(usize::MAX),
        skipped_sessions: 1,
        skipped_events,
        accepted_content_records: skipped_events.saturating_add(skipped_touches),
        failures: checkpoint
            .rejection_details
            .iter()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            })
            .collect(),
        ..ProviderImportSummary::default()
    }
}

pub(super) fn native_source_id(
    source_identity: &str,
    provider_session_id: &str,
    generation: u64,
) -> Uuid {
    stable_capture_uuid(
        &serde_json::to_string(&(
            "native-path-provider-source-v1",
            CaptureProvider::MistralVibe.as_str(),
            MISTRAL_VIBE_SOURCE_FORMAT,
            source_identity,
            provider_session_id,
            generation,
        ))
        .unwrap_or_default(),
        "source",
    )
}

pub(super) fn publication_id(page: &Page, transition: &NativePathCursorTransition) -> String {
    let mut digest = Sha256::new();
    digest.update(PUBLICATION_DOMAIN);
    digest.update(page.expected.complete_prefix_sha256);
    digest.update(page.next.complete_prefix_sha256);
    digest.update(page.expected.complete_prefix_end.to_be_bytes());
    digest.update(page.next.complete_prefix_end.to_be_bytes());
    digest.update(page.physical_records.to_be_bytes());
    digest.update(transition.key().stream().as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update(expected.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("mistral-vibe-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("mistral-vibe-retirement-v1:{:x}", digest.finalize())
}

// Each argument is an independently encoded locator component; keeping them
// explicit makes the exact-content identity inputs visible at both call sites.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_exact_locator(
    metadata: &mut Value,
    role: VerifiedContentRole,
    profile: &str,
    content: &str,
    native_record_id: &str,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> Result<()> {
    let Some(content_ref) = ContentRef::from_bytes(content.as_bytes()) else {
        return Ok(());
    };
    let mut encoded = Vec::with_capacity(80);
    encoded.extend_from_slice(&byte_start.to_be_bytes());
    encoded.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    encoded.extend_from_slice(&domain_digest(
        EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
        source_revision,
    ));
    encoded.extend_from_slice(&domain_digest(
        EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
        path_identity,
    ));
    let Some(locator) = VerifiedContentLocatorV1::new(
        role,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        crate::complete_content::jsonl::EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &encoded,
        native_record_id.to_owned(),
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(metadata, locator).ok_or(CaptureError::SystemInvariant(
        "Mistral Vibe verified-content locator collection is malformed",
    ))
}

pub(super) fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

pub(super) fn hash_stamp(digest: &mut Sha256, stamp: &FileStamp) {
    digest.update(stamp.length.to_be_bytes());
    digest.update([u8::from(stamp.modified.before_epoch)]);
    digest.update(stamp.modified.seconds.to_be_bytes());
    digest.update(stamp.modified.nanos.to_be_bytes());
    digest.update([u8::from(stamp.readonly)]);
    digest.update(stamp.device.unwrap_or(u64::MAX).to_be_bytes());
    digest.update(stamp.inode.unwrap_or(u64::MAX).to_be_bytes());
}

pub(super) fn initial_prefix_hasher() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(PREFIX_HASH_DOMAIN);
    digest
}

pub(super) fn initial_prefix_digest() -> [u8; 32] {
    prefix_digest(&initial_prefix_hasher())
}

pub(super) fn hash_file_prefix(path: &Path, length: u64) -> Result<[u8; 32]> {
    Ok(prefix_digest(&hash_prefix(
        path,
        length,
        initial_prefix_hasher(),
    )?))
}

pub(super) fn hash_prefix(path: &Path, length: u64, mut digest: Sha256) -> Result<Sha256> {
    let mut file = File::open(path)?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("Mistral Vibe prefix length exceeds usize")
        })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(digest)
}

pub(super) fn prefix_digest(digest: &Sha256) -> [u8; 32] {
    digest.clone().finalize().into()
}
