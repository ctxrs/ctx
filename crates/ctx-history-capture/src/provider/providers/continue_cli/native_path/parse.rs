use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

use crate::{
    provider::tool_input, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceLocator,
    ProOutputObservation, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    decode::{
        decode_bool, decode_f64, decode_i64, decode_string, decode_unbounded_string,
        validate_and_root, JsonArrayCursor, JsonKind, JsonSpan,
    },
    normalize::{
        normalize_continue_document, normalize_event, ContinueEventRow,
        ContinueGenerationAuthority, ContinueNativeProfile, ContinuePreparedPage,
        ContinuePreparedSource, ContinueProExtractionFailure, ContinueSessionIdentity,
        ContinueSourceCompleteness, ContinueTransientOutputPayload, NormalizeEventError,
        CONTINUE_NATIVE_MAX_OUTPUT_PAGE_BYTES, CONTINUE_NATIVE_MAX_OUTPUT_PAGE_UNITS,
        CONTINUE_NATIVE_MAX_PAGE_BYTES, CONTINUE_NATIVE_MAX_PAGE_ROWS,
    },
    source::{
        ContinueIndexObservation, ContinueIndexSnapshot, ContinueSourceObservation,
        ContinueSourceSnapshot,
    },
};

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
    pub(crate) result_string_allocations: usize,
    pub(crate) result_body_bytes_decoded: usize,
    pub(crate) result_hashes_created: usize,
    pub(crate) result_previews_created: usize,
    pub(crate) result_touches_created: usize,
    pub(crate) result_fts_documents_created: usize,
    pub(crate) result_handoffs_created: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContinueIncompleteSource {
    pub(crate) observation: ContinueSourceObservation,
    pub(crate) index_dependency: ContinueIndexObservation,
    pub(crate) authority: ContinueGenerationAuthority,
}

#[derive(Debug)]
pub(crate) enum ContinueParseOutcome {
    Complete(Box<ContinueSourcePageStream>),
    Incomplete(Box<ContinueIncompleteSource>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContinueSourceFailureKind {
    Access,
    SourceTooLarge,
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
    pub(super) fn from_access(
        path: &Path,
        kind: ContinueSourceFailureKind,
        message: String,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            observation: None,
            kind,
            message: message.into_boxed_str(),
        }
    }

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

pub(crate) fn parse_continue_source_with_profile(
    snapshot: ContinueSourceSnapshot,
    index: &ContinueIndexSnapshot,
    profile: ContinueNativeProfile,
) -> Result<ContinueParseOutcome, ContinueSourceFailure> {
    let root = match validate_and_root(&snapshot.bytes) {
        Ok(root) => root,
        Err(error) if error.is_eof() => {
            return Ok(ContinueParseOutcome::Incomplete(Box::new(
                ContinueIncompleteSource {
                    observation: snapshot.observation().clone(),
                    index_dependency: index.observation().clone(),
                    authority: ContinueGenerationAuthority {
                        completeness: ContinueSourceCompleteness::Incomplete,
                        observed_history_items: None,
                        retained_events: 0,
                        rejected_items: 0,
                    },
                },
            )));
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
    ContinueSourcePageStream::preflight(snapshot, source, history_range, output_exclusion, profile)
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
    pub(super) kind: Option<String>,
    pub(super) name: Option<String>,
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
    pub(super) exit_code: Option<i64>,
    pub(super) duration_ms: Option<i64>,
    pub(super) timed_out: Option<bool>,
}

#[derive(Debug)]
pub(super) struct RawContinueToolCall {
    pub(super) id: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) name: Option<String>,
    pub(super) function_name: Option<String>,
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
    emitted_events: usize,
    page_ordinal: u64,
    pending_event: Option<ContinueRetainedEvent>,
    authority: ContinueGenerationAuthority,
    output_exclusion: ContinueOutputExclusionStats,
    profile: ContinueNativeProfile,
    output_failure: Option<ContinueProExtractionFailure>,
    done: bool,
}

#[derive(Debug)]
struct ContinueRetainedEvent {
    event: ContinueEventRow,
    outputs: Vec<ProOutputObservation>,
    output_bytes: usize,
}

impl ContinueSourcePageStream {
    fn preflight(
        snapshot: ContinueSourceSnapshot,
        source: ContinuePreparedSource,
        history_range: Option<Range<usize>>,
        mut output_exclusion: ContinueOutputExclusionStats,
        profile: ContinueNativeProfile,
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
        let mut observed = 0_usize;
        let mut retained = 0_usize;
        let mut rejected = 0_usize;
        if let Some(range) = history_range.as_ref() {
            let history = &snapshot.bytes[range.clone()];
            let mut cursor = JsonArrayCursor::new(history).map_err(|error| {
                ContinueSourceFailure::from_snapshot(
                    &snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    scan_error(error),
                )
            })?;
            while let Some(item) = cursor.next(history).map_err(|error| {
                ContinueSourceFailure::from_snapshot(
                    &snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    scan_error(error),
                )
            })? {
                if item.kind() != JsonKind::Object {
                    return Err(ContinueSourceFailure::from_snapshot(
                        &snapshot,
                        ContinueSourceFailureKind::MalformedDocument,
                        "Continue history item is not a JSON object".to_owned(),
                    ));
                }
                let ordinal = u64::try_from(observed).map_err(|_| {
                    ContinueSourceFailure::from_snapshot(
                        &snapshot,
                        ContinueSourceFailureKind::Normalization,
                        "Continue history ordinal exceeds u64".to_owned(),
                    )
                })?;
                observed = observed.saturating_add(1);
                let Some(item) =
                    parse_history_item(item, &mut output_exclusion).map_err(|message| {
                        ContinueSourceFailure::from_snapshot(
                            &snapshot,
                            ContinueSourceFailureKind::MalformedDocument,
                            message,
                        )
                    })?
                else {
                    rejected = rejected.saturating_add(1);
                    continue;
                };
                let event = normalize_event(&identity, ordinal, &item)
                    .map_err(|error| normalization_failure(&snapshot, ordinal, error))?;
                let event_bytes = event.estimated_bytes();
                let event_page_bytes = ContinuePreparedPage::base_estimated_bytes(&identity)
                    .saturating_add(event_bytes);
                if event_page_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES {
                    return Err(ContinueSourceFailure::from_snapshot(
                        &snapshot,
                        ContinueSourceFailureKind::RetainedItemTooLarge,
                        format!(
                            "Continue history item {ordinal} requires {event_page_bytes} page bytes, \
                             exceeding the {CONTINUE_NATIVE_MAX_PAGE_BYTES} byte page bound"
                        ),
                    ));
                }
                retained = retained.saturating_add(1);
            }
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
            emitted_events: 0,
            page_ordinal: 0,
            pending_event: None,
            authority: ContinueGenerationAuthority {
                completeness: ContinueSourceCompleteness::Complete,
                observed_history_items: Some(observed),
                retained_events: retained,
                rejected_items: rejected,
            },
            output_exclusion,
            profile,
            output_failure: None,
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
        let mut outputs = Vec::new();
        let mut output_bytes = 0_usize;

        while self.emitted_events.saturating_add(events.len()) < self.authority.retained_events {
            let mut retained = match self.pending_event.take() {
                Some(event) => event,
                None => self.next_retained_event()?.ok_or_else(|| {
                    ContinueSourceFailure::from_snapshot(
                        &self.snapshot,
                        ContinueSourceFailureKind::Normalization,
                        "Continue preflight/event stream retained-count mismatch".to_owned(),
                    )
                })?,
            };
            if self.output_failure.is_none()
                && (retained.outputs.len() > CONTINUE_NATIVE_MAX_OUTPUT_PAGE_UNITS
                    || retained.output_bytes > CONTINUE_NATIVE_MAX_OUTPUT_PAGE_BYTES)
            {
                self.output_failure = Some(ContinueProExtractionFailure {
                    history_ordinal: retained.event.identity.history_ordinal,
                    observed_outputs: retained.outputs.len(),
                    observed_bytes: retained.output_bytes,
                    message: "one Continue history item exceeds the bounded NativePath output page"
                        .into(),
                });
                retained.outputs.clear();
                retained.output_bytes = 0;
                outputs.clear();
                output_bytes = 0;
            }
            let next_output_units = outputs.len().saturating_add(retained.outputs.len());
            let next_output_bytes = output_bytes.saturating_add(retained.output_bytes);
            if self.output_failure.is_none()
                && !events.is_empty()
                && (next_output_units > CONTINUE_NATIVE_MAX_OUTPUT_PAGE_UNITS
                    || next_output_bytes > CONTINUE_NATIVE_MAX_OUTPUT_PAGE_BYTES)
            {
                self.pending_event = Some(retained);
                break;
            }
            let event_bytes = retained.event.estimated_bytes();
            let next_rows = row_count.saturating_add(1);
            let next_bytes = estimated_bytes.saturating_add(event_bytes);
            if row_count > 0
                && (next_rows > CONTINUE_NATIVE_MAX_PAGE_ROWS
                    || next_bytes > CONTINUE_NATIVE_MAX_PAGE_BYTES)
            {
                self.pending_event = Some(retained);
                break;
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
            if self.output_failure.is_none() {
                output_bytes = next_output_bytes;
                outputs.append(&mut retained.outputs);
            }
            events.push(retained.event);
            row_count = next_rows;
            estimated_bytes = next_bytes;
        }

        let terminal =
            self.emitted_events.saturating_add(events.len()) == self.authority.retained_events;
        let page_ordinal = self.page_ordinal;
        self.page_ordinal = self.page_ordinal.saturating_add(1);
        self.emitted_events = self.emitted_events.saturating_add(events.len());
        self.done = terminal;
        debug_assert!(row_count <= CONTINUE_NATIVE_MAX_PAGE_ROWS);
        debug_assert!(estimated_bytes <= CONTINUE_NATIVE_MAX_PAGE_BYTES);
        Ok(Some(ContinuePreparedPage {
            source,
            session_identity: self.session_identity.clone(),
            page_ordinal,
            events: events.into_boxed_slice(),
            terminal,
            authority: terminal.then(|| self.authority.clone()),
            output_exclusion: terminal.then_some(self.output_exclusion),
            transient_output: self.profile.wants_outputs().then_some(
                ContinueTransientOutputPayload {
                    observations: outputs,
                    failure: terminal.then(|| self.output_failure.clone()).flatten(),
                },
            ),
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
            let mut second_pass_stats = ContinueOutputExclusionStats::default();
            let item_span = item;
            let Some(item) =
                parse_history_item(item_span, &mut second_pass_stats).map_err(|message| {
                    ContinueSourceFailure::from_snapshot(
                        &self.snapshot,
                        ContinueSourceFailureKind::MalformedDocument,
                        message,
                    )
                })?
            else {
                continue;
            };
            let event = normalize_event(&self.session_identity, ordinal, &item)
                .map_err(|error| normalization_failure(&self.snapshot, ordinal, error))?;
            let outputs = if self.profile.wants_outputs() {
                extract_continue_outputs(
                    item_span,
                    &self.snapshot,
                    ordinal,
                    event.occurred_at.map(|value| value.timestamp_millis()),
                    &self.session_identity,
                    &mut self.output_exclusion,
                )?
            } else {
                Vec::new()
            };
            let output_bytes = outputs.iter().fold(0_usize, |bytes, output| {
                bytes.saturating_add(super::normalize::estimated_output_bytes(output))
            });
            return Ok(Some(ContinueRetainedEvent {
                event,
                outputs,
                output_bytes,
            }));
        }
    }
}

fn extract_continue_outputs(
    item: JsonSpan<'_>,
    snapshot: &ContinueSourceSnapshot,
    history_ordinal: u64,
    occurred_at_unix_ms: Option<i64>,
    session: &ContinueSessionIdentity,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Vec<ProOutputObservation>, ContinueSourceFailure> {
    let history_item_index = u32::try_from(history_ordinal).map_err(|_| {
        ContinueSourceFailure::from_snapshot(
            snapshot,
            ContinueSourceFailureKind::Normalization,
            "Continue history ordinal exceeds stable output identity bounds".to_owned(),
        )
    })?;
    let mut native_item_id = None;
    let mut states = None;
    for field in item.as_object().map_err(|error| {
        ContinueSourceFailure::from_snapshot(
            snapshot,
            ContinueSourceFailureKind::MalformedDocument,
            scan_error(error),
        )
    })? {
        let (key, value) = field.map_err(|error| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                scan_error(error),
            )
        })?;
        if key.is("id") {
            native_item_id = decode_string(value, MAX_NATIVE_ITEM_ID_BYTES)
                .map_err(|error| {
                    ContinueSourceFailure::from_snapshot(
                        snapshot,
                        ContinueSourceFailureKind::MalformedDocument,
                        error.to_string(),
                    )
                })?
                .filter(|value| valid_continue_native_id_part(value));
        } else if key.is("toolCallStates") && value.kind() == JsonKind::Array {
            states = Some(value);
        }
    }
    let Some(states) = states else {
        return Ok(Vec::new());
    };
    let mut observations = Vec::new();
    for (state_ordinal, state) in states
        .as_array()
        .map_err(|error| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                scan_error(error),
            )
        })?
        .enumerate()
    {
        let state = state.map_err(|error| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                scan_error(error),
            )
        })?;
        if state.kind() != JsonKind::Object {
            continue;
        }
        let mut output_span = None;
        for field in state.as_object().map_err(|error| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                scan_error(error),
            )
        })? {
            let (key, value) = field.map_err(|error| {
                ContinueSourceFailure::from_snapshot(
                    snapshot,
                    ContinueSourceFailureKind::MalformedDocument,
                    scan_error(error),
                )
            })?;
            if key.is("output") {
                if output_span.replace(value).is_some() {
                    return Err(ContinueSourceFailure::from_snapshot(
                        snapshot,
                        ContinueSourceFailureKind::MalformedDocument,
                        "Continue tool state has duplicate output fields".to_owned(),
                    ));
                }
            }
        }
        let Some(output_span) = output_span.filter(|span| span.kind() != JsonKind::Null) else {
            continue;
        };
        let state_value = serde_json::from_slice::<Value>(state.raw()).map_err(|error| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                format!("invalid Continue tool state: {error}"),
            )
        })?;
        let content = decode_continue_output(output_span).map_err(|message| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::MalformedDocument,
                message,
            )
        })?;
        stats.result_string_allocations = stats.result_string_allocations.saturating_add(1);
        stats.result_body_bytes_decoded = stats
            .result_body_bytes_decoded
            .saturating_add(content.len());
        let tool_state_index = u32::try_from(state_ordinal).map_err(|_| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::Normalization,
                "Continue tool-state index exceeds stable output identity bounds".to_owned(),
            )
        })?;
        let call_id = continue_result_call_id(&state_value);
        let tool_name = continue_result_tool_name(&state_value);
        let native_record_id = continue_tool_result_native_id(
            native_item_id.as_deref(),
            history_item_index,
            call_id.as_deref(),
            tool_state_index,
        );
        let native_sequence =
            continue_result_provider_event_index(history_item_index, tool_state_index).ok_or_else(
                || {
                    ContinueSourceFailure::from_snapshot(
                        snapshot,
                        ContinueSourceFailureKind::Normalization,
                        "Continue output identity exceeds stable bounds".to_owned(),
                    )
                },
            )?;
        let range = output_span.range_within(&snapshot.bytes).ok_or_else(|| {
            ContinueSourceFailure::from_snapshot(
                snapshot,
                ContinueSourceFailureKind::Normalization,
                "Continue output span is outside its certified source".to_owned(),
            )
        })?;
        let command = continue_output_is_command(&tool_name)
            .then(|| continue_output_command_context(&state_value, &tool_name))
            .flatten();
        let kind = if command.is_some() || continue_output_is_command(&tool_name) {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        };
        observations.push(ProOutputObservation {
            kind,
            coordinate: OutputNativeCoordinate {
                unit_key: native_record_id.clone(),
                native_sequence,
                native_record_id: Some(native_record_id.clone()),
                source_record_ordinal: Some(history_ordinal),
                source_record_subrecord_index: Some(tool_state_index),
                byte_start: u64::try_from(range.start).ok(),
                byte_end_exclusive: u64::try_from(range.end).ok(),
            },
            occurred_at_unix_ms,
            associations: OutputAssociations {
                direct_session_id: session.0.clone(),
                root_session_id: session.0.clone(),
                parent_session_id: None,
                provider_session_id: Some(session.0.clone()),
                agent_id: None,
                repository: None,
            },
            call_id,
            command,
            outcome: continue_output_outcome(&state_value),
            locator: OutputSourceLocator {
                version: 1,
                kind: "continue_native_tool_state_range".to_owned(),
                payload: encode_continue_output_locator(
                    history_item_index,
                    tool_state_index,
                    range,
                    &native_record_id,
                ),
            },
            content,
        });
    }
    Ok(observations)
}

fn decode_continue_output(output: JsonSpan<'_>) -> Result<Vec<u8>, String> {
    match output.kind() {
        JsonKind::String => serde_json::from_slice::<String>(output.raw())
            .map(String::into_bytes)
            .map_err(|error| format!("invalid Continue output string: {error}")),
        JsonKind::Null => Ok(Vec::new()),
        JsonKind::Bool | JsonKind::Number | JsonKind::Array | JsonKind::Object => {
            let value = serde_json::from_slice::<Value>(output.raw())
                .map_err(|error| format!("invalid Continue output value: {error}"))?;
            serde_json::to_vec(&value)
                .map_err(|error| format!("failed to encode Continue output value: {error}"))
        }
    }
}

fn continue_tool_result_native_id(
    item_id: Option<&str>,
    history_item_index: u32,
    tool_call_id: Option<&str>,
    tool_state_index: u32,
) -> String {
    match (item_id, tool_call_id) {
        (Some(item_id), Some(tool_call_id)) => {
            format!("{item_id}:tool:{tool_call_id}:result")
        }
        (Some(item_id), None) => format!("{item_id}:tool-state:{tool_state_index}:result"),
        (None, Some(tool_call_id)) => {
            format!("history:{history_item_index}:tool:{tool_call_id}:result")
        }
        (None, None) => {
            format!("history:{history_item_index}:tool-state:{tool_state_index}:result")
        }
    }
}

fn valid_continue_native_id_part(value: &str) -> bool {
    !value.is_empty() && value.len() <= 384 && !value.chars().any(char::is_control)
}

fn continue_result_call_id(state: &Value) -> Option<String> {
    state
        .get("toolCallId")
        .or_else(|| state.pointer("/toolCall/id"))
        .and_then(Value::as_str)
        .filter(|value| valid_continue_native_id_part(value))
        .map(str::to_owned)
}

fn continue_result_tool_name(state: &Value) -> String {
    state
        .pointer("/toolCall/function/name")
        .or_else(|| state.pointer("/toolCall/name"))
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= MAX_TOOL_NAME_BYTES && !value.contains('\0')
        })
        .unwrap_or("tool")
        .to_owned()
}

fn continue_output_is_command(tool_name: &str) -> bool {
    let normalized = tool_name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "bash"
            | "shell"
            | "terminal"
            | "command"
            | "executecommand"
            | "runcommand"
            | "runterminalcommand"
    )
}

fn continue_output_outcome(state: &Value) -> OutputOutcomeMetadata {
    let exit_code = state
        .get("exitCode")
        .or_else(|| state.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = state
        .get("durationMs")
        .or_else(|| state.get("duration_ms"))
        .and_then(Value::as_u64);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let timed_out = ["timedOut", "timed_out", "timeout"]
        .into_iter()
        .any(|key| state.get(key).and_then(Value::as_bool).unwrap_or(false))
        || status
            .as_deref()
            .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if exit_code.is_some_and(|exit_code| exit_code != 0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            )
        })
    {
        OutputOutcome::Failure
    } else if exit_code == Some(0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "done" | "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            )
        })
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn continue_output_command_context(state: &Value, tool_name: &str) -> Option<OutputCommandContext> {
    let input = state
        .pointer("/toolCall/function/arguments")
        .or_else(|| state.pointer("/toolCall/arguments"))
        .or_else(|| state.get("arguments"))?;
    Some(OutputCommandContext {
        tool_name: tool_name.to_owned(),
        command: tool_input::command(input)?,
        working_directory: tool_input::working_directory(input),
    })
}

fn continue_result_provider_event_index(
    history_item_index: u32,
    tool_state_index: u32,
) -> Option<u64> {
    const RESULT_EVENT_NAMESPACE: u64 = 1_u64 << 63;
    const TOOL_STATE_BITS: u32 = 31;
    const MAX_TOOL_STATE_INDEX: u32 = (1_u32 << TOOL_STATE_BITS) - 1;
    (tool_state_index <= MAX_TOOL_STATE_INDEX).then(|| {
        RESULT_EVENT_NAMESPACE
            | (u64::from(history_item_index) << TOOL_STATE_BITS)
            | u64::from(tool_state_index)
    })
}

fn encode_continue_output_locator(
    history_item_index: u32,
    tool_state_index: u32,
    range: Range<usize>,
    native_record_id: &str,
) -> Vec<u8> {
    let native_id = native_record_id.as_bytes();
    let native_id_len = u16::try_from(native_id.len()).unwrap_or(u16::MAX);
    let mut locator = Vec::with_capacity(26_usize.saturating_add(native_id.len()));
    locator.extend_from_slice(&history_item_index.to_be_bytes());
    locator.extend_from_slice(&tool_state_index.to_be_bytes());
    locator.extend_from_slice(&u64::try_from(range.start).unwrap_or(u64::MAX).to_be_bytes());
    locator.extend_from_slice(&u64::try_from(range.end).unwrap_or(u64::MAX).to_be_bytes());
    locator.extend_from_slice(&native_id_len.to_be_bytes());
    locator.extend_from_slice(native_id);
    locator
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
                "Continue history item {history_ordinal} serializes to {observed} bytes, exceeding \
                 the {MAX_PROVIDER_JSONL_LINE_BYTES} byte product bound"
            ),
        ),
        NormalizeEventError::Serialization(message) => {
            (ContinueSourceFailureKind::Normalization, message)
        }
    };
    ContinueSourceFailure::from_snapshot(snapshot, kind, message)
}

fn parse_document(
    root: JsonSpan<'_>,
) -> Result<(RawContinueDocument, Option<JsonSpan<'_>>), String> {
    if root.kind() != JsonKind::Object {
        return Err("Continue session document is not a JSON object".to_owned());
    }
    let mut stats = ContinueOutputExclusionStats::default();
    let mut session_id = None;
    let mut saw_session_id = false;
    let mut session_id_conflict = false;
    let mut title = None;
    let mut created_at = None;
    let mut started_at = None;
    let mut workspace_directory = None;
    let mut mode = None;
    let mut chat_model_title = None;
    let mut usage = None;
    let mut history = None;
    for field in root.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("sessionId") {
            let observed =
                retained_bounded_string(value, MAX_CONTINUE_SESSION_ID_BYTES, &mut stats)?;
            if saw_session_id && session_id != observed {
                session_id_conflict = true;
            } else if !saw_session_id {
                session_id = observed;
            }
            saw_session_id = true;
        } else if key.is("title") {
            title = retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, &mut stats)?;
        } else if key.is("createdAt") {
            created_at = parse_timestamp(value, &mut stats)?;
        } else if key.is("startedAt") {
            started_at = parse_timestamp(value, &mut stats)?;
        } else if key.is("workspaceDirectory") {
            workspace_directory =
                retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, &mut stats)?;
        } else if key.is("mode") {
            mode = retained_bounded_string(value, 128, &mut stats)?;
        } else if key.is("chatModelTitle") {
            chat_model_title = retained_bounded_string(value, 512, &mut stats)?;
        } else if key.is("usage") {
            usage = parse_usage(value)?;
        } else if key.is("history") {
            if history.is_some() {
                return Err("Continue document has duplicate history fields".to_owned());
            }
            if value.kind() != JsonKind::Array {
                return Err("Continue history is not a JSON array".to_owned());
            }
            history = Some(value);
        } else if key.is_result_like() {
            record_result(&mut stats, value);
        }
    }
    Ok((
        RawContinueDocument {
            session_id,
            session_id_conflict,
            title,
            created_at,
            started_at,
            workspace_directory,
            mode,
            chat_model_title,
            usage,
            output_exclusion: stats,
        },
        history,
    ))
}

fn parse_usage(value: JsonSpan<'_>) -> Result<Option<RawContinueUsage>, String> {
    if value.kind() != JsonKind::Object {
        return Ok(None);
    }
    let mut usage = RawContinueUsage {
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
    };
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("promptTokens") {
            usage.prompt_tokens = super::decode::decode_u64(value);
        } else if key.is("completionTokens") {
            usage.completion_tokens = super::decode::decode_u64(value);
        } else if key.is("totalTokens") {
            usage.total_tokens = super::decode::decode_u64(value);
        }
    }
    Ok(Some(usage))
}

fn parse_history_item(
    item: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueHistoryItem>, String> {
    let mut admission = TagAdmission::Missing;
    let mut saw_type = false;
    let mut saw_kind = false;
    let mut saw_message = false;
    let mut saw_editor_state = false;
    let mut saw_context_items = false;
    let mut saw_tool_call_states = false;
    let mut saw_conversation_summary = false;
    let mut saw_result_field = false;
    let mut duplicate_body_field = false;
    for field in item.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") {
            duplicate_body_field |= saw_type;
            saw_type = true;
            admission = admission.observe(value);
        } else if key.is("kind") {
            duplicate_body_field |= saw_kind;
            saw_kind = true;
            admission = admission.observe(value);
        } else if key.is("message") {
            duplicate_body_field |= saw_message;
            saw_message = true;
        } else if key.is("editorState") {
            duplicate_body_field |= saw_editor_state;
            saw_editor_state = true;
        } else if key.is("contextItems") {
            duplicate_body_field |= saw_context_items;
            saw_context_items = true;
        } else if key.is("toolCallStates") {
            duplicate_body_field |= saw_tool_call_states;
            saw_tool_call_states = true;
        } else if key.is("conversationSummary") {
            duplicate_body_field |= saw_conversation_summary;
            saw_conversation_summary = true;
        } else if key.is_result_like() {
            duplicate_body_field |= saw_result_field;
            saw_result_field = true;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, item);
        return Ok(None);
    }
    if duplicate_body_field
        || !matches!(
            admission,
            TagAdmission::Missing | TagAdmission::Text | TagAdmission::Call | TagAdmission::Context
        )
    {
        record_unproven(stats, item);
        return Ok(None);
    }

    let mut id = None;
    let mut timestamp = None;
    let mut created_at = None;
    let mut message = None;
    let mut editor_text = None;
    let mut context_items = Vec::new();
    let mut tool_call_states = Vec::new();
    let mut conversation_summary = None;
    for field in item.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
        } else if key.is("id") {
            id = retained_bounded_string(value, MAX_NATIVE_ITEM_ID_BYTES, stats)?;
        } else if key.is("timestamp") {
            timestamp = parse_timestamp(value, stats)?;
        } else if key.is("createdAt") {
            created_at = parse_timestamp(value, stats)?;
        } else if key.is("message") {
            message = parse_message(value, stats)?;
        } else if key.is("editorState") {
            if value.kind() == JsonKind::String {
                editor_text = retained_unbounded_string(value, stats)?;
            } else if value.kind() != JsonKind::Null {
                record_unproven(stats, value);
            }
        } else if key.is("contextItems") {
            context_items = parse_context_items(value, stats)?;
        } else if key.is("toolCallStates") {
            tool_call_states = parse_tool_call_states(value, stats)?;
        } else if key.is("conversationSummary") {
            if value.kind() == JsonKind::String {
                conversation_summary = retained_unbounded_string(value, stats)?;
            } else if value.kind() != JsonKind::Null {
                record_unproven(stats, value);
            }
        } else if key.is_result_like() {
            record_result(stats, value);
        } else if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
    }
    let retained = message.is_some()
        || editor_text.is_some()
        || !context_items.is_empty()
        || !tool_call_states.is_empty()
        || conversation_summary.is_some();
    Ok(retained.then_some(RawContinueHistoryItem {
        id,
        timestamp,
        created_at,
        message,
        editor_text,
        context_items,
        tool_call_states,
        conversation_summary,
    }))
}

fn parse_message(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueMessage>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut role_admission = RoleAdmission::Missing;
    let mut tag_admission = TagAdmission::Missing;
    let mut saw_role = false;
    let mut saw_type = false;
    let mut saw_kind = false;
    let mut saw_content = false;
    let mut saw_timestamp = false;
    let mut duplicate_field = false;
    let mut saw_result_field = false;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is("role") {
            duplicate_field |= saw_role;
            saw_role = true;
            role_admission = role_admission.observe(field_value);
        } else if key.is("type") {
            duplicate_field |= saw_type;
            saw_type = true;
            tag_admission = tag_admission.observe(field_value);
        } else if key.is("kind") {
            duplicate_field |= saw_kind;
            saw_kind = true;
            tag_admission = tag_admission.observe(field_value);
        } else if key.is("content") {
            duplicate_field |= saw_content;
            saw_content = true;
        } else if key.is("timestamp") {
            duplicate_field |= saw_timestamp;
            saw_timestamp = true;
        } else if key.is_result_like() {
            duplicate_field |= saw_result_field;
            saw_result_field = true;
        }
    }
    let role = match role_admission {
        RoleAdmission::Retained(role) => role,
        RoleAdmission::Result => {
            record_result(stats, value);
            return Ok(None);
        }
        RoleAdmission::Missing | RoleAdmission::Unknown | RoleAdmission::Conflict => {
            record_unproven(stats, value);
            return Ok(None);
        }
    };
    if tag_admission == TagAdmission::Result {
        record_result(stats, value);
        return Ok(None);
    }
    if duplicate_field
        || !matches!(
            tag_admission,
            TagAdmission::Missing | TagAdmission::Text | TagAdmission::Call | TagAdmission::Context
        )
    {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut content = ParsedMessageContent::default();
    let mut timestamp = None;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is("role") || key.is("type") || key.is("kind") {
        } else if key.is("content") {
            content = parse_message_content(field_value, stats)?;
        } else if key.is("timestamp") {
            timestamp = parse_timestamp(field_value, stats)?;
        } else if key.is_result_like() {
            record_result(stats, field_value);
        } else if field_value.kind() != JsonKind::Null {
            record_unproven(stats, field_value);
        }
    }
    if (saw_content || saw_result_field) && !content.admitted {
        return Ok(None);
    }
    let role = role.as_str().to_owned();
    record_retained_string(stats, &role);
    Ok(Some(RawContinueMessage {
        role: Some(role),
        content: content.text,
        calls: content.calls,
        timestamp,
    }))
}

#[derive(Default)]
struct ParsedMessageContent {
    text: Vec<String>,
    calls: Vec<RawContinueMessageCall>,
    admitted: bool,
}

enum ParsedContentBlock {
    Text(String),
    Call(RawContinueMessageCall),
}

fn parse_message_content(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<ParsedMessageContent, String> {
    match value.kind() {
        JsonKind::String => Ok(ParsedMessageContent {
            text: retained_unbounded_string(value, stats)?
                .into_iter()
                .collect(),
            calls: Vec::new(),
            admitted: true,
        }),
        JsonKind::Array => {
            let mut content = ParsedMessageContent::default();
            for block in value.as_array().map_err(scan_error)? {
                let block = block.map_err(scan_error)?;
                match block.kind() {
                    JsonKind::String => {
                        if let Some(text) = retained_unbounded_string(block, stats)? {
                            content.text.push(text);
                            content.admitted = true;
                        }
                    }
                    JsonKind::Object => {
                        if let Some(block) = parse_proven_content_block(block, stats)? {
                            content.admitted = true;
                            match block {
                                ParsedContentBlock::Text(text) => content.text.push(text),
                                ParsedContentBlock::Call(call) => content.calls.push(call),
                            }
                        }
                    }
                    JsonKind::Null => {}
                    JsonKind::Bool | JsonKind::Number | JsonKind::Array => {
                        record_unproven(stats, block);
                    }
                }
            }
            Ok(content)
        }
        JsonKind::Object => {
            let mut content = ParsedMessageContent::default();
            if let Some(block) = parse_proven_content_block(value, stats)? {
                content.admitted = true;
                match block {
                    ParsedContentBlock::Text(text) => content.text.push(text),
                    ParsedContentBlock::Call(call) => content.calls.push(call),
                }
            }
            Ok(content)
        }
        JsonKind::Null => Ok(ParsedMessageContent::default()),
        JsonKind::Bool | JsonKind::Number => {
            record_unproven(stats, value);
            Ok(ParsedMessageContent::default())
        }
    }
}

fn parse_proven_content_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<ParsedContentBlock>, String> {
    let mut admission = TagAdmission::Missing;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
            admission = admission.observe(value);
        }
    }
    match admission {
        TagAdmission::Text => {
            parse_text_block(block, stats).map(|text| text.map(ParsedContentBlock::Text))
        }
        TagAdmission::Call => {
            parse_message_call_block(block, stats).map(|call| call.map(ParsedContentBlock::Call))
        }
        TagAdmission::Result => {
            record_result(stats, block);
            Ok(None)
        }
        TagAdmission::Missing | TagAdmission::Context | TagAdmission::Unknown => {
            record_unproven(stats, block);
            Ok(None)
        }
    }
}

fn parse_text_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let mut text = None;
    let mut structurally_safe = true;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
        } else if key.is("text") || key.is("content") {
            if value.kind() == JsonKind::String && text.is_none() {
                text = Some(value);
            } else {
                structurally_safe = false;
            }
        } else {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, block);
        return Ok(None);
    }
    match text {
        Some(text) => retained_unbounded_string(text, stats),
        None => Ok(None),
    }
}

fn parse_message_call_block(
    block: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueMessageCall>, String> {
    let mut structurally_safe = true;
    let mut contains_result = false;
    let mut saw_id = false;
    let mut saw_name = false;
    let mut saw_function = false;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type")
            || key.is("kind")
            || key.is("id")
            || key.is("name")
            || key.is("function")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters")
        {
            if key.is("id") {
                structurally_safe &= !saw_id;
                saw_id = true;
            } else if key.is("name") {
                structurally_safe &= !saw_name;
                saw_name = true;
            } else if key.is("function") {
                structurally_safe &= !saw_function;
                saw_function = true;
            }
            if (key.is("id") || key.is("name"))
                && !matches!(value.kind(), JsonKind::String | JsonKind::Null)
                || key.is("function") && !matches!(value.kind(), JsonKind::Object | JsonKind::Null)
            {
                structurally_safe = false;
            }
        } else if key.is_result_like() {
            contains_result = true;
        } else {
            structurally_safe = false;
        }
    }
    if contains_result {
        record_result(stats, block);
        return Ok(None);
    }
    if !structurally_safe {
        record_unproven(stats, block);
        return Ok(None);
    }

    let mut id = None;
    let kind = Some("tool_call".to_owned());
    let mut direct_name = None;
    let mut function_name = None;
    for field in block.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("id") {
            id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("type") || key.is("kind") {
        } else if key.is("name") {
            direct_name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("function") {
            function_name = parse_tool_function(value, stats)?;
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
        }
    }
    Ok(Some(RawContinueMessageCall {
        id,
        kind,
        name: function_name.or(direct_name),
    }))
}

fn parse_context_items(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Vec<RawContinueContextItem>, String> {
    if value.kind() != JsonKind::Array {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(Vec::new());
    }
    let mut contexts = Vec::new();
    for context in value.as_array().map_err(scan_error)? {
        let context = context.map_err(scan_error)?;
        if context.kind() != JsonKind::Object {
            if context.kind() != JsonKind::Null {
                record_unproven(stats, context);
            }
            continue;
        }
        if let Some(context) = parse_context_item(context, stats)? {
            contexts.push(context);
        }
    }
    Ok(contexts)
}

fn parse_context_item(
    context: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueContextItem>, String> {
    let mut admission = TagAdmission::Missing;
    let mut name = None;
    let mut content = None;
    let mut description = None;
    let mut structurally_safe = true;
    for field in context.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("type") || key.is("kind") {
            admission = admission.observe(value);
        } else if key.is("name") {
            if value.kind() == JsonKind::String && name.is_none() {
                name = Some(value);
            } else {
                structurally_safe = false;
            }
        } else if key.is("content") {
            if value.kind() == JsonKind::String && content.is_none() {
                content = Some(value);
            } else {
                structurally_safe = false;
            }
        } else if key.is("description") {
            if description.is_some() {
                structurally_safe = false;
            }
            description = Some(value);
            structurally_safe &= matches!(value.kind(), JsonKind::String | JsonKind::Null);
        } else if key.is("uri") {
            structurally_safe &= matches!(value.kind(), JsonKind::String | JsonKind::Null);
        } else {
            structurally_safe = false;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, context);
        return Ok(None);
    }
    if name.is_some_and(is_result_tag) || description.is_some_and(is_result_tag) {
        record_result(stats, context);
        return Ok(None);
    }
    if !structurally_safe || !matches!(admission, TagAdmission::Text | TagAdmission::Context) {
        record_unproven(stats, context);
        return Ok(None);
    }
    let name = match name {
        Some(value) => retained_bounded_string(value, MAX_SESSION_METADATA_STRING_BYTES, stats)?,
        None => None,
    };
    let content = match content {
        Some(value) => retained_unbounded_string(value, stats)?,
        None => None,
    };
    Ok((name.is_some() || content.is_some()).then_some(RawContinueContextItem { name, content }))
}

fn parse_tool_call_states(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Vec<RawContinueToolCallState>, String> {
    if value.kind() != JsonKind::Array {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(Vec::new());
    }
    let mut states = Vec::new();
    for state in value.as_array().map_err(scan_error)? {
        let state = state.map_err(scan_error)?;
        if state.kind() != JsonKind::Object {
            if state.kind() != JsonKind::Null {
                record_unproven(stats, state);
            }
            continue;
        }
        if let Some(state) = parse_tool_call_state(state, stats)? {
            states.push(state);
        }
    }
    Ok(states)
}

fn parse_tool_call_state(
    state: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueToolCallState>, String> {
    let mut structurally_safe = true;
    let mut has_request_identity = false;
    let mut saw_tool_call_id = false;
    let mut saw_tool_call = false;
    let mut saw_status = false;
    let mut saw_exit_code = false;
    let mut saw_duration_ms = false;
    let mut saw_timed_out = false;
    for field in state.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is_result_like() {
            record_result(stats, value);
            continue;
        }
        let known = key.is("toolCallId")
            || key.is("toolCall")
            || key.is("status")
            || key.is("exitCode")
            || key.is("durationMs")
            || key.is("timedOut");
        let duplicate = if key.is("toolCallId") {
            let duplicate = saw_tool_call_id;
            saw_tool_call_id = true;
            duplicate
        } else if key.is("toolCall") {
            let duplicate = saw_tool_call;
            saw_tool_call = true;
            duplicate
        } else if key.is("status") {
            let duplicate = saw_status;
            saw_status = true;
            duplicate
        } else if key.is("exitCode") {
            let duplicate = saw_exit_code;
            saw_exit_code = true;
            duplicate
        } else if key.is("durationMs") {
            let duplicate = saw_duration_ms;
            saw_duration_ms = true;
            duplicate
        } else if key.is("timedOut") {
            let duplicate = saw_timed_out;
            saw_timed_out = true;
            duplicate
        } else {
            false
        };
        has_request_identity |= key.is("toolCallId") || key.is("toolCall");
        let wrong_kind = (key.is("toolCallId") || key.is("status"))
            && !matches!(value.kind(), JsonKind::String | JsonKind::Null)
            || key.is("toolCall") && !matches!(value.kind(), JsonKind::Object | JsonKind::Null)
            || (key.is("exitCode") || key.is("durationMs"))
                && !matches!(value.kind(), JsonKind::Number | JsonKind::Null)
            || key.is("timedOut") && !matches!(value.kind(), JsonKind::Bool | JsonKind::Null);
        if !known || duplicate || wrong_kind {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, state);
        return Ok(None);
    }

    let mut tool_call_id = None;
    let mut tool_call = None;
    let mut status = None;
    let mut exit_code = None;
    let mut duration_ms = None;
    let mut timed_out = None;
    for field in state.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("toolCallId") {
            tool_call_id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("toolCall") {
            tool_call = parse_tool_call(value, stats)?;
        } else if key.is("status") {
            status = retained_bounded_string(value, MAX_TOOL_STATUS_BYTES, stats)?;
        } else if key.is("exitCode") {
            exit_code = decode_i64(value);
        } else if key.is("durationMs") {
            duration_ms = decode_i64(value);
        } else if key.is("timedOut") {
            timed_out = decode_bool(value);
        }
    }
    Ok(
        (has_request_identity && (tool_call_id.is_some() || tool_call.is_some())).then_some(
            RawContinueToolCallState {
                tool_call_id,
                tool_call,
                status,
                exit_code,
                duration_ms,
                timed_out,
            },
        ),
    )
}

fn parse_tool_call(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawContinueToolCall>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut structurally_safe = true;
    let mut admission = TagAdmission::Missing;
    let mut has_request_identity = false;
    let mut saw_id = false;
    let mut saw_name = false;
    let mut saw_function = false;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if key.is_result_like() {
            record_result(stats, field_value);
            continue;
        }
        if key.is("type") || key.is("kind") {
            admission = admission.observe(field_value);
        }
        let known = key.is("id")
            || key.is("type")
            || key.is("kind")
            || key.is("name")
            || key.is("function")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters");
        let duplicate = if key.is("id") {
            let duplicate = saw_id;
            saw_id = true;
            duplicate
        } else if key.is("name") {
            let duplicate = saw_name;
            saw_name = true;
            duplicate
        } else if key.is("function") {
            let duplicate = saw_function;
            saw_function = true;
            duplicate
        } else {
            false
        };
        has_request_identity |= key.is("id") || key.is("name") || key.is("function");
        let wrong_kind = (key.is("id") || key.is("type") || key.is("kind") || key.is("name"))
            && !matches!(field_value.kind(), JsonKind::String | JsonKind::Null)
            || key.is("function")
                && !matches!(field_value.kind(), JsonKind::Object | JsonKind::Null);
        if !known || duplicate || wrong_kind {
            structurally_safe = false;
        }
    }
    if admission == TagAdmission::Result {
        record_result(stats, value);
        return Ok(None);
    }
    if !structurally_safe
        || !has_request_identity
        || !matches!(admission, TagAdmission::Missing | TagAdmission::Call)
    {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut id = None;
    let mut kind = None;
    let mut name = None;
    let mut function_name = None;
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("id") {
            id = retained_bounded_string(value, MAX_CALL_ID_BYTES, stats)?;
        } else if key.is("type") || key.is("kind") {
            kind = Some("tool_call".to_owned());
        } else if key.is("name") {
            name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("function") {
            function_name = parse_tool_function(value, stats)?;
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
        }
    }
    let retained_request_identity = id.is_some() || name.is_some() || function_name.is_some();
    Ok(retained_request_identity.then_some(RawContinueToolCall {
        id,
        kind,
        name,
        function_name,
    }))
}

fn parse_tool_function(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    if value.kind() != JsonKind::Object {
        if value.kind() != JsonKind::Null {
            record_unproven(stats, value);
        }
        return Ok(None);
    }
    let mut structurally_safe = true;
    for field in value.as_object().map_err(scan_error)? {
        let (key, field_value) = field.map_err(scan_error)?;
        if !(key.is("name")
            || key.is("arguments")
            || key.is("input")
            || key.is("parameters")
            || key.is_result_like())
        {
            structurally_safe = false;
        }
        if key.is("name") && !matches!(field_value.kind(), JsonKind::String | JsonKind::Null) {
            structurally_safe = false;
        }
    }
    if !structurally_safe {
        record_unproven(stats, value);
        return Ok(None);
    }

    let mut name = None;
    for field in value.as_object().map_err(scan_error)? {
        let (key, value) = field.map_err(scan_error)?;
        if key.is("name") {
            name = retained_bounded_string(value, MAX_TOOL_NAME_BYTES, stats)?;
        } else if key.is("arguments") || key.is("input") || key.is("parameters") {
            record_call_body(stats, value);
        } else if key.is_result_like() {
            record_result(stats, value);
        }
    }
    Ok(name)
}

fn parse_timestamp(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<RawTimestamp>, String> {
    match value.kind() {
        JsonKind::String => Ok(retained_bounded_string(value, 128, stats)?.map(RawTimestamp::Text)),
        JsonKind::Number => Ok(decode_f64(value).map(RawTimestamp::Number)),
        JsonKind::Null | JsonKind::Bool | JsonKind::Array | JsonKind::Object => Ok(None),
    }
}

fn retained_bounded_string(
    value: JsonSpan<'_>,
    maximum_bytes: usize,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let value = decode_string(value, maximum_bytes).map_err(|error| error.to_string())?;
    if let Some(value) = value.as_ref() {
        record_retained_string(stats, value);
    }
    Ok(value)
}

fn retained_unbounded_string(
    value: JsonSpan<'_>,
    stats: &mut ContinueOutputExclusionStats,
) -> Result<Option<String>, String> {
    let value = decode_unbounded_string(value).map_err(|error| error.to_string())?;
    if let Some(value) = value.as_ref() {
        record_retained_string(stats, value);
    }
    Ok(value)
}

fn record_retained_string(stats: &mut ContinueOutputExclusionStats, value: &str) {
    stats.retained_decode_string_allocations =
        stats.retained_decode_string_allocations.saturating_add(1);
    stats.retained_decode_string_bytes = stats
        .retained_decode_string_bytes
        .saturating_add(u64::try_from(value.len()).unwrap_or(u64::MAX));
}

fn record_result(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.native_results_observed = stats.native_results_observed.saturating_add(1);
    stats.result_payload_bytes_skipped = stats
        .result_payload_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

fn record_unproven(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.unproven_payloads_skipped = stats.unproven_payloads_skipped.saturating_add(1);
    stats.result_payload_bytes_skipped = stats
        .result_payload_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

fn record_call_body(stats: &mut ContinueOutputExclusionStats, value: JsonSpan<'_>) {
    stats.call_body_bytes_skipped = stats
        .call_body_bytes_skipped
        .saturating_add(u64::try_from(value.encoded_len()).unwrap_or(u64::MAX));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownRole {
    User,
    Assistant,
    System,
    Developer,
}

impl KnownRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Developer => "developer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleAdmission {
    Missing,
    Retained(KnownRole),
    Result,
    Unknown,
    Conflict,
}

impl RoleAdmission {
    fn observe(self, role: JsonSpan<'_>) -> Self {
        let observed = if role.string_normalized_is("user") {
            Self::Retained(KnownRole::User)
        } else if role.string_normalized_is("assistant") {
            Self::Retained(KnownRole::Assistant)
        } else if role.string_normalized_is("system") {
            Self::Retained(KnownRole::System)
        } else if role.string_normalized_is("developer") {
            Self::Retained(KnownRole::Developer)
        } else if is_result_role(role) {
            Self::Result
        } else {
            Self::Unknown
        };
        match (self, observed) {
            (Self::Result, _) | (_, Self::Result) => Self::Result,
            (Self::Missing, observed) => observed,
            (Self::Retained(left), Self::Retained(right)) if left == right => Self::Retained(left),
            (Self::Unknown, Self::Unknown) => Self::Unknown,
            (Self::Conflict, _) | (_, Self::Conflict) => Self::Conflict,
            _ => Self::Conflict,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagAdmission {
    Missing,
    Text,
    Call,
    Context,
    Result,
    Unknown,
}

impl TagAdmission {
    fn observe(self, tag: JsonSpan<'_>) -> Self {
        let observed = if is_result_tag(tag) {
            Self::Result
        } else if is_safe_call_tag(tag) {
            Self::Call
        } else if is_safe_text_tag(tag) {
            Self::Text
        } else if is_safe_context_tag(tag) {
            Self::Context
        } else {
            Self::Unknown
        };
        match (self, observed) {
            (Self::Result, _) | (_, Self::Result) => Self::Result,
            (Self::Missing, observed) => observed,
            (left, right) if left == right => left,
            _ => Self::Unknown,
        }
    }
}

fn is_safe_text_tag(tag: JsonSpan<'_>) -> bool {
    ["text", "inputtext", "markdown", "message"]
        .iter()
        .any(|candidate| tag.string_normalized_is(candidate))
}

fn is_safe_context_tag(tag: JsonSpan<'_>) -> bool {
    ["text", "context", "file", "code", "snippet"]
        .iter()
        .any(|candidate| tag.string_normalized_is(candidate))
}

fn is_safe_call_tag(tag: JsonSpan<'_>) -> bool {
    [
        "tooluse",
        "toolcall",
        "function",
        "functioncall",
        "command",
        "shellcommand",
    ]
    .iter()
    .any(|candidate| tag.string_normalized_is(candidate))
}

fn is_result_tag(tag: JsonSpan<'_>) -> bool {
    [
        "toolresult",
        "tooloutput",
        "commandresult",
        "commandoutput",
        "shelloutput",
        "terminaloutput",
        "bashresult",
        "bashexecutionresult",
        "functioncalloutput",
        "customtoolcalloutput",
        "future",
        "futureoutput",
        "result",
        "output",
    ]
    .iter()
    .any(|candidate| tag.string_normalized_is(candidate))
}

fn is_result_role(role: JsonSpan<'_>) -> bool {
    [
        "tool",
        "toolresult",
        "tooloutput",
        "command",
        "commandresult",
        "commandoutput",
        "bashexecution",
        "shelloutput",
        "functionresult",
    ]
    .iter()
    .any(|candidate| role.string_normalized_is(candidate))
}

fn scan_error(error: impl std::fmt::Display) -> String {
    format!("invalid Continue JSON structure: {error}")
}
