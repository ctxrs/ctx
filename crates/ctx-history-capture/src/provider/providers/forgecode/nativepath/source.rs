use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use chrono::Duration;
use ctx_history_core::EventType;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        normalization::{provider_capped_json_value, provider_line_from_index},
        sqlite::{
            ensure_sqlite_table_columns, open_provider_sqlite_readonly, optional_column_expr,
            sqlite_schema_fingerprint, sqlite_table_columns, ProviderSqliteSourceSnapshot,
            ReadOnlySqliteConnection, SqliteLengthPreflightGuard,
        },
    },
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputOutcomeMetadata, OutputSourceLocator, ProOutputObservation, ProviderAdapterContext,
    ProviderImportFailure, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::super::complete_content::ForgeCodeCompleteContentDigest;
use super::super::event::{
    forgecode_event, forgecode_event_type, forgecode_for_each_metric_file_touch_with_limit,
    forgecode_message_parts, forgecode_message_text, forgecode_normalized_result_content,
    forgecode_timestamp, forgecode_tool_result_call_id, forgecode_tool_result_is_error,
    ForgeCodeFileTouch, ForgeCodeNativeEvent,
};

pub(super) const FORGECODE_NATIVE_PARSER_REVISION: u32 = 1;
pub(super) const FORGECODE_NATIVE_POLICY_REVISION: u32 = 6;
pub(super) const FORGECODE_NATIVE_FRONTIER_VERSION: u32 = 1;
pub(super) const FORGECODE_NATIVE_LOCATOR_KIND: &str = "forgecode-conversation-row-v1";
pub(super) const FORGECODE_NATIVE_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const FORGECODE_NATIVE_MAX_MESSAGES_PER_PAGE: usize = 16;
const FORGECODE_NATIVE_MAX_TOUCHES_PER_MESSAGE: usize = 64;
const FORGECODE_NATIVE_MAX_METRIC_TOUCHES: usize = 64;
const FORGECODE_NATIVE_MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const FORGECODE_NATIVE_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 8;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeFrontier {
    pub(super) rowid: Option<i64>,
    pub(super) next_message: u32,
    pub(super) row_complete: bool,
}

impl ForgeCodeFrontier {
    pub(in crate::provider::providers::forgecode) const fn initial() -> Self {
        Self {
            rowid: None,
            next_message: 0,
            row_complete: true,
        }
    }
}

#[derive(Clone)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeSourceObservation {
    pub(super) canonical_path: PathBuf,
    pub(super) snapshot: ProviderSqliteSourceSnapshot,
    pub(super) source_revision: String,
    pub(super) schema_fingerprint: String,
    pub(super) user_version: i64,
    columns: BTreeSet<String>,
}

pub(in crate::provider::providers::forgecode) enum ForgeCodeDiscovery {
    Live(ForgeCodeSourceObservation),
    Missing(PathBuf),
}

pub(in crate::provider::providers::forgecode) fn discover_forgecode_source(
    path: &Path,
) -> Result<ForgeCodeDiscovery> {
    let candidate = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            path.join(".forge.db")
        }
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let missing = if path.extension().is_some_and(|extension| extension == "db") {
                path.to_path_buf()
            } else {
                path.join(".forge.db")
            };
            return Ok(ForgeCodeDiscovery::Missing(absolute_path(&missing)?));
        }
        Err(error) => return Err(error.into()),
    };
    match fs::symlink_metadata(&candidate) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ForgeCodeDiscovery::Missing(absolute_path(&candidate)?));
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: candidate,
                reason: "ForgeCode SQLite source must be a regular non-symlink file",
            });
        }
        Ok(_) => {}
    }
    let canonical_path = fs::canonicalize(&candidate)?;
    let snapshot = ProviderSqliteSourceSnapshot::read(
        &canonical_path,
        "ForgeCode SQLite source must be a regular non-symlink file",
        "ForgeCode SQLite sidecar must be a regular non-symlink file",
    )?;
    let conn = open_provider_sqlite_readonly(&canonical_path)?;
    if !snapshot.revalidate(&canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let columns = sqlite_table_columns(&conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "ForgeCode conversations table",
        &["conversation_id", "workspace_id", "created_at"],
    )?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let user_version = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let source_revision = format!(
        "forgecode-nativepath-v1:parser={FORGECODE_NATIVE_PARSER_REVISION};policy={FORGECODE_NATIVE_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component()
    );
    if !snapshot.revalidate(&canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(ForgeCodeDiscovery::Live(ForgeCodeSourceObservation {
        canonical_path,
        snapshot,
        source_revision,
        schema_fingerprint,
        user_version,
        columns,
    }))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(in crate::provider::providers::forgecode) struct ForgeCodeScanner {
    source: ForgeCodeSourceObservation,
    conn: ReadOnlySqliteConnection,
    frontier: ForgeCodeFrontier,
    context: ProviderAdapterContext,
    source_root: Option<String>,
    wants_outputs: bool,
    exhausted: bool,
}

impl ForgeCodeScanner {
    pub(in crate::provider::providers::forgecode) fn new(
        source: ForgeCodeSourceObservation,
        frontier: ForgeCodeFrontier,
        context: ProviderAdapterContext,
        wants_outputs: bool,
    ) -> Result<Self> {
        let conn = open_provider_sqlite_readonly(&source.canonical_path)?;
        let source_root = context.source_root_display().or_else(|| {
            source
                .canonical_path
                .parent()
                .map(|path| path.display().to_string())
        });
        Ok(Self {
            source,
            conn,
            frontier,
            context,
            source_root,
            wants_outputs,
            exhausted: false,
        })
    }

    pub(in crate::provider::providers::forgecode) fn next_page(
        &mut self,
    ) -> Result<Option<ForgeCodePage>> {
        if self.exhausted {
            return Ok(None);
        }
        if !self
            .source
            .snapshot
            .revalidate(&self.source.canonical_path)?
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_frontier = self.frontier.clone();
        let candidate = self.next_candidate()?;
        let Some(candidate) = candidate else {
            self.exhausted = true;
            let page = ForgeCodePage {
                expected_frontier: expected_frontier.clone(),
                next_frontier: expected_frontier,
                terminal: true,
                row: None,
                events: Vec::new(),
                outputs: Vec::new(),
                touches: Vec::new(),
                rejections: Vec::new(),
                retained_bytes: 512,
            };
            return Ok(Some(page));
        };
        let page = self.page_for_candidate(expected_frontier, candidate)?;
        self.frontier = page.next_frontier.clone();
        if page.terminal {
            self.exhausted = true;
        }
        if !self
            .source
            .snapshot
            .revalidate(&self.source.canonical_path)?
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(Some(page))
    }

    fn next_candidate(&self) -> Result<Option<ForgeCodeRowCandidate>> {
        if self.frontier.rowid.is_some() && !self.frontier.row_complete {
            return self.candidate_at(self.frontier.rowid);
        }
        self.candidate_after(self.frontier.rowid)
    }

    fn candidate_at(&self, rowid: Option<i64>) -> Result<Option<ForgeCodeRowCandidate>> {
        let rowid = rowid.ok_or(CaptureError::SystemInvariant(
            "ForgeCode partial frontier has no rowid",
        ))?;
        let sql = self.candidate_sql("where rowid = ?1");
        with_length_preflight(&self.conn, || {
            self.conn.query_row(&sql, [rowid], row_candidate).optional()
        })
    }

    fn candidate_after(&self, rowid: Option<i64>) -> Result<Option<ForgeCodeRowCandidate>> {
        let predicate = rowid.map_or("", |_| "where rowid > ?1");
        let sql = self.candidate_sql(predicate);
        with_length_preflight(&self.conn, || match rowid {
            Some(rowid) => self.conn.query_row(&sql, [rowid], row_candidate).optional(),
            None => self.conn.query_row(&sql, [], row_candidate).optional(),
        })
    }

    fn candidate_sql(&self, predicate: &str) -> String {
        let title = optional_column_expr(&self.source.columns, "title", "NULL");
        let context = optional_column_expr(&self.source.columns, "context", "NULL");
        let updated_at = optional_column_expr(&self.source.columns, "updated_at", "NULL");
        let metrics = optional_column_expr(&self.source.columns, "metrics", "NULL");
        let retained = retained_length_expr(&[
            "conversation_id",
            title,
            "CASE WHEN typeof(workspace_id) = 'integer' THEN NULL ELSE workspace_id END",
            context,
            "created_at",
            updated_at,
            metrics,
        ]);
        format!(
            "select rowid, {retained}, typeof(conversation_id), typeof({title}), \
             typeof(workspace_id), typeof({context}), typeof(created_at), \
             typeof({updated_at}), typeof({metrics}) from conversations {predicate} \
             order by rowid limit 1"
        )
    }

    fn page_for_candidate(
        &self,
        expected_frontier: ForgeCodeFrontier,
        candidate: ForgeCodeRowCandidate,
    ) -> Result<ForgeCodePage> {
        let row_line = provider_line_from_index(candidate.rowid.max(0) as u64);
        if let Some(reason) = candidate.rejection_reason() {
            return self.rejected_row_page(
                expected_frontier,
                candidate.rowid,
                row_line,
                reason.to_owned(),
            );
        }
        if candidate.observed_bytes()? > MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 {
            return self.rejected_row_page(
                expected_frontier,
                candidate.rowid,
                row_line,
                format!(
                    "ForgeCode conversation row exceeds the {}-byte hydration limit",
                    MAX_PROVIDER_SQLITE_VALUE_BYTES
                ),
            );
        }
        let hydrated = match self.hydrate(candidate.rowid) {
            Ok(row) => row,
            Err(error) => {
                return self.rejected_row_page(
                    expected_frontier,
                    candidate.rowid,
                    row_line,
                    error.to_string(),
                )
            }
        };
        self.project_row(expected_frontier, hydrated)
    }

    fn rejected_row_page(
        &self,
        expected_frontier: ForgeCodeFrontier,
        rowid: i64,
        line: usize,
        error: String,
    ) -> Result<ForgeCodePage> {
        let next_frontier = ForgeCodeFrontier {
            rowid: Some(rowid),
            next_message: 0,
            row_complete: true,
        };
        Ok(ForgeCodePage {
            expected_frontier,
            terminal: !self.has_row_after(rowid)?,
            next_frontier,
            row: None,
            events: Vec::new(),
            outputs: Vec::new(),
            touches: Vec::new(),
            rejections: vec![ProviderImportFailure { line, error }],
            retained_bytes: 1024,
        })
    }

    fn hydrate(&self, rowid: i64) -> Result<ForgeCodeHydratedRow> {
        let title = optional_column_expr(&self.source.columns, "title", "NULL");
        let context = optional_column_expr(&self.source.columns, "context", "NULL");
        let updated_at = optional_column_expr(&self.source.columns, "updated_at", "NULL");
        let metrics = optional_column_expr(&self.source.columns, "metrics", "NULL");
        let sql = format!(
            "select rowid, cast(conversation_id as blob), cast({title} as blob), \
             workspace_id, cast({context} as blob), cast(created_at as blob), \
             cast({updated_at} as blob), cast({metrics} as blob) \
             from conversations where rowid = ?1"
        );
        self.conn
            .query_row(&sql, [rowid], |row| {
                Ok(ForgeCodeHydratedRow {
                    rowid: row.get(0)?,
                    conversation_id: row.get(1)?,
                    title: row.get(2)?,
                    workspace_id: row.get(3)?,
                    context: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    metrics: row.get(7)?,
                })
            })
            .map_err(CaptureError::from)
    }

    fn project_row(
        &self,
        expected_frontier: ForgeCodeFrontier,
        hydrated: ForgeCodeHydratedRow,
    ) -> Result<ForgeCodePage> {
        let rowid = hydrated.rowid;
        let row_line = provider_line_from_index(rowid.max(0) as u64);
        let conversation_id = required_utf8(hydrated.conversation_id, "conversation_id")?;
        let title = optional_utf8(hydrated.title, "title")?;
        let created_at = required_utf8(hydrated.created_at, "created_at")?;
        let updated_at = optional_utf8(hydrated.updated_at, "updated_at")?;
        let context_raw = optional_utf8(hydrated.context, "context")?;
        let metrics_raw = optional_utf8(hydrated.metrics, "metrics")?;
        let mut rejections = Vec::new();
        let context_value = context_raw
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    rejections.push(ProviderImportFailure {
                        line: row_line,
                        error: format!(
                            "invalid JSON in ForgeCode conversations.context {conversation_id}: {error}"
                        ),
                    });
                    None
                }
            });
        let metrics_value = metrics_raw
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .and_then(|raw| match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(error) => {
                    rejections.push(ProviderImportFailure {
                        line: row_line,
                        error: format!(
                            "invalid JSON in ForgeCode conversations.metrics {conversation_id}: {error}"
                        ),
                    });
                    None
                }
            });
        let messages = context_value
            .as_ref()
            .and_then(|value| value.get("messages"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let start = if expected_frontier.rowid == Some(rowid) && !expected_frontier.row_complete {
            usize::try_from(expected_frontier.next_message).map_err(|_| {
                CaptureError::InvalidPayload(
                    "ForgeCode NativePath message frontier exceeds usize".to_owned(),
                )
            })?
        } else {
            0
        };
        if start > messages.len() {
            return Err(CaptureError::InvalidPayload(
                "ForgeCode NativePath message frontier exceeds the current row".to_owned(),
            ));
        }
        let started_at = forgecode_timestamp(Some(&created_at), self.context.imported_at);
        let ended_at = updated_at
            .as_deref()
            .map(|raw| forgecode_timestamp(Some(raw), started_at));
        let context_metadata = context_value
            .as_ref()
            .map(context_without_messages)
            .unwrap_or(Value::Null);
        let initiator = context_value
            .as_ref()
            .and_then(|value| value.get("initiator"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let complete_content = ForgeCodeCompleteContentDigest::new(
            rowid,
            &conversation_id,
            title.as_deref(),
            hydrated.workspace_id,
            context_raw.as_deref(),
            &created_at,
            updated_at.as_deref(),
            metrics_raw.as_deref(),
        )?;
        let row = ForgeCodeConversationRow {
            conversation_id,
            title,
            workspace_id: hydrated.workspace_id,
            created_at,
            updated_at,
            context_metadata,
            metrics_metadata: metrics_value
                .as_ref()
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
            context_message_count: messages.len(),
            initiator,
        };
        let mut events = Vec::new();
        let mut outputs = Vec::new();
        let mut touches = Vec::new();
        let mut retained_bytes = 2_048_usize.saturating_add(estimated_row_bytes(&row));
        let mut next_index = start;
        while next_index < messages.len()
            && next_index.saturating_sub(start) < FORGECODE_NATIVE_MAX_MESSAGES_PER_PAGE
        {
            let entry = &messages[next_index];
            let entry_bytes = serde_json::to_vec(entry)?.len();
            let parts = forgecode_message_parts(entry);
            let event_type = forgecode_event_type(parts);
            let output_outcome =
                (event_type == EventType::ToolOutput).then(|| output_outcome(parts));
            let output_content = output_outcome.as_ref().and_then(|_| {
                forgecode_normalized_result_content(parts.body).map(String::into_bytes)
            });
            let estimated = entry_bytes
                .saturating_add(output_content.as_ref().map(Vec::len).unwrap_or_default())
                .saturating_add(2_048);
            if next_index > start
                && retained_bytes.saturating_add(estimated) > FORGECODE_NATIVE_PAGE_MAX_BYTES
            {
                break;
            }
            let provider_event_index = u64::try_from(next_index)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let occurred_at =
                started_at + Duration::milliseconds(i64::try_from(next_index).unwrap_or(i64::MAX));
            let retained_failure = output_outcome.as_ref().is_some_and(|outcome| {
                matches!(
                    outcome.outcome,
                    OutputOutcome::Failure | OutputOutcome::Timeout
                )
            });
            if output_outcome.is_none() || retained_failure {
                if entry_bytes > FORGECODE_NATIVE_MAX_EVENT_BYTES {
                    rejections.push(ProviderImportFailure {
                        line: provider_line_from_index(provider_event_index),
                        error: format!(
                            "ForgeCode message {provider_event_index} exceeds the {FORGECODE_NATIVE_MAX_EVENT_BYTES}-byte retained-event limit"
                        ),
                    });
                } else {
                    let mut event = forgecode_event(
                        &row.conversation_id,
                        entry,
                        provider_event_index,
                        occurred_at,
                    );
                    if output_outcome.is_none() {
                        complete_content.attach_message(&mut event, || {
                            forgecode_message_text(parts, event_type)
                        })?;
                    }
                    if let Some(metadata) = event.metadata.as_object_mut() {
                        metadata.insert(
                            "source_record_ordinal".to_owned(),
                            Value::from(ordered_rowid(rowid)),
                        );
                        metadata.insert(
                            "source_record_subrecord_index".to_owned(),
                            Value::from(u32::try_from(next_index).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "ForgeCode message index exceeds u32".to_owned(),
                                )
                            })?),
                        );
                    }
                    events.push(ForgeCodeRetainedEvent {
                        event,
                        provider_event_index,
                    });
                }
            }
            if self.wants_outputs {
                if let Some(outcome) = output_outcome {
                    let content = output_content.unwrap_or_default();
                    if content.len() > FORGECODE_NATIVE_MAX_OUTPUT_BYTES {
                        rejections.push(ProviderImportFailure {
                            line: provider_line_from_index(provider_event_index),
                            error: format!(
                                "ForgeCode output {provider_event_index} exceeds the {FORGECODE_NATIVE_MAX_OUTPUT_BYTES}-byte transient-output limit"
                            ),
                        });
                    } else {
                        outputs.push(ProOutputObservation {
                            kind: OutputObservationKind::Tool,
                            coordinate: OutputNativeCoordinate {
                                unit_key: format!(
                                    "forgecode:{}:message:{next_index:010}:output",
                                    row.conversation_id
                                ),
                                native_sequence: ordered_rowid(rowid),
                                native_record_id: Some(format!(
                                    "conversation:{}:message:{provider_event_index}",
                                    row.conversation_id
                                )),
                                source_record_ordinal: Some(ordered_rowid(rowid)),
                                source_record_subrecord_index: Some(
                                    u32::try_from(next_index).map_err(|_| {
                                        CaptureError::InvalidPayload(
                                            "ForgeCode message index exceeds u32".to_owned(),
                                        )
                                    })?,
                                ),
                                byte_start: None,
                                byte_end_exclusive: None,
                            },
                            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
                            associations: OutputAssociations {
                                direct_session_id: row.conversation_id.clone(),
                                root_session_id: row.conversation_id.clone(),
                                parent_session_id: None,
                                provider_session_id: Some(row.conversation_id.clone()),
                                agent_id: row.initiator.clone(),
                                repository: None,
                            },
                            call_id: forgecode_tool_result_call_id(parts),
                            command: None,
                            outcome,
                            locator: OutputSourceLocator {
                                version: 1,
                                kind: FORGECODE_NATIVE_LOCATOR_KIND.to_owned(),
                                payload: rowid.to_be_bytes().to_vec(),
                            },
                            content,
                        });
                    }
                }
            }
            let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                entry,
                event_type_supports_structured_file_touches(event_type),
                FORGECODE_NATIVE_MAX_TOUCHES_PER_MESSAGE,
                |(touch_ordinal, touch)| {
                    let provider_touch_index =
                        if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                            touch_ordinal
                        } else {
                            (provider_event_index << 16) | touch_ordinal
                        };
                    touches.push(ForgeCodeFileTouch {
                        provider_touch_index,
                        provider_event_index: Some(provider_event_index),
                        raw_source_path: Some(self.source.canonical_path.display().to_string()),
                        source_root: self.source_root.clone(),
                        path: touch.path,
                        change_kind: touch.change_kind,
                        old_path: touch.old_path,
                        line_count_delta: None,
                        confidence: touch.confidence,
                        occurred_at,
                        metadata: touch.metadata,
                    });
                    Ok::<(), CaptureError>(())
                },
            )?;
            if touch_outcome.limit_exceeded() {
                rejections.push(ProviderImportFailure {
                    line: provider_line_from_index(provider_event_index),
                    error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                });
            }
            retained_bytes = retained_bytes.saturating_add(estimated);
            next_index = next_index.saturating_add(1);
        }
        let row_complete = next_index == messages.len();
        if row_complete {
            if let Some(metrics) = metrics_value.as_ref() {
                let limit_exceeded = forgecode_for_each_metric_file_touch_with_limit(
                    metrics,
                    &self.source.canonical_path.display().to_string(),
                    ended_at.unwrap_or(started_at),
                    FORGECODE_NATIVE_MAX_METRIC_TOUCHES,
                    |(_, mut touch)| {
                        touch.source_root.clone_from(&self.source_root);
                        touches.push(touch);
                        Ok::<(), CaptureError>(())
                    },
                )?;
                if limit_exceeded {
                    rejections.push(ProviderImportFailure {
                        line: row_line,
                        error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                    });
                }
            }
        }
        retained_bytes = touches.iter().fold(retained_bytes, |bytes, touch| {
            bytes.saturating_add(
                serde_json::to_vec(touch)
                    .map(|encoded| encoded.len())
                    .unwrap_or(usize::MAX),
            )
        });
        if retained_bytes > FORGECODE_NATIVE_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "ForgeCode NativePath page exceeds its retained byte bound".to_owned(),
            ));
        }
        let next_frontier = ForgeCodeFrontier {
            rowid: Some(rowid),
            next_message: u32::try_from(next_index).map_err(|_| {
                CaptureError::InvalidPayload("ForgeCode message index exceeds u32".to_owned())
            })?,
            row_complete,
        };
        Ok(ForgeCodePage {
            expected_frontier,
            terminal: row_complete && !self.has_row_after(rowid)?,
            next_frontier,
            row: Some(row),
            events,
            outputs,
            touches,
            rejections,
            retained_bytes,
        })
    }

    fn has_row_after(&self, rowid: i64) -> Result<bool> {
        self.conn
            .query_row(
                "select exists(select 1 from conversations where rowid > ?1)",
                [rowid],
                |row| row.get::<_, i64>(0),
            )
            .map(|exists| exists != 0)
            .map_err(CaptureError::from)
    }
}

#[derive(Debug)]
pub(in crate::provider::providers::forgecode) struct ForgeCodePage {
    pub(in crate::provider::providers::forgecode) expected_frontier: ForgeCodeFrontier,
    pub(in crate::provider::providers::forgecode) next_frontier: ForgeCodeFrontier,
    pub(in crate::provider::providers::forgecode) terminal: bool,
    pub(in crate::provider::providers::forgecode) row: Option<ForgeCodeConversationRow>,
    pub(in crate::provider::providers::forgecode) events: Vec<ForgeCodeRetainedEvent>,
    pub(in crate::provider::providers::forgecode) outputs: Vec<ProOutputObservation>,
    pub(in crate::provider::providers::forgecode) touches: Vec<ForgeCodeFileTouch>,
    pub(in crate::provider::providers::forgecode) rejections: Vec<ProviderImportFailure>,
    pub(in crate::provider::providers::forgecode) retained_bytes: usize,
}

#[derive(Debug)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeRetainedEvent {
    pub(in crate::provider::providers::forgecode) event: ForgeCodeNativeEvent,
    pub(in crate::provider::providers::forgecode) provider_event_index: u64,
}

#[derive(Debug, Clone)]
pub(in crate::provider::providers::forgecode) struct ForgeCodeConversationRow {
    pub(in crate::provider::providers::forgecode) conversation_id: String,
    pub(in crate::provider::providers::forgecode) title: Option<String>,
    pub(in crate::provider::providers::forgecode) workspace_id: i64,
    pub(in crate::provider::providers::forgecode) created_at: String,
    pub(in crate::provider::providers::forgecode) updated_at: Option<String>,
    pub(in crate::provider::providers::forgecode) context_metadata: Value,
    pub(in crate::provider::providers::forgecode) metrics_metadata: Option<Value>,
    pub(in crate::provider::providers::forgecode) context_message_count: usize,
    pub(in crate::provider::providers::forgecode) initiator: Option<String>,
}

struct ForgeCodeHydratedRow {
    rowid: i64,
    conversation_id: Vec<u8>,
    title: Option<Vec<u8>>,
    workspace_id: i64,
    context: Option<Vec<u8>>,
    created_at: Vec<u8>,
    updated_at: Option<Vec<u8>>,
    metrics: Option<Vec<u8>>,
}

struct ForgeCodeRowCandidate {
    rowid: i64,
    retained_bytes: i64,
    storage_classes: [String; 7],
}

impl ForgeCodeRowCandidate {
    fn observed_bytes(&self) -> Result<u64> {
        let retained = u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "ForgeCode SQLite retained byte count must be nonnegative".to_owned(),
            )
        })?;
        FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(retained)
            .ok_or(CaptureError::SystemInvariant(
                "ForgeCode SQLite retained byte count overflowed",
            ))
    }

    fn rejection_reason(&self) -> Option<&'static str> {
        let [conversation_id, title, workspace_id, context, created_at, updated_at, metrics] =
            self.storage_classes.each_ref();
        let castable_required = |kind: &str| matches!(kind, "integer" | "real" | "text");
        let castable_optional = |kind: &str| kind == "null" || castable_required(kind);
        let optional_text = |kind: &str| matches!(kind, "null" | "text");
        if !castable_required(conversation_id) {
            Some("ForgeCode conversations.conversation_id has an unsupported SQLite storage class")
        } else if !optional_text(title) {
            Some("ForgeCode conversations.title has an unsupported SQLite storage class")
        } else if workspace_id != "integer" {
            Some("ForgeCode conversations.workspace_id has an unsupported SQLite storage class")
        } else if !optional_text(context) {
            Some("ForgeCode conversations.context has an unsupported SQLite storage class")
        } else if !castable_required(created_at) {
            Some("ForgeCode conversations.created_at has an unsupported SQLite storage class")
        } else if !castable_optional(updated_at) {
            Some("ForgeCode conversations.updated_at has an unsupported SQLite storage class")
        } else if !optional_text(metrics) {
            Some("ForgeCode conversations.metrics has an unsupported SQLite storage class")
        } else {
            None
        }
    }
}

fn row_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForgeCodeRowCandidate> {
    Ok(ForgeCodeRowCandidate {
        rowid: row.get(0)?,
        retained_bytes: row.get(1)?,
        storage_classes: [
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ],
    })
}

fn with_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

fn retained_length_expr(expressions: &[&str]) -> String {
    let terms = expressions
        .iter()
        .map(|expression| {
            format!(
                "CASE WHEN {expression} IS NULL THEN 0 \
                 ELSE coalesce(octet_length(CAST({expression} AS BLOB)), 0) END"
            )
        })
        .collect::<Vec<_>>();
    format!("({})", terms.join(" + "))
}

fn required_utf8(value: Vec<u8>, field: &str) -> Result<String> {
    String::from_utf8(value).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "ForgeCode conversations.{field} is not valid UTF-8"
        ))
    })
}

fn optional_utf8(value: Option<Vec<u8>>, field: &str) -> Result<Option<String>> {
    value.map(|value| required_utf8(value, field)).transpose()
}

fn context_without_messages(context: &Value) -> Value {
    let Some(object) = context.as_object() else {
        return Value::Null;
    };
    let metadata = object
        .iter()
        .filter(|(key, _)| key.as_str() != "messages")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    provider_capped_json_value(&Value::Object(metadata), PROVIDER_MAX_PREVIEW_CHARS)
}

fn estimated_row_bytes(row: &ForgeCodeConversationRow) -> usize {
    row.conversation_id
        .len()
        .saturating_add(row.title.as_deref().map(str::len).unwrap_or_default())
        .saturating_add(row.created_at.len())
        .saturating_add(row.updated_at.as_deref().map(str::len).unwrap_or_default())
        .saturating_add(
            serde_json::to_vec(&row.context_metadata)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX),
        )
        .saturating_add(
            row.metrics_metadata
                .as_ref()
                .and_then(|value| serde_json::to_vec(value).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(512)
}

fn output_outcome(parts: super::super::event::ForgeCodeMessageParts<'_>) -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: match forgecode_tool_result_is_error(parts) {
            Some(true) => OutputOutcome::Failure,
            Some(false) => OutputOutcome::Success,
            None => OutputOutcome::Unknown,
        },
        exit_code: None,
        duration_ms: None,
    }
}

pub(super) fn ordered_rowid(rowid: i64) -> u64 {
    (rowid as u64) ^ (1_u64 << 63)
}

pub(super) fn frontier_bytes(frontier: &ForgeCodeFrontier) -> Result<Vec<u8>> {
    serde_json::to_vec(frontier).map_err(CaptureError::from)
}
