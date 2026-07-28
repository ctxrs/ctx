use super::*;

pub(super) const CUSTOM_NATIVE_CURSOR_VERSION: u32 = 1;
pub(super) const CUSTOM_UPSTREAM_CURSOR_VERSION: u32 = 1;
pub(super) const CUSTOM_OUTPUT_FRONTIER_VERSION: u32 = 1;
pub(super) const CUSTOM_PARSER_REVISION: &str = "ctx-history-jsonl-v1-nativepath-parser-v1";
pub(super) const CUSTOM_POLICY_REVISION: &str = "ctx-history-jsonl-v1-core-private-output-v1";
pub(super) const CUSTOM_ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";
pub(super) const CUSTOM_CORE_UNITS_PER_PAGE: usize = 128;
pub(super) const CUSTOM_UPSTREAM_CURSORS_PER_PAGE: usize = 128;
pub(super) const CUSTOM_RETIREMENT_UNITS_PER_PAGE: usize = 512;
pub(super) const CUSTOM_OUTPUTS_PER_PAGE: usize = 32;
pub(super) const CUSTOM_OUTPUT_PAGE_BYTES: usize = 6 * 1024 * 1024;
pub(super) const PAGE_ACCOUNTING_OVERHEAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) struct CustomFileStamp {
    pub(super) canonical_path: PathBuf,
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
    pub(super) source: Arc<OpenedProviderSourceFile>,
}

impl PartialEq for CustomFileStamp {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
            && self.len == other.len
            && self.modified == other.modified
            && self.readonly == other.readonly
            && self.device == other.device
            && self.inode == other.inode
    }
}

impl Eq for CustomFileStamp {}

impl CustomFileStamp {
    pub(super) fn observe(path: &Path) -> Result<Self> {
        let canonical_path = std::path::absolute(path)?;
        let source = Arc::new(open_provider_source_file(&canonical_path)?);
        let metadata = source.metadata();
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
            source,
        })
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        if self.source.revalidate().is_err() {
            return Ok(false);
        }
        let metadata = self.source.file().metadata()?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        Ok(metadata.len() == self.len
            && metadata.modified().ok() == Some(self.modified)
            && metadata.permissions().readonly() == self.readonly
            && device == self.device
            && inode == self.inode)
    }

    pub(super) fn read_all(&self) -> Result<Vec<u8>> {
        let bytes = self.source.read_all_bounded(usize::MAX)?;
        if !self.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(bytes)
    }

    pub(super) fn revision_material(&self, digest: &mut Sha256) {
        digest.update(self.canonical_path.as_os_str().as_encoded_bytes());
        digest.update(self.len.to_be_bytes());
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (1_u8, duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (0_u8, duration.as_secs(), duration.subsec_nanos())
            }
        };
        digest.update([sign]);
        digest.update(seconds.to_be_bytes());
        digest.update(nanos.to_be_bytes());
        digest.update([u8::from(self.readonly)]);
        digest.update(self.device.unwrap_or_default().to_be_bytes());
        digest.update(self.inode.unwrap_or_default().to_be_bytes());
    }
}

#[derive(Debug)]
pub(super) struct ParsedCustomHistory {
    pub(super) summary: ProviderImportSummary,
    pub(super) sources: BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    pub(super) sessions: BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    pub(super) events: Vec<(usize, CtxHistoryJsonlEventRecord)>,
    pub(super) file_touches: Vec<(usize, CtxHistoryJsonlFileTouchRecord)>,
    pub(super) edges: Vec<(usize, CtxHistoryJsonlEdgeRecord)>,
    pub(super) source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomAnchorAuthority {
    pub(super) capture_source_id: Uuid,
    pub(super) canonical_source_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomRetirementFrontier {
    pub(super) kind: String,
    pub(super) id: Uuid,
}

impl CustomRetirementFrontier {
    pub(super) fn from_store(frontier: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: frontier.kind.as_str().to_owned(),
            id: frontier.id,
        }
    }

    pub(super) fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "custom history NativePath retirement frontier is invalid".to_owned(),
                ))
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CustomCursorPhase {
    Publish {
        next_unit: u64,
    },
    Retire {
        after: Option<CustomRetirementFrontier>,
    },
    Blocked {
        next_unit: u64,
    },
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomNativeCursor {
    pub(super) version: u32,
    pub(super) parser_revision: String,
    pub(super) policy_revision: String,
    pub(super) logical_locator: String,
    pub(super) source_revision: String,
    pub(super) generation: u64,
    pub(super) phase: CustomCursorPhase,
    pub(super) anchor: Option<CustomAnchorAuthority>,
    pub(super) retired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomUpstreamCursor {
    pub(super) version: u32,
    pub(super) parser_revision: String,
    pub(super) policy_revision: String,
    pub(super) raw_cursor: String,
}

pub(super) struct CustomUpstreamCursorTarget {
    pub(super) machine_id: String,
    pub(super) stream: String,
    pub(super) raw_cursor: String,
    pub(super) observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CustomOutputFrontier {
    pub(super) version: u32,
    pub(super) source_revision: String,
    pub(super) next_output: u64,
}

pub(super) struct SessionUnit {
    pub(super) session: Session,
}

pub(super) struct EventUnit {
    pub(super) event: Event,
    pub(super) run: Option<Run>,
    pub(super) authority: ProviderEventHashAuthority,
}

pub(super) struct FileTouchUnit {
    pub(super) file: FileTouched,
}

pub(super) struct EdgeUnit {
    pub(super) actor: CanonicalActor,
    pub(super) edge: SessionEdge,
}

// Canonical entity shapes differ substantially and are consumed as one ordered stream.
#[allow(clippy::large_enum_variant)]
pub(super) enum CoreUnit {
    Session(SessionUnit),
    Event(EventUnit),
    FileTouch(FileTouchUnit),
    Edge(EdgeUnit),
}

impl CoreUnit {
    pub(super) fn retained(&self, retained: &mut NativePathRetainedSourceEntities) {
        match self {
            Self::Session(unit) => retained.session_ids.push(unit.session.id),
            Self::Event(unit) => {
                retained.event_ids.push(unit.event.id);
                if let Some(run) = &unit.run {
                    retained.run_ids.push(run.id);
                }
            }
            Self::FileTouch(unit) => retained.file_touch_ids.push(unit.file.id),
            Self::Edge(unit) => retained.session_edge_ids.push(unit.edge.id),
        }
    }

    pub(super) fn retained_bytes(&self) -> Result<usize> {
        Ok(match self {
            Self::Session(unit) => serde_json::to_vec(&unit.session)?.len(),
            Self::Event(unit) => serde_json::to_vec(&unit.event)?.len().saturating_add(
                unit.run
                    .as_ref()
                    .map(serde_json::to_vec)
                    .transpose()?
                    .map_or(0, |encoded| encoded.len()),
            ),
            Self::FileTouch(unit) => serde_json::to_vec(&unit.file)?.len(),
            Self::Edge(unit) => serde_json::to_vec(&unit.edge)?.len(),
        })
    }
}

pub(super) struct CanonicalCustomHistory {
    pub(super) units: Vec<CoreUnit>,
    pub(super) anchor_source: Option<CaptureSource>,
    pub(super) sessions: BTreeMap<(String, String), Session>,
}

pub(super) struct CustomOutput {
    pub(super) source_id: String,
    pub(super) session_id: String,
    pub(super) event_index: u64,
    pub(super) event_id: Option<String>,
    pub(super) event_hash: String,
    pub(super) event_type: EventType,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) parent_session_id: Option<String>,
    pub(super) root_session_id: String,
    pub(super) external_agent_id: Option<String>,
    pub(super) payload: Value,
}

pub(super) fn generation_key(
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
    generation: u64,
    anchor: &CustomAnchorAuthority,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::Custom,
        source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: anchor.canonical_source_identity.clone(),
        locator_identity: logical_locator.to_owned(),
        cursor_stream: stream.to_owned(),
        source_revision: source_revision.to_owned(),
        generation_id: format!("custom-history-nativepath-v1:{generation}:{source_revision}"),
    }
}

pub(super) fn dedupe_retained(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.session_edge_ids.sort_unstable();
    retained.session_edge_ids.dedup();
    retained.run_ids.sort_unstable();
    retained.run_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
    retained.file_touch_ids.sort_unstable();
    retained.file_touch_ids.dedup();
}

pub(super) fn logical_locator(path: &Path) -> String {
    let display = path.display().to_string();
    let normalized = display.replace('\\', "/");
    format!(
        "custom-history-logical-v1:{}",
        stable_capture_uuid(&normalized, "custom-history-logical-locator")
    )
}

pub(super) fn logical_reader_locator(parsed: &ParsedCustomHistory) -> String {
    let identities = parsed
        .sources
        .values()
        .map(|(_, source)| {
            (
                source.provider_key.as_str(),
                source.source_id.as_str(),
                source.source_format.as_str(),
            )
        })
        .collect::<Vec<_>>();
    format!(
        "custom-history-reader-v1:{}",
        stable_capture_uuid(
            &serde_json::to_string(&identities).unwrap_or_default(),
            "custom-history-reader-locator",
        )
    )
}

pub(super) fn custom_native_cursor_stream(logical_locator: &str) -> String {
    format!(
        "provider:custom:ctx-history-jsonl-v1:{}",
        stable_capture_uuid(logical_locator, "custom-history-native-cursor-stream")
    )
}

pub(super) fn canonical_route_identity(logical_locator: &str) -> String {
    stable_capture_uuid(logical_locator, "custom-history-native-canonical-source").to_string()
}

pub(super) fn source_revision(
    bytes: &[u8],
    stamp: Option<&CustomFileStamp>,
    inventory_token: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-source-v1\0");
    if let Some(stamp) = stamp {
        stamp.revision_material(&mut digest);
    }
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    if let Some(token) = inventory_token {
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    }
    format!(
        "custom-history-nativepath-sha256-v1:{:x}",
        digest.finalize()
    )
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
                CaptureProvider::Custom.as_str(),
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

pub(super) fn encode_cursor(cursor: &CustomNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

pub(super) fn decode_cursor(encoded: &str) -> Result<CustomNativeCursor> {
    serde_json::from_str(encoded).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid custom history NativePath cursor: {error}"))
    })
}

pub(super) fn validate_cursor(cursor: &CustomNativeCursor, logical_locator: &str) -> Result<()> {
    if cursor.version != CUSTOM_NATIVE_CURSOR_VERSION
        || cursor.parser_revision != CUSTOM_PARSER_REVISION
        || cursor.policy_revision != CUSTOM_POLICY_REVISION
        || cursor.logical_locator != logical_locator
    {
        return Err(CaptureError::InvalidPayload(
            "custom history NativePath cursor is incompatible with this source".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn publication_id(
    logical_locator: &str,
    generation: u64,
    page_start: usize,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-publication-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update((page_start as u64).to_be_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("custom-history-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn retirement_publication_id(
    logical_locator: &str,
    generation: u64,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-retirement-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "custom-history-nativepath-retirement-v1:{:x}",
        digest.finalize()
    )
}

pub(super) fn missing_publication_id(
    logical_locator: &str,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-missing-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "custom-history-nativepath-missing-v1:{:x}",
        digest.finalize()
    )
}

pub(super) fn revalidate(stamp: Option<&CustomFileStamp>) -> Result<bool> {
    stamp.map(CustomFileStamp::revalidate).unwrap_or(Ok(true))
}
