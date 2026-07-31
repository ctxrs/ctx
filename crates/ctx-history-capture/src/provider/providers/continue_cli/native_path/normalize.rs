use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ctx_history_core::{Confidence, FileChangeKind};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

use super::{
    parse::{
        ContinueOutputExclusionStats, ContinueSourceFailure, ContinueSourceFailureKind,
        RawContinueDocument, RawContinueHistoryItem, RawContinueToolCallState, RawContinueUsage,
        RawTimestamp,
    },
    source::{
        ContinueIndexMetadata, ContinueIndexObservation, ContinueIndexSnapshot,
        ContinueSourceObservation, ContinueSourceSnapshot,
    },
};

const SESSION_METADATA_HASH_DOMAIN: &[u8] = b"ctx-continue-nativepath-session-metadata-v2\0";

pub(crate) const CONTINUE_NATIVE_MAX_RETAINED_ITEM_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;
// The shared NativePath page contract admits at most 64 provider units.
// Reserve four units for the page's source/session/route/cursor mechanics so
// the family consumer can publish a page without exceeding either bound.
pub(crate) const CONTINUE_NATIVE_MAX_PAGE_ROWS: usize = 60;
pub(crate) const CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT: usize =
    CONTINUE_NATIVE_MAX_PAGE_ROWS - 1;
pub(crate) const CONTINUE_NATIVE_MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ContinueSessionIdentity(pub(crate) String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ContinueEventIdentity {
    pub(crate) session: ContinueSessionIdentity,
    pub(crate) history_ordinal: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueEventKind {
    Message,
    ToolCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueEventRole {
    User,
    Assistant,
    System,
    Tool,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueCallRelationship {
    pub(crate) state_ordinal: u32,
    pub(crate) call_id: Option<String>,
    pub(crate) nested_call_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ContinueFileTouch {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
    pub(crate) confidence: Confidence,
    pub(crate) metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueGenerationAuthority {
    pub(crate) observed_history_items: Option<usize>,
    pub(crate) retained_events: usize,
    pub(crate) rejected_items: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContinueSessionRow {
    pub(crate) identity: ContinueSessionIdentity,
    pub(crate) title: Option<String>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) workspace_directory: Option<String>,
    pub(crate) mode: Option<String>,
    pub(crate) chat_model_title: Option<String>,
    pub(crate) usage: Option<RawContinueUsage>,
    pub(crate) index_metadata: Option<ContinueIndexMetadata>,
    pub(crate) metadata_json: String,
    pub(crate) metadata_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueEventRow {
    pub(crate) identity: ContinueEventIdentity,
    pub(crate) native_item_id: Option<String>,
    /// SHA-256 of the exact provider-owned `history` array element.
    ///
    /// This is locator integrity evidence only. It is not part of event
    /// identity and must never be used as a Core output-content hash.
    pub(crate) source_record_digest: [u8; 32],
    pub(crate) kind: ContinueEventKind,
    pub(crate) role: ContinueEventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) search_text: String,
    pub(crate) calls: Box<[ContinueCallRelationship]>,
    pub(crate) file_touches: Box<[ContinueFileTouch]>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ContinuePreparedSource {
    pub(crate) observation: ContinueSourceObservation,
    pub(crate) index_dependency: ContinueIndexObservation,
    pub(crate) session: ContinueSessionRow,
}

impl ContinuePreparedSource {
    pub(super) fn estimated_bytes(&self) -> usize {
        const FIXED_SOURCE_AND_SESSION_BYTES: usize = 1_024;
        let bytes = FIXED_SOURCE_AND_SESSION_BYTES
            .saturating_add(path_budget(self.observation.requested_path()))
            .saturating_add(path_budget(self.observation.canonical_path()))
            .saturating_add(self.observation.session_revision().len())
            .saturating_add(path_budget(self.index_dependency.path()))
            .saturating_add(self.index_dependency.dependency_revision().len())
            .saturating_add(self.session.identity.0.len())
            .saturating_add(option_string_bytes(&self.session.title))
            .saturating_add(option_string_bytes(&self.session.workspace_directory))
            .saturating_add(option_string_bytes(&self.session.mode))
            .saturating_add(option_string_bytes(&self.session.chat_model_title))
            .saturating_add(self.session.metadata_json.len())
            .saturating_add(self.session.metadata_hash.len());
        self.session.index_metadata.as_ref().map_or(bytes, |index| {
            bytes
                .saturating_add(option_string_bytes(&index.title))
                .saturating_add(option_string_bytes(&index.date_created))
                .saturating_add(option_string_bytes(&index.workspace_directory))
                .saturating_add(16)
        })
    }
}

#[derive(Debug)]
pub(crate) struct ContinuePreparedPage {
    pub(crate) source: Option<Box<ContinuePreparedSource>>,
    pub(crate) session_identity: ContinueSessionIdentity,
    pub(crate) events: Box<[ContinueEventRow]>,
    pub(crate) terminal: bool,
    pub(crate) authority: Option<ContinueGenerationAuthority>,
    pub(crate) output_exclusion: Option<ContinueOutputExclusionStats>,
    pub(crate) row_count: usize,
    pub(crate) estimated_bytes: usize,
}

impl ContinuePreparedPage {
    pub(super) fn base_estimated_bytes(session: &ContinueSessionIdentity) -> usize {
        const FIXED_PAGE_BYTES: usize = 512;
        FIXED_PAGE_BYTES.saturating_add(session.0.len())
    }
}

pub(super) fn normalize_continue_document(
    snapshot: &ContinueSourceSnapshot,
    index: &ContinueIndexSnapshot,
    document: RawContinueDocument,
    session_id: String,
) -> Result<ContinuePreparedSource, ContinueSourceFailure> {
    let identity = ContinueSessionIdentity(session_id);
    let indexed_metadata = index.metadata(&identity.0).cloned();
    let session = normalize_session(&document, indexed_metadata).map_err(|message| {
        ContinueSourceFailure::from_snapshot(
            snapshot,
            ContinueSourceFailureKind::Normalization,
            message,
        )
    })?;
    Ok(ContinuePreparedSource {
        observation: snapshot.observation().clone(),
        index_dependency: index.observation().clone(),
        session: ContinueSessionRow {
            identity,
            title: session.title,
            started_at: session.started_at,
            workspace_directory: session.workspace_directory,
            mode: session.mode,
            chat_model_title: session.chat_model_title,
            usage: session.usage,
            index_metadata: session.index_metadata,
            metadata_json: session.metadata_json,
            metadata_hash: session.metadata_hash,
        },
    })
}

struct NormalizedSession {
    title: Option<String>,
    started_at: Option<DateTime<Utc>>,
    workspace_directory: Option<String>,
    mode: Option<String>,
    chat_model_title: Option<String>,
    usage: Option<RawContinueUsage>,
    index_metadata: Option<ContinueIndexMetadata>,
    metadata_json: String,
    metadata_hash: String,
}

#[derive(Serialize)]
struct SessionMetadataBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    created_at: Option<SerializableTimestamp<'a>>,
    #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
    started_at: Option<SerializableTimestamp<'a>>,
    #[serde(rename = "workspaceDirectory", skip_serializing_if = "Option::is_none")]
    workspace_directory: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    #[serde(rename = "chatModelTitle", skip_serializing_if = "Option::is_none")]
    chat_model_title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<&'a RawContinueUsage>,
    #[serde(rename = "sessionIndex", skip_serializing_if = "Option::is_none")]
    session_index: Option<&'a ContinueIndexMetadata>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum SerializableTimestamp<'a> {
    Text(&'a str),
    Number(f64),
}

impl<'a> From<&'a RawTimestamp> for SerializableTimestamp<'a> {
    fn from(value: &'a RawTimestamp) -> Self {
        match value {
            RawTimestamp::Text(value) => Self::Text(value),
            RawTimestamp::Number(value) => Self::Number(*value),
        }
    }
}

fn normalize_session(
    document: &RawContinueDocument,
    index_metadata: Option<ContinueIndexMetadata>,
) -> Result<NormalizedSession, String> {
    let metadata = SessionMetadataBody {
        title: document.title.as_deref(),
        created_at: document.created_at.as_ref().map(Into::into),
        started_at: document.started_at.as_ref().map(Into::into),
        workspace_directory: document.workspace_directory.as_deref(),
        mode: document.mode.as_deref(),
        chat_model_title: document.chat_model_title.as_deref(),
        usage: document.usage.as_ref(),
        session_index: index_metadata.as_ref(),
    };
    let metadata_bytes = serde_json::to_vec(&metadata)
        .map_err(|error| format!("failed to serialize Continue session metadata: {error}"))?;
    let metadata_hash = sha256_hex(SESSION_METADATA_HASH_DOMAIN, &metadata_bytes);
    let metadata_json = String::from_utf8(metadata_bytes)
        .map_err(|error| format!("Continue session metadata is not UTF-8: {error}"))?;
    let indexed_started_at = index_metadata
        .as_ref()
        .and_then(|metadata| metadata.date_created.as_deref())
        .and_then(parse_text_timestamp);
    Ok(NormalizedSession {
        title: document.title.clone().or_else(|| {
            index_metadata
                .as_ref()
                .and_then(|value| value.title.clone())
        }),
        started_at: document
            .created_at
            .as_ref()
            .or(document.started_at.as_ref())
            .and_then(parse_timestamp)
            .or(indexed_started_at),
        workspace_directory: document.workspace_directory.clone().or_else(|| {
            index_metadata
                .as_ref()
                .and_then(|value| value.workspace_directory.clone())
        }),
        mode: document.mode.clone(),
        chat_model_title: document.chat_model_title.clone(),
        usage: document.usage.clone(),
        index_metadata,
        metadata_hash,
        metadata_json,
    })
}

pub(super) enum NormalizeEventError {
    RetainedItemTooLarge { observed: usize },
    FileTouchLimitExceeded,
}

pub(super) fn normalize_event(
    session: &ContinueSessionIdentity,
    history_ordinal: u64,
    item: &RawContinueHistoryItem,
    source_record_digest: [u8; 32],
) -> Result<ContinueEventRow, NormalizeEventError> {
    let search_text = continue_retained_text(item);
    if search_text.len() > CONTINUE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return Err(NormalizeEventError::RetainedItemTooLarge {
            observed: search_text.len(),
        });
    }
    let file_touches = event_file_touches(item)?;
    Ok(ContinueEventRow {
        identity: ContinueEventIdentity {
            session: session.clone(),
            history_ordinal,
        },
        native_item_id: item.id.clone(),
        source_record_digest,
        kind: if item.tool_call_states.is_empty()
            && item
                .message
                .as_ref()
                .is_none_or(|message| message.calls.is_empty())
        {
            ContinueEventKind::Message
        } else {
            ContinueEventKind::ToolCall
        },
        role: continue_role(
            item.message
                .as_ref()
                .and_then(|message| message.role.as_deref()),
        ),
        occurred_at: item
            .timestamp
            .as_ref()
            .or(item.created_at.as_ref())
            .or_else(|| {
                item.message
                    .as_ref()
                    .and_then(|message| message.timestamp.as_ref())
            })
            .and_then(parse_timestamp),
        search_text,
        calls: event_call_relationships(item),
        file_touches,
    })
}

impl ContinueEventRow {
    pub(super) fn logical_units(&self) -> usize {
        1_usize.saturating_add(self.file_touches.len())
    }

    pub(super) fn estimated_bytes(&self) -> usize {
        const FIXED_EVENT_BYTES: usize = 512;
        let event_bytes = self.calls.iter().fold(
            FIXED_EVENT_BYTES
                .saturating_add(self.identity.session.0.len())
                .saturating_add(option_string_bytes(&self.native_item_id))
                .saturating_add(self.search_text.len()),
            |bytes, call| {
                bytes
                    .saturating_add(64)
                    .saturating_add(option_string_bytes(&call.call_id))
                    .saturating_add(option_string_bytes(&call.nested_call_id))
                    .saturating_add(option_string_bytes(&call.tool_name))
                    .saturating_add(option_string_bytes(&call.status))
            },
        );
        self.file_touches.iter().fold(event_bytes, |bytes, touch| {
            bytes
                .saturating_add(128)
                .saturating_add(touch.path.len())
                .saturating_add(option_string_bytes(&touch.old_path))
                .saturating_add(touch.metadata.to_string().len())
        })
    }
}

fn event_file_touches(
    item: &RawContinueHistoryItem,
) -> Result<Box<[ContinueFileTouch]>, NormalizeEventError> {
    let candidates = item
        .message
        .iter()
        .flat_map(|message| message.calls.iter())
        .flat_map(|call| call.file_touches.iter())
        .chain(
            item.tool_call_states
                .iter()
                .filter_map(|state| state.tool_call.as_ref())
                .flat_map(|call| call.file_touches.iter()),
        );
    let mut seen = BTreeSet::new();
    let mut touches = Vec::new();
    for touch in candidates {
        let key = (
            touch.path.clone(),
            touch.old_path.clone(),
            touch.change_kind.map(|kind| kind.as_str().to_owned()),
        );
        if !seen.insert(key) {
            continue;
        }
        if touches.len() == CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT {
            return Err(NormalizeEventError::FileTouchLimitExceeded);
        }
        touches.push(touch.clone());
    }
    Ok(touches.into_boxed_slice())
}

fn call_relationship(ordinal: usize, state: &RawContinueToolCallState) -> ContinueCallRelationship {
    ContinueCallRelationship {
        state_ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
        call_id: state.tool_call_id.clone(),
        nested_call_id: state.tool_call.as_ref().and_then(|call| call.id.clone()),
        tool_name: state
            .tool_call
            .as_ref()
            .and_then(|call| call.function_name.clone().or_else(|| call.name.clone())),
        status: state.status.clone(),
    }
}

fn event_call_relationships(item: &RawContinueHistoryItem) -> Box<[ContinueCallRelationship]> {
    let mut calls = item
        .message
        .as_ref()
        .into_iter()
        .flat_map(|message| message.calls.iter())
        .enumerate()
        .map(|(ordinal, call)| ContinueCallRelationship {
            state_ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
            call_id: call.id.clone(),
            nested_call_id: None,
            tool_name: call.name.clone(),
            status: None,
        })
        .collect::<Vec<_>>();
    let message_call_count = calls.len();
    calls.extend(
        item.tool_call_states
            .iter()
            .enumerate()
            .map(|(ordinal, state)| {
                call_relationship(ordinal.saturating_add(message_call_count), state)
            }),
    );
    calls.into_boxed_slice()
}

fn continue_retained_text(item: &RawContinueHistoryItem) -> String {
    let mut parts = Vec::new();
    if let Some(message) = item.message.as_ref() {
        parts.extend(
            message
                .content
                .iter()
                .filter(|value| !value.trim().is_empty())
                .cloned(),
        );
        for call in &message.calls {
            parts.push(format!(
                "tool: {} | status: call",
                call.name.as_deref().unwrap_or("tool")
            ));
        }
    }
    if let Some(editor_state) = item
        .editor_text
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(editor_state.clone());
    }
    for context in &item.context_items {
        if let Some(content) = context
            .content
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(content.clone());
        } else if let Some(name) = context
            .name
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(name.clone());
        }
    }
    for state in &item.tool_call_states {
        let tool_name = state
            .tool_call
            .as_ref()
            .and_then(|call| call.function_name.as_deref().or(call.name.as_deref()))
            .unwrap_or("tool");
        let status = state.status.as_deref().unwrap_or("unknown");
        parts.push(format!("tool: {tool_name} | status: {status}"));
    }
    if let Some(summary) = item
        .conversation_summary
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(summary.clone());
    }
    parts.join("\n")
}

fn continue_role(role: Option<&str>) -> ContinueEventRole {
    match role {
        Some("user") => ContinueEventRole::User,
        Some("assistant") => ContinueEventRole::Assistant,
        Some("system" | "developer") => ContinueEventRole::System,
        Some("tool" | "toolResult" | "bashExecution") => ContinueEventRole::Tool,
        _ => ContinueEventRole::Unknown,
    }
}

fn parse_timestamp(value: &RawTimestamp) -> Option<DateTime<Utc>> {
    match value {
        RawTimestamp::Text(raw) => parse_text_timestamp(raw),
        RawTimestamp::Number(value) => numeric_timestamp(*value),
    }
}

fn parse_text_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.with_timezone(&Utc))
        .or_else(|| raw.parse::<f64>().ok().and_then(numeric_timestamp))
}

fn numeric_timestamp(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value.abs() > 1_000_000_000_000.0 {
        value.round()
    } else {
        (value * 1_000.0).round()
    };
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
}

fn option_string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::len)
}

fn path_budget(path: &std::path::Path) -> usize {
    path.as_os_str().to_string_lossy().len().saturating_mul(4)
}

fn sha256_hex(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
