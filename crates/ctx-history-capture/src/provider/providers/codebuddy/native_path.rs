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
    compute_payload_hash,
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
    CodeBuddyExtensionMessageError, CodeBuddyExtensionMetadata, CodeBuddyExtensionObservation,
};

const CODEBUDDY_NATIVE_PAGE_MAX_UNITS: usize = 64;
const CODEBUDDY_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const CODEBUDDY_NATIVE_RECORD_MAX_BYTES: usize = CODEBUDDY_NATIVE_PAGE_MAX_BYTES - (64 * 1024);
const CODEBUDDY_MAX_NATIVE_ID_BYTES: usize = 1_024;
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
    #[serde(default)]
    skipped_metadata: u64,
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
        let skipped_metadata = usize::try_from(self.skipped_metadata).map_err(|_| {
            CaptureError::SystemInvariant(
                "CodeBuddy skipped metadata count exceeds platform limits",
            )
        })?;
        let failed = rejected.saturating_add(usize::from(self.incomplete_tail.is_some()));
        let skipped_sessions = usize::from(accepted != 0);
        let mut summary = ProviderImportSummary {
            skipped: accepted
                .saturating_add(skipped_metadata)
                .saturating_add(skipped_sessions),
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
// The native cursor is consumed immediately after classification; preserving
// the explicit wire variants is clearer than allocating the common path.
#[allow(clippy::large_enum_variant)]
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
    classification: CodeBuddyRecordClassification,
    output: Option<CodeBuddyOutputDraft>,
}

#[derive(Debug)]
// This parser-owned sum type is retained inside an already bounded heap page;
// boxing its one data variant would add an allocation for every accepted row.
#[allow(clippy::large_enum_variant)]
enum CodeBuddyRecordClassification {
    AcceptedMessage(CodeBuddyCoreRow),
    SkippedMetadata,
    RejectedRecord,
}

impl CodeBuddyRecordClassification {
    fn core(&self) -> Option<&CodeBuddyCoreRow> {
        match self {
            Self::AcceptedMessage(core) => Some(core),
            Self::SkippedMetadata | Self::RejectedRecord => None,
        }
    }
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

mod discovery;
mod ingestion;
mod lifecycle;
mod outputs;
mod parsing;
mod projection;
mod source_backed;

use discovery::*;
pub(crate) use ingestion::import_codebuddy_nativepath;
#[cfg(test)]
use ingestion::{capture_source, normalized_session};
use lifecycle::*;
use outputs::*;
pub(crate) use outputs::{
    codebuddy_cli_complete_content_record, codebuddy_cli_complete_content_source_from_admitted,
};
use parsing::*;
use projection::*;
pub(crate) use source_backed::{
    hydrate_codebuddy_source_backed_record, scan_codebuddy_source_backed_root,
    CodeBuddyHydratedSourceRecord, CodeBuddySourceBackedPage, CodeBuddySourceBackedRejection,
    CodeBuddySourceBackedScan,
};

#[cfg(test)]
#[path = "native_path_tests.rs"]
mod tests;
