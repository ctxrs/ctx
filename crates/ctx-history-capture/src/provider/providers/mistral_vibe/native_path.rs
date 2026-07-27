use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    Session, SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::visit_all_file_touch_drafts,
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_capped_json_value, provider_local_preview,
            provider_output_event_is_failure, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
        },
        providers::native_jsonl::native_jsonl_timestamp,
        tool_input,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations,
    OutputCommandContext, OutputNativeCoordinate, OutputNativeCursor, OutputObservationKind,
    OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator,
    ProOutputObservation, ProOutputProgress, ProOutputSinkError, ProOutputSourceDisposition,
    ProviderAdapterContext, ProviderImportFailure, ProviderImportOptions, ProviderImportSummary,
    ProviderImportWorkResult, Result, MAX_PROVIDER_JSONL_LINE_BYTES, MISTRAL_VIBE_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    schema::{
        mistral_vibe_bounded_metadata, mistral_vibe_event_id, mistral_vibe_event_text,
        mistral_vibe_event_type, mistral_vibe_metadata_pointer_string,
        mistral_vibe_metadata_string, mistral_vibe_metadata_timestamp, mistral_vibe_result_content,
    },
    source::{visit_mistral_vibe_session_sources, MistralVibeSessionSource},
    MISTRAL_VIBE_CAPTURE_REVISION, MISTRAL_VIBE_POLICY_REVISION,
    MISTRAL_VIBE_RESULT_CONTENT_PROFILE,
};

const CURSOR_VERSION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const PAGE_MAX_UNITS: usize = 64;
const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const PAGE_BASE_BYTES: usize = 4 * 1024;
const EVENT_BASE_BYTES: usize = 1024;
const OUTPUT_BASE_BYTES: usize = 1024;
const MAX_TOUCHES_PER_RECORD: usize = PAGE_MAX_UNITS - 4;
const CURSOR_KIND: &str = "mistral-vibe-nativepath";
const OUTPUT_PARSER_REVISION: &str = "mistral-vibe-nativepath-output-v1";
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-prefix-v1\0";
const PUBLICATION_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-publication-v1\0";
const RETIREMENT_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-retirement-v1\0";
const SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-mistral-vibe-nativepath-source-v1\0";
const EXACT_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";
const EXACT_PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl ObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileStamp {
    length: u64,
    modified: ObservedTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: ObservedTime::from_system_time(metadata.modified()?),
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn same_physical_file(&self, current: &Self) -> bool {
        match (self.device, self.inode, current.device, current.inode) {
            (Some(device), Some(inode), Some(current_device), Some(current_inode)) => {
                device == current_device && inode == current_inode
            }
            _ => self.modified == current.modified && self.readonly == current.readonly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceObservation {
    canonical_metadata_path: PathBuf,
    canonical_messages_path: PathBuf,
    metadata: FileStamp,
    messages: FileStamp,
    metadata_sha256: [u8; 32],
    exact_content_revision: String,
}

impl SourceObservation {
    fn read(source: &MistralVibeSessionSource) -> Result<Self> {
        let canonical_metadata_path = fs::canonicalize(&source.metadata_path)?;
        let canonical_messages_path = fs::canonicalize(&source.messages_path)?;
        let metadata_file = File::open(&canonical_metadata_path)?;
        let messages_file = File::open(&canonical_messages_path)?;
        let metadata = FileStamp::from_metadata(&metadata_file.metadata()?)?;
        let messages = FileStamp::from_metadata(&messages_file.metadata()?)?;
        if metadata.length > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: source.metadata_path.clone(),
                reason: "Mistral Vibe meta.json exceeds the supported size",
            });
        }
        let metadata_sha256 = hash_file_prefix(&canonical_metadata_path, metadata.length)?;
        let exact_content_revision =
            super::source::mistral_vibe_complete_content_revision_from_admitted(
                &metadata_file.metadata()?,
                &messages_file.metadata()?,
            )?;
        Ok(Self {
            canonical_metadata_path,
            canonical_messages_path,
            metadata,
            messages,
            metadata_sha256,
            exact_content_revision,
        })
    }

    fn source_revision(&self, inventory_token: Option<&str>) -> String {
        let mut digest = Sha256::new();
        digest.update(SOURCE_REVISION_DOMAIN);
        digest.update(MISTRAL_VIBE_CAPTURE_REVISION.to_be_bytes());
        digest.update(MISTRAL_VIBE_POLICY_REVISION.to_be_bytes());
        hash_stamp(&mut digest, &self.metadata);
        hash_stamp(&mut digest, &self.messages);
        digest.update(self.metadata_sha256);
        if let Some(token) = inventory_token {
            digest.update((token.len() as u64).to_be_bytes());
            digest.update(token.as_bytes());
        }
        format!("mistral-vibe-nativepath-sha256-v1:{:x}", digest.finalize())
    }

    fn generation_identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-mistral-vibe-generation-v1\0");
        digest.update(self.metadata_sha256);
        match (self.messages.device, self.messages.inode) {
            (Some(device), Some(inode)) => {
                digest.update(device.to_be_bytes());
                digest.update(inode.to_be_bytes());
            }
            _ => {
                digest.update(self.messages.modified.seconds.to_be_bytes());
                digest.update(self.messages.modified.nanos.to_be_bytes());
            }
        }
        digest.finalize().into()
    }

    fn revalidate(&self, source: &MistralVibeSessionSource) -> Result<bool> {
        match Self::read(source) {
            Ok(current) => Ok(&current == self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFact {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    metadata: Value,
}

impl SessionFact {
    fn from_source(
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

    fn is_primary(&self) -> bool {
        self.parent_provider_session_id.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    provider: String,
    machine_id: String,
    source_format: String,
    canonical_metadata_path: PathBuf,
    canonical_messages_path: PathBuf,
    metadata_stamp: FileStamp,
    messages_stamp: FileStamp,
    metadata_sha256: [u8; 32],
    source_revision: String,
    generation_identity: [u8; 32],
    canonical_source_identity: String,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    next_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    metadata_failure_reported: bool,
    generation: u64,
    session: SessionFact,
    terminal: bool,
}

impl Checkpoint {
    fn fresh(
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
            metadata_failure_reported: false,
            generation,
            session,
            terminal: false,
        }
    }

    fn supported(&self) -> bool {
        self.version == CURSOR_VERSION
            && self.capture_revision == MISTRAL_VIBE_CAPTURE_REVISION
            && self.policy_revision == MISTRAL_VIBE_POLICY_REVISION
            && self.provider == CaptureProvider::MistralVibe.as_str()
            && self.source_format == MISTRAL_VIBE_SOURCE_FORMAT
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    version: u32,
    kind: String,
    checkpoint: Checkpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownRoute {
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputFrontier {
    version: u32,
    complete_prefix_end: u64,
    next_ordinal: u64,
    complete_prefix_sha256: [u8; 32],
    generation_identity: [u8; 32],
}

impl OutputFrontier {
    fn safe_frontier(&self) -> Result<NativeSafeFrontier> {
        NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(self)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }

    fn decode(cursor: &OutputNativeCursor) -> Option<Self> {
        if cursor.version != OUTPUT_FRONTIER_VERSION {
            return None;
        }
        let frontier = serde_json::from_slice::<Self>(&cursor.payload).ok()?;
        (frontier.version == OUTPUT_FRONTIER_VERSION).then_some(frontier)
    }
}

#[derive(Debug, Clone)]
struct TouchFact {
    path: String,
    old_path: Option<String>,
    change_kind: Option<FileChangeKind>,
    confidence: Confidence,
}

#[derive(Debug)]
struct EventFact {
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    provider_event_hash: String,
    text: String,
    body: Value,
    metadata: Value,
    touches: Vec<TouchFact>,
}

#[derive(Debug)]
struct Page {
    expected: Checkpoint,
    next: Checkpoint,
    events: Vec<EventFact>,
    detached_touches: Vec<DetachedTouches>,
    rejections: Vec<ProviderImportFailure>,
    physical_records: usize,
    conservative_serialized_bytes: usize,
}

#[derive(Debug)]
struct DetachedTouches {
    ordinal: u64,
    occurred_at: DateTime<Utc>,
    touches: Vec<TouchFact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceLifecycle {
    Fresh,
    NoOp,
    Append,
    Rewrite,
    Truncate,
    Replace,
    Migrated,
}

struct OpenedSource {
    source: MistralVibeSessionSource,
    observation: SourceObservation,
    lifecycle: SourceLifecycle,
    checkpoint: Checkpoint,
    target_source_revision: String,
    target_source_identity: String,
    target_session: SessionFact,
    force_publication: bool,
    metadata_failure: Option<String>,
    reader: BufReader<File>,
    hasher: Sha256,
}

pub(crate) fn import_mistral_vibe_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let root_missing = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    let configured_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let known_routes = load_known_routes(store, &context.machine_id, &configured_root)?;
    let mut sources = Vec::new();
    let discovered = match visit_mistral_vibe_session_sources(path, &mut |source| {
        sources.push(source);
        Ok(())
    }) {
        Ok(count) => count,
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    sources.sort_by(|left, right| left.messages_path.cmp(&right.messages_path));
    let live_streams = sources
        .iter()
        .map(|source| source_cursor_stream(&source.messages_path))
        .collect::<Result<BTreeSet<_>>>()?;
    if discovered == 0 && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mistral Vibe history root contains no complete session directories",
        });
    }
    if options.import_profile.is_replay_only() && discovered == 0 {
        if let Some(sink) = options.import_profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                if root_missing {
                    "mistral_vibe_root_missing"
                } else {
                    "mistral_vibe_source_missing"
                },
                "Mistral Vibe source is unavailable for output replay",
            ));
        }
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;

        for source in sources {
            let file_context = ProviderAdapterContext {
                machine_id: context.machine_id.clone(),
                source_path: Some(source.messages_path.clone()),
                source_root: Some(configured_root.clone()),
                imported_at: context.imported_at,
            };
            let stream = source_cursor_stream(&source.messages_path)?;
            let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
            let observation = SourceObservation::read(&source)?;
            let source_revision =
                observation.source_revision(options.inventory_observation_token.as_deref());
            let (session, metadata_failure) =
                SessionFact::from_source(&source, context.imported_at)?;
            let canonical_source_identity =
                proposed_source_identity(&file_context, &source.messages_path)?;
            let mut opened = open_source(
                source,
                observation,
                &context.machine_id,
                source_revision,
                canonical_source_identity,
                session,
                metadata_failure,
                stored.as_ref(),
            )?;

            if options.import_profile.is_replay_only() {
                let Some(committed) = stored.as_ref() else {
                    if let Some(sink) = options.import_profile.sink() {
                        sink.mark_behind(ProOutputSinkError::new(
                            "mistral_vibe_core_missing",
                            "Mistral Vibe output replay requires committed NativePath Core",
                        ));
                    }
                    continue;
                };
                let Some(checkpoint) = decode_native_checkpoint(&committed.cursor)? else {
                    if let Some(sink) = options.import_profile.sink() {
                        sink.mark_behind(ProOutputSinkError::new(
                            "mistral_vibe_core_upgrade_required",
                            "Mistral Vibe output replay requires a NativePath Core cursor",
                        ));
                    }
                    continue;
                };
                replay_outputs_or_mark_behind(
                    &opened.source,
                    &opened.observation,
                    &checkpoint,
                    &options.import_profile,
                );
                continue;
            }

            if opened.lifecycle == SourceLifecycle::NoOp {
                let mut skipped = summary_from_checkpoint(&opened.checkpoint);
                skipped.set_work_result(ProviderImportWorkResult::NoOp);
                summary.merge_from(skipped);
            } else {
                loop {
                    let Some(page) = next_core_page(&mut opened)? else {
                        break;
                    };
                    let terminal = page.next.terminal;
                    let page_summary = publish_core_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        &file_context,
                        &options,
                        &opened.source,
                        &opened.observation,
                        page,
                    )?;
                    if page_summary.work_result() == ProviderImportWorkResult::Changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    summary.merge_from(page_summary);
                    if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        && changed_groups != 0
                        && !terminal
                    {
                        summary.work_remaining = true;
                        return Ok(summary);
                    }
                    if terminal {
                        break;
                    }
                }
            }

            let stored = store
                .get_sync_cursor(None, &context.machine_id, &stream)?
                .ok_or(CaptureError::SystemInvariant(
                    "Mistral Vibe Core publication lost its cursor",
                ))?;
            let checkpoint =
                decode_native_checkpoint(&stored.cursor)?.ok_or(CaptureError::SystemInvariant(
                    "Mistral Vibe Core publication stored a non-NativePath cursor",
                ))?;
            if options.import_profile.sink().is_some() {
                replay_outputs_or_mark_behind(
                    &opened.source,
                    &opened.observation,
                    &checkpoint,
                    &options.import_profile,
                );
            }
        }

        if options.import_profile.is_replay_only() {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }

        if !known_routes.is_empty() {
            let current_routes = load_known_routes(store, &context.machine_id, &configured_root)?;
            let current_sources = current_routes
                .iter()
                .filter(|entry| live_streams.contains(&entry.cursor_stream))
                .map(|entry| entry.canonical_source_identity.as_str())
                .collect::<BTreeSet<_>>();
            for missing in known_routes.iter().filter(|entry| {
                !live_streams.contains(&entry.cursor_stream)
                    && !current_sources.contains(entry.canonical_source_identity.as_str())
            }) {
                summary.merge_from(retire_missing_source(
                    store,
                    &bulk_guard,
                    &context,
                    missing,
                    if root_missing {
                        ProviderSourceRouteRetirementReason::RootMissing
                    } else {
                        ProviderSourceRouteRetirementReason::SourceMissing
                    },
                )?);
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn open_source(
    source: MistralVibeSessionSource,
    observation: SourceObservation,
    machine_id: &str,
    source_revision: String,
    canonical_source_identity: String,
    mut session: SessionFact,
    metadata_failure: Option<String>,
    stored: Option<&SyncCursor>,
) -> Result<OpenedSource> {
    let mut generation = 0_u64;
    let mut prior = None;
    let mut lifecycle = SourceLifecycle::Fresh;
    let mut force_publication = stored.is_none();
    if let Some(stored) = stored {
        match decode_native_checkpoint(&stored.cursor)? {
            Some(checkpoint) => {
                generation = checkpoint.generation;
                prior = Some(checkpoint);
            }
            None => {
                if let Some(migrated) = migrate_released_cursor(
                    &stored.cursor,
                    &source,
                    &observation,
                    &session,
                    machine_id,
                    &canonical_source_identity,
                    &source_revision,
                )? {
                    force_publication = true;
                    generation = migrated.generation;
                    prior = Some(migrated);
                    lifecycle = SourceLifecycle::Migrated;
                } else {
                    force_publication = true;
                    generation = generation.saturating_add(1);
                    lifecycle = SourceLifecycle::Replace;
                }
            }
        }
    }

    let mut checkpoint = Checkpoint::fresh(
        &observation,
        machine_id,
        source_revision.clone(),
        canonical_source_identity.clone(),
        session.clone(),
        generation,
    );
    let mut hasher = initial_prefix_hasher();
    if let Some(previous) = prior {
        session.started_at = session.started_at.min(previous.session.started_at);
        let migration_required = lifecycle == SourceLifecycle::Migrated;
        let same_paths = previous.canonical_metadata_path == observation.canonical_metadata_path
            && previous.canonical_messages_path == observation.canonical_messages_path;
        let same_metadata = previous.metadata_sha256 == observation.metadata_sha256;
        let same_physical = previous
            .messages_stamp
            .same_physical_file(&observation.messages);
        let enough_bytes = observation.messages.length >= previous.complete_prefix_end;
        let prefix_valid = same_paths
            && same_metadata
            && same_physical
            && enough_bytes
            && hash_file_prefix(
                &observation.canonical_messages_path,
                previous.complete_prefix_end,
            )? == previous.complete_prefix_sha256;
        if prefix_valid {
            hasher = hash_prefix(
                &observation.canonical_messages_path,
                previous.complete_prefix_end,
                initial_prefix_hasher(),
            )?;
            checkpoint = previous;
            let fully_consumed = checkpoint.complete_prefix_end == observation.messages.length;
            let unchanged = fully_consumed
                && checkpoint.terminal
                && checkpoint.metadata_stamp == observation.metadata
                && checkpoint.messages_stamp == observation.messages
                && checkpoint.metadata_sha256 == observation.metadata_sha256
                && checkpoint.source_revision == source_revision
                && checkpoint.generation_identity == observation.generation_identity()
                && checkpoint.canonical_source_identity == canonical_source_identity
                && checkpoint.session == session;
            lifecycle = if unchanged && !force_publication && !migration_required {
                SourceLifecycle::NoOp
            } else {
                SourceLifecycle::Append
            };
        } else {
            force_publication = true;
            lifecycle = if observation.messages.length < previous.complete_prefix_end {
                SourceLifecycle::Truncate
            } else if same_physical {
                SourceLifecycle::Rewrite
            } else {
                SourceLifecycle::Replace
            };
            checkpoint.generation =
                previous
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Mistral Vibe source generation overflowed",
                    ))?;
        }
    }

    let mut file = File::open(&observation.canonical_messages_path)?;
    if FileStamp::from_metadata(&file.metadata()?)? != observation.messages {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(checkpoint.complete_prefix_end))?;
    Ok(OpenedSource {
        source,
        observation,
        lifecycle,
        checkpoint,
        target_source_revision: source_revision,
        target_source_identity: canonical_source_identity,
        target_session: session,
        force_publication,
        metadata_failure,
        reader: BufReader::new(file),
        hasher,
    })
}

fn next_core_page(opened: &mut OpenedSource) -> Result<Option<Page>> {
    if opened.lifecycle == SourceLifecycle::NoOp {
        return Ok(None);
    }
    let expected = opened.checkpoint.clone();
    let mut next = expected.clone();
    let mut events = Vec::new();
    let mut detached_touches = Vec::new();
    let mut rejections = Vec::new();
    let mut physical_records = 0_usize;
    let mut logical_units = 0_usize;
    let mut serialized_bytes = PAGE_BASE_BYTES;

    next.canonical_metadata_path = opened.observation.canonical_metadata_path.clone();
    next.canonical_messages_path = opened.observation.canonical_messages_path.clone();
    next.metadata_stamp = opened.observation.metadata.clone();
    next.messages_stamp = opened.observation.messages.clone();
    next.metadata_sha256 = opened.observation.metadata_sha256;
    next.source_revision = opened.target_source_revision.clone();
    next.generation_identity = opened.observation.generation_identity();
    next.canonical_source_identity = opened.target_source_identity.clone();
    next.session = opened.target_session.clone();
    next.terminal = false;

    if !next.metadata_failure_reported {
        if let Some(failure) = opened.metadata_failure.clone() {
            rejections.push(ProviderImportFailure {
                line: 0,
                error: failure,
            });
            next.rejected_records = next.rejected_records.saturating_add(1);
            logical_units = logical_units.saturating_add(1);
        }
        next.metadata_failure_reported = true;
    }

    while physical_records < PAGE_MAX_UNITS && logical_units < PAGE_MAX_UNITS {
        let start = next.complete_prefix_end;
        let ordinal = next.next_ordinal;
        let hasher_before = opened.hasher.clone();
        let line = read_bounded_line(
            &mut opened.reader,
            &mut opened.hasher,
            opened.observation.messages.length,
            start,
        )?;
        let (bytes, end) = match line {
            Line::EndOfFile => {
                next.terminal = true;
                break;
            }
            Line::IncompleteTail => {
                opened.hasher = hasher_before;
                opened.reader.seek(SeekFrom::Start(start))?;
                next.terminal = false;
                break;
            }
            Line::Oversized { end } => {
                let failure = ProviderImportFailure {
                    line: usize::try_from(ordinal)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: format!(
                        "{}:{} exceeds the {} byte JSONL record limit",
                        opened.source.messages_path.display(),
                        ordinal.saturating_add(1),
                        MAX_PROVIDER_JSONL_LINE_BYTES
                    ),
                };
                let failure_bytes = failure.error.len().saturating_add(128);
                if physical_records != 0
                    && serialized_bytes.saturating_add(failure_bytes) > PAGE_MAX_BYTES
                {
                    opened.hasher = hasher_before;
                    opened.reader.seek(SeekFrom::Start(start))?;
                    break;
                }
                next.complete_prefix_end = end;
                next.next_ordinal = next.next_ordinal.saturating_add(1);
                next.rejected_records = next.rejected_records.saturating_add(1);
                rejections.push(failure);
                physical_records = physical_records.saturating_add(1);
                logical_units = logical_units.saturating_add(1);
                serialized_bytes = serialized_bytes.saturating_add(failure_bytes);
                continue;
            }
            Line::Complete { bytes, end } => (bytes, end),
        };

        let projected = project_core_record(opened, &bytes, ordinal, start, end)?;
        opened.target_session.started_at =
            opened.target_session.started_at.min(projected.occurred_at);
        next.session.started_at = opened.target_session.started_at;
        let projected_units = projected
            .event
            .as_ref()
            .map_or(0, |event| 1_usize.saturating_add(event.touches.len()))
            .saturating_add(projected.detached_touches.len())
            .saturating_add(usize::from(projected.rejection.is_some()));
        let projected_bytes = projected.serialized_bytes;
        if physical_records != 0
            && (logical_units.saturating_add(projected_units) > PAGE_MAX_UNITS
                || serialized_bytes.saturating_add(projected_bytes) > PAGE_MAX_BYTES)
        {
            opened.hasher = hasher_before;
            opened.reader.seek(SeekFrom::Start(start))?;
            break;
        }
        if projected_units > PAGE_MAX_UNITS || projected_bytes > PAGE_MAX_BYTES {
            let failure = ProviderImportFailure {
                line: usize::try_from(ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: format!(
                    "{}:{} expands past a Mistral Vibe NativePath page",
                    opened.source.messages_path.display(),
                    ordinal.saturating_add(1)
                ),
            };
            next.complete_prefix_end = end;
            next.next_ordinal = next.next_ordinal.saturating_add(1);
            next.rejected_records = next.rejected_records.saturating_add(1);
            rejections.push(failure);
            physical_records = physical_records.saturating_add(1);
            logical_units = logical_units.saturating_add(1);
            serialized_bytes = serialized_bytes.saturating_add(128);
            continue;
        }

        next.complete_prefix_end = end;
        next.next_ordinal = next.next_ordinal.saturating_add(1);
        next.accepted_events = next
            .accepted_events
            .saturating_add(u64::from(projected.event.is_some()));
        next.accepted_file_touches = next
            .accepted_file_touches
            .saturating_add(
                projected
                    .event
                    .as_ref()
                    .map_or(0, |event| event.touches.len() as u64),
            )
            .saturating_add(projected.detached_touches.len() as u64);
        if let Some(failure) = projected.rejection {
            next.rejected_records = next.rejected_records.saturating_add(1);
            rejections.push(failure);
        }
        if let Some(event) = projected.event {
            events.push(event);
        }
        if !projected.detached_touches.is_empty() {
            detached_touches.push(DetachedTouches {
                ordinal,
                occurred_at: projected.occurred_at,
                touches: projected.detached_touches,
            });
        }
        physical_records = physical_records.saturating_add(1);
        logical_units = logical_units.saturating_add(projected_units);
        serialized_bytes = serialized_bytes.saturating_add(projected_bytes);
    }

    next.complete_prefix_sha256 = prefix_digest(&opened.hasher);
    next.messages_stamp = opened.observation.messages.clone();
    next.metadata_stamp = opened.observation.metadata.clone();
    if next.complete_prefix_end == opened.observation.messages.length {
        next.terminal = true;
    }
    let checkpoint_changed = next != expected;
    opened.checkpoint = next.clone();
    if !checkpoint_changed && physical_records == 0 && !opened.force_publication {
        return Ok(None);
    }
    opened.force_publication = false;
    Ok(Some(Page {
        expected,
        next,
        events,
        detached_touches,
        rejections,
        physical_records,
        conservative_serialized_bytes: serialized_bytes,
    }))
}

struct ProjectedRecord {
    event: Option<EventFact>,
    detached_touches: Vec<TouchFact>,
    occurred_at: DateTime<Utc>,
    rejection: Option<ProviderImportFailure>,
    serialized_bytes: usize,
}

fn project_core_record(
    opened: &OpenedSource,
    bytes: &[u8],
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<ProjectedRecord> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(ProjectedRecord {
            event: None,
            detached_touches: Vec::new(),
            occurred_at: opened.target_session.started_at,
            rejection: None,
            serialized_bytes: 16,
        });
    }
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe line number exceeds platform limits",
        ))?;
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            let reason = format!(
                "malformed JSONL in {}: {error}",
                opened.source.messages_path.display()
            );
            return Ok(ProjectedRecord {
                event: None,
                detached_touches: Vec::new(),
                occurred_at: opened.target_session.started_at,
                rejection: Some(ProviderImportFailure {
                    line: line_number,
                    error: reason.clone(),
                }),
                serialized_bytes: reason.len().saturating_add(128),
            });
        }
    };
    let role_name = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = mistral_vibe_event_type(role_name, &value);
    let occurred_at = native_jsonl_timestamp(&value).unwrap_or(opened.target_session.started_at);
    let touches = collect_touches(&value)?;
    let touch_limit_exceeded = touches.limit_exceeded;
    let touches = touches.touches;
    let output = (event_type == EventType::ToolOutput).then(|| {
        output_metadata(
            &value,
            line_number,
            role_name,
            opened.target_session.cwd.as_deref(),
        )
    });
    let retain_event = output.as_ref().is_none_or(|output| {
        matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    });
    let (event_touches, detached_touches) = if retain_event {
        (touches, Vec::new())
    } else {
        (Vec::new(), touches)
    };
    let event = retain_event
        .then(|| {
            build_event_fact(
                opened,
                bytes,
                &value,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                role_name,
                event_type,
                occurred_at,
                event_touches,
                output.as_ref(),
            )
        })
        .transpose()?;
    let rejection = touch_limit_exceeded.then(|| ProviderImportFailure {
        line: line_number,
        error: "Mistral Vibe record exceeds the NativePath file-touch page limit".to_owned(),
    });
    let serialized_bytes = bytes
        .len()
        .saturating_add(EVENT_BASE_BYTES)
        .saturating_add(event.as_ref().map_or(0, |event| {
            event.touches.iter().map(|touch| touch.path.len()).sum()
        }))
        .saturating_add(
            detached_touches
                .iter()
                .map(|touch| touch.path.len())
                .sum::<usize>(),
        )
        .saturating_add(rejection.as_ref().map_or(0, |failure| failure.error.len()));
    Ok(ProjectedRecord {
        event,
        detached_touches,
        occurred_at,
        rejection,
        serialized_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_event_fact(
    opened: &OpenedSource,
    record_bytes: &[u8],
    value: &Value,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    role_name: &str,
    mut event_type: EventType,
    occurred_at: DateTime<Utc>,
    touches: Vec<TouchFact>,
    output: Option<&OutputMetadata>,
) -> Result<EventFact> {
    let provider_event_hash = mistral_vibe_event_id(value, line_number, role_name);
    let mut text = mistral_vibe_event_text(role_name, value, event_type);
    let mut body = value.clone();
    let mut metadata = json!({
        "source": MISTRAL_VIBE_SOURCE_FORMAT,
        "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
        "line": line_number,
        "role": role_name,
        "message_id": value.get("message_id").and_then(Value::as_str),
        "reasoning_message_id": value.get("reasoning_message_id").and_then(Value::as_str),
        "tool_call_id": value.get("tool_call_id").and_then(Value::as_str),
        "name": value.get("name").and_then(Value::as_str),
        "tool_calls": value
            .get("tool_calls")
            .map(|calls| provider_capped_json_value(calls, PROVIDER_MAX_PREVIEW_CHARS)),
        "images": value
            .get("images")
            .map(|images| provider_capped_json_value(images, PROVIDER_MAX_PREVIEW_CHARS)),
        "agent_profile": opened.target_session.external_agent_id,
    });
    if let Some(output) = output {
        if output.kind == OutputObservationKind::Command {
            event_type = EventType::CommandOutput;
        }
        let content = mistral_vibe_result_content(value).unwrap_or_default();
        let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
        text = format!(
            "Mistral Vibe failed {} output",
            value.get("name").and_then(Value::as_str).unwrap_or("tool")
        );
        body = json!({
            "result_outcome": if output.outcome.outcome == OutputOutcome::Timeout { "timeout" } else { "failure" },
            "output_bytes": content.len(),
            "output_preview": preview,
            "call_id": output.call_id,
            "exit_code": output.outcome.exit_code,
            "duration_ms": output.outcome.duration_ms,
            "timed_out": output.outcome.outcome == OutputOutcome::Timeout,
            "tool": output.command.as_ref().map(|command| command.tool_name.as_str()),
            "command": output.command.as_ref().map(|command| command.command.as_str()),
            "cwd": output.command.as_ref().and_then(|command| command.working_directory.as_deref()),
        });
        attach_exact_locator(
            &mut metadata,
            VerifiedContentRole::ResultBody,
            MISTRAL_VIBE_RESULT_CONTENT_PROFILE,
            &content,
            &provider_event_hash,
            record_bytes,
            byte_start,
            byte_end_exclusive,
            &opened.observation.exact_content_revision,
            &provider_path_identity(&opened.observation.canonical_messages_path)?,
        )?;
        if let Some(content_ref) = ContentRef::from_bytes(content.as_bytes()) {
            body["result_content_ref"] = serde_json::to_value(content_ref)?;
        }
    } else if event_type == EventType::Message {
        let full_text = mistral_vibe_event_text(role_name, value, event_type);
        if full_text.chars().count() > PROVIDER_MAX_TEXT_CHARS
            && full_text.len() <= COMPLETE_CONTENT_MAX_BODY_BYTES
        {
            let Some(profile) = verified_content_profile(
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                CompleteContentSourceFamily::Jsonl,
                VerifiedContentRole::MessageBody,
            ) else {
                return Err(CaptureError::SystemInvariant(
                    "Mistral Vibe message route has no complete-content profile",
                ));
            };
            attach_exact_locator(
                &mut metadata,
                VerifiedContentRole::MessageBody,
                profile,
                &full_text,
                &provider_event_hash,
                record_bytes,
                byte_start,
                byte_end_exclusive,
                &opened.observation.exact_content_revision,
                &provider_path_identity(&opened.observation.canonical_messages_path)?,
            )?;
        }
    }
    Ok(EventFact {
        ordinal,
        line_number,
        byte_start,
        byte_end_exclusive,
        event_type,
        role: provider_role(Some(role_name)),
        occurred_at,
        provider_event_hash,
        text,
        body,
        metadata,
        touches,
    })
}

struct CollectedTouches {
    touches: Vec<TouchFact>,
    limit_exceeded: bool,
}

fn collect_touches(value: &Value) -> Result<CollectedTouches> {
    let mut seen = BTreeSet::new();
    let mut touches = Vec::new();
    let result = visit_all_file_touch_drafts(value, |draft| {
        let key = (
            draft.path.clone(),
            draft.old_path.clone(),
            draft.change_kind.map(|kind| format!("{kind:?}")),
        );
        if !seen.insert(key) {
            return Ok(());
        }
        if touches.len() >= MAX_TOUCHES_PER_RECORD {
            return Err(());
        }
        touches.push(TouchFact {
            path: draft.path,
            old_path: draft.old_path,
            change_kind: draft.change_kind,
            confidence: draft.confidence,
        });
        Ok(())
    });
    Ok(CollectedTouches {
        touches,
        limit_exceeded: result.is_err(),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    page: Page,
) -> Result<ProviderImportSummary> {
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let stream = source_cursor_stream(&observation.canonical_messages_path)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let next_provider_cursor = encode_checkpoint(&page.next)?;
    let next_cursor = provider_sync_cursor(
        &context.machine_id,
        stream.clone(),
        next_provider_cursor,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = publication_id(&page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.skipped_events = page.events.len();
        summary.skipped = page.events.len();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let raw_source_path = observation.canonical_messages_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&observation.canonical_messages_path)?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::MistralVibe,
            source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream.clone(),
            proposed_source_identity: page.next.canonical_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: page.next.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = if page.next.generation == 0 {
        committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::MistralVibe,
                MISTRAL_VIBE_SOURCE_FORMAT,
                &context.machine_id,
                &resolution.canonical_source_identity,
                &page.next.session.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                native_source_id(
                    &resolution.canonical_source_identity,
                    &page.next.session.provider_session_id,
                    page.next.generation,
                )
            })
    } else {
        native_source_id(
            &resolution.canonical_source_identity,
            &page.next.session.provider_session_id,
            page.next.generation,
        )
    };
    let capture_source = capture_source(
        context,
        &page.next.session,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        &page.next.source_revision,
    );
    group.upsert_capture_source(&capture_source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session = canonical_session(
        committed_store,
        context,
        options,
        &page.next.session,
        source_id,
        &resolution.canonical_source_identity,
    )?;
    let session_existed = committed_store.get_session(session.id).is_ok();
    if let (Some(parent_id), Some(parent_external_id)) = (
        session.parent_session_id,
        page.next.session.parent_provider_session_id.as_deref(),
    ) {
        if committed_store.get_session(parent_id).is_err() {
            group.upsert_session(&relationship_placeholder(
                context,
                options,
                source_id,
                parent_id,
                parent_external_id,
                &resolution.canonical_source_identity,
            ))?;
        }
    }
    group.upsert_session(&session)?;
    let mut summary = ProviderImportSummary::default();
    if session_existed {
        summary.skipped_sessions = 1;
        summary.skipped = 1;
    } else {
        summary.imported_sessions = 1;
        summary.imported = 1;
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            context,
            source_id,
            &session,
            parent_id,
            &resolution.canonical_source_identity,
        );
        let existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    for event in &page.events {
        publish_event(
            &mut group,
            committed_store,
            context,
            options,
            source_id,
            &session,
            event,
            &mut summary,
        )?;
    }
    for detached in &page.detached_touches {
        publish_file_touches(
            &mut group,
            committed_store,
            context,
            options,
            source_id,
            &session,
            detached.ordinal,
            detached.occurred_at,
            None,
            &detached.touches,
            &mut summary,
        )?;
    }
    for rejection in page.rejections {
        summary.record_failure(rejection);
    }
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn capture_source(
    context: &ProviderAdapterContext,
    session: &SessionFact,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::MistralVibe,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(MISTRAL_VIBE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::MistralVibe,
                    &session.provider_session_id,
                    MISTRAL_VIBE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "session_metadata": session.metadata,
                "nativepath_publication": CURSOR_VERSION,
            }),
        ),
    }
}

fn canonical_session(
    committed_store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    fact: &SessionFact,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::MistralVibe,
        &fact.provider_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = fact
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::MistralVibe,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::MistralVibe,
        external_session_id: Some(fact.provider_session_id.clone()),
        external_agent_id: fact.external_agent_id.clone(),
        agent_type: if fact.is_primary() {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if fact.is_primary() {
            "primary".to_owned()
        } else {
            "subagent".to_owned()
        }),
        is_primary: fact.is_primary(),
        status: if fact.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: fact.started_at,
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.provider_session_id,
                "parent_provider_session_id": fact.parent_provider_session_id,
                "root_provider_session_id": fact.parent_provider_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": fact.metadata,
                "nativepath_publication": CURSOR_VERSION,
            }),
        ),
    })
}

fn relationship_placeholder(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::MistralVibe,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    fact: &EventFact,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::MistralVibe,
        provider_session_id,
        source_id,
        fact.ordinal,
        fact.ordinal,
        &fact.provider_event_hash,
        None,
        Some(fact.ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::MistralVibe,
                provider_session_id,
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &fact.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let retained_text = provider_policy_event_text(fact.event_type, &fact.text, &fact.body);
    let body = provider_policy_body(fact.event_type, &fact.body);
    let provider_payload = if matches!(
        fact.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        let mut payload = fact.body.clone();
        payload["result_evidence"] =
            provider_result_identifier_evidence(fact.event_type, &fact.text, &fact.body);
        payload["source_format"] = Value::String(MISTRAL_VIBE_SOURCE_FORMAT.to_owned());
        payload
    } else {
        json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(
                fact.event_type,
                &fact.text,
                &fact.body,
            ),
            "result_outcome": provider_result_outcome_evidence(fact.event_type, &fact.body),
            "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
            "body": provider_capped_json(&body, PROVIDER_MAX_PREVIEW_CHARS),
        })
    };
    let cursor = format!(
        "{}:line:{}",
        context
            .source_path
            .as_deref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        fact.line_number
    );
    let mut provider_metadata = fact.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": fact.ordinal,
        "provider_event_hash": fact.provider_event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": cursor.clone(),
        "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": fact.line_number,
        "imported_at": context.imported_at,
        "source_record_ordinal": fact.ordinal,
        "source_record_subrecord_index": 0,
        "byte_start": fact.byte_start,
        "byte_end_exclusive": fact.byte_end_exclusive,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: Some(fact.role),
        occurred_at: fact.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::MistralVibe.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": fact.ordinal,
            "provider_event_hash": fact.provider_event_hash,
            "cursor": cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(fact.event_type, &provider_payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    publish_file_touches(
        group,
        committed_store,
        context,
        options,
        source_id,
        session,
        fact.ordinal,
        fact.occurred_at,
        Some(normalized.id),
        &fact.touches,
        summary,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_file_touches(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    _context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    ordinal: u64,
    occurred_at: DateTime<Utc>,
    event_id: Option<Uuid>,
    touches: &[TouchFact],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    for (index, touch) in touches.iter().enumerate() {
        let touch_index = ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(index as u64))
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::MistralVibe,
            provider_session_id,
            source_id,
            Some(ordinal),
            touch_index,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::MistralVibe,
                    provider_session_id,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: options.history_record_id,
            run_id: None,
            event_id,
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: touch.change_kind,
            old_path: touch.old_path.clone(),
            line_count_delta: None,
            confidence: touch.confidence,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::MistralVibe.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_touch_index": touch_index,
                    "provider_event_index": ordinal,
                    "source_format": MISTRAL_VIBE_SOURCE_FORMAT,
                    "session_id": session.id,
                }),
            ),
        })?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

fn replay_outputs_or_mark_behind(
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    core: &Checkpoint,
    profile: &ImportProfile,
) {
    if let Err(error) = replay_outputs(source, observation, core, profile) {
        if let Some(sink) = profile.sink() {
            sink.mark_behind(ProOutputSinkError::new(
                "mistral_vibe_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

fn replay_outputs(
    source: &MistralVibeSessionSource,
    observation: &SourceObservation,
    core: &Checkpoint,
    profile: &ImportProfile,
) -> Result<()> {
    let Some(sink) = profile.sink().map(std::sync::Arc::as_ref) else {
        return Ok(());
    };
    if !observation.revalidate(source)?
        || core.complete_prefix_end > observation.messages.length
        || hash_file_prefix(
            &observation.canonical_messages_path,
            core.complete_prefix_end,
        )? != core.complete_prefix_sha256
    {
        sink.mark_behind(ProOutputSinkError::new(
            "source_changed",
            "Mistral Vibe source changed before output replay",
        ));
        return Ok(());
    }
    let output_source_id = format!("{}:{}", core.machine_id, core.session.provider_session_id);
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::MistralVibe.as_str().to_owned(),
        namespace_id: core.machine_id.clone(),
        source_id: output_source_id.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let plan = output_plan(progress, sink.materializer_revision(), observation, core)?;
    if plan.no_op {
        return Ok(());
    }
    let mut reader = BufReader::new(File::open(&observation.canonical_messages_path)?);
    reader.seek(SeekFrom::Start(plan.frontier.complete_prefix_end))?;
    let mut hasher = hash_prefix(
        &observation.canonical_messages_path,
        plan.frontier.complete_prefix_end,
        initial_prefix_hasher(),
    )?;
    let mut frontier = plan.frontier;
    let mut disposition = plan.disposition;
    let mut expected_prior_frontier = plan.expected_prior_frontier;
    let mut expected_prior_epoch = plan.expected_prior_epoch;

    loop {
        let expected = frontier.safe_frontier()?;
        let mut observations = Vec::new();
        let mut physical_records = 0_usize;
        let mut estimated_bytes = PAGE_BASE_BYTES;
        let mut terminal = false;
        while physical_records < PAGE_MAX_UNITS
            && frontier.complete_prefix_end < core.complete_prefix_end
        {
            let start = frontier.complete_prefix_end;
            let ordinal = frontier.next_ordinal;
            let hasher_before = hasher.clone();
            let line =
                read_bounded_line(&mut reader, &mut hasher, core.complete_prefix_end, start)?;
            let (bytes, end) = match line {
                Line::EndOfFile => {
                    terminal = core.terminal;
                    break;
                }
                Line::IncompleteTail => {
                    hasher = hasher_before;
                    reader.seek(SeekFrom::Start(start))?;
                    break;
                }
                Line::Oversized { end } => (Vec::new(), end),
                Line::Complete { bytes, end } => (bytes, end),
            };
            if !bytes.is_empty() {
                match output_observation(&bytes, ordinal, start, end, &core.session) {
                    Ok(Some(output)) => {
                        let output_bytes = estimate_output_bytes(&output);
                        if output_bytes > PAGE_MAX_BYTES {
                            sink.mark_behind(ProOutputSinkError::new(
                                "output_too_large",
                                "Mistral Vibe output exceeds the bounded Pro page",
                            ));
                            return Ok(());
                        }
                        if !observations.is_empty()
                            && estimated_bytes.saturating_add(output_bytes) > PAGE_MAX_BYTES
                        {
                            hasher = hasher_before;
                            reader.seek(SeekFrom::Start(start))?;
                            break;
                        }
                        estimated_bytes = estimated_bytes.saturating_add(output_bytes);
                        observations.push(output);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        sink.mark_behind(ProOutputSinkError::new(
                            "malformed_output",
                            error.to_string(),
                        ));
                        return Ok(());
                    }
                }
            }
            frontier.complete_prefix_end = end;
            frontier.next_ordinal = frontier.next_ordinal.saturating_add(1);
            frontier.complete_prefix_sha256 = prefix_digest(&hasher);
            physical_records = physical_records.saturating_add(1);
        }
        if frontier.complete_prefix_end == core.complete_prefix_end {
            terminal = core.terminal;
        }
        if physical_records == 0
            && observations.is_empty()
            && expected == frontier.safe_frontier()?
        {
            return Ok(());
        }
        let next = frontier.safe_frontier()?;
        let logical_units = observations.len();
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: plan.source_epoch,
            observed_revision: core.source_revision.clone(),
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_prior_epoch,
            expected_prior_frontier: expected_prior_frontier.clone(),
            observations,
        };
        let page = match NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::MistralVibe.as_str(),
                output_source_id.clone(),
            ),
            expected,
            next.clone(),
            terminal,
            NativePageAccounting {
                logical_units,
                conservative_serialized_bytes: estimated_bytes
                    .saturating_add(next.bytes.len())
                    .saturating_add(4096),
            },
            output,
        ) {
            Ok(page) => page,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "invalid_output_page",
                    error.to_string(),
                ));
                return Ok(());
            }
        };
        if process_pro_replay_only(page, sink).is_err() {
            return Ok(());
        }
        disposition = ProOutputSourceDisposition::AppendOrResume;
        expected_prior_epoch = Some(plan.source_epoch);
        expected_prior_frontier = Some(next);
        if terminal {
            return Ok(());
        }
    }
}

struct OutputPlan {
    frontier: OutputFrontier,
    source_epoch: u64,
    disposition: ProOutputSourceDisposition,
    expected_prior_epoch: Option<u64>,
    expected_prior_frontier: Option<NativeSafeFrontier>,
    no_op: bool,
}

fn output_plan(
    progress: Option<ProOutputProgress>,
    materializer_revision: &str,
    observation: &SourceObservation,
    core: &Checkpoint,
) -> Result<OutputPlan> {
    let fresh = OutputFrontier {
        version: OUTPUT_FRONTIER_VERSION,
        complete_prefix_end: 0,
        next_ordinal: 0,
        complete_prefix_sha256: initial_prefix_digest(),
        generation_identity: core.generation_identity,
    };
    let Some(progress) = progress else {
        return Ok(OutputPlan {
            frontier: fresh,
            source_epoch: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            expected_prior_epoch: None,
            expected_prior_frontier: None,
            no_op: false,
        });
    };
    let raw_prior = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            NativeSafeFrontier::new(cursor.version, cursor.payload.clone())
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
        })
        .transpose()?;
    let decoded = progress.cursor.as_ref().and_then(OutputFrontier::decode);
    let valid_prefix = decoded.as_ref().is_some_and(|frontier| {
        frontier.generation_identity == core.generation_identity
            && frontier.complete_prefix_end <= core.complete_prefix_end
            && hash_file_prefix(
                &observation.canonical_messages_path,
                frontier.complete_prefix_end,
            )
            .is_ok_and(|digest| digest == frontier.complete_prefix_sha256)
    });
    let compatible = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == materializer_revision
        && valid_prefix;
    if compatible {
        let frontier = decoded.expect("compatible output frontier is decoded");
        let no_op = progress.terminal
            && core.terminal
            && frontier.complete_prefix_end == core.complete_prefix_end
            && frontier.complete_prefix_sha256 == core.complete_prefix_sha256;
        return Ok(OutputPlan {
            frontier,
            source_epoch: progress.source_epoch,
            disposition: ProOutputSourceDisposition::AppendOrResume,
            expected_prior_epoch: Some(progress.source_epoch),
            expected_prior_frontier: raw_prior,
            no_op,
        });
    }
    Ok(OutputPlan {
        frontier: fresh,
        source_epoch: progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Mistral Vibe output epoch overflowed",
            ))?,
        disposition: ProOutputSourceDisposition::Rewrite,
        expected_prior_epoch: Some(progress.source_epoch),
        expected_prior_frontier: raw_prior,
        no_op: false,
    })
}

fn output_observation(
    bytes: &[u8],
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    session: &SessionFact,
) -> Result<Option<ProOutputObservation>> {
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let role = value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if mistral_vibe_event_type(role, &value) != EventType::ToolOutput {
        return Ok(None);
    }
    let line_number = usize::try_from(ordinal)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe output line number exceeds platform limits",
        ))?;
    let metadata = output_metadata(&value, line_number, role, session.cwd.as_deref());
    let content = mistral_vibe_result_content(&value).unwrap_or_default();
    let mut locator = Vec::with_capacity(16);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(Some(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: metadata.native_record_id.clone(),
            native_sequence: ordinal,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(
            native_jsonl_timestamp(&value)
                .unwrap_or(session.started_at)
                .timestamp_millis(),
        ),
        associations: OutputAssociations {
            direct_session_id: session.provider_session_id.clone(),
            root_session_id: session
                .parent_provider_session_id
                .clone()
                .unwrap_or_else(|| session.provider_session_id.clone()),
            parent_session_id: session.parent_provider_session_id.clone(),
            provider_session_id: Some(session.provider_session_id.clone()),
            agent_id: session.external_agent_id.clone(),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "mistral-vibe-jsonl-range-v1".to_owned(),
            payload: locator,
        },
        content: content.into_bytes(),
    }))
}

struct OutputMetadata {
    kind: OutputObservationKind,
    native_record_id: String,
    call_id: Option<String>,
    command: Option<OutputCommandContext>,
    outcome: OutputOutcomeMetadata,
}

fn output_metadata(
    value: &Value,
    line_number: usize,
    role: &str,
    session_cwd: Option<&str>,
) -> OutputMetadata {
    let call_id = value
        .get("tool_call_id")
        .or_else(|| value.get("toolCallId"))
        .or_else(|| value.get("call_id"))
        .or_else(|| value.get("callId"))
        .or_else(|| value.get("tool_use_id"))
        .or_else(|| value.get("toolUseId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let tool_name = value
        .get("name")
        .or_else(|| value.get("tool_name"))
        .or_else(|| value.get("tool"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_owned();
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: value
            .get("input")
            .or_else(|| value.get("arguments"))
            .or_else(|| value.get("args"))
            .and_then(tool_input::command)
            .unwrap_or_default(),
        working_directory: value
            .get("input")
            .or_else(|| value.get("arguments"))
            .or_else(|| value.get("args"))
            .and_then(tool_input::working_directory)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = value_timed_out(value);
    let exit_code =
        i64_field(value, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = i64_field(value, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputMetadata {
        kind,
        native_record_id: mistral_vibe_event_id(value, line_number, role),
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

fn value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| values.values().find_map(|value| i64_field(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn estimate_output_bytes(output: &ProOutputObservation) -> usize {
    OUTPUT_BASE_BYTES
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(output.content.len())
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(
            output
                .command
                .as_ref()
                .map_or(0, |command| command.tool_name.len() + command.command.len()),
        )
        .saturating_add(output.locator.payload.len())
}

fn retire_missing_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    entry: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &entry.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Mistral Vibe retirement lost its source cursor",
        ))?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).ok();
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::MistralVibe,
        source_format: MISTRAL_VIBE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: entry.locator_identity.clone(),
        cursor_stream: entry.cursor_stream.clone(),
        expected_canonical_source_identity: entry.canonical_source_identity.clone(),
        expected_source_revision: entry.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed
        .as_ref()
        .is_some_and(|committed| committed.publication_id() == publication_id)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let provider_cursor = committed
        .as_ref()
        .map(|committed| committed.provider_cursor().to_owned())
        .unwrap_or_else(|| stored.cursor.clone());
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            entry.cursor_stream.clone(),
            provider_cursor,
            context.imported_at,
        ),
    );
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped_sessions = 1;
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

fn load_known_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::MistralVibe
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(MISTRAL_VIBE_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let canonical_messages_path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&canonical_messages_path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::MistralVibe,
            MISTRAL_VIBE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(cursor) = store.get_sync_cursor(None, machine_id, &cursor_stream)? else {
            continue;
        };
        let checkpoint = decode_native_checkpoint(&cursor.cursor)?;
        if checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.canonical_messages_path != canonical_messages_path
                || source.descriptor.external_session_id.as_deref()
                    != Some(checkpoint.session.provider_session_id.as_str())
        }) {
            continue;
        }
        let source_revision = checkpoint
            .map(|checkpoint| checkpoint.source_revision)
            .or_else(|| {
                source
                    .sync
                    .metadata
                    .get("source_revision")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let Some(source_revision) = source_revision else {
            continue;
        };
        let route = KnownRoute {
            locator_identity,
            cursor_stream: cursor_stream.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
        };
        if let Some(previous) = routes.insert(cursor_stream, route.clone()) {
            if previous != route {
                return Err(CaptureError::SystemInvariant(
                    "Mistral Vibe persisted conflicting routes for one transcript",
                ));
            }
        }
    }
    Ok(routes.into_values().collect())
}

fn proposed_source_identity(
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

pub(super) fn source_cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::MistralVibe,
        MISTRAL_VIBE_SOURCE_FORMAT,
        &identity,
    ))
}

fn provider_sync_cursor(
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

fn encode_checkpoint(checkpoint: &Checkpoint) -> Result<String> {
    Ok(serde_json::to_string(&CursorWire {
        version: CURSOR_VERSION,
        kind: CURSOR_KIND.to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

fn decode_native_checkpoint(encoded_store_cursor: &str) -> Result<Option<Checkpoint>> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<CursorWire>(&encoded) else {
        return Ok(None);
    };
    if wire.version != CURSOR_VERSION || wire.kind != CURSOR_KIND || !wire.checkpoint.supported() {
        return Ok(None);
    }
    Ok(Some(wire.checkpoint))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyParserCheckpoint {
    metadata_revision: String,
    metadata_failure_reported: bool,
    next_ordinal: u64,
    #[serde(rename = "accepted_captures")]
    _accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
}

fn migrate_released_cursor(
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
    let old_observation = super::source::MistralVibeSessionObservation::read(source)?;
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
        metadata_failure_reported: old.metadata_failure_reported,
        generation: 0,
        session: session.clone(),
        terminal: complete_prefix_end == observation.messages.length,
    }))
}

fn summary_from_checkpoint(checkpoint: &Checkpoint) -> ProviderImportSummary {
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
        ..ProviderImportSummary::default()
    }
}

fn native_source_id(source_identity: &str, provider_session_id: &str, generation: u64) -> Uuid {
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

fn publication_id(page: &Page, transition: &NativePathCursorTransition) -> String {
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

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
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

fn attach_exact_locator(
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

fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn hash_stamp(digest: &mut Sha256, stamp: &FileStamp) {
    digest.update(stamp.length.to_be_bytes());
    digest.update([u8::from(stamp.modified.before_epoch)]);
    digest.update(stamp.modified.seconds.to_be_bytes());
    digest.update(stamp.modified.nanos.to_be_bytes());
    digest.update([u8::from(stamp.readonly)]);
    digest.update(stamp.device.unwrap_or(u64::MAX).to_be_bytes());
    digest.update(stamp.inode.unwrap_or(u64::MAX).to_be_bytes());
}

fn initial_prefix_hasher() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(PREFIX_HASH_DOMAIN);
    digest
}

fn initial_prefix_digest() -> [u8; 32] {
    prefix_digest(&initial_prefix_hasher())
}

fn hash_file_prefix(path: &Path, length: u64) -> Result<[u8; 32]> {
    Ok(prefix_digest(&hash_prefix(
        path,
        length,
        initial_prefix_hasher(),
    )?))
}

fn hash_prefix(path: &Path, length: u64, mut digest: Sha256) -> Result<Sha256> {
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

fn prefix_digest(digest: &Sha256) -> [u8; 32] {
    digest.clone().finalize().into()
}

enum Line {
    EndOfFile,
    IncompleteTail,
    Oversized { end: u64 },
    Complete { bytes: Vec<u8>, end: u64 },
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<Line> {
    if start >= frozen_length {
        return Ok(Line::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                Line::EndOfFile
            } else {
                Line::IncompleteTail
            });
        }
        let remaining = frozen_length.saturating_sub(start.saturating_add(total));
        if remaining == 0 {
            return Ok(Line::IncompleteTail);
        }
        let bounded = &available[..available
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX))];
        let take = bounded
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bounded.len(), |index| index.saturating_add(1));
        let chunk = &bounded[..take];
        hasher.update(chunk);
        total = total.saturating_add(chunk.len() as u64);
        if !oversized {
            if bytes.len().saturating_add(chunk.len())
                > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let complete = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if complete {
            let end = start.saturating_add(total);
            if oversized {
                return Ok(Line::Oversized { end });
            }
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Line::Complete { bytes, end });
        }
        if start.saturating_add(total) == frozen_length {
            return Ok(Line::IncompleteTail);
        }
    }
}
