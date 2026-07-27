//! Authoritative Hermes record layouts.
//!
//! Each `FieldSpec` is the single source for projection order, compatibility fallback,
//! accepted SQLite storage, hydration conversion, rejection labels, and named decode.

use std::collections::BTreeSet;

use rusqlite::{Connection, Row};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

/// Provider-owned SQLite values retained only for one bounded Hermes page.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum HermesSqliteValue {
    Null,
    Integer(i64),
    RealBits(u64),
    Text(String),
}

impl HermesSqliteValue {
    fn from_real(value: f64) -> Self {
        Self::RealBits(value.to_bits())
    }

    fn as_real(&self) -> Option<f64> {
        match self {
            Self::RealBits(bits) => Some(f64::from_bits(*bits)),
            Self::Null | Self::Integer(_) | Self::Text(_) => None,
        }
    }
}

#[derive(Clone)]
pub(super) struct HermesSchema {
    sessions: RecordLayout<SessionField>,
    messages: RecordLayout<MessageField>,
    message_visibility: String,
}

impl HermesSchema {
    pub(super) fn detect(conn: &Connection) -> Result<Self> {
        let session_columns = hermes_table_columns(
            conn,
            "sessions",
            "Hermes state.db is missing required sessions table",
            "Hermes sessions table",
            &["id", "source", "started_at"],
        )?;
        let message_columns = hermes_table_columns(
            conn,
            "messages",
            "Hermes state.db is missing required messages table",
            "Hermes messages table",
            &["id", "session_id", "role", "timestamp"],
        )?;
        Ok(Self {
            sessions: RecordLayout::resolve(&SESSION_FIELDS, &session_columns, "s")?,
            messages: RecordLayout::resolve(&MESSAGE_FIELDS, &message_columns, "m")?,
            message_visibility: hermes_message_visibility(&message_columns, "m"),
        })
    }

    pub(super) fn sessions(&self) -> &RecordLayout<SessionField> {
        &self.sessions
    }

    pub(super) fn messages(&self) -> &RecordLayout<MessageField> {
        &self.messages
    }

    pub(super) fn message_visibility(&self) -> &str {
        &self.message_visibility
    }
}

fn hermes_table_columns(
    conn: &Connection,
    table: &str,
    missing_table_error: &str,
    table_label: &str,
    required: &[&str],
) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, table)? {
        return Err(CaptureError::InvalidPayload(missing_table_error.to_owned()));
    }
    let columns = sqlite_table_columns(conn, table)?;
    ensure_sqlite_table_columns(&columns, table_label, required)?;
    Ok(columns)
}

#[derive(Clone)]
pub(super) struct RecordLayout<F> {
    fields: Vec<ResolvedField<F>>,
}

impl<F: Copy + Eq> RecordLayout<F> {
    fn resolve(
        specs: &'static [FieldSpec<F>],
        columns: &BTreeSet<String>,
        alias: &str,
    ) -> Result<Self> {
        let fields = specs
            .iter()
            .map(|spec| {
                let expression = if columns.contains(spec.column) {
                    format!("{alias}.{}", spec.column)
                } else {
                    spec.fallback_sql
                        .ok_or(CaptureError::SystemInvariant(
                            "Hermes required field is missing after schema validation",
                        ))?
                        .to_owned()
                };
                Ok(ResolvedField {
                    spec: *spec,
                    expression,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { fields })
    }

    pub(super) fn projection(&self) -> String {
        self.fields
            .iter()
            .map(|field| field.expression.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn retained_length_expr(&self) -> String {
        // octet_length returns integer metadata without copying a stored TEXT/BLOB value. The
        // candidate statements return only rowids, this sum, and a storage-class error code while
        // the SQLite length limit is temporarily raised; hydration runs after the cap is restored.
        self.fields
            .iter()
            .map(|field| format!("coalesce(octet_length({}), 0)", field.expression))
            .collect::<Vec<_>>()
            .join(" + ")
    }

    pub(super) fn storage_class_error_expr(&self) -> String {
        let checks = self
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                format!(
                    "when typeof({}) not in {} then {}",
                    field.expression,
                    field.spec.kind.accepted_sql(),
                    index + 1
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("case {checks} else 0 end")
    }

    pub(super) fn capture_values(
        &self,
        row: &Row<'_>,
        offset: usize,
    ) -> rusqlite::Result<Vec<HermesSqliteValue>> {
        self.fields
            .iter()
            .enumerate()
            .map(|(index, field)| field.spec.kind.capture_value(row, offset + index))
            .collect()
    }

    pub(super) fn rejected_column(&self, code: i64) -> Result<&'static str> {
        let index = usize::try_from(code)
            .ok()
            .and_then(|code| code.checked_sub(1))
            .ok_or(CaptureError::SystemInvariant(
                "Hermes SQLite storage-class rejection code is invalid",
            ))?;
        self.fields
            .get(index)
            .map(|field| field.spec.column)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes SQLite storage-class rejection code is invalid",
            ))
    }

    fn values<'layout, 'values>(
        &'layout self,
        values: &'values [HermesSqliteValue],
        offset: usize,
        exact: bool,
        invalid_count: &'static str,
    ) -> Result<RecordValues<'layout, 'values, F>> {
        let expected_end = offset.saturating_add(self.fields.len());
        if values.len() < expected_end || (exact && values.len() != expected_end) {
            return Err(CaptureError::SystemInvariant(invalid_count));
        }
        Ok(RecordValues {
            layout: self,
            values,
            offset,
        })
    }
}

#[derive(Clone)]
struct ResolvedField<F> {
    spec: FieldSpec<F>,
    expression: String,
}

#[derive(Clone, Copy)]
struct FieldSpec<F> {
    field: F,
    column: &'static str,
    fallback_sql: Option<&'static str>,
    kind: ValueKind,
}

impl<F> FieldSpec<F> {
    const fn required(field: F, column: &'static str, kind: ValueKind) -> Self {
        Self {
            field,
            column,
            fallback_sql: None,
            kind,
        }
    }

    const fn compatible(
        field: F,
        column: &'static str,
        fallback_sql: &'static str,
        kind: ValueKind,
    ) -> Self {
        Self {
            field,
            column,
            fallback_sql: Some(fallback_sql),
            kind,
        }
    }
}

#[derive(Clone, Copy)]
enum ValueKind {
    Text,
    OptionalText,
    Real,
    OptionalReal,
    Integer,
    OptionalInteger,
}

impl ValueKind {
    fn accepted_sql(self) -> &'static str {
        match self {
            Self::Text => "('text')",
            Self::OptionalText => "('null', 'text')",
            Self::Real => "('integer', 'real')",
            Self::OptionalReal => "('null', 'integer', 'real')",
            Self::Integer => "('integer')",
            Self::OptionalInteger => "('null', 'integer')",
        }
    }

    fn capture_value(self, row: &Row<'_>, index: usize) -> rusqlite::Result<HermesSqliteValue> {
        match self {
            Self::Text => row.get(index).map(HermesSqliteValue::Text),
            Self::OptionalText => row
                .get::<_, Option<String>>(index)
                .map(|value| value.map_or(HermesSqliteValue::Null, HermesSqliteValue::Text)),
            Self::Real => row.get(index).map(HermesSqliteValue::from_real),
            Self::OptionalReal => row
                .get::<_, Option<f64>>(index)
                .map(|value| value.map_or(HermesSqliteValue::Null, HermesSqliteValue::from_real)),
            Self::Integer => row.get(index).map(HermesSqliteValue::Integer),
            Self::OptionalInteger => row
                .get::<_, Option<i64>>(index)
                .map(|value| value.map_or(HermesSqliteValue::Null, HermesSqliteValue::Integer)),
        }
    }
}

struct RecordValues<'layout, 'values, F> {
    layout: &'layout RecordLayout<F>,
    values: &'values [HermesSqliteValue],
    offset: usize,
}

impl<F: Copy + Eq> RecordValues<'_, '_, F> {
    fn value(&self, field: F) -> Result<&HermesSqliteValue> {
        let index = self
            .layout
            .fields
            .iter()
            .position(|resolved| resolved.spec.field == field)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes record layout is missing a decoded field",
            ))?;
        self.values
            .get(self.offset + index)
            .ok_or(CaptureError::SystemInvariant(
                "Hermes record layout points outside its logical values",
            ))
    }

    fn text(&self, field: F) -> Result<&str> {
        match self.value(field)? {
            HermesSqliteValue::Text(value) => Ok(value),
            _ => Err(CaptureError::SystemInvariant(
                "Hermes logical row has an invalid text value",
            )),
        }
    }

    fn optional_text(&self, field: F) -> Result<Option<String>> {
        match self.value(field)? {
            HermesSqliteValue::Null => Ok(None),
            HermesSqliteValue::Text(value) => Ok(Some(value.clone())),
            _ => Err(CaptureError::SystemInvariant(
                "Hermes logical row has an invalid optional text value",
            )),
        }
    }

    fn integer(&self, field: F) -> Result<i64> {
        match self.value(field)? {
            HermesSqliteValue::Integer(value) => Ok(*value),
            _ => Err(CaptureError::SystemInvariant(
                "Hermes logical row has an invalid integer value",
            )),
        }
    }

    fn optional_integer(&self, field: F) -> Result<Option<i64>> {
        match self.value(field)? {
            HermesSqliteValue::Null => Ok(None),
            HermesSqliteValue::Integer(value) => Ok(Some(*value)),
            _ => Err(CaptureError::SystemInvariant(
                "Hermes logical row has an invalid optional integer value",
            )),
        }
    }

    fn real(&self, field: F) -> Result<f64> {
        self.value(field)?
            .as_real()
            .ok_or(CaptureError::SystemInvariant(
                "Hermes logical row has an invalid real value",
            ))
    }

    fn optional_real(&self, field: F) -> Result<Option<f64>> {
        match self.value(field)? {
            HermesSqliteValue::Null => Ok(None),
            value => value
                .as_real()
                .map(Some)
                .ok_or(CaptureError::SystemInvariant(
                    "Hermes logical row has an invalid optional real value",
                )),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionField {
    Id,
    Source,
    ParentSessionId,
    Model,
    ModelConfig,
    StartedAt,
    EndedAt,
    EndReason,
    MessageCount,
    ToolCallCount,
    InputTokens,
    OutputTokens,
    CacheReadTokens,
    CacheWriteTokens,
    ReasoningTokens,
    Cwd,
    GitBranch,
    GitRepoRoot,
    BillingProvider,
    BillingBaseUrl,
    BillingMode,
    EstimatedCostUsd,
    ActualCostUsd,
    Title,
    Archived,
}

const SESSION_FIELDS: [FieldSpec<SessionField>; 25] = [
    FieldSpec::required(SessionField::Id, "id", ValueKind::Text),
    FieldSpec::required(SessionField::Source, "source", ValueKind::Text),
    FieldSpec::compatible(
        SessionField::ParentSessionId,
        "parent_session_id",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::Model,
        "model",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::ModelConfig,
        "model_config",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::required(SessionField::StartedAt, "started_at", ValueKind::Real),
    FieldSpec::compatible(
        SessionField::EndedAt,
        "ended_at",
        "NULL",
        ValueKind::OptionalReal,
    ),
    FieldSpec::compatible(
        SessionField::EndReason,
        "end_reason",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::MessageCount,
        "message_count",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::ToolCallCount,
        "tool_call_count",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::InputTokens,
        "input_tokens",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::OutputTokens,
        "output_tokens",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::CacheReadTokens,
        "cache_read_tokens",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::CacheWriteTokens,
        "cache_write_tokens",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(
        SessionField::ReasoningTokens,
        "reasoning_tokens",
        "0",
        ValueKind::Integer,
    ),
    FieldSpec::compatible(SessionField::Cwd, "cwd", "NULL", ValueKind::OptionalText),
    FieldSpec::compatible(
        SessionField::GitBranch,
        "git_branch",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::GitRepoRoot,
        "git_repo_root",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::BillingProvider,
        "billing_provider",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::BillingBaseUrl,
        "billing_base_url",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::BillingMode,
        "billing_mode",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        SessionField::EstimatedCostUsd,
        "estimated_cost_usd",
        "NULL",
        ValueKind::OptionalReal,
    ),
    FieldSpec::compatible(
        SessionField::ActualCostUsd,
        "actual_cost_usd",
        "NULL",
        ValueKind::OptionalReal,
    ),
    FieldSpec::compatible(
        SessionField::Title,
        "title",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(SessionField::Archived, "archived", "0", ValueKind::Integer),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageField {
    Id,
    SessionId,
    Role,
    Content,
    ToolCallId,
    ToolCalls,
    ToolName,
    Timestamp,
    TokenCount,
    FinishReason,
    Reasoning,
    ReasoningContent,
    ReasoningDetails,
    CodexReasoningItems,
    CodexMessageItems,
    PlatformMessageId,
    Observed,
    Active,
    Compacted,
}

const MESSAGE_FIELDS: [FieldSpec<MessageField>; 19] = [
    FieldSpec::required(MessageField::Id, "id", ValueKind::Integer),
    FieldSpec::required(MessageField::SessionId, "session_id", ValueKind::Text),
    FieldSpec::required(MessageField::Role, "role", ValueKind::Text),
    FieldSpec::compatible(
        MessageField::Content,
        "content",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::ToolCallId,
        "tool_call_id",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::ToolCalls,
        "tool_calls",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::ToolName,
        "tool_name",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::required(MessageField::Timestamp, "timestamp", ValueKind::Real),
    FieldSpec::compatible(
        MessageField::TokenCount,
        "token_count",
        "NULL",
        ValueKind::OptionalInteger,
    ),
    FieldSpec::compatible(
        MessageField::FinishReason,
        "finish_reason",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::Reasoning,
        "reasoning",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::ReasoningContent,
        "reasoning_content",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::ReasoningDetails,
        "reasoning_details",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::CodexReasoningItems,
        "codex_reasoning_items",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::CodexMessageItems,
        "codex_message_items",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(
        MessageField::PlatformMessageId,
        "platform_message_id",
        "NULL",
        ValueKind::OptionalText,
    ),
    FieldSpec::compatible(MessageField::Observed, "observed", "0", ValueKind::Integer),
    FieldSpec::compatible(MessageField::Active, "active", "1", ValueKind::Integer),
    FieldSpec::compatible(
        MessageField::Compacted,
        "compacted",
        "0",
        ValueKind::Integer,
    ),
];

fn hermes_message_visibility(columns: &BTreeSet<String>, alias: &str) -> String {
    if !columns.contains("active") && !columns.contains("compacted") {
        return String::new();
    }
    let active = qualified_or(columns, alias, "active", "1");
    let compacted = qualified_or(columns, alias, "compacted", "0");
    format!("({active} = 1 or {compacted} = 1)")
}

fn qualified_or(columns: &BTreeSet<String>, alias: &str, column: &str, fallback: &str) -> String {
    if columns.contains(column) {
        format!("{alias}.{column}")
    } else {
        fallback.to_owned()
    }
}

#[derive(Debug)]
pub(super) struct HermesSessionRow {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) parent_session_id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) model_config: Option<String>,
    pub(super) started_at: f64,
    pub(super) ended_at: Option<f64>,
    pub(super) end_reason: Option<String>,
    pub(super) message_count: i64,
    pub(super) tool_call_count: i64,
    pub(super) input_tokens: i64,
    pub(super) output_tokens: i64,
    pub(super) cache_read_tokens: i64,
    pub(super) cache_write_tokens: i64,
    pub(super) reasoning_tokens: i64,
    pub(super) cwd: Option<String>,
    pub(super) git_branch: Option<String>,
    pub(super) git_repo_root: Option<String>,
    pub(super) billing_provider: Option<String>,
    pub(super) billing_base_url: Option<String>,
    pub(super) billing_mode: Option<String>,
    pub(super) estimated_cost_usd: Option<f64>,
    pub(super) actual_cost_usd: Option<f64>,
    pub(super) title: Option<String>,
    pub(super) archived: i64,
}

#[derive(Debug, Clone)]
pub(super) struct HermesMessageRow {
    pub(super) id: i64,
    pub(super) session_id: String,
    pub(super) role: String,
    pub(super) content: Option<String>,
    pub(super) tool_call_id: Option<String>,
    pub(super) tool_calls: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) timestamp: f64,
    pub(super) token_count: Option<i64>,
    pub(super) finish_reason: Option<String>,
    pub(super) reasoning: Option<String>,
    pub(super) reasoning_content: Option<String>,
    pub(super) reasoning_details: Option<String>,
    pub(super) codex_reasoning_items: Option<String>,
    pub(super) codex_message_items: Option<String>,
    pub(super) platform_message_id: Option<String>,
    pub(super) observed: i64,
    pub(super) active: i64,
    pub(super) compacted: i64,
}

pub(super) fn decode_hermes_session(
    schema: &HermesSchema,
    values: &[HermesSqliteValue],
    offset: usize,
) -> Result<HermesSessionRow> {
    let values = schema.sessions.values(
        values,
        offset,
        false,
        "Hermes session logical row has an invalid value count",
    )?;
    Ok(HermesSessionRow {
        id: values.text(SessionField::Id)?.to_owned(),
        source: values.text(SessionField::Source)?.to_owned(),
        parent_session_id: values.optional_text(SessionField::ParentSessionId)?,
        model: values.optional_text(SessionField::Model)?,
        model_config: values.optional_text(SessionField::ModelConfig)?,
        started_at: values.real(SessionField::StartedAt)?,
        ended_at: values.optional_real(SessionField::EndedAt)?,
        end_reason: values.optional_text(SessionField::EndReason)?,
        message_count: values.integer(SessionField::MessageCount)?,
        tool_call_count: values.integer(SessionField::ToolCallCount)?,
        input_tokens: values.integer(SessionField::InputTokens)?,
        output_tokens: values.integer(SessionField::OutputTokens)?,
        cache_read_tokens: values.integer(SessionField::CacheReadTokens)?,
        cache_write_tokens: values.integer(SessionField::CacheWriteTokens)?,
        reasoning_tokens: values.integer(SessionField::ReasoningTokens)?,
        cwd: values.optional_text(SessionField::Cwd)?,
        git_branch: values.optional_text(SessionField::GitBranch)?,
        git_repo_root: values.optional_text(SessionField::GitRepoRoot)?,
        billing_provider: values.optional_text(SessionField::BillingProvider)?,
        billing_base_url: values.optional_text(SessionField::BillingBaseUrl)?,
        billing_mode: values.optional_text(SessionField::BillingMode)?,
        estimated_cost_usd: values.optional_real(SessionField::EstimatedCostUsd)?,
        actual_cost_usd: values.optional_real(SessionField::ActualCostUsd)?,
        title: values.optional_text(SessionField::Title)?,
        archived: values.integer(SessionField::Archived)?,
    })
}

pub(super) fn decode_hermes_message(
    schema: &HermesSchema,
    values: &[HermesSqliteValue],
) -> Result<HermesMessageRow> {
    let values = schema.messages.values(
        values,
        0,
        true,
        "Hermes message logical row has an invalid value count",
    )?;
    Ok(HermesMessageRow {
        id: values.integer(MessageField::Id)?,
        session_id: values.text(MessageField::SessionId)?.to_owned(),
        role: values.text(MessageField::Role)?.to_owned(),
        content: values.optional_text(MessageField::Content)?,
        tool_call_id: values.optional_text(MessageField::ToolCallId)?,
        tool_calls: values.optional_text(MessageField::ToolCalls)?,
        tool_name: values.optional_text(MessageField::ToolName)?,
        timestamp: values.real(MessageField::Timestamp)?,
        token_count: values.optional_integer(MessageField::TokenCount)?,
        finish_reason: values.optional_text(MessageField::FinishReason)?,
        reasoning: values.optional_text(MessageField::Reasoning)?,
        reasoning_content: values.optional_text(MessageField::ReasoningContent)?,
        reasoning_details: values.optional_text(MessageField::ReasoningDetails)?,
        codex_reasoning_items: values.optional_text(MessageField::CodexReasoningItems)?,
        codex_message_items: values.optional_text(MessageField::CodexMessageItems)?,
        platform_message_id: values.optional_text(MessageField::PlatformMessageId)?,
        observed: values.integer(MessageField::Observed)?,
        active: values.integer(MessageField::Active)?,
        compacted: values.integer(MessageField::Compacted)?,
    })
}
