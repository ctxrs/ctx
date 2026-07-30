use std::{ops::Range, path::PathBuf};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    provider::file_touches::visit_provider_file_touch_drafts_with_limit,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    decode::{
        decode_bool, decode_f64, decode_i64, decode_string, decode_unbounded_string,
        validate_and_root, JsonArrayCursor, JsonKind, JsonSpan,
    },
    normalize::{
        normalize_continue_document, normalize_event, ContinueEventRow, ContinueFileTouch,
        ContinueGenerationAuthority, ContinuePreparedPage, ContinuePreparedSource,
        ContinueSessionIdentity, NormalizeEventError, CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT,
        CONTINUE_NATIVE_MAX_PAGE_BYTES, CONTINUE_NATIVE_MAX_PAGE_ROWS,
    },
    source::{ContinueIndexSnapshot, ContinueSourceObservation, ContinueSourceSnapshot},
};

mod document;
mod event;

use document::*;
use event::scan_error;

const MAX_CONTINUE_SESSION_ID_BYTES: usize = 512;
const MAX_NATIVE_ITEM_ID_BYTES: usize = 384;
const MAX_CALL_ID_BYTES: usize = 384;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_STATUS_BYTES: usize = 64;
const MAX_SESSION_METADATA_STRING_BYTES: usize = MAX_PROVIDER_JSONL_LINE_BYTES;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ContinueOutputExclusionStats {
    pub(crate) native_results_observed: usize,
    pub(crate) unproven_payloads_skipped: usize,
    pub(crate) result_payload_bytes_skipped: u64,
    pub(crate) call_body_bytes_skipped: u64,
    pub(crate) retained_decode_string_allocations: usize,
    pub(crate) retained_decode_string_bytes: u64,
}

#[derive(Debug)]
pub(crate) enum ContinueParseOutcome {
    Complete(Box<ContinueSourcePageStream>),
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueSourceFailureKind {
    MalformedDocument,
    MissingSessionId,
    InvalidSessionId,
    RetainedItemTooLarge,
    Normalization,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueSourceFailure {
    pub(crate) path: PathBuf,
    pub(crate) observation: Option<Box<ContinueSourceObservation>>,
    pub(crate) kind: ContinueSourceFailureKind,
    pub(crate) message: Box<str>,
}

impl ContinueSourceFailure {
    pub(super) fn from_snapshot(
        snapshot: &ContinueSourceSnapshot,
        kind: ContinueSourceFailureKind,
        message: String,
    ) -> Self {
        Self {
            path: snapshot.observation().requested_path().to_path_buf(),
            observation: Some(Box::new(snapshot.observation().clone())),
            kind,
            message: message.into_boxed_str(),
        }
    }
}

pub(crate) fn parse_continue_source(
    snapshot: ContinueSourceSnapshot,
    index: &ContinueIndexSnapshot,
) -> Result<ContinueParseOutcome, ContinueSourceFailure> {
    let root = match validate_and_root(&snapshot.bytes) {
        Ok(root) => root,
        Err(error) if error.is_eof() => {
            return Ok(ContinueParseOutcome::Incomplete);
        }
        Err(error) => {
            return Err(ContinueSourceFailure::from_snapshot(
                &snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                format!("invalid Continue session JSON: {error}"),
            ));
        }
    };
    let (mut document, history) = parse_document(root).map_err(|message| {
        ContinueSourceFailure::from_snapshot(
            &snapshot,
            ContinueSourceFailureKind::MalformedDocument,
            message,
        )
    })?;
    if document.session_id_conflict {
        return Err(ContinueSourceFailure::from_snapshot(
            &snapshot,
            ContinueSourceFailureKind::InvalidSessionId,
            "Continue document asserts conflicting duplicate top-level sessionId values".to_owned(),
        ));
    }
    let session_id = document
        .session_id
        .take()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ContinueSourceFailure::from_snapshot(
                &snapshot,
                ContinueSourceFailureKind::MissingSessionId,
                "Continue NativePath requires a nonempty top-level sessionId".to_owned(),
            )
        })?;
    if session_id.len() > MAX_CONTINUE_SESSION_ID_BYTES || session_id.chars().any(char::is_control)
    {
        return Err(ContinueSourceFailure::from_snapshot(
            &snapshot,
            ContinueSourceFailureKind::InvalidSessionId,
            "Continue sessionId exceeds identity bounds or contains control characters".to_owned(),
        ));
    }
    let history_range = history
        .map(|history| {
            history.range_within(&snapshot.bytes).ok_or_else(|| {
                ContinueSourceFailure::from_snapshot(
                    &snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    "Continue history span is outside the validated source".to_owned(),
                )
            })
        })
        .transpose()?;
    let output_exclusion = document.output_exclusion;
    let source = normalize_continue_document(&snapshot, index, document, session_id)?;
    ContinueSourcePageStream::preflight(snapshot, source, history_range, output_exclusion)
        .map(|stream| ContinueParseOutcome::Complete(Box::new(stream)))
}

#[derive(Debug)]
pub(super) struct RawContinueDocument {
    pub(super) session_id: Option<String>,
    pub(super) session_id_conflict: bool,
    pub(super) title: Option<String>,
    pub(super) created_at: Option<RawTimestamp>,
    pub(super) started_at: Option<RawTimestamp>,
    pub(super) workspace_directory: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) chat_model_title: Option<String>,
    pub(super) usage: Option<RawContinueUsage>,
    pub(super) output_exclusion: ContinueOutputExclusionStats,
}

#[derive(Debug)]
pub(super) struct RawContinueHistoryItem {
    pub(super) id: Option<String>,
    pub(super) timestamp: Option<RawTimestamp>,
    pub(super) created_at: Option<RawTimestamp>,
    pub(super) message: Option<RawContinueMessage>,
    pub(super) editor_text: Option<String>,
    pub(super) context_items: Vec<RawContinueContextItem>,
    pub(super) tool_call_states: Vec<RawContinueToolCallState>,
    pub(super) conversation_summary: Option<String>,
}

#[derive(Debug)]
pub(super) struct RawContinueMessage {
    pub(super) role: Option<String>,
    pub(super) content: Vec<String>,
    pub(super) calls: Vec<RawContinueMessageCall>,
    pub(super) timestamp: Option<RawTimestamp>,
}

#[derive(Debug)]
pub(super) struct RawContinueMessageCall {
    pub(super) id: Option<String>,
    // Preserve provider call classification in the decoded release shape even
    // while Core normalization uses the call name and file-touch evidence.
    #[allow(dead_code)]
    pub(super) kind: Option<String>,
    pub(super) name: Option<String>,
    pub(super) file_touches: Vec<ContinueFileTouch>,
}

#[derive(Debug, Serialize)]
pub(super) struct RawContinueContextItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<String>,
}

#[derive(Debug)]
pub(super) struct RawContinueToolCallState {
    pub(super) tool_call_id: Option<String>,
    pub(super) tool_call: Option<RawContinueToolCall>,
    pub(super) status: Option<String>,
    // Exit accounting remains exact provider evidence for Pro/cross-target
    // consumers; Core currently derives outcome from status.
    #[allow(dead_code)]
    pub(super) exit_code: Option<i64>,
    #[allow(dead_code)]
    pub(super) duration_ms: Option<i64>,
    #[allow(dead_code)]
    pub(super) timed_out: Option<bool>,
}

#[derive(Debug)]
pub(super) struct RawContinueToolCall {
    pub(super) id: Option<String>,
    #[allow(dead_code)]
    pub(super) kind: Option<String>,
    pub(super) name: Option<String>,
    pub(super) function_name: Option<String>,
    pub(super) file_touches: Vec<ContinueFileTouch>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct RawContinueUsage {
    #[serde(rename = "promptTokens", skip_serializing_if = "Option::is_none")]
    pub(super) prompt_tokens: Option<u64>,
    #[serde(rename = "completionTokens", skip_serializing_if = "Option::is_none")]
    pub(super) completion_tokens: Option<u64>,
    #[serde(rename = "totalTokens", skip_serializing_if = "Option::is_none")]
    pub(super) total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) enum RawTimestamp {
    Text(String),
    Number(f64),
}

#[derive(Debug)]
pub(crate) struct ContinueSourcePageStream {
    snapshot: ContinueSourceSnapshot,
    source: Option<Box<ContinuePreparedSource>>,
    session_identity: ContinueSessionIdentity,
    history_range: Option<Range<usize>>,
    cursor: Option<JsonArrayCursor>,
    next_history_ordinal: u64,
    pending_event: Option<ContinueRetainedEvent>,
    authority: ContinueGenerationAuthority,
    output_exclusion: ContinueOutputExclusionStats,
    done: bool,
}

#[derive(Debug)]
struct ContinueRetainedEvent {
    event: ContinueEventRow,
}

impl ContinueSourcePageStream {
    fn preflight(
        snapshot: ContinueSourceSnapshot,
        source: ContinuePreparedSource,
        history_range: Option<Range<usize>>,
        output_exclusion: ContinueOutputExclusionStats,
    ) -> Result<Self, ContinueSourceFailure> {
        let identity = source.session.identity.clone();
        let source_bytes = ContinuePreparedPage::base_estimated_bytes(&identity)
            .saturating_add(source.estimated_bytes());
        if source_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES {
            return Err(ContinueSourceFailure::from_snapshot(
                &snapshot,
                ContinueSourceFailureKind::RetainedItemTooLarge,
                format!(
                    "Continue source/session rows require {source_bytes} retained bytes, exceeding \
                     the {CONTINUE_NATIVE_MAX_PAGE_BYTES} byte page bound"
                ),
            ));
        }
        let cursor = history_range
            .as_ref()
            .map(|range| JsonArrayCursor::new(&snapshot.bytes[range.clone()]))
            .transpose()
            .map_err(|error| {
                ContinueSourceFailure::from_snapshot(
                    &snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    scan_error(error),
                )
            })?;
        Ok(Self {
            snapshot,
            source: Some(Box::new(source)),
            session_identity: identity,
            history_range,
            cursor,
            next_history_ordinal: 0,
            pending_event: None,
            authority: ContinueGenerationAuthority {
                observed_history_items: Some(0),
                retained_events: 0,
                rejected_items: 0,
            },
            output_exclusion,
            done: false,
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> Result<Option<ContinuePreparedPage>, ContinueSourceFailure> {
        if self.done {
            return Ok(None);
        }
        let source = self.source.take();
        let mut row_count: usize = source.as_ref().map_or(0, |_| 2);
        let mut estimated_bytes =
            ContinuePreparedPage::base_estimated_bytes(&self.session_identity)
                .saturating_add(source.as_ref().map_or(0, |source| source.estimated_bytes()));
        let mut events = Vec::new();

        let terminal = loop {
            let retained = match self.pending_event.take() {
                Some(event) => event,
                None => match self.next_retained_event()? {
                    Some(event) => event,
                    None => break true,
                },
            };
            let event_bytes = retained.event.estimated_bytes();
            let next_rows = row_count.saturating_add(retained.event.logical_units());
            let next_bytes = estimated_bytes.saturating_add(event_bytes);
            if row_count > 0
                && (next_rows > CONTINUE_NATIVE_MAX_PAGE_ROWS
                    || next_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES)
            {
                self.pending_event = Some(retained);
                break false;
            }
            if next_rows > CONTINUE_NATIVE_MAX_PAGE_ROWS
                || next_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES
            {
                return Err(ContinueSourceFailure::from_snapshot(
                    &self.snapshot,
                    ContinueSourceFailureKind::RetainedItemTooLarge,
                    "Continue retained event cannot fit an empty bounded page".to_owned(),
                ));
            }
            events.push(retained.event);
            row_count = next_rows;
            estimated_bytes = next_bytes;
        };

        self.done = terminal;
        debug_assert!(row_count <= CONTINUE_NATIVE_MAX_PAGE_ROWS);
        debug_assert!(estimated_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
        Ok(Some(ContinuePreparedPage {
            source,
            session_identity: self.session_identity.clone(),
            events: events.into_boxed_slice(),
            terminal,
            authority: terminal.then(|| self.authority.clone()),
            output_exclusion: terminal.then_some(self.output_exclusion),
            row_count,
            estimated_bytes,
        }))
    }

    fn next_retained_event(
        &mut self,
    ) -> Result<Option<ContinueRetainedEvent>, ContinueSourceFailure> {
        let Some(range) = self.history_range.as_ref() else {
            return Ok(None);
        };
        let history = &self.snapshot.bytes[range.clone()];
        let cursor = self.cursor.as_mut().ok_or_else(|| {
            ContinueSourceFailure::from_snapshot(
                &self.snapshot,
                ContinueSourceFailureKind::Normalization,
                "Continue event stream has no history cursor".to_owned(),
            )
        })?;
        loop {
            let Some(item) = cursor.next(history).map_err(|error| {
                ContinueSourceFailure::from_snapshot(
                    &self.snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    scan_error(error),
                )
            })?
            else {
                return Ok(None);
            };
            let ordinal = self.next_history_ordinal;
            self.next_history_ordinal =
                self.next_history_ordinal.checked_add(1).ok_or_else(|| {
                    ContinueSourceFailure::from_snapshot(
                        &self.snapshot,
                        ContinueSourceFailureKind::Normalization,
                        "Continue history ordinal exceeds u64".to_owned(),
                    )
                })?;
            self.authority.observed_history_items = self
                .authority
                .observed_history_items
                .and_then(|count| count.checked_add(1));
            if self.authority.observed_history_items.is_none() {
                return Err(ContinueSourceFailure::from_snapshot(
                    &self.snapshot,
                    ContinueSourceFailureKind::Normalization,
                    "Continue observed history count exceeds usize".to_owned(),
                ));
            }
            let source_record_digest = Sha256::digest(item.raw()).into();
            let Some(item) =
                parse_history_item(item, &mut self.output_exclusion).map_err(|message| {
                    ContinueSourceFailure::from_snapshot(
                        &self.snapshot,
                        ContinueSourceFailureKind::MalformedDocument,
                        message,
                    )
                })?
            else {
                self.authority.rejected_items = self
                    .authority
                    .rejected_items
                    .checked_add(1)
                    .ok_or_else(|| {
                        ContinueSourceFailure::from_snapshot(
                            &self.snapshot,
                            ContinueSourceFailureKind::Normalization,
                            "Continue rejected history count exceeds usize".to_owned(),
                        )
                    })?;
                continue;
            };
            let event =
                normalize_event(&self.session_identity, ordinal, &item, source_record_digest)
                    .map_err(|error| normalization_failure(&self.snapshot, ordinal, error))?;
            self.authority.retained_events = self
                .authority
                .retained_events
                .checked_add(1)
                .ok_or_else(|| {
                    ContinueSourceFailure::from_snapshot(
                        &self.snapshot,
                        ContinueSourceFailureKind::Normalization,
                        "Continue retained history count exceeds usize".to_owned(),
                    )
                })?;
            return Ok(Some(ContinueRetainedEvent { event }));
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ContinueExactHistoryLookup<'a> {
    DifferentSession,
    MissingItem,
    Items(Vec<&'a [u8]>),
}

/// Resolves one exact physical `history` element with the same bounded
/// structural parser used by ingestion.
///
/// The returned bytes borrow the already-certified whole-document snapshot;
/// no second JSON representation or normalized record is created.
pub(super) fn locate_continue_exact_history_items<'a>(
    bytes: &'a [u8],
    expected_session_id: &str,
    history_ordinals: &[u64],
) -> Result<ContinueExactHistoryLookup<'a>, String> {
    let root = validate_and_root(bytes)
        .map_err(|error| format!("invalid Continue session JSON: {error}"))?;
    let (mut document, history) = parse_document(root)?;
    if document.session_id_conflict {
        return Err(
            "Continue document asserts conflicting duplicate top-level sessionId values".to_owned(),
        );
    }
    let session_id = document
        .session_id
        .take()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Continue document has no nonempty top-level sessionId".to_owned())?;
    if session_id.len() > MAX_CONTINUE_SESSION_ID_BYTES || session_id.chars().any(char::is_control)
    {
        return Err(
            "Continue sessionId exceeds identity bounds or contains control characters".to_owned(),
        );
    }
    if session_id != expected_session_id {
        return Ok(ContinueExactHistoryLookup::DifferentSession);
    }
    let Some(history) = history else {
        return Ok(ContinueExactHistoryLookup::MissingItem);
    };
    let mut resolved = vec![None; history_ordinals.len()];
    let mut cursor = JsonArrayCursor::new(history.raw()).map_err(scan_error)?;
    let mut ordinal = 0_u64;
    while let Some(item) = cursor.next(history.raw()).map_err(scan_error)? {
        for (index, expected) in history_ordinals.iter().enumerate() {
            if ordinal == *expected {
                resolved[index] = Some(item.raw());
            }
        }
        ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| "Continue history ordinal exceeds u64".to_owned())?;
    }
    Ok(match resolved.into_iter().collect::<Option<Vec<_>>>() {
        Some(items) => ContinueExactHistoryLookup::Items(items),
        None => ContinueExactHistoryLookup::MissingItem,
    })
}

fn normalization_failure(
    snapshot: &ContinueSourceSnapshot,
    history_ordinal: u64,
    error: NormalizeEventError,
) -> ContinueSourceFailure {
    let (kind, message) = match error {
        NormalizeEventError::RetainedItemTooLarge { observed } => (
            ContinueSourceFailureKind::RetainedItemTooLarge,
            format!(
                "Continue history item {history_ordinal} retains {observed} lexical bytes, \
                 exceeding the {MAX_PROVIDER_JSONL_LINE_BYTES} byte product bound"
            ),
        ),
        NormalizeEventError::FileTouchLimitExceeded => (
            ContinueSourceFailureKind::Normalization,
            format!(
                "Continue history item exceeds the {CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT} \
                 unique file-touch transaction bound"
            ),
        ),
    };
    ContinueSourceFailure::from_snapshot(snapshot, kind, message)
}
