use std::{fmt, path::PathBuf, sync::Arc};

use ctx_history_core::{Confidence, FileChangeKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{OutputOutcome, OutputOutcomeMetadata, ProOutputObservation};

use super::source::{ClineComponent, ClineComponentObservation};

const EVENT_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-event-v2\0";
const EVENT_FILE_TOUCH_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-event-file-touches-v1\0";
const ITEM_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-item-v2\0";
const ARRAY_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-array-v2\0";
const PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-native-ingestion-page-v1\0";
const SESSION_HASH_DOMAIN: &[u8] = b"ctx-cline-nativepath-session-v2\0";

pub(crate) const CLINE_NATIVE_PAGE_MAX_UNITS: usize = 64;
pub(crate) const CLINE_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(super) const CLINE_NATIVE_CORE_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES: usize = 64 * 1024;
pub(super) const CLINE_NATIVE_MAX_REJECTIONS: usize = 32;
pub(super) const CLINE_NATIVE_MAX_FAILURE_PREVIEW_BYTES: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineNativeProfile {
    CoreOnly,
    CoreAndPro,
}

impl ClineNativeProfile {
    pub(super) fn wants_outputs(self) -> bool {
        self == Self::CoreAndPro
    }
}

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
        component_ordinal: u64,
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
    pub(crate) preview: Option<Box<str>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineEventRow {
    pub(crate) identity: ClineEventIdentity,
    pub(crate) native_order: ClineNativeOrder,
    pub(crate) kind: ClineEventKind,
    pub(crate) role: ClineEventRole,
    pub(crate) occurred_at_millis: Option<i64>,
    pub(crate) body: Option<Box<str>>,
    pub(crate) content_hash: [u8; 32],
    pub(crate) preview: Option<Box<str>>,
    pub(crate) tool_call: Option<ClineToolCall>,
    pub(crate) sparse_output: Option<ClineSparseOutputDiagnostic>,
    pub(crate) file_touches: Box<[ClineFileTouch]>,
}

impl ClineEventRow {
    pub(super) fn message(
        context: ClineEventContext<'_>,
        sub_index: u32,
        kind: ClineEventKind,
        body: String,
    ) -> Self {
        let preview = body
            .chars()
            .take(crate::PROVIDER_MAX_PREVIEW_CHARS)
            .collect::<String>();
        let content_hash = event_hash(context, sub_index, kind, context.role, body.as_bytes());
        Self {
            identity: event_identity(context, sub_index),
            native_order: native_order(context, sub_index),
            kind,
            role: context.role,
            occurred_at_millis: context.occurred_at_millis,
            body: Some(body.into_boxed_str()),
            content_hash,
            preview: Some(preview.into_boxed_str()),
            tool_call: None,
            sparse_output: None,
            file_touches: Box::default(),
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
            preview: None,
            tool_call: Some(ClineToolCall {
                call_id: call_id.map(String::into_boxed_str),
                name: name.map(String::into_boxed_str),
            }),
            sparse_output: None,
            file_touches: Box::default(),
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
        if let Some(preview) = diagnostic.preview.as_deref() {
            safe.extend_from_slice(preview.as_bytes());
        }
        Self {
            identity: event_identity(context, sub_index),
            native_order: native_order(context, sub_index),
            kind,
            role: ClineEventRole::Unknown,
            occurred_at_millis: context.occurred_at_millis,
            body: None,
            content_hash: event_hash(context, sub_index, kind, ClineEventRole::Unknown, &safe),
            preview: None,
            tool_call: None,
            sparse_output: Some(diagnostic),
            file_touches: Box::default(),
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
    context: ClineEventContext<'_>,
    sub_index: u32,
    kind: ClineEventKind,
    role: ClineEventRole,
    safe: &[u8],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(EVENT_HASH_DOMAIN);
    hasher.update(context.task.as_str().as_bytes());
    hasher.update([context.component as u8]);
    hash_native_key(&mut hasher, context.item);
    hasher.update(sub_index.to_le_bytes());
    hasher.update([kind as u8, role as u8]);
    hasher.update(safe);
    hasher.finalize().into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClineItemRejectionKind {
    OversizedRetainedItem,
    OversizedTransientOutput,
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

    pub(super) fn advance_metadata(&self, metadata_hash: &[u8; 32]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"ctx-cline-nativepath-metadata-frontier-v1\0");
        hasher.update(self.prefix_semantic_sha256);
        hasher.update(metadata_hash);
        Self {
            version: self.version,
            next_native_index: 1,
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

    pub(super) fn estimated_bytes(&self) -> usize {
        // Exact length of the provider-owned checkpoint encoding: component
        // tag, observation, content digest, counters, and terminal frontier.
        1_usize
            .saturating_add(estimated_observation_bytes(&self.observation))
            .saturating_add(32)
            .saturating_add(8)
            .saturating_add(8)
            .saturating_add(8)
            .saturating_add(estimated_frontier_bytes(&self.final_frontier))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineSessionRow {
    pub(crate) identity: ClineTaskIdentity,
    pub(crate) identity_origin: ClineTaskIdentityOrigin,
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
    Unchanged,
    Append { prior_items: usize },
    Rewrite,
    ControlOnlyRewrite,
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
pub(crate) struct ClineCertifiedRevision {
    pub(crate) revision_sha256: [u8; 32],
    pub(crate) observed_stamp_token: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineFileSourceIdentity {
    pub(crate) provider: &'static str,
    pub(crate) task: ClineTaskIdentity,
    pub(crate) component: ClineComponent,
    pub(crate) canonical_path: PathBuf,
    pub(crate) stable_id: Box<str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ClineNativePageIdentity(pub(crate) [u8; 32]);

impl fmt::Debug for ClineNativePageIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ClineNativePageIdentity")
            .field(&format_args!("{:02x?}", &self.0[..8]))
            .finish()
    }
}

impl ClineNativePageIdentity {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClineTerminalEvidence {
    CompleteArray {
        observed_items: u64,
        complete_bytes: u64,
        certified_revision_sha256: [u8; 32],
    },
    CompleteMetadata {
        content_sha256: Option<[u8; 32]>,
    },
    Deleted,
    ControlOnly {
        certified_revision_sha256: [u8; 32],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClinePageAccounting {
    pub(crate) core_units: usize,
    pub(crate) potential_output_units: usize,
    pub(crate) logical_units: usize,
    pub(crate) conservative_core_bytes: usize,
    pub(crate) transient_output_bytes: usize,
    pub(crate) conservative_serialized_bytes: usize,
}

#[derive(Debug)]
pub(crate) struct ClineCorePayload {
    #[cfg(test)]
    pub(crate) transition: ClineComponentTransition,
    pub(crate) session: Option<ClineSessionRow>,
    pub(crate) events: Box<[ClineEventRow]>,
    pub(crate) rejections: Box<[ClineItemRejection]>,
    pub(crate) terminal_metadata_checkpoint: Option<Box<ClineMetadataCheckpoint>>,
}

#[derive(Debug)]
pub(crate) struct ClineTransientOutputPayload {
    pub(crate) observations: Vec<ProOutputObservation>,
    pub(crate) rejected_outputs: Box<[ClineItemRejection]>,
}

#[derive(Debug)]
pub(crate) struct ClineCertifiedPage {
    pub(crate) identity: ClineNativePageIdentity,
    pub(crate) source: ClineFileSourceIdentity,
    pub(crate) source_revision: ClineCertifiedRevision,
    pub(crate) expected_frontier: ClinePageFrontier,
    pub(crate) next_safe_frontier: ClinePageFrontier,
    pub(crate) terminal: bool,
    #[cfg(test)]
    pub(crate) terminal_evidence: Option<ClineTerminalEvidence>,
    pub(crate) accounting: ClinePageAccounting,
    pub(crate) core: ClineCorePayload,
    pub(crate) transient: Option<ClineTransientOutputPayload>,
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
    pub(crate) output_bodies_hydrated: usize,
    pub(crate) output_body_bytes_hydrated: usize,
    pub(crate) success_unknown_core_rows: usize,
    pub(crate) success_unknown_hashes: usize,
    pub(crate) success_unknown_previews: usize,
    pub(crate) success_unknown_touches: usize,
    pub(crate) success_unknown_blobs: usize,
    pub(crate) success_unknown_fts_documents: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineCatalogEntry {
    pub(crate) task_id: Box<str>,
    pub(crate) title: Option<Box<str>>,
    pub(crate) workspace_directory: Option<Box<str>>,
    pub(crate) timestamp_millis: Option<i64>,
    pub(crate) tokens_input: Option<u64>,
    pub(crate) tokens_output: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClineCatalogIndex {
    Missing,
    Parsed {
        content_sha256: [u8; 32],
        entries: Box<[ClineCatalogEntry]>,
    },
    Incomplete(ClineCatalogRejection),
    Malformed(ClineCatalogRejection),
    Unavailable(ClineCatalogRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineCatalogRejection {
    pub(crate) path: PathBuf,
    pub(crate) retryable: bool,
    pub(crate) message: Box<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClineCatalogCompletion {
    pub(crate) inventory_complete: bool,
    pub(crate) inventory_revalidated: bool,
    pub(crate) root_index: ClineCatalogIndex,
    pub(crate) component_outcomes: Box<[ClineComponentReadOutcome]>,
    pub(crate) live_checkpoints: Box<[ClineTaskCheckpoint]>,
    pub(crate) missing_task_paths: Box<[PathBuf]>,
}

pub(super) fn page_identity(
    source: &ClineFileSourceIdentity,
    revision: &ClineCertifiedRevision,
    expected: &ClinePageFrontier,
    next: &ClinePageFrontier,
    terminal: bool,
    core_fingerprint: &[u8; 32],
) -> ClineNativePageIdentity {
    let mut hasher = Sha256::new();
    hasher.update(PAGE_IDENTITY_DOMAIN);
    hash_field(&mut hasher, source.provider.as_bytes());
    hash_field(&mut hasher, source.stable_id.as_bytes());
    hash_field(&mut hasher, &revision.revision_sha256);
    hash_frontier(&mut hasher, expected);
    hash_frontier(&mut hasher, next);
    hash_field(&mut hasher, core_fingerprint);
    hasher.update([u8::from(terminal)]);
    ClineNativePageIdentity(hasher.finalize().into())
}

pub(super) fn core_payload_fingerprint(
    component: ClineComponent,
    transition: ClineComponentTransition,
    session: Option<&ClineSessionRow>,
    items: &[ClineItemCheckpoint],
    rejections: &[ClineItemRejection],
    terminal: bool,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-cline-nativepath-core-payload-v1\0");
    hasher.update([component as u8, transition_tag(transition)]);
    if let Some(session) = session {
        hasher.update(session.metadata_hash);
    }
    for item in items {
        hasher.update(item.semantic_hash);
    }
    for rejection in rejections {
        hasher.update([rejection.kind as u8]);
        hasher.update(rejection.native_index.to_le_bytes());
    }
    hasher.update([u8::from(terminal)]);
    hasher.finalize().into()
}

pub(super) fn estimated_event_bytes(row: &ClineEventRow) -> usize {
    // This is the exact size of the provider-owned length-prefixed page
    // encoding. Strings and byte arrays carry an eight-byte length. Optional
    // values carry a one-byte presence tag.
    encoded_str(row.identity.task.as_str())
        .saturating_add(1)
        .saturating_add(estimated_native_key_bytes(&row.identity.item))
        .saturating_add(4)
        .saturating_add(1 + 8 + 4)
        .saturating_add(1 + 1)
        .saturating_add(encoded_option_i64(row.occurred_at_millis))
        .saturating_add(encoded_option_str(row.body.as_deref()))
        .saturating_add(32)
        .saturating_add(encoded_option_str(row.preview.as_deref()))
        .saturating_add(row.tool_call.as_ref().map_or(1, |call| {
            1_usize
                .saturating_add(encoded_option_str(call.call_id.as_deref()))
                .saturating_add(encoded_option_str(call.name.as_deref()))
        }))
        .saturating_add(row.sparse_output.as_ref().map_or(1, |output| {
            1_usize
                .saturating_add(1)
                .saturating_add(encoded_option_i32(output.exit_code))
                .saturating_add(encoded_option_u64(output.duration_ms))
                .saturating_add(8)
                .saturating_add(encoded_option_str(output.preview.as_deref()))
                .saturating_add(encoded_option_str(output.call_id.as_deref()))
        }))
        .saturating_add(8)
        .saturating_add(row.file_touches.iter().fold(0_usize, |bytes, touch| {
            bytes
                .saturating_add(encoded_str(&touch.path))
                .saturating_add(encoded_option_str(touch.old_path.as_deref()))
                .saturating_add(1)
                .saturating_add(1)
                .saturating_add(encoded_str(
                    &serde_json::to_string(&touch.metadata)
                        .expect("file-touch metadata should serialize"),
                ))
        }))
}

pub(super) fn estimated_session_bytes(session: &ClineSessionRow) -> usize {
    encoded_str(session.identity.as_str())
        .saturating_add(1)
        .saturating_add(encoded_option_str(session.title.as_deref()))
        .saturating_add(encoded_option_str(session.workspace_directory.as_deref()))
        .saturating_add(encoded_option_str(session.created_at.as_deref()))
        .saturating_add(encoded_option_str(session.last_modified.as_deref()))
        .saturating_add(encoded_option_str(session.model_id.as_deref()))
        .saturating_add(encoded_option_str(session.model_provider.as_deref()))
        .saturating_add(encoded_option_u64(session.tokens_input))
        .saturating_add(encoded_option_u64(session.tokens_output))
        .saturating_add(32)
}

pub(super) fn estimated_metadata_checkpoint_bytes(checkpoint: &ClineMetadataCheckpoint) -> usize {
    estimated_observation_bytes(&checkpoint.observation)
        .saturating_add(1 + usize::from(checkpoint.content_sha256.is_some()) * 32)
        .saturating_add(estimated_session_bytes(&checkpoint.session))
}

pub(super) fn estimated_rejection_bytes(rejection: &ClineItemRejection) -> usize {
    1_usize
        .saturating_add(8)
        .saturating_add(encoded_option_str(rejection.native_id.as_deref()))
        .saturating_add(1)
        .saturating_add(8)
        .saturating_add(encoded_str(&rejection.detail))
}

pub(super) fn estimated_output_bytes(output: &ProOutputObservation) -> usize {
    let coordinate = encoded_str(&output.coordinate.unit_key)
        .saturating_add(8)
        .saturating_add(encoded_option_str(
            output.coordinate.native_record_id.as_deref(),
        ))
        .saturating_add(encoded_option_u64(output.coordinate.source_record_ordinal))
        .saturating_add(encoded_option_u32(
            output.coordinate.source_record_subrecord_index,
        ))
        .saturating_add(encoded_option_u64(output.coordinate.byte_start))
        .saturating_add(encoded_option_u64(output.coordinate.byte_end_exclusive));
    let associations = encoded_str(&output.associations.direct_session_id)
        .saturating_add(encoded_str(&output.associations.root_session_id))
        .saturating_add(encoded_option_str(
            output.associations.parent_session_id.as_deref(),
        ))
        .saturating_add(encoded_option_str(
            output.associations.provider_session_id.as_deref(),
        ))
        .saturating_add(encoded_option_str(output.associations.agent_id.as_deref()))
        .saturating_add(
            output
                .associations
                .repository
                .as_ref()
                .map_or(1, |repository| {
                    1_usize
                        .saturating_add(encoded_str(&repository.repository_id))
                        .saturating_add(encoded_option_str(repository.checkout_id.as_deref()))
                        .saturating_add(encoded_option_str(repository.worktree_id.as_deref()))
                        .saturating_add(encoded_option_str(repository.object_format.as_deref()))
                }),
        );
    let command = output.command.as_ref().map_or(1, |command| {
        1_usize
            .saturating_add(encoded_str(&command.tool_name))
            .saturating_add(encoded_str(&command.command))
            .saturating_add(encoded_option_str(command.working_directory.as_deref()))
    });
    1_usize
        .saturating_add(coordinate)
        .saturating_add(encoded_option_i64(output.occurred_at_unix_ms))
        .saturating_add(associations)
        .saturating_add(encoded_option_str(output.call_id.as_deref()))
        .saturating_add(command)
        .saturating_add(1)
        .saturating_add(encoded_option_i32(output.outcome.exit_code))
        .saturating_add(encoded_option_u64(output.outcome.duration_ms))
        .saturating_add(4)
        .saturating_add(encoded_str(&output.locator.kind))
        .saturating_add(encoded_bytes(&output.locator.payload))
        .saturating_add(encoded_bytes(&output.content))
}

pub(super) fn estimated_observation_bytes(observation: &ClineComponentObservation) -> usize {
    1_usize
        .saturating_add(encoded_bytes(
            observation.path.as_os_str().as_encoded_bytes(),
        ))
        .saturating_add(1)
        .saturating_add(match &observation.state {
            super::source::ClineObservedFileState::Missing => 0,
            super::source::ClineObservedFileState::Present(stamp) => {
                8_usize.saturating_add(encoded_str(&stamp.token()))
            }
            super::source::ClineObservedFileState::Unavailable(message) => encoded_str(message),
        })
}

pub(super) fn estimated_source_bytes(source: &ClineFileSourceIdentity) -> usize {
    encoded_str(source.provider)
        .saturating_add(encoded_str(source.task.as_str()))
        .saturating_add(1)
        .saturating_add(encoded_bytes(
            source.canonical_path.as_os_str().as_encoded_bytes(),
        ))
        .saturating_add(encoded_str(&source.stable_id))
}

pub(super) fn estimated_revision_bytes(revision: &ClineCertifiedRevision) -> usize {
    32_usize.saturating_add(encoded_str(&revision.observed_stamp_token))
}

pub(super) fn estimated_frontier_bytes(_frontier: &ClinePageFrontier) -> usize {
    4 + 8 + 32
}

fn estimated_native_key_bytes(key: &ClineNativeItemKey) -> usize {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            component_ordinal,
        } => 1_usize
            .saturating_add(encoded_str(native_id))
            .saturating_add(std::mem::size_of_val(component_ordinal)),
        ClineNativeItemKey::ComponentOrdinal(_) => 1 + 8,
    }
}

fn encoded_str(value: &str) -> usize {
    encoded_bytes(value.as_bytes())
}

fn encoded_bytes(value: &[u8]) -> usize {
    8_usize.saturating_add(value.len())
}

fn encoded_option_str(value: Option<&str>) -> usize {
    1_usize.saturating_add(value.map_or(0, encoded_str))
}

fn encoded_option_i32(value: Option<i32>) -> usize {
    1 + usize::from(value.is_some()) * 4
}

fn encoded_option_u32(value: Option<u32>) -> usize {
    1 + usize::from(value.is_some()) * 4
}

fn encoded_option_i64(value: Option<i64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

fn encoded_option_u64(value: Option<u64>) -> usize {
    1 + usize::from(value.is_some()) * 8
}

fn transition_tag(transition: ClineComponentTransition) -> u8 {
    match transition {
        ClineComponentTransition::Cold => 0,
        ClineComponentTransition::Unchanged => 1,
        ClineComponentTransition::Append { .. } => 2,
        ClineComponentTransition::Rewrite => 3,
        ClineComponentTransition::ControlOnlyRewrite => 5,
        ClineComponentTransition::LogicalEmpty => 6,
        ClineComponentTransition::MissingPhysical => 7,
    }
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_frontier(hasher: &mut Sha256, frontier: &ClinePageFrontier) {
    hasher.update(frontier.version.to_le_bytes());
    hasher.update(frontier.next_native_index.to_le_bytes());
    hasher.update(frontier.prefix_semantic_sha256);
}

fn hash_native_key(hasher: &mut Sha256, key: &ClineNativeItemKey) {
    match key {
        ClineNativeItemKey::NativeId {
            native_id,
            component_ordinal,
        } => {
            hasher.update(b"id\0");
            hasher.update(native_id.as_bytes());
            hasher.update(component_ordinal.to_le_bytes());
        }
        ClineNativeItemKey::ComponentOrdinal(ordinal) => {
            hasher.update(b"ordinal\0");
            hasher.update(ordinal.to_le_bytes());
        }
    }
    hasher.update(b"\0");
}
