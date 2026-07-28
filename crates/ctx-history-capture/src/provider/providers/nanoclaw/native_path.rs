use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
    CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
    VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::provider::importer::{
    compact_provider_result_payload, provider_event_import_identity_with_exact_legacy_source,
    provider_import_session_uuid, provider_path_identity, provider_scoped_source_identity_key,
    provider_scoped_source_uuid, provider_session_uuid, provider_source_cursor_stream_for_path,
    provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
};
use crate::provider::native_ingestion::{
    process_pro_replay_only, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
    NativeSafeFrontier, NativeSourceIdentity,
};
use crate::provider::normalization::{provider_nonnegative_i64_to_u64, provider_timestamp_millis};
use crate::provider::sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, NANOCLAW_SOURCE_FORMAT,
};

use super::position::NanoClawFrontier;
use super::project::{nanoclaw_project_root, NanoClawProjectSnapshot};
use super::projection::{nanoclaw_core_event, NanoClawCoreEvent};
use super::rows::{nanoclaw_message_digest_values, NanoClawSessionRow};
use super::source::{NanoClawNativePage, NanoClawNativeScanner, NanoClawNativeUnit};
use super::{NANOCLAW_CAPTURE_REVISION, NANOCLAW_POLICY_REVISION};

mod lifecycle;
mod publication;
mod scanner;
pub(crate) mod source_backed;

use lifecycle::*;
use scanner::*;

const NANOCLAW_NATIVE_CURSOR_VERSION: u32 = 1;
const NANOCLAW_NATIVE_CURSOR_PREFIX: &str = "nanoclaw-nativepath-v1:";
const NANOCLAW_NATIVE_PUBLICATION_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-publication-v1\0";
const NANOCLAW_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-retirement-v1\0";
const NANOCLAW_NATIVE_GENERATION_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-generation-v1\0";
const NANOCLAW_NATIVE_SOURCE_STAGE_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-source-stage-v1\0";
const NANOCLAW_NATIVE_OMISSION_DOMAIN: &[u8] = b"ctx-nanoclaw-nativepath-omission-v1\0";
const NANOCLAW_NATIVE_PUBLICATION_REVISION: &str = "nanoclaw-nativepath-v1";
const NANOCLAW_OUTPUT_FRONTIER_VERSION: u32 = 1;
const NANOCLAW_OUTPUT_PARSER_REVISION: &str = "nanoclaw-nativepath-output-v1";
const NANOCLAW_PROJECT_EXTERNAL_SESSION: &str = "__nanoclaw_project__";
const NANOCLAW_OUTPUT_PAGE_BYTES: usize = 4 * 1024;
const NANOCLAW_LEGACY_POSITION_KIND: &str = "nanoclaw-project-keyset-v1";
const NANOCLAW_SOURCE_STAGE_PAGE_IDS: usize = 64;
const NANOCLAW_RETIREMENT_PAGE_ENTITIES: usize = 64;
const NANOCLAW_LIFECYCLE_PAGE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NanoClawNativeCursor {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    generation: u64,
    anchor_source_id: Uuid,
    source_revision: String,
    frontier: NanoClawFrontier,
    prefix_digest: String,
    terminal: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    stage_generation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_stage: Option<NanoClawSourceStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retirement: Option<NanoClawRetirementRequest>,
    retained_sessions: u64,
    retained_events: u64,
    rejected_records: u64,
}

impl NanoClawNativeCursor {
    fn initial(
        anchor_source_id: Uuid,
        source_revision: String,
        generation: u64,
        stage_generation: bool,
    ) -> Self {
        Self {
            version: NANOCLAW_NATIVE_CURSOR_VERSION,
            capture_revision: NANOCLAW_CAPTURE_REVISION,
            policy_revision: NANOCLAW_POLICY_REVISION,
            generation,
            anchor_source_id,
            source_revision,
            frontier: NanoClawFrontier::initial(),
            prefix_digest: initial_prefix_digest(),
            terminal: false,
            stage_generation,
            source_stage: None,
            retirement: None,
            retained_sessions: 0,
            retained_events: 0,
            rejected_records: 0,
        }
    }

    fn validate(&self, expected_anchor_source_id: Uuid) -> Result<()> {
        if self.version != NANOCLAW_NATIVE_CURSOR_VERSION
            || self.capture_revision != NANOCLAW_CAPTURE_REVISION
            || self.policy_revision != NANOCLAW_POLICY_REVISION
            || self.anchor_source_id != expected_anchor_source_id
            || self.source_revision.is_empty()
            || self.prefix_digest.len() != 64
            || !self
                .prefix_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || (self.terminal
                && (self.stage_generation
                    || self.source_stage.is_some()
                    || self.retirement.is_some()))
            || (!self.stage_generation
                && (self.source_stage.is_some() || self.retirement.is_some()))
            || (self.source_stage.is_some() && self.retirement.is_some())
        {
            return Err(CaptureError::InvalidPayload(
                "NanoClaw NativePath cursor is inconsistent".to_owned(),
            ));
        }
        self.frontier.validate()?;
        if let Some(retirement) = &self.retirement {
            retirement.validate()?;
        }
        Ok(())
    }

    fn encode(&self) -> Result<String> {
        let wire = serde_json::to_string(self)?;
        Ok(format!("{NANOCLAW_NATIVE_CURSOR_PREFIX}{wire}"))
    }

    fn decode(encoded: &str, expected_anchor_source_id: Uuid) -> Result<Self> {
        let wire = encoded
            .strip_prefix(NANOCLAW_NATIVE_CURSOR_PREFIX)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "NanoClaw Store cursor has an unknown NativePath provider payload".to_owned(),
                )
            })?;
        let cursor: Self = serde_json::from_str(wire)?;
        cursor.validate(expected_anchor_source_id)?;
        if cursor.encode()? != encoded {
            return Err(CaptureError::InvalidPayload(
                "NanoClaw NativePath cursor is not canonical".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NanoClawSourceStage {
    after: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NanoClawRetirementRequest {
    after: Option<NanoClawRetirementFrontier>,
}

impl NanoClawRetirementRequest {
    fn validate(&self) -> Result<()> {
        self.after
            .as_ref()
            .map(NanoClawRetirementFrontier::to_store)
            .transpose()
            .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NanoClawRetirementFrontier {
    kind: String,
    id: Uuid,
}

impl NanoClawRetirementFrontier {
    fn from_store(frontier: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: frontier.kind.as_str().to_owned(),
            id: frontier.id,
        }
    }

    fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "NanoClaw retirement cursor has an unsupported entity kind".to_owned(),
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

// This single import-lifecycle value is decoded and consumed once; retaining the
// cursor inline avoids heap allocation without growing any per-record container.
#[allow(clippy::large_enum_variant)]
enum PriorCursor {
    None,
    Legacy(SyncCursor),
    Native {
        stored: SyncCursor,
        cursor: NanoClawNativeCursor,
        retired: bool,
    },
}

struct NanoClawLiveProject {
    root: PathBuf,
    central_path: PathBuf,
    machine_id: String,
    cursor_stream: String,
    locator_identity: String,
    proposed_source_identity: String,
    raw_source_path: String,
    source_root: String,
    source_revision: String,
    user_version: i64,
    schema_fingerprint: String,
    anchor_source_id: Uuid,
}

struct CoreOutcome {
    summary: ProviderImportSummary,
    terminal_cursor: Option<NanoClawNativeCursor>,
}

pub(super) fn import_nanoclaw_project(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let requested_root = requested_project_root(path)?;
    let central_path = requested_root.join("data").join("v2.db");
    if !requested_root.is_dir() || !central_path.is_file() {
        if options.import_profile.is_replay_only() {
            mark_output_behind(
                options.import_profile.sink().map(AsRef::as_ref),
                "NanoClaw source is unavailable for output replay",
            );
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing_project(
            store,
            &requested_root,
            &context,
            if !requested_root.exists() {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let root = fs::canonicalize(nanoclaw_project_root(path)?)?;
    let central_path = root.join("data").join("v2.db");
    let snapshot = NanoClawProjectSnapshot::read(&root, &central_path)?;
    let central = open_provider_sqlite_readonly(&central_path)?;
    let user_version = central.query_row("pragma user_version", [], |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&central)?;
    let source_revision = snapshot.source_revision(user_version, &schema_fingerprint);
    let live = live_project(
        &root,
        &central_path,
        &source_revision,
        user_version,
        &schema_fingerprint,
        &context,
    )?;

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            &live,
            &snapshot,
            options.import_profile.sink().map(AsRef::as_ref),
        );
        return Ok(ProviderImportSummary::default());
    }

    ensure_active_journal(store)?;
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = import_core(
        store,
        &committed_store,
        &bulk_guard,
        &central,
        &snapshot,
        &live,
        &context,
        &options,
    );
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let outcome = match (operation, finish) {
        (Ok(outcome), Ok(())) => outcome,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if outcome.terminal_cursor.is_some() {
        replay_outputs_or_mark_behind(
            store,
            &live,
            &snapshot,
            options.import_profile.sink().map(AsRef::as_ref),
        );
    }
    Ok(outcome.summary)
}

// Core publication deliberately keeps mutable and committed stores, source
// authority, and import policy separate so their transaction roles stay clear.
