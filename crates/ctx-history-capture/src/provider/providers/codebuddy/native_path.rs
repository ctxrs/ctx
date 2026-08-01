//! Production source-backed CodeBuddy discovery, parsing, and direct Core projection.
//!
//! CodeBuddy owns two unrelated persisted products: extension session
//! directories made of whole-JSON message files, and CLI project JSONL
//! transcripts. They share bounded normalization while retaining independent
//! source authority, native coordinates, and complete normalized content.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, CaptureProvider, EventRole, EventType, SourceKey};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourceFile, OpenedProviderSourcePath,
        ProviderSourceDirectory, ProviderSourceRoot,
    },
    provider::{
        normalization::{provider_role, provider_value_text},
        provider_safe_path_segment,
    },
    CaptureError, ProviderAdapterContext, Result, CODEBUDDY_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    normalization::{
        codebuddy_clean_content, codebuddy_decoded_message, codebuddy_message_text,
        codebuddy_normalized_rows, codebuddy_title_from_text, CodeBuddyEventDraft,
        CodeBuddyEventInput, CodeBuddySessionDraft, CodeBuddySessionInput,
    },
    source::{CodeBuddyFrozenFile, CodeBuddyRevisionHasher},
    CODEBUDDY_CLI_POLICY_REVISION, CODEBUDDY_MAX_FAILURE_BYTES, CODEBUDDY_MAX_SCAN_REJECTIONS,
};

#[path = "extension/source.rs"]
mod extension_source;

use extension_source::{
    codebuddy_extension_line_number, codebuddy_extension_metadata_from_admitted,
    codebuddy_extension_metadata_text, codebuddy_message_time, CodeBuddyExtensionMetadata,
};

const CODEBUDDY_NATIVE_PAGE_MAX_UNITS: usize = 64;
const CODEBUDDY_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const CODEBUDDY_NATIVE_RECORD_MAX_BYTES: usize = CODEBUDDY_NATIVE_PAGE_MAX_BYTES - (64 * 1024);
const CODEBUDDY_MAX_NATIVE_ID_BYTES: usize = 1_024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeBuddySourceShape {
    Extension,
    Cli,
}

impl CodeBuddySourceShape {
    fn shape_tag(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CodeBuddySessionState {
    native_session_id: String,
    project_hash: String,
    cwd: Option<String>,
    generated_title: Option<String>,
    row_count: u64,
}

impl CodeBuddySessionState {
    fn provider_session_id(&self) -> String {
        format!("{}/{}", self.project_hash, self.native_session_id)
    }

    fn estimated_bytes(&self) -> usize {
        256_usize
            .saturating_add(self.native_session_id.len())
            .saturating_add(self.project_hash.len())
            .saturating_add(self.cwd.as_ref().map_or(0, String::len))
            .saturating_add(self.generated_title.as_ref().map_or(0, String::len))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeBuddyScanState {
    shape: CodeBuddySourceShape,
    source_revision: String,
    next_native_offset: u64,
    next_native_ordinal: u64,
    certified_prefix_sha256: String,
    file_identity: Option<String>,
    terminal: bool,
    accepted_events: u64,
    skipped_metadata: u64,
    rejected_records: u64,
    failures: Vec<CodeBuddyScanRejection>,
    incomplete_tail: Option<CodeBuddyScanRejection>,
    session: CodeBuddySessionState,
}

impl CodeBuddyScanState {
    fn estimated_bytes(&self) -> usize {
        self.failures.iter().fold(
            512_usize
                .saturating_add(self.source_revision.len())
                .saturating_add(self.certified_prefix_sha256.len())
                .saturating_add(self.file_identity.as_ref().map_or(0, String::len))
                .saturating_add(self.session.estimated_bytes())
                .saturating_add(
                    self.incomplete_tail
                        .as_ref()
                        .map_or(0, CodeBuddyScanRejection::estimated_bytes),
                ),
            |bytes, rejection| bytes.saturating_add(rejection.estimated_bytes()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeBuddyScanRejection {
    line: usize,
    error: String,
}

impl CodeBuddyScanRejection {
    fn estimated_bytes(&self) -> usize {
        32_usize.saturating_add(self.error.len())
    }
}

#[derive(Debug, Clone)]
struct CodeBuddySource {
    shape: CodeBuddySourceShape,
    path: PathBuf,
    canonical_path: PathBuf,
    source_revision: String,
    session_ordinal: usize,
    frozen: Option<CodeBuddyFrozenFile>,
    capability: Option<Arc<CodeBuddyCapabilitySource>>,
}

#[derive(Debug)]
struct CodeBuddyCapabilitySource {
    authority: ProviderSourceRoot,
    primary: Option<OpenedProviderSourceFile>,
    extension: Option<CodeBuddyExtensionCapability>,
}

#[derive(Debug)]
struct CodeBuddyExtensionCapability {
    metadata: CodeBuddyExtensionMetadata,
    messages: BTreeMap<String, CodeBuddyObservedFile>,
}

#[derive(Debug, Clone)]
struct CodeBuddyObservedFile {
    relative_path: PathBuf,
    display_path: PathBuf,
    frozen: CodeBuddyFrozenFile,
    authority_fingerprint: [u8; 32],
}

#[derive(Debug)]
struct CodeBuddyRecord {
    native_ordinal: u64,
    native_bytes: Vec<u8>,
    classification: CodeBuddyRecordClassification,
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

#[derive(Debug)]
struct CodeBuddyCoreRow {
    session: CodeBuddySessionDraft,
    event: CodeBuddyEventDraft,
}

#[derive(Debug)]
struct CodeBuddyPage {
    records: Vec<CodeBuddyRecord>,
    next_state: CodeBuddyScanState,
}

mod discovery;
mod parsing;
mod projection;
mod source_backed;

use discovery::*;
use parsing::*;
use projection::*;
pub(crate) use source_backed::registration::register as register_source_backed_route;
