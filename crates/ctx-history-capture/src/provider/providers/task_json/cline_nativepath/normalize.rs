use std::{path::PathBuf, sync::Arc};

use ctx_history_core::{Confidence, FileChangeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{OutputOutcome, OutputOutcomeMetadata};

use super::source::{ClineComponent, ClineComponentObservation};

const EVENT_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-event-v2\0";
const EVENT_FILE_TOUCH_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-event-file-touches-v1\0";
const ITEM_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-item-v2\0";
const ARRAY_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-array-v2\0";
const SESSION_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-session-v2\0";

pub(crate) const CLINE_NATIVE_PAGE_MAX_UNITS: usize = 64;
pub(crate) const CLINE_NATIVE_FIXED_PAGE_UNITS: usize = 4;
pub(crate) const CLINE_NATIVE_SESSION_PAGE_UNITS: usize = 1;
pub(super) const CLINE_NATIVE_CORE_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClineTaskIdentity(pub(crate) Arc<str>);

impl ClineTaskIdentity {
    pub(crate) fn new(value: impl Into<Arc<str>>) -> Self {
        Self(value.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineTaskIdentityOrigin {
    TaskMetadata,
    DirectoryNameDegraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub(crate) enum ClineEventComponent {
    ApiHistory = 0,
    UiMessages = 1,
    FallbackHistory = 2,
}

impl ClineEventComponent {
    pub(crate) fn source_component(self) -> ClineComponent {
        match self {
            Self::ApiHistory => ClineComponent::ApiHistory,
            Self::UiMessages => ClineComponent::UiMessages,
            Self::FallbackHistory => ClineComponent::FallbackHistory,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ClineNativeItemKey {
    NativeId {
        native_id: Box<str>,
        occurrence: u64,
    },
    ComponentOrdinal(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClineEventIdentity {
    pub(crate) task: ClineTaskIdentity,
    pub(crate) component: ClineEventComponent,
    pub(crate) item: ClineNativeItemKey,
    pub(crate) sub_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClineNativeOrder {
    pub(crate) component: ClineEventComponent,
    pub(crate) item_index: u64,
    pub(crate) sub_index: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineEventKind {
    Message,
    Summary,
    Notice,
    ToolCall,
    ToolOutput,
    CommandOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineEventRole {
    User,
    Assistant,
    System,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineToolCall {
    pub(crate) call_id: Option<Box<str>>,
    pub(crate) name: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineSparseOutputDiagnostic {
    pub(crate) outcome: OutputOutcome,
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) output_bytes: usize,
    pub(crate) call_id: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineFileTouch {
    pub(crate) path: Box<str>,
    pub(crate) old_path: Option<Box<str>>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClineSourceRecordEvidence {
    pub(crate) native_index: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_length: u64,
    pub(crate) record_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineEventRow {
    pub(crate) identity: ClineEventIdentity,
    pub(crate) native_order: ClineNativeOrder,
    pub(crate) kind: ClineEventKind,
    pub(crate) role: ClineEventRole,
    pub(crate) occurred_at_millis: Option<i64>,
    pub(crate) body: Option<Box<str>>,
    pub(crate) content_hash: [u8; 32],
    pub(crate) tool_call: Option<ClineToolCall>,
    pub(crate) sparse_output: Option<ClineSparseOutputDiagnostic>,
    pub(crate) file_touches: Box<[ClineFileTouch]>,
    pub(crate) source_record: Option<ClineSourceRecordEvidence>,
}

impl ClineEventRow {
    pub(super) fn message(
        context: ClineEventContext<'_>,
        sub_index: u32,
        kind: ClineEventKind,
        body: String,
    ) -> Self {
        let content_hash = event_hash(context, sub_index, kind, context.role, body.as_bytes());
        Self {
            identity: event_identity(context, sub_index),
            native_order: native_order(context, sub_index),
            kind,
            role: context.role,
            occurred_at_millis: context.occurred_at_millis,
            body: Some(body.into_boxed_str()),
            content_hash,
            tool_call: None,
            sparse_output: None,
            file_touches: Box::default(),
            source_record: None,
        }
    }

    pub(super) fn tool_call(
        context: ClineEventContext<'_>,
        sub_index: u32,
        call_id: Option<String>,
        name: Option<String>,
    ) -> Self {
        let mut safe = Vec::new();
        if let Some(call_id) = call_id.as_deref() {
            safe.extend_from_slice(call_id.as_bytes());
        }
        safe.push(0);
        if let Some(name) = name.as_deref() {
            safe.extend_from_slice(name.as_bytes());
        }
        Self {
            identity: event_identity(context, sub_index),
            native_order: native_order(context, sub_index),
            kind: ClineEventKind::ToolCall,
            role: context.role,
            occurred_at_millis: context.occurred_at_millis,
            content_hash: event_hash(
                context,
                sub_index,
                ClineEventKind::ToolCall,
                context.role,
                &safe,
            ),
            body: None,
            tool_call: Some(ClineToolCall {
                call_id: call_id.map(String::into_boxed_str),
                name: name.map(String::into_boxed_str),
            }),
            sparse_output: None,
            file_touches: Box::default(),
            source_record: None,
        }
    }

    pub(super) fn sparse_output(
        context: ClineEventContext<'_>,
        sub_index: u32,
        kind: ClineEventKind,
        diagnostic: ClineSparseOutputDiagnostic,
    ) -> Self {
        let mut safe = Vec::new();
        safe.push(diagnostic.outcome as u8);
        safe.extend_from_slice(&diagnostic.exit_code.unwrap_or_default().to_le_bytes());
        safe.extend_from_slice(&diagnostic.duration_ms.unwrap_or_default().to_le_bytes());
        safe.extend_from_slice(
            &u64::try_from(diagnostic.output_bytes)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        if let Some(call_id) = diagnostic.call_id.as_deref() {
            safe.extend_from_slice(call_id.as_bytes());
        }
        Self {
            identity: event_identity(context, sub_index),
            native_order: native_order(context, sub_index),
            kind,
            role: ClineEventRole::Unknown,
            occurred_at_millis: context.occurred_at_millis,
            body: None,
            content_hash: event_hash(context, sub_index, kind, ClineEventRole::Unknown, &safe),
            tool_call: None,
            sparse_output: Some(diagnostic),
            file_touches: Box::default(),
            source_record: None,
        }
    }

    pub(super) fn attach_file_touches(&mut self, file_touches: Vec<ClineFileTouch>) {
        if file_touches.is_empty() {
            return;
        }
        let mut hasher = Sha256::new();
        hasher.update(EVENT_FILE_TOUCH_HASH_DOMAIN);
        hasher.update(self.content_hash);
        for touch in &file_touches {
            hash_field(&mut hasher, touch.path.as_bytes());
            hash_field(
                &mut hasher,
                touch.old_path.as_deref().unwrap_or_default().as_bytes(),
            );
            hash_field(
                &mut hasher,
                touch
                    .change_kind
                    .map(FileChangeKind::as_str)
                    .unwrap_or_default()
                    .as_bytes(),
            );
            hash_field(&mut hasher, touch.confidence.as_str().as_bytes());
            hash_field(
                &mut hasher,
                serde_json::to_string(&touch.metadata)
                    .expect("file-touch metadata should serialize")
                    .as_bytes(),
            );
        }
        self.content_hash = hasher.finalize().into();
        self.file_touches = file_touches.into_boxed_slice();
    }
}

#[derive(Clone, Copy)]
pub(super) struct ClineEventContext<'a> {
    pub(super) task: &'a ClineTaskIdentity,
    pub(super) component: ClineEventComponent,
    pub(super) item: &'a ClineNativeItemKey,
    pub(super) item_index: u64,
    pub(super) role: ClineEventRole,
    pub(super) occurred_at_millis: Option<i64>,
}

fn event_identity(context: ClineEventContext<'_>, sub_index: u32) -> ClineEventIdentity {
    ClineEventIdentity {
        task: context.task.clone(),
        component: context.component,
        item: context.item.clone(),
        sub_index,
    }
}

fn native_order(context: ClineEventContext<'_>, sub_index: u32) -> ClineNativeOrder {
    ClineNativeOrder {
        component: context.component,
        item_index: context.item_index,
        sub_index,
    }
}

fn event_hash(
    _context: ClineEventContext<'_>,
    _sub_index: u32,
    kind: ClineEventKind,
    role: ClineEventRole,
    safe: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(EVENT_HASH_DOMAIN);
    hasher.update([kind as u8, role as u8]);
    hasher.update(safe);
    hasher.finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineItemRejectionKind {
    OversizedRetainedItem,
    MalformedRecord,
    ConflictingDiscriminator,
    UnsupportedShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineItemRejection {
    pub(crate) component: ClineEventComponent,
    pub(crate) native_index: u64,
    pub(crate) native_id: Option<Box<str>>,
    pub(crate) kind: ClineItemRejectionKind,
    pub(crate) observed_bytes: u64,
    pub(crate) detail: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineItemCheckpoint {
    pub(crate) native_key: ClineNativeItemKey,
    pub(crate) semantic_hash: [u8; 32],
    pub(crate) retained_rows: u32,
    pub(crate) output_outcomes: u32,
}

impl ClineItemCheckpoint {
    pub(super) fn new(
        native_key: ClineNativeItemKey,
        rows: &[ClineEventRow],
        output_outcomes: &[OutputOutcomeMetadata],
        rejection: Option<&ClineItemRejection>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(ITEM_HASH_DOMAIN);
        hash_native_key(&mut hasher, &native_key);
        for row in rows {
            hasher.update(row.content_hash);
        }
        for outcome in output_outcomes {
            hasher.update(b"output\0");
            hasher.update([outcome.outcome as u8]);
            hasher.update(outcome.exit_code.unwrap_or_default().to_le_bytes());
            hasher.update(outcome.duration_ms.unwrap_or_default().to_le_bytes());
        }
        if let Some(rejection) = rejection {
            hasher.update(b"rejection\0");
            hasher.update([rejection.kind as u8]);
        }
        Self {
            native_key,
            semantic_hash: hasher.finalize().into(),
            retained_rows: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            output_outcomes: u32::try_from(output_outcomes.len()).unwrap_or(u32::MAX),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClinePageFrontier {
    pub(crate) version: u32,
    pub(crate) next_native_index: u64,
    pub(crate) prefix_semantic_sha256: [u8; 32],
}

impl ClinePageFrontier {
    pub(super) fn zero(component: ClineEventComponent) -> Self {
        Self::zero_component(component.source_component())
    }

    pub(super) fn zero_component(component: ClineComponent) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(ARRAY_HASH_DOMAIN);
        hasher.update([component as u8]);
        Self {
            version: 1,
            next_native_index: 0,
            prefix_semantic_sha256: hasher.finalize().into(),
        }
    }

    pub(super) fn advance(&self, item: &ClineItemCheckpoint) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"ctx-cline-nativepath-frontier-v1\0");
        hasher.update(self.prefix_semantic_sha256);
        hash_native_key(&mut hasher, &item.native_key);
        hasher.update(item.semantic_hash);
        hasher.update(item.retained_rows.to_le_bytes());
        hasher.update(item.output_outcomes.to_le_bytes());
        Self {
            version: self.version,
            next_native_index: self.next_native_index.saturating_add(1),
            prefix_semantic_sha256: hasher.finalize().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineArrayCheckpoint {
    pub(crate) component: ClineEventComponent,
    pub(crate) observation: ClineComponentObservation,
    pub(crate) certified_revision_sha256: [u8; 32],
    pub(crate) complete_bytes: u64,
    pub(crate) observed_items: u64,
    pub(crate) retained_rows: u64,
    pub(crate) final_frontier: ClinePageFrontier,
}

impl ClineArrayCheckpoint {
    pub(super) fn new(
        component: ClineEventComponent,
        observation: ClineComponentObservation,
        certified_revision_sha256: [u8; 32],
        complete_bytes: u64,
        observed_items: u64,
        retained_rows: u64,
        final_frontier: ClinePageFrontier,
    ) -> Self {
        Self {
            component,
            observation,
            certified_revision_sha256,
            complete_bytes,
            observed_items,
            retained_rows,
            final_frontier,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineSessionRow {
    pub(crate) identity: ClineTaskIdentity,
    pub(crate) identity_origin: ClineTaskIdentityOrigin,
    pub(crate) identity_aliases: Box<[ClineTaskIdentity]>,
    pub(crate) title: Option<Box<str>>,
    pub(crate) workspace_directory: Option<Box<str>>,
    pub(crate) created_at: Option<Box<str>>,
    pub(crate) last_modified: Option<Box<str>>,
    pub(crate) model_id: Option<Box<str>>,
    pub(crate) model_provider: Option<Box<str>>,
    pub(crate) tokens_input: Option<u64>,
    pub(crate) tokens_output: Option<u64>,
    pub(crate) metadata_hash: [u8; 32],
}

impl ClineSessionRow {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        identity: ClineTaskIdentity,
        identity_origin: ClineTaskIdentityOrigin,
        title: Option<Box<str>>,
        workspace_directory: Option<Box<str>>,
        created_at: Option<Box<str>>,
        last_modified: Option<Box<str>>,
        model_id: Option<Box<str>>,
        model_provider: Option<Box<str>>,
        tokens_input: Option<u64>,
        tokens_output: Option<u64>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(SESSION_HASH_DOMAIN);
        hasher.update(identity.as_str().as_bytes());
        for value in [
            title.as_deref(),
            workspace_directory.as_deref(),
            created_at.as_deref(),
            last_modified.as_deref(),
            model_id.as_deref(),
            model_provider.as_deref(),
        ] {
            if let Some(value) = value {
                hasher.update(value.as_bytes());
            }
            hasher.update(b"\0");
        }
        hasher.update(tokens_input.unwrap_or_default().to_le_bytes());
        hasher.update(tokens_output.unwrap_or_default().to_le_bytes());
        Self {
            identity,
            identity_origin,
            identity_aliases: Box::new([]),
            title,
            workspace_directory,
            created_at,
            last_modified,
            model_id,
            model_provider,
            tokens_input,
            tokens_output,
            metadata_hash: hasher.finalize().into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineMetadataCheckpoint {
    pub(crate) observation: ClineComponentObservation,
    pub(crate) content_sha256: Option<[u8; 32]>,
    pub(crate) session: ClineSessionRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineTaskCheckpoint {
    pub(crate) identity: ClineTaskIdentity,
    pub(crate) canonical_task_path: PathBuf,
    pub(crate) api_history: Option<ClineArrayCheckpoint>,
    pub(crate) ui_messages: Option<ClineArrayCheckpoint>,
    pub(crate) fallback_history: Option<ClineArrayCheckpoint>,
    pub(crate) task_metadata: ClineMetadataCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineComponentTransition {
    Cold,
    LogicalEmpty,
    MissingPhysical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineComponentFailureKind {
    LocalIo,
    IncompleteJson,
    MalformedJson,
    SourceChanged,
    AuthorityBound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineComponentFailure {
    pub(crate) component: ClineComponent,
    pub(crate) path: PathBuf,
    pub(crate) kind: ClineComponentFailureKind,
    pub(crate) message: Box<str>,
    pub(crate) retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineComponentReadOutcome {
    pub(crate) component: ClineComponent,
    pub(crate) path: PathBuf,
    pub(crate) transition: Option<ClineComponentTransition>,
    pub(crate) pages: usize,
    pub(crate) failure: Option<ClineComponentFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineFileSourceIdentity {
    pub(crate) component: ClineComponent,
    pub(crate) canonical_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ClineCorePayload {
    pub(crate) session: Option<ClineSessionRow>,
    pub(crate) events: Box<[ClineEventRow]>,
    pub(crate) rejections: Box<[ClineItemRejection]>,
}

#[derive(Debug)]
pub(crate) struct ClineCertifiedPage {
    pub(crate) source: ClineFileSourceIdentity,
    pub(crate) core: ClineCorePayload,
    pub(crate) source_record: Option<ClineSourceRecordEvidence>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClinePublicationStats {
    pub(crate) component_hydrations: usize,
    pub(crate) component_parse_passes: usize,
    pub(crate) array_item_parse_attempts: usize,
    pub(crate) max_array_item_bytes_retained: usize,
    pub(crate) max_pages_buffered: usize,
    pub(crate) pages_certified: usize,
    pub(crate) core_rows: usize,
    pub(crate) local_rejections: usize,
    pub(crate) output_outcomes_observed: usize,
}

mod metrics;

pub(super) use metrics::*;
use metrics::{hash_field, hash_native_key};
