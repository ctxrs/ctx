use chrono::{DateTime, Utc};
use ctx_history_core::{ContentRef, EventRole, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::lifecycle::WarpNativePersistedState;
use crate::{
    complete_content::CompleteContentBodyDigest, CaptureError, OutputOutcome, ProOutputObservation,
    Result,
};

pub(in super::super) const WARP_NATIVE_PAGE_MAX_ROWS: usize = 64;
pub(in super::super) const WARP_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub(in super::super) const WARP_NATIVE_PRO_OUTPUT_MAX_BODY_BYTES: usize =
    WARP_NATIVE_PAGE_MAX_BYTES;
const WARP_NATIVE_BODY_MAX_CHARS: usize = 16_000;
const WARP_NATIVE_PREVIEW_MAX_CHARS: usize = 240;
const WARP_NATIVE_REJECTION_KEY_MAX_CHARS: usize = 512;
const WARP_NATIVE_REJECTION_REASON_MAX_CHARS: usize = 1_024;
const WARP_NATIVE_OUTPUT_REJECTION_RESERVE_BYTES: usize = 40 * 1024;
const WARP_CORE_DIGEST_DOMAIN: &[u8] = b"ctx-warp-core-generation-v2\0";
pub(super) const WARP_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-warp-source-integrity-v4\0";
const WARP_DIGEST_CHAIN_DOMAIN: &[u8] = b"ctx-warp-native-digest-chain-v1\0";
const WARP_CORE_UNIT_DIGEST_DOMAIN: &[u8] = b"ctx-warp-native-core-unit-v1\0";
const WARP_EVENT_HASH_DOMAIN: &[u8] = b"ctx-warp-retained-event-v2\0";
const WARP_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-warp-native-safe-page-v2\0";
const WARP_PRO_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-warp-native-pro-output-page-v1\0";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in super::super) enum WarpNativeProfile {
    #[default]
    CoreOnly,
    CoreAndPro,
}

impl WarpNativeProfile {
    pub(in super::super) fn wants_transient_outputs(self) -> bool {
        self == Self::CoreAndPro
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in super::super) enum WarpNativeFrontierPhase {
    #[default]
    Start,
    Conversations,
    Tasks,
}

/// A deterministic provider-native safe prefix.
///
/// Conversation and hierarchy counters advance only after a complete row or
/// edge. During task decoding, `last_task_rowid` plus
/// `next_message_ordinal` identifies the next complete protobuf message. A
/// completed task clears the message ordinal and increments
/// `completed_task_rows`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in super::super) struct WarpNativeFrontier {
    pub(in super::super) phase: WarpNativeFrontierPhase,
    pub(in super::super) completed_conversation_rows: u64,
    pub(in super::super) completed_hierarchy_edges: u64,
    /// SQLite rowid of the last conversation emitted from this exact
    /// immutable snapshot.
    pub(in super::super) last_conversation_rowid: Option<i64>,
    pub(in super::super) completed_task_rows: u64,
    /// SQLite rowid of the last task touched by this exact immutable snapshot.
    /// A zero next ordinal means the task is complete; a nonzero ordinal
    /// resumes inside it. No provider string key enters durable cursor state.
    pub(in super::super) last_task_rowid: Option<i64>,
    pub(in super::super) next_message_ordinal: u32,
    pub(in super::super) retained_events: u64,
    pub(in super::super) source_digest: [u8; 32],
    pub(in super::super) core_digest: [u8; 32],
}

impl WarpNativeFrontier {
    pub(super) fn after_conversation(
        completed_conversations: u64,
        completed_edges: u64,
        conversation_rowid: i64,
    ) -> Self {
        Self {
            phase: WarpNativeFrontierPhase::Conversations,
            completed_conversation_rows: completed_conversations,
            completed_hierarchy_edges: completed_edges,
            last_conversation_rowid: Some(conversation_rowid),
            ..Self::default()
        }
    }

    pub(super) fn in_task(
        completed_conversations: u64,
        completed_edges: u64,
        completed_tasks: u64,
        task_rowid: i64,
        next_message_ordinal: u32,
    ) -> Self {
        Self {
            phase: WarpNativeFrontierPhase::Tasks,
            completed_conversation_rows: completed_conversations,
            completed_hierarchy_edges: completed_edges,
            completed_task_rows: completed_tasks,
            last_task_rowid: Some(task_rowid),
            next_message_ordinal,
            ..Self::default()
        }
    }

    pub(super) fn after_task(
        completed_conversations: u64,
        completed_edges: u64,
        completed_tasks: u64,
        task_rowid: i64,
    ) -> Self {
        Self {
            phase: WarpNativeFrontierPhase::Tasks,
            completed_conversation_rows: completed_conversations,
            completed_hierarchy_edges: completed_edges,
            completed_task_rows: completed_tasks,
            last_task_rowid: Some(task_rowid),
            next_message_ordinal: 0,
            ..Self::default()
        }
    }

    pub(in super::super) fn is_persistable(&self) -> bool {
        let conversation_cursor_valid = self.last_conversation_rowid.is_none_or(|rowid| rowid > 0);
        let task_cursor_valid = self.last_task_rowid.is_none_or(|rowid| rowid > 0);
        let counts_valid = self.completed_hierarchy_edges <= self.completed_conversation_rows;
        let phase_valid = match self.phase {
            WarpNativeFrontierPhase::Start => {
                self.completed_conversation_rows == 0
                    && self.completed_hierarchy_edges == 0
                    && self.last_conversation_rowid.is_none()
                    && self.completed_task_rows == 0
                    && self.last_task_rowid.is_none()
                    && self.next_message_ordinal == 0
            }
            WarpNativeFrontierPhase::Conversations => {
                self.completed_conversation_rows > 0
                    && self.last_conversation_rowid.is_some()
                    && self.completed_task_rows == 0
                    && self.last_task_rowid.is_none()
                    && self.next_message_ordinal == 0
            }
            WarpNativeFrontierPhase::Tasks => {
                self.last_conversation_rowid.is_none()
                    && self.last_task_rowid.is_some()
                    && (self.completed_task_rows > 0 || self.next_message_ordinal > 0)
            }
        };
        conversation_cursor_valid && task_cursor_valid && counts_valid && phase_valid
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(in super::super) struct WarpNativePageIdentity(pub(in super::super) [u8; 32]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(in super::super) struct WarpNativeProOutputPageIdentity(pub(in super::super) [u8; 32]);

#[derive(Clone, Debug, PartialEq)]
pub(in super::super) struct WarpNativeSession {
    pub(in super::super) conversation_id: String,
    pub(in super::super) parent_conversation_id: Option<String>,
    pub(in super::super) root_conversation_id: String,
    pub(in super::super) parent_present: bool,
    pub(in super::super) title: String,
    pub(in super::super) modified_at: Option<DateTime<Utc>>,
    pub(in super::super) metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeHierarchyEdge {
    pub(in super::super) child_conversation_id: String,
    pub(in super::super) parent_conversation_id: String,
    pub(in super::super) parent_present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in super::super) enum WarpNativeMessageIdentity {
    ProviderId(String),
    MessageOrdinal(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in super::super) struct WarpNativeEventIdentity {
    pub(in super::super) conversation_id: String,
    pub(in super::super) task_id: String,
    pub(in super::super) message: WarpNativeMessageIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeOrder {
    pub(in super::super) provider_event_index: u64,
    pub(in super::super) task_rowid: i64,
    pub(in super::super) task_key: String,
    pub(in super::super) message_ordinal: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub(in super::super) struct WarpNativeEvent {
    pub(in super::super) identity: WarpNativeEventIdentity,
    pub(in super::super) native_order: WarpNativeOrder,
    pub(in super::super) event_type: EventType,
    pub(in super::super) role: Option<EventRole>,
    pub(in super::super) kind: &'static str,
    pub(in super::super) request_id: Option<String>,
    pub(in super::super) result_outcome: Option<OutputOutcome>,
    pub(in super::super) call_id: Option<String>,
    pub(in super::super) occurred_at: Option<DateTime<Utc>>,
    pub(in super::super) body: String,
    pub(in super::super) content_hash: String,
    pub(in super::super) preview: String,
    pub(in super::super) complete_content_ref: Option<ContentRef>,
    pub(in super::super) source_record_digest: CompleteContentBodyDigest,
}

pub(in super::super) struct WarpNativeEventDraft {
    pub(in super::super) provider_event_index: u64,
    pub(in super::super) task_rowid: i64,
    pub(in super::super) conversation_id: String,
    pub(in super::super) task_id: String,
    pub(in super::super) message_id: Option<String>,
    pub(in super::super) message_ordinal: u32,
    pub(in super::super) event_type: EventType,
    pub(in super::super) role: Option<EventRole>,
    pub(in super::super) kind: &'static str,
    pub(in super::super) request_id: Option<String>,
    pub(in super::super) result_outcome: Option<OutputOutcome>,
    pub(in super::super) call_id: Option<String>,
    pub(in super::super) occurred_at: Option<DateTime<Utc>>,
    pub(in super::super) body: String,
    pub(in super::super) source_record_digest: CompleteContentBodyDigest,
}

impl WarpNativeEvent {
    pub(in super::super) fn from_draft(draft: WarpNativeEventDraft) -> Result<Self> {
        let complete_content_ref = (draft.body.chars().count() > WARP_NATIVE_BODY_MAX_CHARS)
            .then(|| ContentRef::from_bytes(draft.body.as_bytes()))
            .flatten();
        if draft.body.chars().count() > WARP_NATIVE_BODY_MAX_CHARS && complete_content_ref.is_none()
        {
            return Err(CaptureError::InvalidPayload(
                "Warp complete message content exceeds ContentRef bounds".to_owned(),
            ));
        }
        let body = truncate_chars(&draft.body, WARP_NATIVE_BODY_MAX_CHARS);
        let preview = truncate_chars(&body, WARP_NATIVE_PREVIEW_MAX_CHARS);
        let message = draft.message_id.map_or(
            WarpNativeMessageIdentity::MessageOrdinal(draft.message_ordinal),
            WarpNativeMessageIdentity::ProviderId,
        );
        let identity = WarpNativeEventIdentity {
            conversation_id: draft.conversation_id,
            task_id: draft.task_id.clone(),
            message,
        };
        let content_hash = retained_event_hash(
            &identity,
            &body,
            draft.result_outcome,
            draft.call_id.as_deref(),
        )?;
        Ok(Self {
            identity,
            native_order: WarpNativeOrder {
                provider_event_index: draft.provider_event_index,
                task_rowid: draft.task_rowid,
                task_key: draft.task_id,
                message_ordinal: draft.message_ordinal,
            },
            event_type: draft.event_type,
            role: draft.role,
            kind: draft.kind,
            request_id: draft.request_id,
            result_outcome: draft.result_outcome,
            call_id: draft.call_id,
            occurred_at: draft.occurred_at,
            body,
            content_hash,
            preview,
            complete_content_ref,
            source_record_digest: draft.source_record_digest,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) enum WarpNativeRejectionKind {
    ConversationRecord,
    TaskRecord,
    OversizedTask,
    OversizedNormalizedUnit,
    MalformedProtobuf,
    MissingConversation,
    DuplicateMessageIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeRejection {
    pub(in super::super) kind: WarpNativeRejectionKind,
    pub(in super::super) native_key: String,
    pub(in super::super) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum WarpNativeOutputRejectionKind {
    Malformed,
    Oversized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeOutputRejection {
    pub(in super::super) kind: WarpNativeOutputRejectionKind,
    pub(in super::super) native_key: String,
    pub(in super::super) reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeProOutputPageReceipt {
    pub(in super::super) identity: WarpNativeProOutputPageIdentity,
    pub(in super::super) expected_frontier: WarpNativeFrontier,
    pub(in super::super) committed_frontier: WarpNativeFrontier,
    pub(in super::super) accepted_outputs: usize,
    pub(in super::super) accepted_rejections: usize,
}

/// One independently bounded transient-output prefix.
///
/// Its boundaries and identity may depend on Pro bytes. They never participate
/// in Core page accounting, Core frontiers, or Core identity.
#[derive(Debug)]
pub(in super::super) struct WarpNativeProOutputPage {
    pub(in super::super) identity: WarpNativeProOutputPageIdentity,
    pub(in super::super) expected_frontier: WarpNativeFrontier,
    pub(in super::super) next_safe_frontier: WarpNativeFrontier,
    pub(in super::super) outputs: Vec<ProOutputObservation>,
    pub(in super::super) rejections: Vec<WarpNativeOutputRejection>,
    pub(in super::super) logical_units: usize,
    pub(in super::super) estimated_bytes: usize,
}

impl WarpNativeProOutputPage {
    pub(in super::super) fn row_count(&self) -> usize {
        self.outputs.len().saturating_add(self.rejections.len())
    }

    pub(in super::super) fn logical_unit_count(&self) -> usize {
        self.logical_units
    }

    pub(in super::super) fn receipt(&self) -> WarpNativeProOutputPageReceipt {
        WarpNativeProOutputPageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_outputs: self.outputs.len(),
            accepted_rejections: self.rejections.len(),
        }
    }
}

#[derive(Debug)]
pub(in super::super) struct WarpNativePage {
    pub(in super::super) identity: WarpNativePageIdentity,
    pub(in super::super) expected_frontier: WarpNativeFrontier,
    pub(in super::super) next_safe_frontier: WarpNativeFrontier,
    pub(in super::super) sessions: Vec<WarpNativeSession>,
    pub(in super::super) hierarchy_edges: Vec<WarpNativeHierarchyEdge>,
    pub(in super::super) events: Vec<WarpNativeEvent>,
    pub(in super::super) rejections: Vec<WarpNativeRejection>,
    pub(in super::super) logical_units: usize,
    pub(in super::super) estimated_bytes: usize,
}

impl WarpNativePage {
    pub(in super::super) fn row_count(&self) -> usize {
        self.sessions
            .len()
            .saturating_add(self.hierarchy_edges.len())
            .saturating_add(self.events.len())
            .saturating_add(self.rejections.len())
    }

    pub(in super::super) fn logical_unit_count(&self) -> usize {
        self.logical_units
    }
}

pub(in super::super) trait WarpNativeSink {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()>;

    /// Transfers one owned Pro page and acknowledges its exact prefix.
    ///
    /// The handoff is deliberately infallible: malformed or oversized output
    /// is represented inside the Pro page, and cannot block or roll back Core.
    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in super::super) struct WarpNativeCounters {
    pub(in super::super) conversation_rows: u64,
    pub(in super::super) conversation_rows_hydrated: u64,
    pub(in super::super) conversation_json_objects_parsed: u64,
    pub(in super::super) sessions_retained: u64,
    pub(in super::super) hierarchy_nodes_retained: u64,
    pub(in super::super) peak_session_metadata_rows: u64,
    pub(in super::super) hierarchy_edges: u64,
    pub(in super::super) task_rows: u64,
    pub(in super::super) oversized_task_rows: u64,
    pub(in super::super) oversized_normalized_units: u64,
    pub(in super::super) oversized_output_records: u64,
    pub(in super::super) malformed_output_records: u64,
    pub(in super::super) duplicate_message_identity_tasks: u64,
    pub(in super::super) peak_task_identity_entries: u64,
    pub(in super::super) protobuf_bytes_scanned: u64,
    pub(in super::super) retained_events: u64,
    pub(in super::super) retained_body_bytes: u64,
    pub(in super::super) retained_content_hashes: u64,
    pub(in super::super) retained_previews: u64,
    pub(in super::super) tool_calls_retained: u64,
    pub(in super::super) malformed_task_cells: u64,
    pub(in super::super) unknown_fields: u64,
    pub(in super::super) unknown_oneofs: u64,
    pub(in super::super) native_result_records: u64,
    pub(in super::super) native_result_envelope_bytes: u64,
    pub(in super::super) native_result_body_bytes_observed: u64,
    pub(in super::super) native_results_success: u64,
    pub(in super::super) native_results_failure: u64,
    pub(in super::super) native_results_timeout: u64,
    pub(in super::super) native_results_unknown: u64,
    pub(in super::super) result_body_bytes_decoded: u64,
    pub(in super::super) result_body_strings_allocated: u64,
    pub(in super::super) result_events_created: u64,
    pub(in super::super) result_hashes_created: u64,
    pub(in super::super) result_previews_created: u64,
    pub(in super::super) result_file_touches_created: u64,
    pub(in super::super) result_fts_documents_created: u64,
    pub(in super::super) result_handoffs_created: u64,
    pub(in super::super) generic_envelope_rows: u64,
    pub(in super::super) durable_transaction_rotations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeSourceAuthority {
    pub(in super::super) source_complete: bool,
    pub(in super::super) zero_authoritative_rows: bool,
    pub(in super::super) has_useful_content: bool,
    pub(in super::super) physical_locator: String,
    pub(in super::super) snapshot_revision: String,
    pub(in super::super) capability_digest: String,
    pub(in super::super) source_integrity_digest: String,
    pub(in super::super) core_generation_digest: String,
    pub(in super::super) persisted_state: Box<WarpNativePersistedState>,
    pub(in super::super) pages_emitted: u64,
    pub(in super::super) pro_output_pages_emitted: u64,
    pub(in super::super) counters: WarpNativeCounters,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum WarpNativeIncompleteReason {
    SnapshotCertificationRace,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct WarpNativeIncomplete {
    pub(in super::super) source_complete: bool,
    pub(in super::super) reason: WarpNativeIncompleteReason,
    pub(in super::super) physical_locator: String,
    pub(in super::super) pages_emitted: u64,
    pub(in super::super) pro_output_pages_emitted: u64,
    pub(in super::super) counters: WarpNativeCounters,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) enum WarpNativeScanOutcome {
    Complete(WarpNativeSourceAuthority),
    Incomplete(WarpNativeIncomplete),
}

pub(super) struct WarpNativeUnit {
    core: WarpNativeCoreUnit,
    pro: WarpNativeProUnit,
}

pub(super) struct WarpNativeCoreUnit {
    sessions: Vec<WarpNativeSession>,
    hierarchy_edges: Vec<WarpNativeHierarchyEdge>,
    events: Vec<WarpNativeEvent>,
    rejections: Vec<WarpNativeRejection>,
    estimated_bytes: usize,
}

impl WarpNativeCoreUnit {
    fn row_count(&self) -> usize {
        self.sessions
            .len()
            .saturating_add(self.hierarchy_edges.len())
            .saturating_add(self.events.len())
            .saturating_add(self.rejections.len())
    }
}

pub(super) struct WarpNativeProUnit {
    outputs: Vec<ProOutputObservation>,
    rejections: Vec<WarpNativeOutputRejection>,
    estimated_bytes: usize,
}

impl WarpNativeUnit {
    pub(super) fn progress() -> Self {
        Self {
            core: WarpNativeCoreUnit {
                sessions: Vec::new(),
                hierarchy_edges: Vec::new(),
                events: Vec::new(),
                rejections: Vec::new(),
                estimated_bytes: WARP_NATIVE_OUTPUT_REJECTION_RESERVE_BYTES,
            },
            pro: WarpNativeProUnit {
                outputs: Vec::new(),
                rejections: Vec::new(),
                estimated_bytes: 0,
            },
        }
    }

    pub(super) fn push_session(&mut self, session: WarpNativeSession) -> Result<()> {
        self.core.estimated_bytes = checked_add(
            self.core.estimated_bytes,
            session_estimated_bytes(&session)?,
            "Warp NativePath session byte count overflowed",
        )?;
        self.core.sessions.push(session);
        Ok(())
    }

    pub(super) fn push_edge(&mut self, edge: WarpNativeHierarchyEdge) -> Result<()> {
        let text = edge
            .child_conversation_id
            .len()
            .saturating_add(edge.parent_conversation_id.len());
        self.core.estimated_bytes = checked_add(
            self.core.estimated_bytes,
            conservative_text_bytes(text, 128),
            "Warp NativePath hierarchy byte count overflowed",
        )?;
        self.core.hierarchy_edges.push(edge);
        Ok(())
    }

    pub(super) fn push_event(&mut self, event: WarpNativeEvent) -> Result<()> {
        self.core.estimated_bytes = checked_add(
            self.core.estimated_bytes,
            event_estimated_bytes(&event),
            "Warp NativePath event byte count overflowed",
        )?;
        self.core.events.push(event);
        Ok(())
    }

    pub(super) fn try_push_output(&mut self, output: ProOutputObservation) -> bool {
        let next_bytes = self
            .pro
            .estimated_bytes
            .saturating_add(output_estimated_bytes(&output));
        if next_bytes > WARP_NATIVE_PAGE_MAX_BYTES {
            return false;
        }
        self.pro.estimated_bytes = next_bytes;
        self.pro.outputs.push(output);
        true
    }

    pub(super) fn push_output_rejection(
        &mut self,
        mut rejection: WarpNativeOutputRejection,
    ) -> Result<()> {
        bound_rejection_text(&mut rejection.native_key, &mut rejection.reason);
        let text = rejection
            .native_key
            .len()
            .saturating_add(rejection.reason.len());
        self.pro.estimated_bytes = checked_add(
            self.pro.estimated_bytes,
            conservative_text_bytes(text, 128),
            "Warp NativePath Pro rejection byte count overflowed",
        )?;
        self.pro.rejections.push(rejection);
        Ok(())
    }

    pub(super) fn push_rejection(&mut self, mut rejection: WarpNativeRejection) -> Result<()> {
        bound_rejection_text(&mut rejection.native_key, &mut rejection.reason);
        let text = rejection
            .native_key
            .len()
            .saturating_add(rejection.reason.len());
        self.core.estimated_bytes = checked_add(
            self.core.estimated_bytes,
            conservative_text_bytes(text, 128),
            "Warp NativePath rejection byte count overflowed",
        )?;
        self.core.rejections.push(rejection);
        Ok(())
    }

    pub(super) fn estimated_bytes(&self) -> usize {
        self.core.estimated_bytes
    }

    pub(super) fn into_oversized_rejection(
        mut self,
        kind: WarpNativeRejectionKind,
        native_key: String,
    ) -> Result<Self> {
        let observed_bytes = self.core.estimated_bytes;
        let mut replacement = Self::progress();
        replacement.push_rejection(WarpNativeRejection {
            kind,
            native_key,
            reason: format!(
                "Warp normalized unit exceeds the {WARP_NATIVE_PAGE_MAX_BYTES}-byte \
                safe-page limit ({observed_bytes} estimated bytes)"
            ),
        })?;
        self.core = replacement.core;
        Ok(self)
    }

    pub(super) fn into_lanes(self) -> (WarpNativeCoreUnit, WarpNativeProUnit) {
        (self.core, self.pro)
    }
}

pub(super) struct WarpNativePageAccumulator {
    page: WarpNativePage,
}

impl WarpNativePageAccumulator {
    pub(super) fn new(expected_frontier: WarpNativeFrontier) -> Self {
        Self {
            page: WarpNativePage {
                identity: WarpNativePageIdentity::default(),
                expected_frontier: expected_frontier.clone(),
                next_safe_frontier: expected_frontier,
                sessions: Vec::new(),
                hierarchy_edges: Vec::new(),
                events: Vec::new(),
                rejections: Vec::new(),
                logical_units: 0,
                estimated_bytes: 0,
            },
        }
    }

    pub(super) fn is_full(&self) -> bool {
        self.page.logical_units == WARP_NATIVE_PAGE_MAX_ROWS
            || self.page.row_count() == WARP_NATIVE_PAGE_MAX_ROWS
    }

    pub(super) fn can_accept(&self, unit: &WarpNativeCoreUnit) -> bool {
        self.page.logical_units < WARP_NATIVE_PAGE_MAX_ROWS
            && self
                .page
                .row_count()
                .checked_add(unit.row_count())
                .is_some_and(|rows| rows <= WARP_NATIVE_PAGE_MAX_ROWS)
            && self
                .page
                .estimated_bytes
                .checked_add(unit.estimated_bytes)
                .is_some_and(|bytes| bytes <= WARP_NATIVE_PAGE_MAX_BYTES)
    }

    pub(super) fn push(
        &mut self,
        unit: WarpNativeCoreUnit,
        mut next_frontier: WarpNativeFrontier,
        core_hasher: &mut WarpNativeDigestChain,
    ) -> Result<WarpNativeFrontier> {
        if !self.can_accept(&unit) {
            return Err(CaptureError::SystemInvariant(
                "Warp NativePath unit was pushed past a page bound",
            ));
        }
        let retained_events = u64::try_from(unit.events.len())
            .map_err(|_| CaptureError::SystemInvariant("Warp event count exceeds u64"))?;
        core_hasher.push(b"core-unit\0", core_unit_digest(&unit)?)?;
        next_frontier.core_digest = core_hasher.state();
        next_frontier.retained_events = self
            .page
            .next_safe_frontier
            .retained_events
            .checked_add(retained_events)
            .ok_or(CaptureError::SystemInvariant(
                "Warp retained event frontier overflowed",
            ))?;

        self.page.sessions.extend(unit.sessions);
        self.page.hierarchy_edges.extend(unit.hierarchy_edges);
        self.page.events.extend(unit.events);
        self.page.rejections.extend(unit.rejections);
        self.page.logical_units = self.page.logical_units.saturating_add(1);
        self.page.estimated_bytes = self
            .page
            .estimated_bytes
            .checked_add(unit.estimated_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "Warp NativePath page byte count overflowed",
            ))?;
        self.page.next_safe_frontier = next_frontier.clone();
        Ok(next_frontier)
    }

    pub(super) fn finish(mut self) -> Result<Option<WarpNativePage>> {
        if self.page.logical_units == 0 {
            return Ok(None);
        }
        self.page.identity = page_identity(&self.page)?;
        Ok(Some(self.page))
    }
}

pub(super) struct WarpNativeProPageAccumulator {
    page: WarpNativeProOutputPage,
}

impl WarpNativeProPageAccumulator {
    pub(super) fn new(expected_frontier: WarpNativeFrontier) -> Self {
        Self {
            page: WarpNativeProOutputPage {
                identity: WarpNativeProOutputPageIdentity::default(),
                expected_frontier: expected_frontier.clone(),
                next_safe_frontier: expected_frontier,
                outputs: Vec::new(),
                rejections: Vec::new(),
                logical_units: 0,
                estimated_bytes: 0,
            },
        }
    }

    pub(super) fn is_full(&self) -> bool {
        self.page.logical_units == WARP_NATIVE_PAGE_MAX_ROWS
    }

    pub(super) fn can_accept(&self, unit: &WarpNativeProUnit) -> bool {
        self.page.logical_units < WARP_NATIVE_PAGE_MAX_ROWS
            && self
                .page
                .estimated_bytes
                .checked_add(unit.estimated_bytes)
                .is_some_and(|bytes| bytes <= WARP_NATIVE_PAGE_MAX_BYTES)
    }

    pub(super) fn push(
        &mut self,
        unit: WarpNativeProUnit,
        next_frontier: WarpNativeFrontier,
    ) -> Result<()> {
        if !self.can_accept(&unit) {
            return Err(CaptureError::SystemInvariant(
                "Warp NativePath Pro unit was pushed past a page bound",
            ));
        }
        self.page.outputs.extend(unit.outputs);
        self.page.rejections.extend(unit.rejections);
        self.page.logical_units = self.page.logical_units.saturating_add(1);
        self.page.estimated_bytes = self
            .page
            .estimated_bytes
            .checked_add(unit.estimated_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "Warp NativePath Pro page byte count overflowed",
            ))?;
        self.page.next_safe_frontier = next_frontier;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Option<WarpNativeProOutputPage>> {
        if self.page.logical_units == 0 {
            return Ok(None);
        }
        self.page.identity = pro_page_identity(&self.page)?;
        Ok(Some(self.page))
    }
}

pub(super) struct WarpNativeDigestChain {
    domain: &'static [u8],
    state: [u8; 32],
}

impl WarpNativeDigestChain {
    pub(super) fn new(domain: &'static [u8], persisted: [u8; 32]) -> Self {
        let state = if persisted == [0; 32] {
            let mut hasher = Sha256::new();
            hasher.update(WARP_DIGEST_CHAIN_DOMAIN);
            hasher.update(domain);
            hasher.finalize().into()
        } else {
            persisted
        };
        Self { domain, state }
    }

    pub(super) fn push(&mut self, label: &[u8], digest: [u8; 32]) -> Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(WARP_DIGEST_CHAIN_DOMAIN);
        hasher.update(self.domain);
        hash_bytes(&mut hasher, label)?;
        hasher.update(self.state);
        hasher.update(digest);
        self.state = hasher.finalize().into();
        Ok(())
    }

    pub(super) fn state(&self) -> [u8; 32] {
        self.state
    }
}

pub(super) fn new_core_hasher(persisted: [u8; 32]) -> WarpNativeDigestChain {
    WarpNativeDigestChain::new(WARP_CORE_DIGEST_DOMAIN, persisted)
}

pub(super) fn finish_core_hasher(hasher: &WarpNativeDigestChain) -> String {
    hex_digest(hasher.state())
}

fn core_unit_digest(unit: &WarpNativeCoreUnit) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_CORE_UNIT_DIGEST_DOMAIN);
    for session in &unit.sessions {
        hash_session(&mut hasher, session)?;
    }
    for edge in &unit.hierarchy_edges {
        hash_edge(&mut hasher, edge)?;
    }
    for event in &unit.events {
        hash_event(&mut hasher, event)?;
    }
    for rejection in &unit.rejections {
        hash_rejection(&mut hasher, rejection)?;
    }
    Ok(hasher.finalize().into())
}

fn page_identity(page: &WarpNativePage) -> Result<WarpNativePageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier)?;
    hash_frontier(&mut hasher, &page.next_safe_frontier)?;
    hash_usize(
        &mut hasher,
        page.logical_units,
        "Warp NativePath page logical-unit count exceeds u64",
    )?;
    for session in &page.sessions {
        hash_session(&mut hasher, session)?;
    }
    for edge in &page.hierarchy_edges {
        hash_edge(&mut hasher, edge)?;
    }
    for event in &page.events {
        hash_event(&mut hasher, event)?;
    }
    for rejection in &page.rejections {
        hash_rejection(&mut hasher, rejection)?;
    }
    Ok(WarpNativePageIdentity(hasher.finalize().into()))
}

fn pro_page_identity(page: &WarpNativeProOutputPage) -> Result<WarpNativeProOutputPageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_PRO_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier)?;
    hash_frontier(&mut hasher, &page.next_safe_frontier)?;
    hash_usize(
        &mut hasher,
        page.logical_units,
        "Warp NativePath Pro page logical-unit count exceeds u64",
    )?;
    for output in &page.outputs {
        hash_pro_output(&mut hasher, output)?;
    }
    for rejection in &page.rejections {
        hash_output_rejection(&mut hasher, rejection)?;
    }
    Ok(WarpNativeProOutputPageIdentity(hasher.finalize().into()))
}

fn hash_pro_output(hasher: &mut Sha256, output: &ProOutputObservation) -> Result<()> {
    hasher.update(b"output\0");
    hasher.update([match output.kind {
        crate::OutputObservationKind::Command => 1,
        crate::OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_le_bytes());
    hash_optional_text(hasher, output.coordinate.native_record_id.as_deref())?;
    hash_optional_u64(hasher, output.coordinate.source_record_ordinal);
    hash_optional_u32(hasher, output.coordinate.source_record_subrecord_index);
    hash_optional_u64(hasher, output.coordinate.byte_start);
    hash_optional_u64(hasher, output.coordinate.byte_end_exclusive);
    hash_optional_i64(hasher, output.occurred_at_unix_ms);

    let associations = &output.associations;
    hash_text(hasher, &associations.direct_session_id)?;
    hash_text(hasher, &associations.root_session_id)?;
    hash_optional_text(hasher, associations.parent_session_id.as_deref())?;
    hash_optional_text(hasher, associations.provider_session_id.as_deref())?;
    hash_optional_text(hasher, associations.agent_id.as_deref())?;
    hasher.update([u8::from(associations.repository.is_some())]);
    if let Some(repository) = &associations.repository {
        hash_text(hasher, &repository.repository_id)?;
        hash_optional_text(hasher, repository.checkout_id.as_deref())?;
        hash_optional_text(hasher, repository.worktree_id.as_deref())?;
        hash_optional_text(hasher, repository.object_format.as_deref())?;
    }

    hash_optional_text(hasher, output.call_id.as_deref())?;
    hasher.update([u8::from(output.command.is_some())]);
    if let Some(command) = &output.command {
        hash_text(hasher, &command.tool_name)?;
        hash_text(hasher, &command.command)?;
        hash_optional_text(hasher, command.working_directory.as_deref())?;
    }
    hash_optional_outcome(hasher, Some(output.outcome.outcome));
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_le_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hash_bytes(hasher, &output.content)
}

fn hash_output_rejection(hasher: &mut Sha256, rejection: &WarpNativeOutputRejection) -> Result<()> {
    hasher.update(b"output-rejection\0");
    hasher.update([match rejection.kind {
        WarpNativeOutputRejectionKind::Malformed => 1,
        WarpNativeOutputRejectionKind::Oversized => 2,
    }]);
    hash_text(hasher, &rejection.native_key)?;
    hash_text(hasher, &rejection.reason)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value.chars().take(limit).collect()
}

fn retained_event_hash(
    identity: &WarpNativeEventIdentity,
    body: &str,
    result_outcome: Option<OutputOutcome>,
    call_id: Option<&str>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(WARP_EVENT_HASH_DOMAIN);
    hash_text(&mut hasher, &identity.conversation_id)?;
    hash_text(&mut hasher, &identity.task_id)?;
    match &identity.message {
        WarpNativeMessageIdentity::ProviderId(value) => {
            hasher.update([1]);
            hash_text(&mut hasher, value)?;
        }
        WarpNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hash_text(&mut hasher, body)?;
    hash_optional_outcome(&mut hasher, result_outcome);
    hash_optional_text(&mut hasher, call_id)?;
    Ok(hex_digest(hasher.finalize().into()))
}

fn session_estimated_bytes(session: &WarpNativeSession) -> Result<usize> {
    let metadata_bytes = serde_json::to_vec(&session.metadata)?.len();
    let text = session
        .conversation_id
        .len()
        .saturating_add(
            session
                .parent_conversation_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(session.root_conversation_id.len())
        .saturating_add(session.title.len())
        .saturating_add(metadata_bytes);
    Ok(conservative_text_bytes(text, 384))
}

fn event_estimated_bytes(event: &WarpNativeEvent) -> usize {
    let text = event
        .identity
        .conversation_id
        .len()
        .saturating_add(event.identity.task_id.len())
        .saturating_add(match &event.identity.message {
            WarpNativeMessageIdentity::ProviderId(value) => value.len(),
            WarpNativeMessageIdentity::MessageOrdinal(_) => 4,
        })
        .saturating_add(event.native_order.task_key.len())
        .saturating_add(event.request_id.as_ref().map_or(0, String::len))
        .saturating_add(event.call_id.as_ref().map_or(0, String::len))
        .saturating_add(event.body.len())
        .saturating_add(event.content_hash.len())
        .saturating_add(event.preview.len())
        .saturating_add(event.kind.len());
    conservative_text_bytes(text, 512)
}

fn output_estimated_bytes(output: &ProOutputObservation) -> usize {
    let associations = &output.associations;
    let repository_bytes = associations.repository.as_ref().map_or(0, |repository| {
        repository
            .repository_id
            .len()
            .saturating_add(repository.checkout_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.worktree_id.as_ref().map_or(0, String::len))
            .saturating_add(repository.object_format.as_ref().map_or(0, String::len))
    });
    let command_bytes = output.command.as_ref().map_or(0, |command| {
        command
            .tool_name
            .len()
            .saturating_add(command.command.len())
            .saturating_add(command.working_directory.as_ref().map_or(0, String::len))
    });
    let text = output
        .coordinate
        .unit_key
        .len()
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.direct_session_id.len())
        .saturating_add(associations.root_session_id.len())
        .saturating_add(
            associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(
            associations
                .provider_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(associations.agent_id.as_ref().map_or(0, String::len))
        .saturating_add(repository_bytes)
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(command_bytes)
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len());
    conservative_text_bytes(text, 1_024).saturating_add(output.content.len())
}

fn conservative_text_bytes(text_bytes: usize, overhead: usize) -> usize {
    text_bytes.saturating_mul(6).saturating_add(overhead)
}

fn checked_add(total: usize, value: usize, message: &'static str) -> Result<usize> {
    total
        .checked_add(value)
        .ok_or(CaptureError::SystemInvariant(message))
}

fn hash_frontier(hasher: &mut Sha256, frontier: &WarpNativeFrontier) -> Result<()> {
    hasher.update([match frontier.phase {
        WarpNativeFrontierPhase::Start => 0,
        WarpNativeFrontierPhase::Conversations => 1,
        WarpNativeFrontierPhase::Tasks => 2,
    }]);
    hasher.update(frontier.completed_conversation_rows.to_le_bytes());
    hasher.update(frontier.completed_hierarchy_edges.to_le_bytes());
    hash_optional_i64(hasher, frontier.last_conversation_rowid);
    hasher.update(frontier.completed_task_rows.to_le_bytes());
    hash_optional_i64(hasher, frontier.last_task_rowid);
    hasher.update(frontier.next_message_ordinal.to_le_bytes());
    hasher.update(frontier.retained_events.to_le_bytes());
    hasher.update(frontier.source_digest);
    hasher.update(frontier.core_digest);
    Ok(())
}

fn hash_session(hasher: &mut Sha256, session: &WarpNativeSession) -> Result<()> {
    hasher.update(b"session\0");
    hash_text(hasher, &session.conversation_id)?;
    hash_optional_text(hasher, session.parent_conversation_id.as_deref())?;
    hash_text(hasher, &session.root_conversation_id)?;
    hasher.update([u8::from(session.parent_present)]);
    hash_text(hasher, &session.title)?;
    hash_optional_i64(
        hasher,
        session.modified_at.map(|value| value.timestamp_millis()),
    );
    hash_bytes(hasher, &serde_json::to_vec(&session.metadata)?)?;
    Ok(())
}

fn hash_edge(hasher: &mut Sha256, edge: &WarpNativeHierarchyEdge) -> Result<()> {
    hasher.update(b"edge\0");
    hash_text(hasher, &edge.child_conversation_id)?;
    hash_text(hasher, &edge.parent_conversation_id)?;
    hasher.update([u8::from(edge.parent_present)]);
    Ok(())
}

fn hash_event(hasher: &mut Sha256, event: &WarpNativeEvent) -> Result<()> {
    hasher.update(b"event\0");
    hash_text(hasher, &event.identity.conversation_id)?;
    hash_text(hasher, &event.identity.task_id)?;
    match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(value) => {
            hasher.update([1]);
            hash_text(hasher, value)?;
        }
        WarpNativeMessageIdentity::MessageOrdinal(value) => {
            hasher.update([2]);
            hasher.update(value.to_le_bytes());
        }
    }
    hasher.update(event.native_order.provider_event_index.to_le_bytes());
    hasher.update(event.native_order.task_rowid.to_le_bytes());
    hash_text(hasher, &event.native_order.task_key)?;
    hasher.update(event.native_order.message_ordinal.to_le_bytes());
    hash_text(hasher, event.event_type.as_str())?;
    hash_optional_text(hasher, event.role.map(EventRole::as_str))?;
    hash_text(hasher, event.kind)?;
    hash_optional_text(hasher, event.request_id.as_deref())?;
    hash_optional_outcome(hasher, event.result_outcome);
    hash_optional_text(hasher, event.call_id.as_deref())?;
    hash_optional_i64(
        hasher,
        event.occurred_at.map(|value| value.timestamp_millis()),
    );
    hash_text(hasher, &event.body)?;
    hash_text(hasher, &event.content_hash)?;
    hash_text(hasher, event.source_record_digest.as_str())?;
    if let Some(content_ref) = &event.complete_content_ref {
        hasher.update([1]);
        hash_text(hasher, content_ref.sha256())?;
        hasher.update(content_ref.byte_len().to_le_bytes());
    } else {
        hasher.update([0]);
    }
    Ok(())
}

fn hash_rejection(hasher: &mut Sha256, rejection: &WarpNativeRejection) -> Result<()> {
    hasher.update(b"rejection\0");
    hasher.update([match rejection.kind {
        WarpNativeRejectionKind::ConversationRecord => 1,
        WarpNativeRejectionKind::TaskRecord => 2,
        WarpNativeRejectionKind::MalformedProtobuf => 3,
        WarpNativeRejectionKind::MissingConversation => 4,
        WarpNativeRejectionKind::OversizedTask => 5,
        WarpNativeRejectionKind::OversizedNormalizedUnit => 6,
        WarpNativeRejectionKind::DuplicateMessageIdentity => 7,
    }]);
    hash_text(hasher, &rejection.native_key)?;
    hash_text(hasher, &rejection.reason)?;
    Ok(())
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) -> Result<()> {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_text(hasher, value)?;
    }
    Ok(())
}

fn hash_text(hasher: &mut Sha256, value: &str) -> Result<()> {
    hash_bytes(hasher, value.as_bytes())
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    hash_usize(
        hasher,
        value.len(),
        "Warp NativePath digest field length exceeds u64",
    )?;
    hasher.update(value);
    Ok(())
}

fn hash_usize(hasher: &mut Sha256, value: usize, message: &'static str) -> Result<()> {
    let value = u64::try_from(value).map_err(|_| CaptureError::SystemInvariant(message))?;
    hasher.update(value.to_le_bytes());
    Ok(())
}

fn hash_optional_outcome(hasher: &mut Sha256, outcome: Option<OutputOutcome>) {
    hasher.update([match outcome {
        None => 0,
        Some(OutputOutcome::Success) => 1,
        Some(OutputOutcome::Failure) => 2,
        Some(OutputOutcome::Timeout) => 3,
        Some(OutputOutcome::Unknown) => 4,
    }]);
}

fn bound_rejection_text(native_key: &mut String, reason: &mut String) {
    *native_key = truncate_chars(native_key, WARP_NATIVE_REJECTION_KEY_MAX_CHARS);
    *reason = truncate_chars(reason, WARP_NATIVE_REJECTION_REASON_MAX_CHARS);
}

pub(in super::super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
