mod digest;

use digest::{
    bound_rejection_text, checked_add, conservative_text_bytes, event_estimated_bytes, hash_bytes,
    page_identity, session_estimated_bytes,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{record_evidence::RecordDigest, CaptureError, OutputOutcome, Result};

use super::decode::WarpMcpToolInvocation;

pub(in super::super) const WARP_NATIVE_PAGE_MAX_ROWS: usize = 64;
pub(in super::super) const WARP_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const WARP_NATIVE_REJECTION_KEY_MAX_CHARS: usize = 512;
const WARP_NATIVE_REJECTION_REASON_MAX_CHARS: usize = 1_024;
const WARP_NATIVE_REJECTION_RESERVE_BYTES: usize = 40 * 1024;
pub(super) const WARP_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-warp-source-integrity-v4\0";
const WARP_DIGEST_CHAIN_DOMAIN: &[u8] = b"ctx-warp-native-digest-chain-v1\0";
const WARP_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx-warp-native-safe-page-v2\0";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(in super::super) struct WarpNativePageIdentity(pub(in super::super) [u8; 32]);

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
    pub(in super::super) legacy_provider_event_index: Option<u64>,
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
    pub(in super::super) mcp_invocation: Option<WarpMcpToolInvocation>,
    pub(in super::super) mcp_attribution: bool,
    pub(in super::super) occurred_at: Option<DateTime<Utc>>,
    pub(in super::super) lexical_body: String,
    pub(in super::super) source_record_digest: RecordDigest,
}

pub(in super::super) struct WarpNativeEventDraft {
    pub(in super::super) provider_event_index: u64,
    pub(in super::super) legacy_provider_event_index: Option<u64>,
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
    pub(in super::super) mcp_invocation: Option<WarpMcpToolInvocation>,
    pub(in super::super) mcp_attribution: bool,
    pub(in super::super) occurred_at: Option<DateTime<Utc>>,
    pub(in super::super) body: String,
    pub(in super::super) source_record_digest: RecordDigest,
}

impl WarpNativeEvent {
    pub(in super::super) fn from_draft(draft: WarpNativeEventDraft) -> Result<Self> {
        let message = draft.message_id.map_or(
            WarpNativeMessageIdentity::MessageOrdinal(draft.message_ordinal),
            WarpNativeMessageIdentity::ProviderId,
        );
        let identity = WarpNativeEventIdentity {
            conversation_id: draft.conversation_id,
            task_id: draft.task_id.clone(),
            message,
        };
        Ok(Self {
            identity,
            native_order: WarpNativeOrder {
                provider_event_index: draft.provider_event_index,
                legacy_provider_event_index: draft.legacy_provider_event_index,
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
            mcp_invocation: draft.mcp_invocation,
            mcp_attribution: draft.mcp_attribution,
            occurred_at: draft.occurred_at,
            lexical_body: draft.body,
            source_record_digest: draft.source_record_digest,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) enum WarpNativeRejectionKind {
    ConversationRecord,
    TaskRecord,
    OversizedTask,
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

#[derive(Debug)]
pub(in super::super) struct WarpNativePage {
    pub(in super::super) identity: WarpNativePageIdentity,
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
}

pub(in super::super) trait WarpNativeSink {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()>;
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
    pub(in super::super) malformed_output_records: u64,
    pub(in super::super) duplicate_message_identity_tasks: u64,
    pub(in super::super) peak_task_identity_entries: u64,
    pub(in super::super) protobuf_bytes_scanned: u64,
    pub(in super::super) retained_events: u64,
    pub(in super::super) ignored_messages: u64,
    pub(in super::super) retained_body_bytes: u64,
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
    pub(in super::super) result_events_created: u64,
}

pub(super) struct WarpNativeUnit {
    core: WarpNativeCoreUnit,
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

    pub(super) fn retained_event_count(&self) -> usize {
        self.events.len()
    }
}

impl WarpNativeUnit {
    pub(super) fn progress() -> Self {
        Self {
            core: WarpNativeCoreUnit {
                sessions: Vec::new(),
                hierarchy_edges: Vec::new(),
                events: Vec::new(),
                rejections: Vec::new(),
                estimated_bytes: WARP_NATIVE_REJECTION_RESERVE_BYTES,
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

    #[cfg(test)]
    pub(super) fn estimated_bytes(&self) -> usize {
        self.core.estimated_bytes
    }

    pub(super) fn into_core(self) -> WarpNativeCoreUnit {
        self.core
    }
}

pub(super) struct WarpNativePageAccumulator {
    page: WarpNativePage,
}

impl WarpNativePageAccumulator {
    pub(super) fn new() -> Self {
        Self {
            page: WarpNativePage {
                identity: WarpNativePageIdentity::default(),
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
                .is_some_and(|bytes| {
                    self.page.logical_units == 0 || bytes <= WARP_NATIVE_PAGE_MAX_BYTES
                })
    }

    pub(super) fn push(&mut self, unit: WarpNativeCoreUnit) -> Result<()> {
        if !self.can_accept(&unit) {
            return Err(CaptureError::SystemInvariant(
                "Warp NativePath unit was pushed past a page bound",
            ));
        }
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
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Option<WarpNativePage>> {
        if self.page.logical_units == 0 {
            return Ok(None);
        }
        self.page.identity = page_identity(&self.page)?;
        Ok(Some(self.page))
    }
}

pub(super) struct WarpNativeDigestChain {
    domain: &'static [u8],
    state: [u8; 32],
}

impl WarpNativeDigestChain {
    pub(super) fn new(domain: &'static [u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(WARP_DIGEST_CHAIN_DOMAIN);
        hasher.update(domain);
        let state = hasher.finalize().into();
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

pub(in super::super) fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indivisible_event_larger_than_page_target_is_accepted() {
        let body = format!(
            "warp-page-head-{}-warp-page-tail",
            "x".repeat(8 * 1024 * 1024)
        );
        let mut unit = WarpNativeUnit::progress();
        unit.push_event(WarpNativeEvent {
            identity: WarpNativeEventIdentity {
                conversation_id: "conversation".to_owned(),
                task_id: "task".to_owned(),
                message: WarpNativeMessageIdentity::MessageOrdinal(0),
            },
            native_order: WarpNativeOrder {
                provider_event_index: 0,
                legacy_provider_event_index: Some(0),
                task_rowid: 1,
                task_key: "task".to_owned(),
                message_ordinal: 0,
            },
            event_type: EventType::ToolOutput,
            role: Some(EventRole::Tool),
            kind: "run_shell_command",
            request_id: None,
            result_outcome: Some(OutputOutcome::Success),
            call_id: Some("call".to_owned()),
            mcp_invocation: None,
            mcp_attribution: false,
            occurred_at: None,
            lexical_body: body.clone(),
            source_record_digest: RecordDigest::from_text("warp large result"),
        })
        .unwrap();
        assert!(unit.estimated_bytes() > WARP_NATIVE_PAGE_MAX_BYTES);
        let mut page = WarpNativePageAccumulator::new();
        assert!(page.can_accept(&unit.core));
        page.push(unit.into_core()).unwrap();
        let page = page.finish().unwrap().unwrap();
        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].lexical_body, body);
        assert!(page.estimated_bytes > WARP_NATIVE_PAGE_MAX_BYTES);
    }
}
