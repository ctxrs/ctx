use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    common::io::{
        ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    },
    complete_content::{
        attach_verified_content_locator, structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, AUGGIE_SESSION_JSON_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    auggie_entry_time, auggie_event, auggie_node_is_tool_metadata, auggie_request_text,
    auggie_response_text, AuggieEvent, AuggieEventInput, AuggieSessionData,
};

const AUGGIE_NATIVE_CURSOR_VERSION: u32 = 1;
const AUGGIE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const AUGGIE_PARSER_REVISION: &str = "auggie-nativepath-json-v1";
const AUGGIE_POLICY_REVISION: &str = "auggie-core-private-output-v1";
const AUGGIE_CORE_EVENTS_PER_PAGE: usize = 60;
const AUGGIE_OUTPUTS_PER_PAGE: usize = 32;
const AUGGIE_OUTPUT_PAGE_CONTENT_BYTES: usize = 6 * 1024 * 1024;
const AUGGIE_GENERATION_EVENT_STRIDE: u64 = 1 << 32;
const AUGGIE_MAX_DISCOVERED_FILES: usize = 4_096;
const AUGGIE_MAX_DISCOVERED_DIRECTORIES: usize = 4_096;
const AUGGIE_MAX_DISCOVERY_DEPTH: usize = 64;
const PAGE_ACCOUNTING_OVERHEAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuggieFileStamp {
    canonical_path: PathBuf,
    len: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl AuggieFileStamp {
    fn observe(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path: fs::canonicalize(path)?,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn revalidate(&self) -> Result<bool> {
        match Self::observe(&self.canonical_path) {
            Ok(current) => Ok(&current == self),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn revision_material(&self, digest: &mut Sha256) {
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

struct AuggieInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

struct ParsedAuggieSource {
    stamp: AuggieFileStamp,
    source_revision: String,
    session: ParsedAuggieSession,
    events: Vec<ParsedAuggieEvent>,
    outputs: Vec<ParsedAuggieOutput>,
}

struct ParsedAuggieSession {
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    external_agent_id: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    raw_source_path: String,
    source_metadata: Value,
    session_metadata: Value,
}

struct ParsedAuggieEvent {
    event: AuggieEvent,
    chat_index: usize,
    sub_index: u32,
}

struct ParsedAuggieOutput {
    output_sequence: u32,
    chat_index: usize,
    node_collection: &'static str,
    node_index: usize,
    occurred_at: Option<DateTime<Utc>>,
    call_id: Option<String>,
    outcome: OutputOutcomeMetadata,
    content: Vec<u8>,
    content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuggieNativeCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    source_path: PathBuf,
    source_revision: String,
    generation: u64,
    next_event: u64,
    prefix_sha256: String,
    terminal: bool,
    event_count: u64,
    provider_session_id: String,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuggieOutputFrontier {
    version: u32,
    source_revision: String,
    next_output: u64,
}

#[derive(Clone)]
struct KnownAuggieRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    session_id: Uuid,
    provider_session_id: String,
    current_cursor: SyncCursor,
    provider_cursor: AuggieNativeCursor,
}

struct SourceCompletion {
    changed_groups: usize,
    terminal: bool,
    session_id: Uuid,
}

#[derive(Clone)]
struct RelationshipFact {
    path: PathBuf,
    stamp: AuggieFileStamp,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    session_id: Uuid,
}

enum CursorPlan {
    AlreadyCommitted(AuggieNativeCursor),
    Publish {
        expected_cursor: Option<String>,
        generation: u64,
        next_event: usize,
        rejected_records: u64,
    },
}

pub(crate) fn import_auggie_sessions_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_auggie_sources(path)?;
    let known_routes = known_auggie_routes(store, &context.machine_id, &configured_source_root)?;

    if import_options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &inventory.paths,
            &configured_source_root,
            &context,
            &import_options,
        );
        return Ok(ProviderImportSummary::default());
    }

    if inventory.paths.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Auggie session JSON files were found",
        });
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut relationships = Vec::new();
        let mut session_index = current_session_index(&known_routes, &inventory.paths);
        let known_by_path = known_routes
            .iter()
            .map(|route| (route.path.clone(), route))
            .collect::<BTreeMap<_, _>>();

        for source_path in &inventory.paths {
            let parsed = match parse_auggie_source(
                source_path,
                &context,
                import_options.inventory_observation_token.as_deref(),
                import_options.import_profile.sink().is_some(),
            ) {
                Ok(parsed) => parsed,
                Err(error) => {
                    summary.record_failure(ProviderImportFailure {
                        line: 1,
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            let completion = import_auggie_source(
                store,
                &committed_store,
                &bulk_guard,
                &configured_source_root,
                &context,
                &import_options,
                &parsed,
                known_by_path.get(source_path).copied(),
                &session_index,
                &mut summary,
            )?;
            changed_groups = changed_groups.saturating_add(completion.changed_groups);
            session_index_insert(
                &mut session_index,
                parsed.session.provider_session_id.clone(),
                completion.session_id,
            );
            relationships.push(RelationshipFact {
                path: source_path.clone(),
                stamp: parsed.stamp.clone(),
                provider_session_id: parsed.session.provider_session_id.clone(),
                parent_provider_session_id: parsed.session.parent_provider_session_id.clone(),
                root_provider_session_id: parsed.session.root_provider_session_id.clone(),
                session_id: completion.session_id,
            });

            if completion.terminal {
                replay_parsed_outputs_or_mark_behind(
                    &parsed,
                    &configured_source_root,
                    import_options.import_profile.sink().map(AsRef::as_ref),
                );
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        for route in known_routes
            .iter()
            .filter(|route| !inventory.paths.contains(&route.path))
        {
            let changed = retire_auggie_route(
                store,
                &bulk_guard,
                &context,
                route,
                if inventory.root_missing {
                    ProviderSourceRouteRetirementReason::RootMissing
                } else {
                    ProviderSourceRouteRetirementReason::SourceMissing
                },
            )?;
            if changed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
                changed_groups = changed_groups.saturating_add(1);
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        for relationship in &relationships {
            if reconcile_auggie_relationship(
                store,
                &bulk_guard,
                &context,
                relationship,
                &session_index,
            )? {
                summary.set_work_result(ProviderImportWorkResult::Changed);
                changed_groups = changed_groups.saturating_add(1);
            }
            if stop_after_changed_group(&import_options, changed_groups) {
                summary.work_remaining = true;
                return Ok(summary);
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

fn stop_after_changed_group(options: &ProviderImportOptions, changed_groups: usize) -> bool {
    options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0
}

fn discover_auggie_sources(root: &Path) -> Result<AuggieInventory> {
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuggieInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(invalid_source_path(
            root,
            "symlinked provider transcript roots are rejected",
        ));
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if root_metadata.is_file() {
        ensure_regular_provider_transcript_file(root)?;
        let mut paths = BTreeSet::new();
        if root.extension().and_then(|extension| extension.to_str()) == Some("json") {
            paths.insert(fs::canonicalize(root)?);
        }
        return Ok(AuggieInventory {
            paths,
            root_missing: false,
        });
    }
    if !root_metadata.is_dir() {
        return Err(invalid_source_path(
            root,
            "Auggie transcript root is neither a file nor a directory",
        ));
    }

    let mut paths = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let mut directories = 0_usize;
    while let Some((directory, depth)) = stack.pop() {
        directories = directories.saturating_add(1);
        if directories > AUGGIE_MAX_DISCOVERED_DIRECTORIES {
            return Err(invalid_source_path(
                root,
                "Auggie transcript discovery exceeds the directory bound",
            ));
        }
        if depth > AUGGIE_MAX_DISCOVERY_DEPTH {
            return Err(invalid_source_path(
                root,
                "Auggie transcript discovery exceeds the depth bound",
            ));
        }
        let mut entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let entry_path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                return Err(invalid_source_path(
                    &entry_path,
                    "symlinked Auggie transcript entries are rejected",
                ));
            }
            if file_type.is_dir() {
                stack.push((entry_path, depth.saturating_add(1)));
            } else if file_type.is_file()
                && entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("json")
            {
                ensure_regular_provider_transcript_file(&entry_path)?;
                paths.insert(fs::canonicalize(entry_path)?);
                if paths.len() > AUGGIE_MAX_DISCOVERED_FILES {
                    return Err(invalid_source_path(
                        root,
                        "Auggie transcript discovery exceeds the file bound",
                    ));
                }
            }
        }
    }
    Ok(AuggieInventory {
        paths,
        root_missing: false,
    })
}

fn parse_auggie_source(
    path: &Path,
    context: &ProviderAdapterContext,
    inventory_token: Option<&str>,
    include_outputs: bool,
) -> Result<ParsedAuggieSource> {
    let before = AuggieFileStamp::observe(path)?;
    let max_bytes = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES).unwrap_or(u64::MAX);
    if before.len > max_bytes {
        return Err(CaptureError::InvalidPayload(format!(
            "Auggie session JSON exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
        )));
    }
    let bytes = fs::read(&before.canonical_path)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != before.len {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let after = AuggieFileStamp::observe(&before.canonical_path)?;
    if after != before {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let root = serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Auggie session JSON: {error}"))
    })?;
    let data = AuggieSessionData::parse(&root, &before.canonical_path, context)?;
    let source_revision = source_revision(&before, &bytes, inventory_token);
    let session = ParsedAuggieSession {
        provider_session_id: data.provider_session_id.clone(),
        parent_provider_session_id: data.parent_provider_session_id.clone(),
        root_provider_session_id: data.root_provider_session_id.clone(),
        external_agent_id: data.external_agent_id.clone(),
        started_at: data.started_at,
        ended_at: data.ended_at,
        cwd: data.cwd.clone(),
        raw_source_path: data.raw_source_path.clone(),
        source_metadata: data.source_metadata.clone(),
        session_metadata: data.session_metadata.clone(),
    };
    let events = parse_core_events(&data, &bytes)?;
    let outputs = if include_outputs {
        parse_outputs(&data)?
    } else {
        Vec::new()
    };
    Ok(ParsedAuggieSource {
        stamp: before,
        source_revision,
        session,
        events,
        outputs,
    })
}

fn parse_core_events(data: &AuggieSessionData<'_>, bytes: &[u8]) -> Result<Vec<ParsedAuggieEvent>> {
    let mut events = Vec::new();
    let mut provider_event_index = 0_u64;
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let base_time = auggie_entry_time(entry, Some(exchange)).unwrap_or_else(|| {
            data.started_at + Duration::milliseconds(saturating_i64(chat_index).saturating_mul(2))
        });
        for (role, label, occurred_at, text) in [
            (
                EventRole::User,
                "request",
                base_time,
                auggie_request_text(exchange),
            ),
            (
                EventRole::Assistant,
                "response",
                base_time + Duration::milliseconds(1),
                auggie_response_text(exchange),
            ),
        ] {
            let Some(text) = text else {
                continue;
            };
            let complete_text = text.clone();
            let mut event = auggie_event(AuggieEventInput {
                provider_session_id: &data.provider_session_id,
                provider_event_index,
                chat_index,
                role,
                label,
                occurred_at,
                text,
                entry,
                exchange,
                raw_source_path: &data.raw_source_path,
            });
            let event_hash = event.provider_event_hash.clone();
            let sub_index = u32::try_from(events.len()).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Auggie session contains too many normalized messages".to_owned(),
                )
            })?;
            attach_auggie_complete_content_locator(
                &mut event,
                0,
                sub_index,
                &event_hash,
                bytes,
                &complete_text,
            )?;
            events.push(ParsedAuggieEvent {
                event,
                chat_index,
                sub_index,
            });
            provider_event_index =
                provider_event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Auggie provider event index overflowed",
                    ))?;
        }
    }
    Ok(events)
}

fn attach_auggie_complete_content_locator(
    event: &mut AuggieEvent,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > 1_024
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "Auggie complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator_value = auggie_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Auggie complete content exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie complete-content profile is not registered",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie complete-content locator exceeds its typed bounds",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Auggie complete-content locator metadata is malformed"),
    )?;
    Ok(())
}

fn auggie_structured_locator(
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    let provider = CaptureProvider::Auggie.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("Auggie provider identity is too long"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "Auggie complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut value = Vec::with_capacity(4 + 1 + provider.len() + 8 + 4 + 2 + native_id.len());
    value.extend_from_slice(b"SC\0\x01");
    value.push(provider_len);
    value.extend_from_slice(provider);
    value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    value.extend_from_slice(&source_record_subrecord_index.to_be_bytes());
    value.extend_from_slice(&native_len.to_be_bytes());
    value.extend_from_slice(native_id);
    Ok(value)
}

fn parse_outputs(data: &AuggieSessionData<'_>) -> Result<Vec<ParsedAuggieOutput>> {
    let mut outputs = Vec::new();
    for (chat_index, entry) in data.chat_history.iter().enumerate() {
        let exchange = entry.get("exchange").unwrap_or(entry);
        let occurred_at = auggie_entry_time(entry, Some(exchange));
        for (node_collection, nodes) in [
            (
                "request",
                exchange
                    .get("request_nodes")
                    .or_else(|| exchange.get("requestNodes"))
                    .and_then(Value::as_array),
            ),
            (
                "response",
                exchange
                    .get("response_nodes")
                    .or_else(|| exchange.get("responseNodes"))
                    .and_then(Value::as_array),
            ),
        ]
        .into_iter()
        .filter_map(|(collection, nodes)| nodes.map(|nodes| (collection, nodes)))
        {
            for (node_index, node) in nodes.iter().enumerate() {
                if !auggie_node_is_tool_result(node) {
                    continue;
                }
                let Some(content) = auggie_tool_result_content(node) else {
                    continue;
                };
                let content = content.into_bytes();
                let output_sequence = u32::try_from(outputs.len()).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "Auggie session contains too many output observations".to_owned(),
                    )
                })?;
                outputs.push(ParsedAuggieOutput {
                    output_sequence,
                    chat_index,
                    node_collection,
                    node_index,
                    occurred_at,
                    call_id: provider_text(
                        node,
                        &["call_id", "callId", "tool_call_id", "toolCallId", "id"],
                    ),
                    outcome: auggie_output_outcome(node),
                    content_sha256: format!("{:x}", Sha256::digest(&content)),
                    content,
                });
            }
        }
    }
    Ok(outputs)
}

fn auggie_node_is_tool_result(node: &Value) -> bool {
    let kind = node
        .get("type")
        .or_else(|| node.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    (auggie_node_is_tool_metadata(node)
        && (kind.contains("result") || kind.contains("output") || node.get("is_error").is_some()))
        || node.get("tool_result").is_some()
        || node.get("toolResult").is_some()
}

fn auggie_tool_result_content(node: &Value) -> Option<String> {
    [
        node.get("content"),
        node.get("output"),
        node.get("result"),
        node.pointer("/text_node/content"),
        node.pointer("/textNode/content"),
        node.pointer("/tool_result/content"),
        node.pointer("/toolResult/content"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        value
            .as_str()
            .map(str::to_owned)
            .or_else(|| (!value.is_null()).then(|| value.to_string()))
    })
    .filter(|content| !content.is_empty())
}

fn auggie_output_outcome(node: &Value) -> OutputOutcomeMetadata {
    let exit_code = node
        .get("exit_code")
        .or_else(|| node.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = node
        .get("duration_ms")
        .or_else(|| node.get("durationMs"))
        .and_then(Value::as_u64);
    let status = node
        .get("status")
        .or_else(|| node.get("outcome"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let outcome = if node
        .get("timed_out")
        .or_else(|| node.get("timedOut"))
        .and_then(Value::as_bool)
        == Some(true)
        || status.as_deref() == Some("timeout")
    {
        OutputOutcome::Timeout
    } else if node
        .get("is_error")
        .or_else(|| node.get("isError"))
        .and_then(Value::as_bool)
        == Some(true)
        || exit_code.is_some_and(|code| code != 0)
        || matches!(status.as_deref(), Some("failure" | "failed" | "error"))
    {
        OutputOutcome::Failure
    } else if node
        .get("is_error")
        .or_else(|| node.get("isError"))
        .and_then(Value::as_bool)
        == Some(false)
        || exit_code == Some(0)
        || matches!(status.as_deref(), Some("success" | "succeeded" | "ok"))
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

#[allow(clippy::too_many_arguments)]
fn import_auggie_source(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    known_route: Option<&KnownAuggieRoute>,
    session_index: &BTreeMap<String, Option<Uuid>>,
    summary: &mut ProviderImportSummary,
) -> Result<SourceCompletion> {
    let path = &parsed.stamp.canonical_path;
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(stored.as_ref(), parsed)?;
    if let CursorPlan::AlreadyCommitted(cursor) = plan {
        let route = known_route.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie NativePath cursor has no matching canonical source route".to_owned(),
            )
        })?;
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped_events = summary.skipped_events.saturating_add(parsed.events.len());
        summary.skipped = summary
            .skipped
            .saturating_add(parsed.events.len().saturating_add(1));
        summary.accepted_content_records = summary
            .accepted_content_records
            .saturating_add(parsed.events.len());
        summary.failed = summary
            .failed
            .saturating_add(usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX));
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(SourceCompletion {
            changed_groups: 0,
            terminal: true,
            session_id: route.session_id,
        });
    }
    let CursorPlan::Publish {
        mut expected_cursor,
        generation,
        mut next_event,
        rejected_records,
    } = plan
    else {
        unreachable!("already-committed cursor returned above");
    };
    let mut changed_groups = 0_usize;
    let mut session_id = known_route.map(|route| route.session_id);
    loop {
        let page_end = next_event
            .saturating_add(AUGGIE_CORE_EVENTS_PER_PAGE)
            .min(parsed.events.len());
        let terminal = page_end == parsed.events.len();
        let prefix_sha256 = event_prefix_digest(&parsed.events[..page_end])?;
        let provider_cursor = AuggieNativeCursor {
            version: AUGGIE_NATIVE_CURSOR_VERSION,
            parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
            policy_revision: AUGGIE_POLICY_REVISION.to_owned(),
            source_path: path.clone(),
            source_revision: parsed.source_revision.clone(),
            generation,
            next_event: u64::try_from(page_end).map_err(|_| {
                CaptureError::InvalidPayload("Auggie event frontier exceeds u64".to_owned())
            })?,
            prefix_sha256,
            terminal,
            event_count: u64::try_from(parsed.events.len()).map_err(|_| {
                CaptureError::InvalidPayload("Auggie event count exceeds u64".to_owned())
            })?,
            provider_session_id: parsed.session.provider_session_id.clone(),
            rejected_records,
        };
        let next_cursor = provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            encode_cursor(&provider_cursor)?,
            context.imported_at,
        );
        let transition =
            NativePathCursorTransition::new(expected_cursor.clone(), next_cursor.clone());
        let page = &parsed.events[next_event..page_end];
        let publication_id =
            source_publication_id(parsed, page, generation, next_event, &transition);
        let retained_bytes = retained_core_page_bytes(parsed, page)?;
        let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
        if !parsed.stamp.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let admission = store.admit_event_search_bulk_group(bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(admission, accounting)?;
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                session_id = session_id.or_else(|| known_route.map(|route| route.session_id));
            }
            NativePathCursorSetClassification::AllExpected => {
                let resolved = publish_source_and_session(
                    committed_store,
                    &mut group,
                    configured_source_root,
                    context,
                    options,
                    parsed,
                    &locator_identity,
                    &stream,
                    session_index,
                    summary,
                    next_event == 0,
                )?;
                session_id = Some(resolved.1);
                publish_events(
                    committed_store,
                    &mut group,
                    context,
                    options,
                    parsed,
                    generation,
                    resolved.0,
                    resolved.1,
                    page,
                    summary,
                )?;
                if !parsed.stamp.revalidate()? {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                group.commit()?;
                changed_groups = changed_groups.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
        }
        expected_cursor = store
            .get_sync_cursor(None, &context.machine_id, &stream)?
            .map(|cursor| cursor.cursor);
        next_event = page_end;
        if terminal || stop_after_changed_group(options, changed_groups) {
            return Ok(SourceCompletion {
                changed_groups,
                terminal,
                session_id: session_id.ok_or(CaptureError::SystemInvariant(
                    "Auggie publication lost its session identity",
                ))?,
            });
        }
    }
}

fn classify_cursor(stored: Option<&SyncCursor>, parsed: &ParsedAuggieSource) -> Result<CursorPlan> {
    let Some(stored) = stored else {
        return Ok(CursorPlan::Publish {
            expected_cursor: None,
            generation: 0,
            next_event: 0,
            rejected_records: 0,
        });
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let prior = decode_cursor(committed.provider_cursor())?;
        validate_native_cursor(&prior, &parsed.stamp.canonical_path)?;
        if prior.provider_session_id != parsed.session.provider_session_id {
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation.checked_add(1).ok_or(
                    CaptureError::SystemInvariant("Auggie source generation exhausted"),
                )?,
                next_event: 0,
                rejected_records: 0,
            });
        }
        let prior_next = usize::try_from(prior.next_event).map_err(|_| {
            CaptureError::InvalidPayload(
                "Auggie cursor event frontier exceeds platform limits".into(),
            )
        })?;
        if prior.source_revision == parsed.source_revision {
            if prior_next > parsed.events.len()
                || event_prefix_digest(&parsed.events[..prior_next])? != prior.prefix_sha256
            {
                return Err(CaptureError::InvalidPayload(
                    "Auggie NativePath cursor does not match its certified event prefix".to_owned(),
                ));
            }
            if prior.terminal {
                if prior_next != parsed.events.len()
                    || prior.event_count != u64::try_from(parsed.events.len()).unwrap_or(u64::MAX)
                {
                    return Err(CaptureError::InvalidPayload(
                        "Auggie terminal cursor does not match its source".to_owned(),
                    ));
                }
                return Ok(CursorPlan::AlreadyCommitted(prior));
            }
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation,
                next_event: prior_next,
                rejected_records: prior.rejected_records,
            });
        }
        let prefix_matches = prior_next <= parsed.events.len()
            && event_prefix_digest(&parsed.events[..prior_next])? == prior.prefix_sha256;
        if prefix_matches {
            return Ok(CursorPlan::Publish {
                expected_cursor: Some(stored.cursor.clone()),
                generation: prior.generation,
                next_event: prior_next,
                rejected_records: prior.rejected_records,
            });
        }
        return Ok(CursorPlan::Publish {
            expected_cursor: Some(stored.cursor.clone()),
            generation: prior
                .generation
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Auggie source generation exhausted",
                ))?,
            next_event: 0,
            rejected_records: 0,
        });
    }

    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Auggie cursor is neither NativePath nor a released migration cursor".to_owned(),
        ));
    }
    Ok(CursorPlan::Publish {
        expected_cursor: Some(stored.cursor.clone()),
        generation: 0,
        next_event: 0,
        rejected_records: 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_source_and_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    locator_identity: &str,
    stream: &str,
    session_index: &BTreeMap<String, Option<Uuid>>,
    summary: &mut ProviderImportSummary,
    count_session: bool,
) -> Result<(Uuid, Uuid)> {
    let source_root = configured_source_root.display().to_string();
    let raw_source_path = parsed.session.raw_source_path.clone();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Auggie NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Auggie,
            source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: parsed.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Auggie,
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            &parsed.session.provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Auggie,
                &parsed.session.provider_session_id,
                AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = capture_source(
        configured_source_root,
        context,
        parsed,
        source_id,
        &resolution.canonical_source_identity,
    );
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Auggie,
        &parsed.session.provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let parent_session_id = parsed
        .session
        .parent_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id));
    let root_session_id = parsed
        .session
        .root_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id))
        .or(parent_session_id);
    let session = canonical_session(
        context,
        options,
        parsed,
        source_id,
        session_id,
        parent_session_id,
        root_session_id,
    );
    let existed = committed_store.get_session(session_id).is_ok();
    group.upsert_session(&session)?;
    if count_session {
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    Ok((source_id, session_id))
}

fn capture_source(
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    parsed: &ParsedAuggieSource,
    source_id: Uuid,
    canonical_source_identity: &str,
) -> CaptureSource {
    let source_root = configured_source_root.display().to_string();
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Auggie,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: parsed.session.cwd.clone(),
            raw_source_path: Some(parsed.session.raw_source_path.clone()),
            source_format: Some(AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(parsed.session.provider_session_id.clone()),
        },
        started_at: parsed.session.started_at,
        ended_at: parsed.session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": parsed.session.provider_session_id,
                "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": parsed.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Auggie,
                    &parsed.session.provider_session_id,
                    AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                    Some(&parsed.session.raw_source_path),
                ),
                "source_metadata": parsed.session.source_metadata,
                "nativepath_publication": AUGGIE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn canonical_session(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    source_id: Uuid,
    session_id: Uuid,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Auggie,
        external_session_id: Some(parsed.session.provider_session_id.clone()),
        external_agent_id: parsed.session.external_agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if parsed.session.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: parsed.session.started_at,
        ended_at: parsed.session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": parsed.session.provider_session_id,
                "parent_provider_session_id": parsed.session.parent_provider_session_id,
                "root_provider_session_id": parsed.session.root_provider_session_id,
                "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": parsed.session.session_metadata,
                "nativepath_publication": AUGGIE_PARSER_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    parsed: &ParsedAuggieSource,
    generation: u64,
    source_id: Uuid,
    session_id: Uuid,
    events: &[ParsedAuggieEvent],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    for event in events {
        let retained = &event.event;
        let provider_event_index = generation
            .checked_mul(AUGGIE_GENERATION_EVENT_STRIDE)
            .and_then(|base| base.checked_add(retained.provider_event_index))
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie generation-scoped event index exceeds u64".to_owned(),
                )
            })?;
        let event_hash = retained.provider_event_hash.as_str();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Auggie,
            &parsed.session.provider_session_id,
            source_id,
            provider_event_index,
            provider_event_index,
            event_hash,
            None,
            Some(u64::try_from(event.chat_index).unwrap_or(u64::MAX)),
            session_id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Auggie,
                    &parsed.session.provider_session_id,
                ),
        )?;
        let dedupe_key =
            Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
                .unwrap_or(identity.dedupe_key);
        let mut provider_metadata = retained.metadata.clone();
        let verified_locators = provider_metadata
            .as_object_mut()
            .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
        let mut sync_metadata = json!({
            "provider_session_id": parsed.session.provider_session_id,
            "provider_event_index": provider_event_index,
            "native_provider_event_index": retained.provider_event_index,
            "source_generation": generation,
            "provider_event_hash": event_hash,
            "provider_event_hash_authority": "provider_supplied",
            "cursor": retained.cursor,
            "source_format": AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            "source_trust": "provider_native",
            "imported_at": context.imported_at,
            "source_record_ordinal": event.chat_index,
            "source_record_subrecord_index": event.sub_index,
            "metadata": provider_metadata,
        });
        if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators)
        {
            metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
        }
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: None,
            event_type: retained.event_type,
            role: Some(retained.role),
            occurred_at: retained.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::Auggie.as_str(),
                "provider_session_id": parsed.session.provider_session_id,
                "provider_event_index": provider_event_index,
                "native_provider_event_index": retained.provider_event_index,
                "source_generation": generation,
                "provider_event_hash": event_hash,
                "cursor": retained.cursor,
                "artifacts": [],
                "body": crate::provider::importer::compact_provider_result_payload(
                    retained.event_type,
                    &retained.payload,
                ),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
        };
        if group
            .reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)?
        {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

fn known_auggie_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownAuggieRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownAuggieRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Auggie
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(AUGGIE_SESSION_JSON_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity), Some(provider_session_id)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source.descriptor.external_session_id.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Auggie,
            AUGGIE_SESSION_JSON_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let source_revision = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie persisted source is missing its source revision".to_owned(),
                )
            })?
            .to_owned();
        let provider_cursor = migrate_or_decode_known_cursor(
            &current_cursor.cursor,
            &path,
            &source_revision,
            provider_session_id,
        )?;
        let session = store
            .session_by_capture_source_and_external_session(
                source.id,
                CaptureProvider::Auggie,
                provider_session_id,
            )?
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Auggie persisted source has no canonical session".to_owned(),
                )
            })?;
        let route = KnownAuggieRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
            session_id: session.id,
            provider_session_id: provider_session_id.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Auggie persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn migrate_or_decode_known_cursor(
    encoded: &str,
    path: &Path,
    source_revision: &str,
    provider_session_id: &str,
) -> Result<AuggieNativeCursor> {
    if let Ok(committed) = decode_native_path_committed_cursor(encoded) {
        let cursor = decode_cursor(committed.provider_cursor())?;
        validate_native_cursor(&cursor, path)?;
        return Ok(cursor);
    }
    if CertifiedProviderCursor::decode_if_certified(encoded)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Auggie persisted route has an unsupported released cursor".to_owned(),
        ));
    }
    Ok(AuggieNativeCursor {
        version: AUGGIE_NATIVE_CURSOR_VERSION,
        parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
        policy_revision: AUGGIE_POLICY_REVISION.to_owned(),
        source_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        generation: 0,
        next_event: 0,
        prefix_sha256: empty_digest(),
        terminal: true,
        event_count: 0,
        provider_session_id: provider_session_id.to_owned(),
        rejected_records: 0,
    })
}

fn retire_auggie_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownAuggieRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            encode_cursor(&route.provider_cursor)?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Auggie,
        source_format: AUGGIE_SESSION_JSON_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if decode_native_path_committed_cursor(&route.current_cursor.cursor)
        .is_ok_and(|committed| committed.publication_id() == publication_id)
    {
        return Ok(false);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

fn current_session_index(
    known_routes: &[KnownAuggieRoute],
    live_paths: &BTreeSet<PathBuf>,
) -> BTreeMap<String, Option<Uuid>> {
    let mut index = BTreeMap::new();
    for route in known_routes
        .iter()
        .filter(|route| live_paths.contains(&route.path))
    {
        session_index_insert(
            &mut index,
            route.provider_session_id.clone(),
            route.session_id,
        );
    }
    index
}

fn session_index_insert(
    index: &mut BTreeMap<String, Option<Uuid>>,
    provider_session_id: String,
    session_id: Uuid,
) {
    match index.entry(provider_session_id) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(Some(session_id));
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            if entry.get().is_some_and(|existing| existing != session_id) {
                entry.insert(None);
            }
        }
    }
}

fn unique_session_id(
    index: &BTreeMap<String, Option<Uuid>>,
    provider_session_id: &str,
) -> Option<Uuid> {
    index.get(provider_session_id).copied().flatten()
}

fn reconcile_auggie_relationship(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    relationship: &RelationshipFact,
    session_index: &BTreeMap<String, Option<Uuid>>,
) -> Result<bool> {
    let mut session = store.get_session(relationship.session_id)?;
    let parent_session_id = relationship
        .parent_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id));
    let root_session_id = relationship
        .root_provider_session_id
        .as_ref()
        .and_then(|provider_id| unique_session_id(session_index, provider_id))
        .or(parent_session_id);
    if session.parent_session_id == parent_session_id && session.root_session_id == root_session_id
    {
        return Ok(false);
    }
    if !relationship.stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    session.parent_session_id = parent_session_id;
    session.root_session_id = root_session_id;
    session.timestamps.updated_at = context.imported_at;
    let locator_identity = provider_path_identity(&relationship.path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie relationship reconciliation requires committed Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let provider_cursor = decode_cursor(committed.provider_cursor())?;
    if !provider_cursor.terminal || !relationship.stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            stream,
            encode_cursor(&provider_cursor)?,
            context.imported_at,
        ),
    );
    let publication_id = relationship_publication_id(
        relationship,
        parent_session_id,
        root_session_id,
        &transition,
    );
    let retained_bytes = serde_json::to_vec(&session)?
        .len()
        .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES)
        .min(ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                group.upsert_session(&session)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                true
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

fn replay_outputs_or_mark_behind(
    store: &Store,
    paths: &BTreeSet<PathBuf>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) {
    let Some(sink) = options.import_profile.sink().map(AsRef::as_ref) else {
        return;
    };
    for path in paths {
        let parsed = match parse_auggie_source(
            path,
            context,
            options.inventory_observation_token.as_deref(),
            true,
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "auggie_nativepath_output_source",
                    error.to_string(),
                ));
                continue;
            }
        };
        if let Err(error) = verify_committed_core(store, context, &parsed) {
            sink.mark_behind(ProOutputSinkError::new(
                "auggie_nativepath_output_core",
                error.to_string(),
            ));
            continue;
        }
        replay_parsed_outputs_or_mark_behind(&parsed, configured_source_root, Some(sink));
    }
}

fn replay_parsed_outputs_or_mark_behind(
    parsed: &ParsedAuggieSource,
    configured_source_root: &Path,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_parsed_outputs(parsed, configured_source_root, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "auggie_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn verify_committed_core(
    store: &Store,
    context: &ProviderAdapterContext,
    parsed: &ParsedAuggieSource,
) -> Result<()> {
    let locator_identity = provider_path_identity(&parsed.stamp.canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Auggie,
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Auggie output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = decode_cursor(committed.provider_cursor())?;
    validate_native_cursor(&cursor, &parsed.stamp.canonical_path)?;
    if !cursor.terminal
        || cursor.source_revision != parsed.source_revision
        || cursor.provider_session_id != parsed.session.provider_session_id
    {
        return Err(CaptureError::InvalidPayload(
            "Auggie output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn replay_parsed_outputs(
    parsed: &ParsedAuggieSource,
    configured_source_root: &Path,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let locator_identity = provider_path_identity(&parsed.stamp.canonical_path)?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Auggie.as_str().to_owned(),
        namespace_id: configured_source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let mut state = AuggieOutputState::new(
        output_source,
        progress,
        &parsed.source_revision,
        sink.materializer_revision(),
    )?;
    let mut next_output = state.next_output.min(parsed.outputs.len());
    if parsed.outputs.is_empty() {
        publish_output_page(parsed, sink, &locator_identity, &mut state, 0, 0, true)?;
        return Ok(());
    }
    while next_output < parsed.outputs.len() {
        let mut end = next_output;
        let mut content_bytes = 0_usize;
        while end < parsed.outputs.len()
            && end.saturating_sub(next_output) < AUGGIE_OUTPUTS_PER_PAGE
        {
            let next_bytes = content_bytes.saturating_add(parsed.outputs[end].content.len());
            if end != next_output && next_bytes > AUGGIE_OUTPUT_PAGE_CONTENT_BYTES {
                break;
            }
            if next_bytes > AUGGIE_OUTPUT_PAGE_CONTENT_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "one Auggie output body exceeds the bounded Pro page".to_owned(),
                ));
            }
            content_bytes = next_bytes;
            end = end.saturating_add(1);
        }
        let terminal = end == parsed.outputs.len();
        if !publish_output_page(
            parsed,
            sink,
            &locator_identity,
            &mut state,
            next_output,
            end,
            terminal,
        )? {
            break;
        }
        next_output = end;
    }
    Ok(())
}

struct AuggieOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    next_output: usize,
}

impl AuggieOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        observed_revision: &str,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                next_output: 0,
            });
        };
        let decoded = progress
            .cursor
            .as_ref()
            .filter(|cursor| cursor.version == AUGGIE_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<AuggieOutputFrontier>(&cursor.payload).ok())
            .filter(|frontier| frontier.version == AUGGIE_OUTPUT_FRONTIER_VERSION);
        let can_resume = progress.parser_revision == AUGGIE_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == observed_revision
            && decoded
                .as_ref()
                .is_some_and(|frontier| frontier.source_revision == observed_revision);
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let source_epoch = if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Auggie output source epoch exhausted",
                ))?
        };
        Ok(Self {
            source,
            source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if can_resume {
                ProOutputSourceDisposition::AppendOrResume
            } else {
                ProOutputSourceDisposition::Rewrite
            },
            next_output: if can_resume {
                decoded
                    .and_then(|frontier| usize::try_from(frontier.next_output).ok())
                    .unwrap_or(0)
            } else {
                0
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_output_page(
    parsed: &ParsedAuggieSource,
    sink: &dyn ProOutputSink,
    locator_identity: &str,
    state: &mut AuggieOutputState,
    start: usize,
    end: usize,
    terminal: bool,
) -> Result<bool> {
    let expected_frontier = output_frontier(&parsed.source_revision, start)?;
    let next_frontier = output_frontier(&parsed.source_revision, end)?;
    let observations = parsed.outputs[start..end]
        .iter()
        .map(|output| output_observation(parsed, output))
        .collect::<Result<Vec<_>>>()?;
    let content_bytes = parsed.outputs[start..end]
        .iter()
        .fold(0_usize, |total, output| {
            total.saturating_add(output.content.len())
        });
    let accounting = NativePageAccounting {
        logical_units: observations.len().max(1),
        conservative_serialized_bytes: content_bytes.saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES),
    };
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: parsed.source_revision.clone(),
        parser_revision: AUGGIE_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Auggie.as_str(), locator_identity),
        expected_frontier,
        next_frontier.clone(),
        terminal,
        accounting,
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        return Ok(false);
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn output_observation(
    parsed: &ParsedAuggieSource,
    output: &ParsedAuggieOutput,
) -> Result<ProOutputObservation> {
    let direct_session_id = parsed.session.provider_session_id.clone();
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                output.chat_index, output.node_collection, output.node_index
            ),
            native_sequence: u64::from(output.output_sequence),
            native_record_id: output.call_id.clone(),
            source_record_ordinal: Some(u64::try_from(output.chat_index).unwrap_or(u64::MAX)),
            source_record_subrecord_index: Some(output.output_sequence),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: output.occurred_at.map(|time| time.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: parsed
                .session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: parsed.session.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: parsed.session.external_agent_id.clone(),
            repository: None,
        },
        call_id: output.call_id.clone(),
        command: None,
        outcome: output.outcome.clone(),
        locator: OutputSourceLocator {
            version: 1,
            kind: "auggie-session-json-node-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": parsed.stamp.canonical_path,
                "chat_index": output.chat_index,
                "node_collection": output.node_collection,
                "node_index": output.node_index,
                "content_sha256": output.content_sha256,
            }))?,
        },
        content: output.content.clone(),
    })
}

fn output_frontier(source_revision: &str, next_output: usize) -> Result<NativeSafeFrontier> {
    let frontier = AuggieOutputFrontier {
        version: AUGGIE_OUTPUT_FRONTIER_VERSION,
        source_revision: source_revision.to_owned(),
        next_output: u64::try_from(next_output).map_err(|_| {
            CaptureError::InvalidPayload("Auggie output frontier exceeds u64".to_owned())
        })?,
    };
    NativeSafeFrontier::new(
        AUGGIE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn source_revision(stamp: &AuggieFileStamp, bytes: &[u8], inventory_token: Option<&str>) -> String {
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

fn event_prefix_digest(events: &[ParsedAuggieEvent]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-event-prefix-v1\0");
    for event in events {
        let encoded = released_auggie_event_encoding(&event.event)?;
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn empty_digest() -> String {
    format!("{:x}", Sha256::digest([]))
}

fn encode_cursor(cursor: &AuggieNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

fn decode_cursor(encoded: &str) -> Result<AuggieNativeCursor> {
    serde_json::from_str(encoded).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Auggie NativePath cursor: {error}"))
    })
}

fn validate_native_cursor(cursor: &AuggieNativeCursor, path: &Path) -> Result<()> {
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
                CaptureProvider::Auggie.as_str(),
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

fn source_publication_id(
    parsed: &ParsedAuggieSource,
    events: &[ParsedAuggieEvent],
    generation: u64,
    start: usize,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-publication-v1\0");
    digest.update(parsed.stamp.canonical_path.as_os_str().as_encoded_bytes());
    digest.update(parsed.source_revision.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update((start as u64).to_be_bytes());
    for event in events {
        digest.update(event.event.provider_event_hash.as_bytes());
    }
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("auggie-nativepath-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("auggie-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn relationship_publication_id(
    relationship: &RelationshipFact,
    parent_session_id: Option<Uuid>,
    root_session_id: Option<Uuid>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-auggie-nativepath-relationship-v1\0");
    digest.update(relationship.path.as_os_str().as_encoded_bytes());
    digest.update(relationship.provider_session_id.as_bytes());
    digest.update(
        parent_session_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    );
    digest.update(root_session_id.map(|id| id.to_string()).unwrap_or_default());
    digest.update(transition.next().cursor.as_bytes());
    format!("auggie-nativepath-relationship-v1:{:x}", digest.finalize())
}

fn retained_core_page_bytes(
    parsed: &ParsedAuggieSource,
    events: &[ParsedAuggieEvent],
) -> Result<usize> {
    let mut retained = serde_json::to_vec(&parsed.session.session_metadata)?
        .len()
        .saturating_add(serde_json::to_vec(&parsed.session.source_metadata)?.len())
        .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES);
    for event in events {
        retained = retained.saturating_add(released_auggie_event_encoding(&event.event)?.len());
    }
    if retained > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Auggie Core page exceeds the NativePath retained-byte bound".to_owned(),
        ));
    }
    Ok(retained)
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

fn released_auggie_event_encoding(event: &AuggieEvent) -> Result<Vec<u8>> {
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

fn provider_text(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
}

fn invalid_source_path(path: &Path, reason: &'static str) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason,
    }
}

fn saturating_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
