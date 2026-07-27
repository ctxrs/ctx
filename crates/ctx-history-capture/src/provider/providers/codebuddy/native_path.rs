//! Production CodeBuddy NativePath ingestion.
//!
//! CodeBuddy owns two unrelated persisted products: extension session
//! directories made of whole-JSON message files, and CLI project JSONL
//! transcripts.  They intentionally share only normalization and Store
//! publication policy.  Discovery, source revision, cursors, parsing, and
//! output replay remain shape-specific.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
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
        attach_verified_content_locator, jsonl::EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND, verified_content_address_supported,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentLocatorsV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{provider_role, provider_value_text},
        providers::task_json::task_json_time_field,
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    CODEBUDDY_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    normalization::{
        codebuddy_clean_content, codebuddy_decoded_message, codebuddy_message_text,
        codebuddy_normalized_rows, codebuddy_session_draft, codebuddy_title_from_text,
        CodeBuddyEventDraft, CodeBuddyEventInput, CodeBuddyNativeShape, CodeBuddySessionDraft,
        CodeBuddySessionInput,
    },
    source::CodeBuddyFrozenFile,
    CODEBUDDY_CLI_POLICY_REVISION, CODEBUDDY_MAX_CHECKPOINT_FAILURES, CODEBUDDY_MAX_FAILURE_BYTES,
    CODEBUDDY_NATIVE_CURSOR_VERSION,
};

#[path = "extension/discovery.rs"]
mod extension_discovery;
#[path = "extension/source.rs"]
mod extension_source;

use extension_source::{
    codebuddy_extension_line_number, codebuddy_extension_message_file,
    codebuddy_extension_metadata, codebuddy_extension_metadata_text, codebuddy_message_time,
    CodeBuddyExtensionMetadata, CodeBuddyExtensionObservation,
};

const CODEBUDDY_NATIVE_PAGE_MAX_UNITS: usize = 64;
const CODEBUDDY_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const CODEBUDDY_NATIVE_RECORD_MAX_BYTES: usize = CODEBUDDY_NATIVE_PAGE_MAX_BYTES - (64 * 1024);
const CODEBUDDY_OUTPUT_FRONTIER_VERSION: u32 = 1;
const CODEBUDDY_OUTPUT_PARSER_REVISION: &str = "codebuddy-nativepath-output-v1";
const CODEBUDDY_NATIVE_PUBLICATION_REVISION: &str = "codebuddy-nativepath-store-v1";
const CODEBUDDY_PUBLICATION_DOMAIN: &[u8] = b"ctx-codebuddy-nativepath-publication-v1\0";
const CODEBUDDY_RETIREMENT_DOMAIN: &[u8] = b"ctx-codebuddy-nativepath-retirement-v1\0";
const CODEBUDDY_INVENTORY_REVISION_DOMAIN: &[u8] = b"ctx-inventory-observed-source-revision-v1\0";
const CODEBUDDY_EXACT_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-source-revision-v1\0";
const CODEBUDDY_EXACT_PATH_IDENTITY_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-path-identity-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodeBuddySourceShape {
    Extension,
    Cli,
}

impl CodeBuddySourceShape {
    fn cursor_tag(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddySessionCheckpoint {
    native_session_id: String,
    project_hash: String,
    cwd: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    generated_title_anchor: Option<CodeBuddyGeneratedTitleAnchor>,
    row_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
enum CodeBuddyGeneratedTitleAnchor {
    Cli {
        native_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        payload_sha256: String,
    },
    Extension {
        message_index: u64,
    },
}

impl CodeBuddySessionCheckpoint {
    fn started_at(&self) -> Result<Option<DateTime<Utc>>> {
        checkpoint_time(self.started_at.as_deref(), "start time")
    }

    fn ended_at(&self) -> Result<Option<DateTime<Utc>>> {
        checkpoint_time(self.ended_at.as_deref(), "end time")
    }

    fn provider_session_id(&self) -> String {
        format!("{}/{}", self.project_hash, self.native_session_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddyNativeCursor {
    version: u32,
    shape: CodeBuddySourceShape,
    canonical_path: PathBuf,
    source_revision: String,
    source_identity: String,
    generation: u64,
    next_native_offset: u64,
    next_native_ordinal: u64,
    certified_prefix_sha256: String,
    file_identity: Option<String>,
    terminal: bool,
    accepted_events: u64,
    rejected_records: u64,
    failures: Vec<CodeBuddyCursorFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    incomplete_tail: Option<CodeBuddyCursorFailure>,
    session: CodeBuddySessionCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBuddyCursorFailure {
    line: usize,
    error: String,
}

impl CodeBuddyNativeCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::Json)
    }

    fn decode(value: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(value)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if cursor.version != CODEBUDDY_NATIVE_CURSOR_VERSION
            || cursor.source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.certified_prefix_sha256.len() != 64
            || cursor.failures.len() > CODEBUDDY_MAX_CHECKPOINT_FAILURES
        {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy NativePath cursor is malformed".to_owned(),
            ));
        }
        Ok(cursor)
    }

    fn replay_summary(&self) -> Result<ProviderImportSummary> {
        let accepted = usize::try_from(self.accepted_events).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy accepted event count exceeds platform limits")
        })?;
        let rejected = usize::try_from(self.rejected_records).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy rejection count exceeds platform limits")
        })?;
        let failed = rejected.saturating_add(usize::from(self.incomplete_tail.is_some()));
        let skipped_sessions = usize::from(accepted != 0);
        let mut summary = ProviderImportSummary {
            skipped: accepted.saturating_add(skipped_sessions),
            failed,
            skipped_sessions,
            skipped_events: accepted,
            accepted_content_records: accepted,
            failures: self
                .failures
                .iter()
                .map(|failure| ProviderImportFailure {
                    line: failure.line,
                    error: failure.error.clone(),
                })
                .chain(
                    self.incomplete_tail
                        .iter()
                        .map(|failure| ProviderImportFailure {
                            line: failure.line,
                            error: failure.error.clone(),
                        }),
                )
                .collect(),
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        Ok(summary)
    }
}

#[derive(Debug, Clone)]
struct CodeBuddySource {
    shape: CodeBuddySourceShape,
    path: PathBuf,
    canonical_path: PathBuf,
    configured_root: PathBuf,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    base_source_revision: String,
    source_revision: String,
    inventory_observation_token: Option<String>,
    session_ordinal: usize,
    frozen: Option<CodeBuddyFrozenFile>,
}

impl CodeBuddySource {
    fn output_identity(&self) -> OutputSourceIdentity {
        OutputSourceIdentity {
            provider: CaptureProvider::CodeBuddy.as_str().to_owned(),
            namespace_id: self.configured_root.display().to_string(),
            source_id: self.locator_identity.clone(),
        }
    }

    fn revalidate(&self) -> Result<bool> {
        match self.shape {
            CodeBuddySourceShape::Cli => self
                .frozen
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI source lost its frozen observation",
                ))?
                .revalidate(&self.path),
            CodeBuddySourceShape::Extension => {
                let (metadata, _) = codebuddy_extension_metadata(&self.path, self.session_ordinal)?;
                let Some(metadata) = metadata else {
                    return Ok(false);
                };
                let mut ignored = ProviderImportSummary::default();
                let current = CodeBuddyExtensionObservation::read(
                    &metadata,
                    self.session_ordinal,
                    &mut ignored,
                )?;
                Ok(current.canonical_session_dir == self.canonical_path
                    && effective_source_revision(
                        &current.source_revision,
                        self.inventory_observation_token.as_deref(),
                    ) == self.source_revision)
            }
        }
    }
}

#[derive(Debug)]
struct CodeBuddyInventory {
    sources: Vec<CodeBuddySource>,
    root_missing: bool,
}

#[derive(Debug)]
enum StoredCursor {
    None,
    Native {
        stored: SyncCursor,
        cursor: CodeBuddyNativeCursor,
    },
    ReleasedLegacy {
        stored: SyncCursor,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeBuddySourceChange {
    Fresh,
    Resume,
    Append,
    Rewrite,
    LegacyMigration,
}

struct CodeBuddySourcePlan {
    change: CodeBuddySourceChange,
    expected_store_cursor: Option<String>,
    cursor: CodeBuddyNativeCursor,
}

#[derive(Debug)]
struct CodeBuddyRecord {
    native_ordinal: u64,
    physical_line: usize,
    byte_start: Option<u64>,
    byte_end_exclusive: Option<u64>,
    native_bytes: Vec<u8>,
    core: Option<CodeBuddyCoreRow>,
    output: Option<CodeBuddyOutputDraft>,
}

#[derive(Debug)]
struct CodeBuddyCoreRow {
    session: CodeBuddySessionDraft,
    event: CodeBuddyEventDraft,
}

#[derive(Debug)]
struct CodeBuddyOutputDraft {
    native_record_id: String,
    content: Vec<u8>,
    occurred_at_unix_ms: i64,
    outcome: OutputOutcomeMetadata,
    kind: OutputObservationKind,
    call_id: Option<String>,
}

#[derive(Debug)]
struct CodeBuddyPage {
    records: Vec<CodeBuddyRecord>,
    expected_cursor: CodeBuddyNativeCursor,
    next_cursor: CodeBuddyNativeCursor,
    retained_bytes: usize,
}

impl CodeBuddyPage {
    fn logical_units(&self) -> usize {
        self.records.len().max(1)
    }
}

pub(crate) fn import_codebuddy_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_sources(path, &configured_root, &import_options)?;
    let committed_store = Store::open_read_only(store.path())?;
    let known_routes = known_routes(&committed_store, &context, &configured_root)?;

    if inventory.sources.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no CodeBuddy history sessions with index.json and messages/*.json or CLI project JSONL files were found",
        });
    }

    if import_options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &inventory.sources,
            &committed_store,
            &context,
            &import_options.import_profile,
        );
        return Ok(ProviderImportSummary::default());
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut summary = ProviderImportSummary::default();
    let operation = (|| {
        let mut changed_groups = 0_usize;
        let live_locators = inventory
            .sources
            .iter()
            .map(|source| source.locator_identity.as_str())
            .collect::<BTreeSet<_>>();
        let pending_retirement = known_routes
            .iter()
            .any(|route| !live_locators.contains(route.locator_identity.as_str()));
        for (source_index, source) in inventory.sources.iter().enumerate() {
            let source_summary = import_source_core(
                store,
                &committed_store,
                &bulk_guard,
                source,
                &context,
                &import_options,
                &mut changed_groups,
            )?;
            let stop = source_summary.work_remaining;
            summary.merge_from(source_summary);
            if stop {
                return Ok(summary);
            }
            if import_options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining =
                    source_index.saturating_add(1) < inventory.sources.len() || pending_retirement;
                return Ok(summary);
            }
        }

        summary.merge_from(retire_missing_routes(
            store,
            &bulk_guard,
            &context,
            &known_routes,
            &inventory.sources,
            import_options.capture_work_limit,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        )?);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let mut summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if !summary.work_remaining {
        replay_outputs_or_mark_behind(
            &inventory.sources,
            &committed_store,
            &context,
            &import_options.import_profile,
        );
    }
    if !inventory.sources.is_empty()
        && !summary.has_accepted_content()
        && summary.failed == 0
        && !summary.work_remaining
    {
        summary.record_failure(ProviderImportFailure {
            line: 0,
            error: "CodeBuddy history contained no real conversation messages".to_owned(),
        });
    }
    Ok(summary)
}

fn discover_sources(
    root: &Path,
    configured_root: &Path,
    options: &ProviderImportOptions,
) -> Result<CodeBuddyInventory> {
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if root_metadata.is_none() {
        return Ok(CodeBuddyInventory {
            sources: Vec::new(),
            root_missing: true,
        });
    }

    let mut extension_paths = BTreeSet::new();
    extension_discovery::visit_codebuddy_extension_sessions(root, &mut |path| {
        extension_paths.insert(fs::canonicalize(path)?);
        Ok(())
    })?;
    let cli_paths = discover_cli_paths(root)?;
    let mut sources = Vec::with_capacity(extension_paths.len().saturating_add(cli_paths.len()));

    for (index, canonical_path) in extension_paths.into_iter().enumerate() {
        let path = canonical_path.clone();
        let session_ordinal = index.saturating_add(1);
        let (metadata, _) = codebuddy_extension_metadata(&path, session_ordinal)?;
        let Some(metadata) = metadata else {
            continue;
        };
        let mut ignored = ProviderImportSummary::default();
        let observation =
            CodeBuddyExtensionObservation::read(&metadata, session_ordinal, &mut ignored)?;
        let locator_identity = provider_path_identity(&canonical_path)?;
        let source_revision = effective_source_revision(
            &observation.source_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Extension,
            path,
            canonical_path,
            configured_root,
            locator_identity,
            observation.source_revision,
            source_revision,
            options.inventory_observation_token.clone(),
            session_ordinal,
            None,
        )?);
    }
    let extension_count = sources.len();
    for (index, canonical_path) in cli_paths.into_iter().enumerate() {
        let frozen = CodeBuddyFrozenFile::read(&canonical_path)?;
        let base_revision =
            frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION);
        let locator_identity = provider_path_identity(&canonical_path)?;
        let source_revision = effective_source_revision(
            &base_revision,
            options.inventory_observation_token.as_deref(),
        );
        sources.push(build_source(
            CodeBuddySourceShape::Cli,
            canonical_path.clone(),
            canonical_path,
            configured_root,
            locator_identity,
            base_revision,
            source_revision,
            options.inventory_observation_token.clone(),
            extension_count.saturating_add(index).saturating_add(1),
            Some(frozen),
        )?);
    }
    sources.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
    Ok(CodeBuddyInventory {
        sources,
        root_missing: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_source(
    shape: CodeBuddySourceShape,
    path: PathBuf,
    canonical_path: PathBuf,
    configured_root: &Path,
    locator_identity: String,
    base_source_revision: String,
    source_revision: String,
    inventory_observation_token: Option<String>,
    session_ordinal: usize,
    frozen: Option<CodeBuddyFrozenFile>,
) -> Result<CodeBuddySource> {
    let raw_source_path = canonical_path.display().to_string();
    let source_root = configured_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "CodeBuddy NativePath source has no canonical identity",
    ))?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &locator_identity,
    );
    Ok(CodeBuddySource {
        shape,
        path,
        canonical_path,
        configured_root: configured_root.to_path_buf(),
        locator_identity,
        cursor_stream,
        proposed_source_identity,
        base_source_revision,
        source_revision,
        inventory_observation_token,
        session_ordinal,
        frozen,
    })
}

fn discover_cli_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let metadata = fs::symlink_metadata(root)?;
    let mut paths = BTreeSet::new();
    if metadata.file_type().is_file() {
        ensure_regular_provider_transcript_file(root)?;
        if root.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            paths.insert(fs::canonicalize(root)?);
        }
        return Ok(paths);
    }
    if !metadata.file_type().is_dir() {
        return Ok(paths);
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    let scan_root = if root.join("projects").is_dir() {
        root.join("projects")
    } else if root.file_name().and_then(|name| name.to_str()) == Some("projects")
        || root
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("projects")
    {
        root.to_path_buf()
    } else {
        return Ok(paths);
    };
    visit_cli_tree(&scan_root, &mut paths)?;
    Ok(paths)
}

fn visit_cli_tree(root: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit_cli_tree(&path, paths)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        {
            ensure_regular_provider_transcript_file(&path)?;
            paths.insert(fs::canonicalize(path)?);
        }
    }
    Ok(())
}

fn effective_source_revision(base: &str, inventory_token: Option<&str>) -> String {
    let Some(token) = inventory_token else {
        return base.to_owned();
    };
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_INVENTORY_REVISION_DOMAIN);
    digest.update((base.len() as u64).to_be_bytes());
    digest.update(base.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!(
        "inventory-observation-sha256-v1:{}",
        hex(&digest.finalize())
    )
}

fn checkpoint_time(value: Option<&str>, field: &str) -> Result<Option<DateTime<Utc>>> {
    value
        .map(|value| {
            value.parse::<DateTime<Utc>>().map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "CodeBuddy NativePath cursor has an invalid {field}"
                ))
            })
        })
        .transpose()
}

#[derive(Debug, Clone)]
struct KnownRoute {
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
}

fn known_routes(
    store: &Store,
    context: &ProviderAdapterContext,
    configured_root: &Path,
) -> Result<Vec<KnownRoute>> {
    let source_root = configured_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::CodeBuddy
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(CODEBUDDY_SOURCE_FORMAT)
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
        let source_revision = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .or_else(|| {
                source
                    .sync
                    .metadata
                    .pointer("/source_metadata/source_revision")
                    .and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_owned();
        if source_revision.is_empty() {
            continue;
        }
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let cursor_stream = provider_source_cursor_stream_for_path(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            &locator_identity,
        );
        if store
            .get_sync_cursor(None, &context.machine_id, &cursor_stream)?
            .is_none()
        {
            continue;
        }
        routes.insert(
            locator_identity.clone(),
            KnownRoute {
                locator_identity,
                cursor_stream,
                canonical_source_identity: canonical_source_identity.to_owned(),
                source_revision,
            },
        );
    }
    Ok(routes.into_values().collect())
}

fn load_stored_cursor(store: &Store, machine_id: &str, stream: &str) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(StoredCursor::Native {
            cursor: CodeBuddyNativeCursor::decode(committed.provider_cursor())?,
            stored,
        });
    }

    // Released pre-v0.27 CodeBuddy cursors are accepted only as a migration
    // input.  They never become a runtime resume frontier.
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
        return Ok(StoredCursor::ReleasedLegacy { stored });
    }
    Err(CaptureError::InvalidPayload(
        "CodeBuddy cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}

fn plan_source(
    store: &Store,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddySourcePlan> {
    let stored = load_stored_cursor(store, &context.machine_id, &source.cursor_stream)?;
    let initial = initial_cursor(source, context)?;
    match stored {
        StoredCursor::None => Ok(CodeBuddySourcePlan {
            change: CodeBuddySourceChange::Fresh,
            expected_store_cursor: None,
            cursor: initial,
        }),
        StoredCursor::ReleasedLegacy { stored } => {
            let mut cursor = initial;
            cursor.generation = 1;
            Ok(CodeBuddySourcePlan {
                change: CodeBuddySourceChange::LegacyMigration,
                expected_store_cursor: Some(stored.cursor),
                cursor,
            })
        }
        StoredCursor::Native { stored, mut cursor } => {
            if cursor.shape != source.shape
                || cursor.canonical_path != source.canonical_path
                || cursor.source_identity != source.proposed_source_identity
            {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy NativePath cursor route does not match the selected source"
                        .to_owned(),
                ));
            }
            if cursor.source_revision == source.source_revision {
                return Ok(CodeBuddySourcePlan {
                    change: CodeBuddySourceChange::Resume,
                    expected_store_cursor: Some(stored.cursor),
                    cursor,
                });
            }

            if source.shape == CodeBuddySourceShape::Cli && cli_prefix_matches(source, &cursor)? {
                cursor.source_revision.clone_from(&source.source_revision);
                cursor.terminal = false;
                cursor.incomplete_tail = None;
                return Ok(CodeBuddySourcePlan {
                    change: CodeBuddySourceChange::Append,
                    expected_store_cursor: Some(stored.cursor),
                    cursor,
                });
            }

            let generation =
                cursor
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy source generation overflowed",
                    ))?;
            let mut replacement = initial;
            replacement.generation = generation;
            Ok(CodeBuddySourcePlan {
                change: CodeBuddySourceChange::Rewrite,
                expected_store_cursor: Some(stored.cursor),
                cursor: replacement,
            })
        }
    }
}

fn initial_cursor(
    source: &CodeBuddySource,
    _context: &ProviderAdapterContext,
) -> Result<CodeBuddyNativeCursor> {
    let session = match source.shape {
        CodeBuddySourceShape::Cli => {
            let native_session_id = source
                .canonical_path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("unknown-session")
                .to_owned();
            CodeBuddySessionCheckpoint {
                native_session_id,
                project_hash: cli_project_hash(&source.canonical_path),
                ..CodeBuddySessionCheckpoint::default()
            }
        }
        CodeBuddySourceShape::Extension => {
            let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
            let metadata = metadata.ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: source.path.clone(),
                reason: "CodeBuddy extension session index is unreadable",
            })?;
            CodeBuddySessionCheckpoint {
                native_session_id: metadata.native_session_id.clone(),
                project_hash: metadata.project_hash.clone(),
                cwd: None,
                started_at: metadata
                    .conversation
                    .as_ref()
                    .and_then(|value| {
                        task_json_time_field(value, &["createdAt", "created_at", "timestamp"])
                    })
                    .map(|value| value.to_rfc3339()),
                ended_at: metadata
                    .conversation
                    .as_ref()
                    .and_then(|value| {
                        task_json_time_field(
                            value,
                            &["lastMessageAt", "updatedAt", "completedAt", "last_modified"],
                        )
                    })
                    .map(|value| value.to_rfc3339()),
                generated_title_anchor: None,
                row_count: 0,
            }
        }
    };
    Ok(CodeBuddyNativeCursor {
        version: CODEBUDDY_NATIVE_CURSOR_VERSION,
        shape: source.shape,
        canonical_path: source.canonical_path.clone(),
        source_revision: source.source_revision.clone(),
        source_identity: source.proposed_source_identity.clone(),
        generation: 0,
        next_native_offset: 0,
        next_native_ordinal: 0,
        certified_prefix_sha256: sha256_hex(&[]),
        file_identity: source
            .frozen
            .as_ref()
            .map(CodeBuddyFrozenFile::identity_token),
        terminal: false,
        accepted_events: 0,
        rejected_records: 0,
        failures: Vec::new(),
        incomplete_tail: None,
        session,
    })
}

fn cli_prefix_matches(source: &CodeBuddySource, cursor: &CodeBuddyNativeCursor) -> Result<bool> {
    let Some(frozen) = source.frozen.as_ref() else {
        return Ok(false);
    };
    if cursor.next_native_offset > frozen.length
        || cursor.file_identity.as_deref() != Some(frozen.identity_token().as_str())
    {
        return Ok(false);
    }
    Ok(file_prefix_sha256(&source.path, cursor.next_native_offset)?
        == cursor.certified_prefix_sha256)
}

fn file_prefix_sha256(path: &Path, length: u64) -> Result<String> {
    let mut file = File::open(path)?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    let mut digest = Sha256::new();
    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("CodeBuddy prefix length exceeds platform limits")
        })?;
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hex(&digest.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex(&digest.finalize())
}

fn import_source_core(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let mut plan = plan_source(store, source, context)?;
    if plan.change == CodeBuddySourceChange::Resume && plan.cursor.terminal {
        return plan.cursor.replay_summary();
    }

    let mut summary = ProviderImportSummary::default();
    loop {
        let expected = plan.cursor.clone();
        let Some(page) = next_source_page(source, &expected, context)? else {
            break;
        };
        if !source.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut page_summary = publish_core_page(
            store,
            committed_store,
            bulk_guard,
            source,
            context,
            options,
            plan.expected_store_cursor.as_deref(),
            page,
        )?;
        if page_summary.work_result() == ProviderImportWorkResult::Changed {
            *changed_groups = changed_groups.saturating_add(1);
        }
        page_summary.work_remaining = false;
        summary.merge_from(page_summary);

        let stored = store
            .get_sync_cursor(None, &context.machine_id, &source.cursor_stream)?
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy NativePath commit did not publish its cursor",
            ))?;
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        plan.cursor = CodeBuddyNativeCursor::decode(committed.provider_cursor())?;
        plan.expected_store_cursor = Some(stored.cursor);

        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && *changed_groups != 0 {
            summary.work_remaining = !plan.cursor.terminal;
            return Ok(summary);
        }
        if plan.cursor.terminal {
            break;
        }
    }
    Ok(summary)
}

fn next_source_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<Option<CodeBuddyPage>> {
    if cursor.terminal {
        return Ok(None);
    }
    match source.shape {
        CodeBuddySourceShape::Cli => next_cli_page(source, cursor, context).map(Some),
        CodeBuddySourceShape::Extension => next_extension_page(source, cursor, context).map(Some),
    }
}

fn next_cli_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let frozen = source.frozen.as_ref().ok_or(CaptureError::SystemInvariant(
        "CodeBuddy CLI page has no frozen source",
    ))?;
    if cursor.next_native_offset > frozen.length {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy CLI cursor exceeds its source".to_owned(),
        ));
    }
    let file = File::open(&source.path)?;
    if CodeBuddyFrozenFile::from_metadata(&file.metadata()?)? != *frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(cursor.next_native_offset))?;
    let mut next = cursor.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.file_identity = Some(frozen.identity_token());
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut offset = cursor.next_native_offset;
    let mut reached_eof = false;
    let mut session_title = codebuddy_session_title(source, &next.session)?;

    while records.len() < CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
        let start = offset;
        let record = read_bounded_jsonl_record(
            &mut reader,
            CODEBUDDY_NATIVE_RECORD_MAX_BYTES.min(MAX_PROVIDER_JSONL_LINE_BYTES),
        )?;
        if record.observed_bytes == 0 {
            reached_eof = true;
            break;
        }
        offset = offset
            .checked_add(record.observed_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI byte offset overflowed",
            ))?;
        let mut payload = record.payload.as_slice();
        if record.newline_terminated && payload.last() == Some(&b'\n') {
            payload = &payload[..payload.len().saturating_sub(1)];
            if payload.last() == Some(&b'\r') {
                payload = &payload[..payload.len().saturating_sub(1)];
            }
        }
        let ordinal = next.next_native_ordinal;
        let physical_line = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy CLI line exceeds platform limits",
            ))?;
        let record_bytes = payload.to_vec();
        let record_bound = record_bytes.len().saturating_add(4 * 1024);
        if !records.is_empty()
            && retained_bytes.saturating_add(record_bound) > CODEBUDDY_NATIVE_PAGE_MAX_BYTES
        {
            break;
        }

        next.next_native_offset = offset;
        next.next_native_ordinal =
            next.next_native_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI ordinal overflowed",
                ))?;
        if record.oversized {
            record_cursor_failure(
                &mut next,
                physical_line,
                format!(
                    "provider record exceeds the NativePath page bound (observed {} bytes)",
                    record.observed_bytes
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: Some(start),
                byte_end_exclusive: Some(offset),
                native_bytes: Vec::new(),
                core: None,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(256);
            continue;
        }
        if record_bytes.iter().all(u8::is_ascii_whitespace) {
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: Some(start),
                byte_end_exclusive: Some(offset),
                native_bytes: record_bytes,
                core: None,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(record_bound);
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(_) if !record.newline_terminated => {
                next.next_native_offset = start;
                next.next_native_ordinal = ordinal;
                next.incomplete_tail = Some(CodeBuddyCursorFailure {
                    line: physical_line,
                    error: bounded_failure(format!(
                        "{}: incomplete trailing JSONL record",
                        source.path.display()
                    )),
                });
                reached_eof = true;
                break;
            }
            Err(error) => {
                record_cursor_failure(
                    &mut next,
                    physical_line,
                    format!("{}: malformed JSONL: {error}", source.path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    physical_line,
                    byte_start: Some(start),
                    byte_end_exclusive: Some(offset),
                    native_bytes: record_bytes,
                    core: None,
                    output: None,
                });
                retained_bytes = retained_bytes.saturating_add(record_bound);
                continue;
            }
        };
        next.session.row_count =
            next.session
                .row_count
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI row count overflowed",
                ))?;
        update_cli_session(&mut next.session, &value, context.imported_at);
        if session_title.is_none()
            && next.session.generated_title_anchor.is_none()
            && provider_role(value.get("role").and_then(Value::as_str)) == EventRole::User
        {
            session_title = codebuddy_title_from_text(&cli_message_text(&value));
            if session_title.is_some() {
                next.session.generated_title_anchor = Some(CodeBuddyGeneratedTitleAnchor::Cli {
                    native_ordinal: ordinal,
                    byte_start: start,
                    byte_end_exclusive: offset,
                    payload_sha256: sha256_hex(&record_bytes),
                });
            }
        }
        let (core, output) = cli_core_row(
            source,
            context,
            &next.session,
            session_title.as_deref(),
            next.generation,
            ordinal,
            physical_line,
            start,
            offset,
            &record_bytes,
            value,
        )?;
        if core.is_some() {
            next.accepted_events =
                next.accepted_events
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy accepted event count overflowed",
                    ))?;
        }
        records.push(CodeBuddyRecord {
            native_ordinal: ordinal,
            physical_line,
            byte_start: Some(start),
            byte_end_exclusive: Some(offset),
            native_bytes: record_bytes,
            core,
            output,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if offset == frozen.length {
        reached_eof = true;
    }
    next.terminal = reached_eof;
    next.certified_prefix_sha256 = file_prefix_sha256(&source.path, next.next_native_offset)?;
    retained_bytes = retained_bytes
        .saturating_add(serde_json::to_vec(&next)?.len())
        .saturating_add(serde_json::to_vec(cursor)?.len());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        expected_cursor: cursor.clone(),
        next_cursor: next,
        retained_bytes,
    })
}

struct BoundedJsonlRecord {
    observed_bytes: u64,
    payload: Vec<u8>,
    newline_terminated: bool,
    oversized: bool,
}

fn read_bounded_jsonl_record(
    reader: &mut impl BufRead,
    payload_limit: usize,
) -> Result<BoundedJsonlRecord> {
    let retained_limit = payload_limit.saturating_add(2);
    let mut observed_bytes = 0_u64;
    let mut payload = Vec::new();
    let mut newline_terminated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| {
                newline_terminated = true;
                index.saturating_add(1)
            })
            .unwrap_or(available.len());
        observed_bytes =
            observed_bytes
                .checked_add(consumed as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy CLI record length overflowed",
                ))?;
        if payload.len() < retained_limit {
            let retained = consumed.min(retained_limit.saturating_sub(payload.len()));
            payload.extend_from_slice(&available[..retained]);
        }
        reader.consume(consumed);
        if newline_terminated {
            break;
        }
    }
    let observed_payload_bytes = observed_bytes.saturating_sub(u64::from(newline_terminated));
    Ok(BoundedJsonlRecord {
        observed_bytes,
        oversized: observed_payload_bytes > payload_limit as u64,
        payload,
        newline_terminated,
    })
}

fn codebuddy_session_title(
    source: &CodeBuddySource,
    session: &CodeBuddySessionCheckpoint,
) -> Result<Option<String>> {
    if source.shape == CodeBuddySourceShape::Extension {
        let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
        let metadata = metadata.ok_or(CaptureError::SourceChangedDuringCapture)?;
        if let Some(title) = codebuddy_extension_metadata_text(&metadata, &["name", "title"]) {
            return Ok(Some(title));
        }
    }
    let Some(anchor) = session.generated_title_anchor.as_ref() else {
        return Ok(None);
    };
    let title = match (source.shape, anchor) {
        (
            CodeBuddySourceShape::Cli,
            CodeBuddyGeneratedTitleAnchor::Cli {
                native_ordinal: _,
                byte_start,
                byte_end_exclusive,
                payload_sha256,
            },
        ) => {
            let length =
                byte_end_exclusive
                    .checked_sub(*byte_start)
                    .ok_or(CaptureError::InvalidPayload(
                        "CodeBuddy CLI title anchor has an invalid byte range".to_owned(),
                    ))?;
            if length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy CLI title anchor exceeds its record bound".to_owned(),
                ));
            }
            let mut file = File::open(&source.path)?;
            file.seek(SeekFrom::Start(*byte_start))?;
            let mut record = vec![
                0_u8;
                usize::try_from(length).map_err(|_| {
                    CaptureError::InvalidPayload(
                        "CodeBuddy CLI title anchor exceeds platform limits".to_owned(),
                    )
                })?
            ];
            file.read_exact(&mut record)?;
            if record.last() == Some(&b'\n') {
                record.pop();
                if record.last() == Some(&b'\r') {
                    record.pop();
                }
            }
            if sha256_hex(&record) != *payload_sha256 {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let value: Value = serde_json::from_slice(&record)?;
            if provider_role(value.get("role").and_then(Value::as_str)) != EventRole::User {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy CLI title anchor no longer identifies a user message".to_owned(),
                ));
            }
            codebuddy_title_from_text(&cli_message_text(&value))
        }
        (
            CodeBuddySourceShape::Extension,
            CodeBuddyGeneratedTitleAnchor::Extension { message_index },
        ) => {
            let message_index = usize::try_from(*message_index).map_err(|_| {
                CaptureError::InvalidPayload(
                    "CodeBuddy extension title anchor exceeds platform limits".to_owned(),
                )
            })?;
            let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
            let metadata = metadata.ok_or(CaptureError::SourceChangedDuringCapture)?;
            let message_ref = metadata
                .messages()
                .get(message_index)
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
            let (message_path, frozen) =
                codebuddy_extension_message_file(&metadata.session_dir, message_ref)
                    .map_err(CaptureError::InvalidPayload)?;
            let record = fs::read(&message_path)?;
            if !frozen.revalidate(&message_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let raw_message: Value = serde_json::from_slice(&record)?;
            let role = message_ref
                .get("role")
                .and_then(Value::as_str)
                .or_else(|| raw_message.get("role").and_then(Value::as_str));
            if provider_role(role) != EventRole::User {
                return Err(CaptureError::InvalidPayload(
                    "CodeBuddy extension title anchor no longer identifies a user message"
                        .to_owned(),
                ));
            }
            let decoded = codebuddy_decoded_message(&raw_message);
            codebuddy_title_from_text(&codebuddy_message_text(&decoded, &raw_message))
        }
        _ => {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy title anchor does not match its source shape".to_owned(),
            ));
        }
    };
    title.map(Some).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "CodeBuddy title anchor no longer resolves to non-empty text".to_owned(),
        )
    })
}

fn next_extension_page(
    source: &CodeBuddySource,
    cursor: &CodeBuddyNativeCursor,
    context: &ProviderAdapterContext,
) -> Result<CodeBuddyPage> {
    let (metadata, metadata_summary) =
        codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
    let metadata = metadata.ok_or(CaptureError::InvalidProviderTranscriptPath {
        path: source.path.clone(),
        reason: "CodeBuddy extension session index is unreadable",
    })?;
    let mut next = cursor.clone();
    next.source_revision.clone_from(&source.source_revision);
    next.terminal = false;
    let mut records = Vec::new();
    let mut retained_bytes = 0_usize;
    let mut valid_ordinal = 0_u64;
    let mut reached_end = true;
    let mut session_title = codebuddy_session_title(source, &next.session)?;

    if cursor.next_native_ordinal == 0 {
        for failure in metadata_summary.failures {
            record_cursor_failure(&mut next, failure.line, failure.error)?;
        }
    }

    for (message_index, message_ref) in metadata.messages().iter().enumerate() {
        let (message_path, frozen) =
            match codebuddy_extension_message_file(&metadata.session_dir, message_ref) {
                Ok(value) => value,
                Err(error) => {
                    if cursor.next_native_ordinal == 0 {
                        record_cursor_failure(
                            &mut next,
                            codebuddy_extension_line_number(source.session_ordinal, message_index),
                            error,
                        )?;
                    }
                    continue;
                }
            };
        let ordinal = valid_ordinal;
        valid_ordinal = valid_ordinal
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy extension ordinal overflowed",
            ))?;
        if ordinal < cursor.next_native_ordinal {
            continue;
        }
        if records.len() >= CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
            reached_end = false;
            break;
        }
        let physical_line = codebuddy_extension_line_number(source.session_ordinal, message_index);
        let record_bound = usize::try_from(frozen.length)
            .unwrap_or(usize::MAX)
            .saturating_add(4 * 1024);
        if !records.is_empty()
            && retained_bytes.saturating_add(record_bound) > CODEBUDDY_NATIVE_PAGE_MAX_BYTES
        {
            reached_end = false;
            break;
        }
        next.next_native_ordinal =
            next.next_native_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy extension ordinal overflowed",
                ))?;
        next.session.row_count =
            next.session
                .row_count
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "CodeBuddy extension row count overflowed",
                ))?;
        if frozen.length > CODEBUDDY_NATIVE_RECORD_MAX_BYTES as u64 {
            record_cursor_failure(
                &mut next,
                physical_line,
                format!(
                    "{}: CodeBuddy message JSON exceeds the NativePath page bound",
                    message_path.display()
                ),
            )?;
            records.push(CodeBuddyRecord {
                native_ordinal: ordinal,
                physical_line,
                byte_start: None,
                byte_end_exclusive: None,
                native_bytes: Vec::new(),
                core: None,
                output: None,
            });
            retained_bytes = retained_bytes.saturating_add(256);
            continue;
        }
        let record_bytes = fs::read(&message_path)?;
        if !frozen.revalidate(&message_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let raw_message = match serde_json::from_slice::<Value>(&record_bytes) {
            Ok(value) => value,
            Err(error) => {
                record_cursor_failure(
                    &mut next,
                    physical_line,
                    format!("{}: json error: {error}", message_path.display()),
                )?;
                records.push(CodeBuddyRecord {
                    native_ordinal: ordinal,
                    physical_line,
                    byte_start: None,
                    byte_end_exclusive: None,
                    native_bytes: record_bytes,
                    core: None,
                    output: None,
                });
                retained_bytes = retained_bytes.saturating_add(record_bound);
                continue;
            }
        };
        let (core, output) = extension_core_row(
            context,
            &metadata,
            &mut next.session,
            &mut session_title,
            next.generation,
            ordinal,
            message_index,
            message_ref,
            &message_path,
            &record_bytes,
            raw_message,
        )?;
        if core.is_some() {
            next.accepted_events =
                next.accepted_events
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "CodeBuddy accepted event count overflowed",
                    ))?;
        }
        records.push(CodeBuddyRecord {
            native_ordinal: ordinal,
            physical_line,
            byte_start: None,
            byte_end_exclusive: None,
            native_bytes: record_bytes,
            core,
            output,
        });
        retained_bytes = retained_bytes.saturating_add(record_bound);
    }

    if next.next_native_ordinal < valid_ordinal {
        reached_end = false;
    }
    next.terminal = reached_end;
    next.certified_prefix_sha256 = sha256_hex(source.source_revision.as_bytes());
    retained_bytes = retained_bytes
        .saturating_add(serde_json::to_vec(&next)?.len())
        .saturating_add(serde_json::to_vec(cursor)?.len());
    validate_page_bounds(records.len().max(1), retained_bytes)?;
    Ok(CodeBuddyPage {
        records,
        expected_cursor: cursor.clone(),
        next_cursor: next,
        retained_bytes,
    })
}

fn validate_page_bounds(units: usize, bytes: usize) -> Result<()> {
    if units == 0 || units > CODEBUDDY_NATIVE_PAGE_MAX_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "CodeBuddy NativePath page has {units} logical units"
        )));
    }
    if bytes > CODEBUDDY_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "CodeBuddy NativePath page has {bytes} conservatively encoded bytes"
        )));
    }
    Ok(())
}

fn record_cursor_failure(
    cursor: &mut CodeBuddyNativeCursor,
    line: usize,
    error: String,
) -> Result<()> {
    cursor.rejected_records =
        cursor
            .rejected_records
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy rejection count overflowed",
            ))?;
    if cursor.failures.len() < CODEBUDDY_MAX_CHECKPOINT_FAILURES {
        cursor.failures.push(CodeBuddyCursorFailure {
            line,
            error: bounded_failure(error),
        });
    }
    Ok(())
}

fn bounded_failure(mut error: String) -> String {
    if error.is_empty() {
        return "CodeBuddy record was deterministically rejected".to_owned();
    }
    if error.len() <= CODEBUDDY_MAX_FAILURE_BYTES {
        return error;
    }
    let mut boundary = CODEBUDDY_MAX_FAILURE_BYTES;
    while !error.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    error.truncate(boundary);
    error
}

fn update_cli_session(
    session: &mut CodeBuddySessionCheckpoint,
    value: &Value,
    imported_at: DateTime<Utc>,
) {
    if let Some(session_id) = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 8 * 1024)
    {
        session.native_session_id = session_id.to_owned();
    }
    if session.cwd.is_none() {
        session.cwd = value
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.len() <= 8 * 1024)
            .map(str::to_owned);
    }
    let Some(occurred_at) = cli_message_time(value, imported_at) else {
        return;
    };
    let prior_start = session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    let prior_end = session
        .ended_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok());
    session.started_at = Some(
        prior_start
            .map(|prior| prior.min(occurred_at))
            .unwrap_or(occurred_at)
            .to_rfc3339(),
    );
    session.ended_at = Some(
        prior_end
            .map(|prior| prior.max(occurred_at))
            .unwrap_or(occurred_at)
            .to_rfc3339(),
    );
}

#[allow(clippy::too_many_arguments)]
fn cli_core_row(
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    session: &CodeBuddySessionCheckpoint,
    session_title: Option<&str>,
    generation: u64,
    ordinal: u64,
    physical_line: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_bytes: &[u8],
    value: Value,
) -> Result<(Option<CodeBuddyCoreRow>, Option<CodeBuddyOutputDraft>)> {
    let text = cli_message_text(&value);
    let role = value.get("role").and_then(Value::as_str).map(str::to_owned);
    let ref_type = value.get("type").and_then(Value::as_str).map(str::to_owned);
    let event_type = codebuddy_event_type(role.as_deref(), ref_type.as_deref(), &value);
    let accepted_type = matches!(
        event_type,
        EventType::Message | EventType::ToolCall | EventType::ToolOutput | EventType::CommandOutput
    );
    if !accepted_type || text.trim().is_empty() {
        return Ok((None, None));
    }
    let native_message_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{physical_line}"));
    let occurred_at = cli_message_time(&value, context.imported_at).unwrap_or(context.imported_at);
    let output = output_draft(
        event_type,
        ordinal,
        native_message_id.clone(),
        occurred_at,
        &text,
        &value,
    );
    let provider_session_id = session.provider_session_id();
    let source_path = source.canonical_path.display().to_string();
    let session_index = json!({
        "source": "codebuddy_cli_jsonl",
        "path": source_path,
        "rows": session.row_count,
    });
    let started_at = session.started_at()?.unwrap_or(occurred_at);
    let (session, mut event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            native_session_id: &session.native_session_id,
            project_hash: &session.project_hash,
            started_at,
            ended_at: session.ended_at()?,
            title: session_title,
            cwd: session.cwd.as_deref(),
            project_index: None,
            conversation: None,
            session_index: &session_index,
            file_names: &["projects/*/*.jsonl"],
            shape: CodeBuddyNativeShape::Cli,
        },
        CodeBuddyEventInput {
            provider_event_index: native_provider_event_index(generation, ordinal)?,
            native_message_id,
            event_type,
            role,
            ref_type,
            occurred_at,
            text,
            raw_message: value.clone(),
            decoded_message: value.clone(),
        },
    );
    if event_type == EventType::Message {
        attach_cli_complete_content_locator(
            &mut event,
            &value,
            physical_line,
            record_bytes,
            source,
            byte_start,
            byte_end_exclusive,
        )?;
    }
    Ok((Some(CodeBuddyCoreRow { session, event }), output))
}

#[allow(clippy::too_many_arguments)]
fn extension_core_row(
    context: &ProviderAdapterContext,
    metadata: &CodeBuddyExtensionMetadata,
    session: &mut CodeBuddySessionCheckpoint,
    session_title: &mut Option<String>,
    generation: u64,
    ordinal: u64,
    message_index: usize,
    message_ref: &Value,
    message_path: &Path,
    record_bytes: &[u8],
    raw_message: Value,
) -> Result<(Option<CodeBuddyCoreRow>, Option<CodeBuddyOutputDraft>)> {
    let decoded_message = codebuddy_decoded_message(&raw_message);
    let text = codebuddy_message_text(&decoded_message, &raw_message);
    if text.trim().is_empty() {
        return Ok((None, None));
    }
    let role = message_ref
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| raw_message.get("role").and_then(Value::as_str))
        .map(str::to_owned);
    let ref_type = message_ref
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| raw_message.get("type").and_then(Value::as_str))
        .map(str::to_owned);
    let event_type = codebuddy_event_type(role.as_deref(), ref_type.as_deref(), &decoded_message);
    let message_id = message_ref
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy extension message lost its manifest identity",
        ))?
        .to_owned();
    let occurred_at = codebuddy_message_time(
        &raw_message,
        &decoded_message,
        message_path,
        context.imported_at,
    );
    update_session_times(session, occurred_at);
    if session_title.is_none()
        && session.generated_title_anchor.is_none()
        && provider_role(role.as_deref()) == EventRole::User
    {
        *session_title = codebuddy_title_from_text(&text);
        if session_title.is_some() {
            session.generated_title_anchor = Some(CodeBuddyGeneratedTitleAnchor::Extension {
                message_index: message_index as u64,
            });
        }
    }
    let output = output_draft(
        event_type,
        ordinal,
        message_id.clone(),
        occurred_at,
        &text,
        &raw_message,
    );
    let provider_session_id = session.provider_session_id();
    let cwd = codebuddy_extension_metadata_text(
        metadata,
        &["projectPath", "project_path", "cwd", "workspace"],
    );
    let (session, mut event) = codebuddy_normalized_rows(
        &CodeBuddySessionInput {
            provider_session_id: &provider_session_id,
            native_session_id: &session.native_session_id,
            project_hash: &session.project_hash,
            started_at: session.started_at()?.unwrap_or(occurred_at),
            ended_at: session.ended_at()?,
            title: session_title.as_deref(),
            cwd: cwd.as_deref(),
            project_index: metadata.project_index.as_ref(),
            conversation: metadata.conversation.as_ref(),
            session_index: &metadata.session_index,
            file_names: &["index.json", "messages/*.json"],
            shape: CodeBuddyNativeShape::Extension,
        },
        CodeBuddyEventInput {
            provider_event_index: native_provider_event_index(generation, message_index as u64)?,
            native_message_id: message_id,
            event_type,
            role,
            ref_type,
            occurred_at,
            text: text.clone(),
            raw_message,
            decoded_message,
        },
    );
    if event_type == EventType::Message {
        let native_id = event.event_hash.clone();
        attach_extension_complete_content_locator(
            &mut event,
            ordinal,
            &native_id,
            record_bytes,
            &text,
        )?;
    }
    Ok((Some(CodeBuddyCoreRow { session, event }), output))
}

fn native_provider_event_index(generation: u64, native_index: u64) -> Result<u64> {
    const INDEX_BITS: u32 = 48;
    const MAX_GENERATION: u64 = (1_u64 << (u64::BITS - INDEX_BITS)) - 1;
    const MAX_NATIVE_INDEX: u64 = (1_u64 << INDEX_BITS) - 1;
    if generation > MAX_GENERATION || native_index > MAX_NATIVE_INDEX {
        return Err(CaptureError::InvalidPayload(
            "CodeBuddy NativePath generation or event index exceeds identity bounds".to_owned(),
        ));
    }
    Ok((generation << INDEX_BITS) | native_index)
}

fn update_session_times(session: &mut CodeBuddySessionCheckpoint, occurred_at: DateTime<Utc>) {
    let started = session
        .started_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.min(occurred_at))
        .unwrap_or(occurred_at);
    let ended = session
        .ended_at
        .as_deref()
        .and_then(|value| value.parse::<DateTime<Utc>>().ok())
        .map(|value| value.max(occurred_at))
        .unwrap_or(occurred_at);
    session.started_at = Some(started.to_rfc3339());
    session.ended_at = Some(ended.to_rfc3339());
}

fn attach_cli_complete_content_locator(
    event: &mut CodeBuddyEventDraft,
    value: &Value,
    physical_line: usize,
    record_bytes: &[u8],
    source: &CodeBuddySource,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<()> {
    if event.event_type != EventType::Message
        || !verified_content_address_supported(
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        )
    {
        return Ok(());
    }
    let Some((text, native_record_id)) =
        codebuddy_cli_complete_content_record(value, physical_line)
    else {
        return Ok(());
    };
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported exact CodeBuddy JSONL route must have a verified-content profile",
        ));
    };

    let mut locator = [0_u8; 80];
    locator[..8].copy_from_slice(&byte_start.to_be_bytes());
    locator[8..16].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    locator[16..48].copy_from_slice(&codebuddy_complete_content_digest(
        CODEBUDDY_EXACT_SOURCE_REVISION_DIGEST_DOMAIN,
        &source.base_source_revision,
    ));
    locator[48..].copy_from_slice(&codebuddy_complete_content_digest(
        CODEBUDDY_EXACT_PATH_IDENTITY_DIGEST_DOMAIN,
        &source.locator_identity,
    ));
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )
}

fn attach_extension_complete_content_locator(
    event: &mut CodeBuddyEventDraft,
    source_record_ordinal: u64,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    const STRUCTURED_LOCATOR_MAGIC: &[u8; 4] = b"SC\0\x01";
    const MAX_NATIVE_RECORD_ID_BYTES: usize = 1_024;

    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > MAX_NATIVE_RECORD_ID_BYTES
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "structured complete-content native record identity is invalid".to_owned(),
        ));
    }

    let provider = CaptureProvider::CodeBuddy.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "structured complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut locator_value = Vec::with_capacity(
        STRUCTURED_LOCATOR_MAGIC.len() + 1 + provider.len() + 8 + 4 + 2 + native_id.len(),
    );
    locator_value.extend_from_slice(STRUCTURED_LOCATOR_MAGIC);
    locator_value.push(provider_len);
    locator_value.extend_from_slice(provider);
    locator_value.extend_from_slice(&source_record_ordinal.to_be_bytes());
    locator_value.extend_from_slice(&0_u32.to_be_bytes());
    locator_value.extend_from_slice(&native_len.to_be_bytes());
    locator_value.extend_from_slice(native_id);

    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("structured content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported structured message route must have a verified-content profile",
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
        "structured complete-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn codebuddy_complete_content_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn codebuddy_event_type(role: Option<&str>, ref_type: Option<&str>, value: &Value) -> EventType {
    let role = role.unwrap_or_default().to_ascii_lowercase();
    let kind = ref_type.unwrap_or_default().to_ascii_lowercase();
    if matches!(role.as_str(), "tool" | "function")
        || kind.contains("tool_result")
        || kind.contains("tool-result")
        || kind.contains("tool_output")
        || kind.contains("tool-output")
        || kind == "result"
        || kind == "output"
        || value.get("toolUseResult").is_some()
        || value.get("tool_result").is_some()
    {
        EventType::ToolOutput
    } else if kind.contains("tool_call")
        || kind.contains("tool-call")
        || value.get("toolUse").is_some()
        || value.get("tool_call").is_some()
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

fn output_draft(
    event_type: EventType,
    ordinal: u64,
    native_record_id: String,
    occurred_at: DateTime<Utc>,
    text: &str,
    value: &Value,
) -> Option<CodeBuddyOutputDraft> {
    if !matches!(event_type, EventType::ToolOutput | EventType::CommandOutput) {
        return None;
    }
    let (outcome, exit_code, duration_ms) = output_outcome(value);
    let call_id = value
        .get("callId")
        .or_else(|| value.get("call_id"))
        .or_else(|| value.get("toolCallId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    Some(CodeBuddyOutputDraft {
        native_record_id,
        content: text.as_bytes().to_vec(),
        occurred_at_unix_ms: occurred_at.timestamp_millis(),
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
        kind: OutputObservationKind::Tool,
        call_id: call_id.or_else(|| Some(format!("codebuddy-output-{ordinal}"))),
    })
}

fn output_outcome(value: &Value) -> (OutputOutcome, Option<i32>, Option<u64>) {
    let exit_code = value
        .get("exitCode")
        .or_else(|| value.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = value
        .get("durationMs")
        .or_else(|| value.get("duration_ms"))
        .and_then(Value::as_u64);
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let success = value
        .get("success")
        .or_else(|| value.get("ok"))
        .and_then(Value::as_bool);
    let outcome = if status.contains("timeout") {
        OutputOutcome::Timeout
    } else if success == Some(false)
        || exit_code.is_some_and(|value| value != 0)
        || matches!(
            status.as_str(),
            "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
        )
    {
        OutputOutcome::Failure
    } else if success == Some(true)
        || exit_code == Some(0)
        || matches!(
            status.as_str(),
            "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
        )
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    (outcome, exit_code, duration_ms)
}

fn cli_project_hash(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty() && *name != "projects")
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown-project".to_owned())
}

fn cli_message_text(value: &Value) -> String {
    let text = value
        .get("content")
        .and_then(provider_value_text)
        .or_else(|| {
            value
                .pointer("/message/content")
                .and_then(provider_value_text)
        })
        .unwrap_or_default();
    codebuddy_clean_content(&text)
}

fn cli_message_time(value: &Value, fallback: DateTime<Utc>) -> Option<DateTime<Utc>> {
    value
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(crate::common::time::parse_rfc3339_utc)
        })
        .or_else(|| {
            value
                .get("__timestamp")
                .and_then(Value::as_str)
                .and_then(crate::common::time::parse_rfc3339_utc)
        })
        .or(Some(fallback))
}

#[derive(Debug)]
struct ResolvedCodeBuddySource {
    source_id: Uuid,
    session: Session,
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    expected_store_cursor: Option<&str>,
    page: CodeBuddyPage,
) -> Result<ProviderImportSummary> {
    let next_cursor = page.next_cursor.encode()?;
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: source.cursor_stream.clone(),
        cursor: next_cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(expected_store_cursor.map(str::to_owned), next);
    let publication_id = publication_id(source, &page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let template = page
        .records
        .iter()
        .find_map(|record| record.core.as_ref().map(|core| &core.session))
        .cloned();
    let template = match template {
        Some(template) => Some(template),
        None => cursor_session_draft(source, context, &page.next_cursor)?,
    };
    let resolved = template
        .as_ref()
        .map(|session| {
            resolve_source(
                committed_store,
                &mut group,
                source,
                context,
                options,
                session,
                &mut summary,
            )
        })
        .transpose()?;

    if let Some(resolved) = resolved.as_ref() {
        for record in &page.records {
            let Some(core) = record.core.as_ref() else {
                continue;
            };
            publish_event(
                committed_store,
                &mut group,
                context,
                options,
                &core.event,
                record.physical_line,
                resolved,
                &mut summary,
            )?;
        }
    }

    let prior_rejected = page.expected_cursor.rejected_records;
    let new_rejected = page
        .next_cursor
        .rejected_records
        .saturating_sub(prior_rejected);
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(new_rejected).unwrap_or(usize::MAX));
    let prior_failure_count = page.expected_cursor.failures.len();
    summary.failures.extend(
        page.next_cursor
            .failures
            .iter()
            .skip(prior_failure_count)
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            }),
    );
    if page.expected_cursor.incomplete_tail != page.next_cursor.incomplete_tail {
        if let Some(failure) = page.next_cursor.incomplete_tail.as_ref() {
            summary.failed = summary.failed.saturating_add(1);
            summary.failures.push(ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            });
        }
    }

    if !source.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    session_draft: &CodeBuddySessionDraft,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedCodeBuddySource> {
    let raw_source_path = source.canonical_path.display().to_string();
    let source_root = source.configured_root.display().to_string();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::CodeBuddy,
            source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: source.locator_identity.clone(),
            cursor_stream: source.cursor_stream.clone(),
            proposed_source_identity: source.proposed_source_identity.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let provider_session_id = &session_draft.provider_session_id;
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::CodeBuddy,
        CODEBUDDY_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        provider_session_id.as_str(),
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::CodeBuddy,
                provider_session_id,
                CODEBUDDY_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source_record = capture_source(
        source_id,
        source,
        context,
        session_draft,
        &resolution.canonical_source_identity,
        &source_root,
    );
    group.upsert_capture_source(&source_record)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::CodeBuddy,
        provider_session_id.as_str(),
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session_existed = committed_store.get_session(session_id).is_ok();
    let session = normalized_session(session_id, source_id, context, options, session_draft);
    group.upsert_session(&session)?;
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedCodeBuddySource { source_id, session })
}

fn capture_source(
    source_id: Uuid,
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    session: &CodeBuddySessionDraft,
    canonical_source_identity: &str,
    source_root: &str,
) -> CaptureSource {
    let raw_source_path = source.canonical_path.display().to_string();
    let source_identity_key = provider_scoped_source_identity_key(
        CaptureProvider::CodeBuddy,
        &session.provider_session_id,
        CODEBUDDY_SOURCE_FORMAT,
        Some(&raw_source_path),
    );
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::CodeBuddy,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(CODEBUDDY_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source.source_revision,
                "source_identity_key": source_identity_key,
                "source_metadata": session.source_metadata,
                "session_metadata": session.session_metadata,
                "nativepath_publication": CODEBUDDY_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

fn normalized_session(
    session_id: Uuid,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    session: &CodeBuddySessionDraft,
) -> Session {
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::CodeBuddy,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": CODEBUDDY_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:codebuddy:{}", session.provider_session_id),
                "artifacts": [],
                "metadata": session.session_metadata,
                "nativepath_publication": CODEBUDDY_NATIVE_PUBLICATION_REVISION,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    event: &CodeBuddyEventDraft,
    line_number: usize,
    resolved: &ResolvedCodeBuddySource,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id =
        resolved
            .session
            .external_session_id
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy NativePath session lost its provider identity",
            ))?;
    let event_hash = &event.event_hash;
    let provider_event_index = event.provider_event_index;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::CodeBuddy,
        provider_session_id,
        resolved.source_id,
        provider_event_index,
        provider_event_index,
        event_hash,
        None,
        None,
        resolved.session.id
            == provider_session_uuid(CaptureProvider::CodeBuddy, provider_session_id),
    )?;

    let mut provider_metadata = event.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "verified content locator annotation is malformed".to_owned(),
                )
            })
        })
        .transpose()?;
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event_hash,
        "source_format": CODEBUDDY_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key":
            format!("provider-event:codebuddy:{CODEBUDDY_SOURCE_FORMAT}:{event_hash}"),
        "source_record_ordinal": null,
        "source_record_subrecord_index": null,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (
        sync_metadata.as_object_mut(),
        verified_content_locators.as_ref(),
    ) {
        metadata.insert(
            VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(),
            locators.to_metadata_value(),
        );
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::CodeBuddy.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event_hash,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
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
    Ok(())
}

fn cursor_session_draft(
    source: &CodeBuddySource,
    context: &ProviderAdapterContext,
    cursor: &CodeBuddyNativeCursor,
) -> Result<Option<CodeBuddySessionDraft>> {
    let started_at = cursor.session.started_at()?.unwrap_or(context.imported_at);
    let provider_session_id = cursor.session.provider_session_id();
    let source_path = source.canonical_path.display().to_string();
    let session_title = codebuddy_session_title(source, &cursor.session)?;
    match source.shape {
        CodeBuddySourceShape::Cli => {
            let session_index = json!({
                "source": "codebuddy_cli_jsonl",
                "path": source_path,
                "rows": cursor.session.row_count,
            });
            Ok(Some(codebuddy_session_draft(&CodeBuddySessionInput {
                provider_session_id: &provider_session_id,
                native_session_id: &cursor.session.native_session_id,
                project_hash: &cursor.session.project_hash,
                started_at,
                ended_at: cursor.session.ended_at()?,
                title: session_title.as_deref(),
                cwd: cursor.session.cwd.as_deref(),
                project_index: None,
                conversation: None,
                session_index: &session_index,
                file_names: &["projects/*/*.jsonl"],
                shape: CodeBuddyNativeShape::Cli,
            })))
        }
        CodeBuddySourceShape::Extension => {
            let (metadata, _) = codebuddy_extension_metadata(&source.path, source.session_ordinal)?;
            let Some(metadata) = metadata else {
                return Ok(None);
            };
            let cwd = codebuddy_extension_metadata_text(
                &metadata,
                &["projectPath", "project_path", "cwd", "workspace"],
            );
            Ok(Some(codebuddy_session_draft(&CodeBuddySessionInput {
                provider_session_id: &provider_session_id,
                native_session_id: &cursor.session.native_session_id,
                project_hash: &cursor.session.project_hash,
                started_at,
                ended_at: cursor.session.ended_at()?,
                title: session_title.as_deref(),
                cwd: cwd.as_deref(),
                project_index: metadata.project_index.as_ref(),
                conversation: metadata.conversation.as_ref(),
                session_index: &metadata.session_index,
                file_names: &["index.json", "messages/*.json"],
                shape: CodeBuddyNativeShape::Extension,
            })))
        }
    }
}

fn publication_id(
    source: &CodeBuddySource,
    page: &CodeBuddyPage,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_PUBLICATION_DOMAIN);
    digest.update(source.shape.cursor_tag().as_bytes());
    digest.update(source.locator_identity.as_bytes());
    digest.update(source.source_revision.as_bytes());
    digest.update(page.expected_cursor.next_native_offset.to_be_bytes());
    digest.update(page.expected_cursor.next_native_ordinal.to_be_bytes());
    digest.update(page.next_cursor.next_native_offset.to_be_bytes());
    digest.update(page.next_cursor.next_native_ordinal.to_be_bytes());
    digest.update([u8::from(page.next_cursor.terminal)]);
    for record in &page.records {
        digest.update(record.native_ordinal.to_be_bytes());
        digest.update((record.native_bytes.len() as u64).to_be_bytes());
        digest.update(Sha256::digest(&record.native_bytes));
    }
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("codebuddy-nativepath:{}", hex(&digest.finalize()))
}

fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known: &[KnownRoute],
    live: &[CodeBuddySource],
    work_limit: CaptureWorkLimit,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let live = live
        .iter()
        .map(|source| source.locator_identity.as_str())
        .collect::<BTreeSet<_>>();
    let missing = known
        .iter()
        .filter(|route| !live.contains(route.locator_identity.as_str()))
        .collect::<Vec<_>>();
    let mut summary = ProviderImportSummary::default();
    for (index, route) in missing.iter().enumerate() {
        let route_summary = retire_route(store, bulk_guard, context, route, reason)?;
        let changed = route_summary.work_result() == ProviderImportWorkResult::Changed;
        summary.merge_from(route_summary);
        if work_limit == CaptureWorkLimit::OneSafeGroup && changed {
            summary.work_remaining = index.saturating_add(1) < missing.len();
            break;
        }
    }
    Ok(summary)
}

fn retire_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &route.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "CodeBuddy route retirement lost its cursor",
        ))?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::CodeBuddy,
        source_format: CODEBUDDY_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor_stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: route.cursor_stream.clone(),
        cursor: committed.provider_cursor().to_owned(),
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
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
    let mut summary = ProviderImportSummary::default();
    if changed {
        summary.skipped = 1;
        summary.skipped_sessions = 1;
        summary.set_work_result(ProviderImportWorkResult::Changed);
    } else {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(CODEBUDDY_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("codebuddy-retirement:{}", hex(&digest.finalize()))
}

fn replay_outputs_or_mark_behind(
    sources: &[CodeBuddySource],
    store: &Store,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) {
    let Some(sink) = profile.sink() else {
        return;
    };
    for source in sources {
        if let Err(error) = replay_source_outputs(source, store, context, sink.as_ref()) {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

fn replay_source_outputs(
    source: &CodeBuddySource,
    store: &Store,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let core_cursor = match load_stored_cursor(store, &context.machine_id, &source.cursor_stream)? {
        StoredCursor::Native { cursor, .. }
            if cursor.source_revision == source.source_revision && cursor.terminal =>
        {
            cursor
        }
        _ => {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_core_not_committed",
                "CodeBuddy output replay requires terminal matching NativePath Core",
            ));
            return Ok(());
        }
    };
    let output_source = source.output_identity();
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == CODEBUDDY_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<CodeBuddyNativeCursor>(&cursor.payload).ok())
        .filter(|cursor| {
            cursor.version == CODEBUDDY_NATIVE_CURSOR_VERSION
                && cursor.shape == source.shape
                && cursor.canonical_path == source.canonical_path
                && cursor.source_identity == source.proposed_source_identity
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == CODEBUDDY_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == source.source_revision
            && progress_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.generation == core_cursor.generation)
    });
    if can_resume && progress.as_ref().is_some_and(|progress| progress.terminal) {
        return Ok(());
    }
    let mut scan_cursor = if can_resume {
        progress_cursor
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy resumable output progress lost its cursor",
            ))?
    } else {
        let mut cursor = initial_cursor(source, context)?;
        cursor.generation = core_cursor.generation;
        cursor
    };
    let prior_epoch = progress.as_ref().map(|progress| progress.source_epoch);
    let source_epoch = if can_resume {
        prior_epoch.unwrap_or(1)
    } else {
        prior_epoch
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "CodeBuddy output source epoch overflowed",
            ))?
    };
    let mut disposition = if can_resume {
        ProOutputSourceDisposition::AppendOrResume
    } else if progress.is_some() {
        ProOutputSourceDisposition::Rewrite
    } else {
        ProOutputSourceDisposition::NewSource
    };
    let mut expected_prior_epoch = prior_epoch;
    let mut expected_prior_frontier = progress_cursor.as_ref().map(output_frontier).transpose()?;

    while !scan_cursor.terminal {
        let page = next_source_page(source, &scan_cursor, context)?.ok_or(
            CaptureError::SystemInvariant("CodeBuddy output scanner stopped before terminal"),
        )?;
        if page.next_cursor.next_native_offset > core_cursor.next_native_offset
            || page.next_cursor.next_native_ordinal > core_cursor.next_native_ordinal
        {
            return Err(CaptureError::InvalidPayload(
                "CodeBuddy output replay exceeded committed Core authority".to_owned(),
            ));
        }
        if !source.revalidate()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_frontier = output_frontier(&page.expected_cursor)?;
        let next_frontier = output_frontier(&page.next_cursor)?;
        let observations = page
            .records
            .iter()
            .filter_map(|record| {
                record.output.as_ref().map(|output| {
                    output_observation(source, &page.next_cursor.session, record, output)
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: source.source_revision.clone(),
            parser_revision: CODEBUDDY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_prior_epoch,
            expected_prior_frontier: expected_prior_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(
                CaptureProvider::CodeBuddy.as_str(),
                &source.locator_identity,
            ),
            expected_frontier,
            next_frontier.clone(),
            page.next_cursor.terminal,
            NativePageAccounting {
                logical_units: page.logical_units(),
                conservative_serialized_bytes: page.retained_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            sink.mark_behind(ProOutputSinkError::new(
                "codebuddy_nativepath_output_page",
                "CodeBuddy output sink did not commit the requested replay page",
            ));
            return Ok(());
        }
        scan_cursor = page.next_cursor;
        expected_prior_epoch = Some(source_epoch);
        expected_prior_frontier = Some(next_frontier);
        disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

fn output_frontier(cursor: &CodeBuddyNativeCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CODEBUDDY_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(cursor)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    source: &CodeBuddySource,
    session: &CodeBuddySessionCheckpoint,
    record: &CodeBuddyRecord,
    output: &CodeBuddyOutputDraft,
) -> Result<ProOutputObservation> {
    let provider_session_id = session.provider_session_id();
    Ok(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "codebuddy:{}:{}",
                source.shape.cursor_tag(),
                record.native_ordinal
            ),
            native_sequence: record.native_ordinal,
            native_record_id: Some(output.native_record_id.clone()),
            source_record_ordinal: Some(record.native_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: record.byte_start,
            byte_end_exclusive: record.byte_end_exclusive,
        },
        occurred_at_unix_ms: Some(output.occurred_at_unix_ms),
        associations: OutputAssociations {
            direct_session_id: provider_session_id.clone(),
            root_session_id: provider_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id.clone(),
        command: None,
        outcome: output.outcome.clone(),
        locator: OutputSourceLocator {
            version: 1,
            kind: format!("codebuddy-{}-native-v1", source.shape.cursor_tag()),
            payload: serde_json::to_vec(&json!({
                "path": source.canonical_path,
                "source_revision": source.source_revision,
                "native_ordinal": record.native_ordinal,
                "byte_start": record.byte_start,
                "byte_end_exclusive": record.byte_end_exclusive,
            }))?,
        },
        content: output.content.clone(),
    })
}

pub(crate) fn codebuddy_cli_complete_content_record(
    value: &Value,
    physical_line: usize,
) -> Option<(String, String)> {
    let text = cli_message_text(value);
    if codebuddy_event_type(
        value.get("role").and_then(Value::as_str),
        value.get("type").and_then(Value::as_str),
        value,
    ) != EventType::Message
        || text.trim().is_empty()
    {
        return None;
    }
    let native_record_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{physical_line}"));
    Some((text, native_record_id))
}

pub(crate) fn codebuddy_cli_complete_content_source_from_admitted(
    metadata: &Metadata,
    path_identity: String,
) -> Result<(String, String)> {
    let frozen = CodeBuddyFrozenFile::from_metadata(metadata)?;
    Ok((
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        path_identity,
    ))
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
#[path = "native_path_tests.rs"]
mod tests;
