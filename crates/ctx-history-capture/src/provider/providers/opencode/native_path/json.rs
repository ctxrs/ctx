use std::{
    collections::BTreeSet,
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use rusqlite::{
    functions::{Context, FunctionFlags},
    Connection,
};
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::{Map, Number, Value};

use crate::{CaptureError, Result};

use super::super::{normalization::opencode_event_time, schema::OPENCODE_SQLITE_DIALECT};
use super::model::{
    OpenCodeNativeProfile, OpenCodeNativeRejectionKind, OpenCodeNativeSchemaFamily,
};

const PROJECT_JSON_FUNCTION: &str = "ctx_opencode_nativepath_project_v1";
const PROJECT_TIMESTAMP_FUNCTION: &str = "ctx_opencode_nativepath_timestamp_rejection_v1";
const TAG_RETAINED: u8 = 1;
const TAG_EXCLUDED_OUTPUT: u8 = 2;
const TAG_MALFORMED_JSON: u8 = 3;
const TAG_MALFORMED_RESULT_JSON: u8 = 4;
const TAG_UNSUPPORTED_STORAGE: u8 = 5;
const TAG_OVERSIZED: u8 = 6;
const TAG_MISSING_SESSION: u8 = 7;
const TAG_MISSING_MESSAGE: u8 = 8;
const TAG_RELATIONSHIP_MISMATCH: u8 = 9;
const TAG_UNKNOWN_TYPE: u8 = 10;
const TAG_OUTPUT: u8 = 11;
const TAG_INVALID_TIMESTAMP: u8 = 12;
const OUTPUT_DIAGNOSTIC_PREVIEW_CHARS: usize = 4_096;
const MAX_OUTPUT_SUBRECORDS_PER_NATIVE_ROW: usize = 4_096;

pub(super) const OVERSIZED_PROJECTION: &[u8] = &[TAG_OVERSIZED];
pub(super) const MISSING_SESSION_PROJECTION: &[u8] = &[TAG_MISSING_SESSION];
pub(super) const MISSING_MESSAGE_PROJECTION: &[u8] = &[TAG_MISSING_MESSAGE];
pub(super) const RELATIONSHIP_MISMATCH_PROJECTION: &[u8] = &[TAG_RELATIONSHIP_MISMATCH];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeRetainedJson {
    pub(super) effective_type: String,
    pub(super) role: String,
    pub(super) body: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OpenCodeOutputDraft {
    pub(super) subrecord_index: u32,
    pub(super) kind: u8,
    pub(super) call_id: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) command: Option<String>,
    pub(super) working_directory: Option<String>,
    pub(super) outcome: u8,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeOutputJson {
    pub(super) diagnostic: Option<OpenCodeRetainedJson>,
    pub(super) outputs: Vec<OpenCodeOutputDraft>,
    pub(super) pro_rejection: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum OpenCodeJsonProjection {
    Retained(OpenCodeRetainedJson),
    Output(OpenCodeOutputJson),
    ExcludedOutput,
    Rejected(OpenCodeNativeRejectionKind),
    RejectedWithReason(OpenCodeNativeRejectionKind, String),
}

#[derive(Debug, Default)]
pub(super) struct OpenCodeJsonVisitorMetrics {
    records: AtomicU64,
    bytes: AtomicU64,
}

impl OpenCodeJsonVisitorMetrics {
    pub(super) fn records(&self) -> u64 {
        self.records.load(Ordering::Relaxed)
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
}

#[derive(Serialize, Deserialize)]
struct RetainedWire {
    effective_type: String,
    role: String,
    body: Value,
}

#[derive(Serialize, Deserialize)]
struct OutputWire {
    diagnostic: Option<RetainedWire>,
    outputs: Vec<OpenCodeOutputDraft>,
    pro_rejection: Option<String>,
}

pub(super) fn register_projection_function(
    conn: &Connection,
    profile: OpenCodeNativeProfile,
) -> Result<Arc<OpenCodeJsonVisitorMetrics>> {
    let metrics = Arc::new(OpenCodeJsonVisitorMetrics::default());
    let function_metrics = Arc::clone(&metrics);
    conn.create_scalar_function(
        PROJECT_JSON_FUNCTION,
        4,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| project_json_function(context, &function_metrics, profile),
    )?;
    conn.create_scalar_function(
        PROJECT_TIMESTAMP_FUNCTION,
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        timestamp_rejection_function,
    )?;
    Ok(metrics)
}

pub(super) fn projection_sql(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
    family: OpenCodeNativeSchemaFamily,
    max_bytes_parameter: &str,
    profile: OpenCodeNativeProfile,
) -> String {
    let parent_preflight = parent_data.map_or_else(String::new, |parent| {
        format!(
            "when typeof({parent}) <> 'text' then X'{unsupported}'
             when octet_length({parent}) > {max_bytes_parameter} then X'{oversized}'",
            unsupported = hex_tag(TAG_UNSUPPORTED_STORAGE),
            oversized = hex_tag(TAG_OVERSIZED),
        )
    });
    let parent_argument = parent_data.unwrap_or("null");
    let core_output_guard = if profile == OpenCodeNativeProfile::CoreOnly {
        let output = output_predicate_sql(data, column_type, parent_data);
        let failure = failure_predicate_sql(data, parent_data);
        format!(
            "when json_valid({data}) and {parent_valid}
                   and ({output}) and not ({failure})
             then X'{excluded}'",
            parent_valid = parent_data
                .map(|parent| format!("json_valid({parent})"))
                .unwrap_or_else(|| "1".to_owned()),
            excluded = hex_tag(TAG_EXCLUDED_OUTPUT),
        )
    } else {
        String::new()
    };
    let timestamp_rejection = if family != OpenCodeNativeSchemaFamily::MessagePart {
        format!(
            "when json_valid({data})
                  and json_type({data}, '$.time.created') is not null
                  and {PROJECT_TIMESTAMP_FUNCTION}(
                      json_type({data}, '$.time.created'),
                      json_extract({data}, '$.time.created')
                  ) is not null
             then {PROJECT_TIMESTAMP_FUNCTION}(
                 json_type({data}, '$.time.created'),
                 json_extract({data}, '$.time.created')
             )"
        )
    } else {
        String::new()
    };
    format!(
        "case
             when typeof({data}) <> 'text' then X'{unsupported}'
             when octet_length({data}) > {max_bytes_parameter} then X'{oversized}'
             {parent_preflight}
             {timestamp_rejection}
             {core_output_guard}
             else {PROJECT_JSON_FUNCTION}(
                 {data}, {column_type}, {parent_argument}, '{family}'
             )
         end",
        unsupported = hex_tag(TAG_UNSUPPORTED_STORAGE),
        oversized = hex_tag(TAG_OVERSIZED),
        family = family.label(),
    )
}

fn timestamp_rejection_function(context: &Context<'_>) -> rusqlite::Result<Option<Vec<u8>>> {
    let json_type = context.get::<String>(0)?;
    let error = if json_type == "integer" {
        let millis = context.get::<i64>(1)?;
        provider_required_event_timestamp(millis).err()
    } else {
        Some(CaptureError::InvalidPayload(
            "OpenCode event time.created must be integer millis".to_owned(),
        ))
    };
    Ok(error.map(|error| encode_rejection_reason(error.to_string())))
}

fn provider_required_event_timestamp(millis: i64) -> Result<()> {
    crate::provider::normalization::provider_required_timestamp_millis(
        millis,
        OPENCODE_SQLITE_DIALECT.event_time_created_field,
    )
    .map(|_| ())
}

pub(super) fn decode_projection(bytes: &[u8]) -> Result<OpenCodeJsonProjection> {
    let Some((&tag, payload)) = bytes.split_first() else {
        return Err(CaptureError::SystemInvariant(
            "OpenCode NativePath JSON projection is empty",
        ));
    };
    match tag {
        TAG_RETAINED => {
            let wire: RetainedWire = serde_json::from_slice(payload).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "OpenCode retained projection no longer decodes: {error}"
                ))
            })?;
            Ok(OpenCodeJsonProjection::Retained(OpenCodeRetainedJson {
                effective_type: wire.effective_type,
                role: wire.role,
                body: wire.body,
            }))
        }
        TAG_EXCLUDED_OUTPUT => Ok(OpenCodeJsonProjection::ExcludedOutput),
        TAG_OUTPUT => {
            let wire: OutputWire = serde_json::from_slice(payload).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "OpenCode output projection no longer decodes: {error}"
                ))
            })?;
            Ok(OpenCodeJsonProjection::Output(OpenCodeOutputJson {
                diagnostic: wire.diagnostic.map(|diagnostic| OpenCodeRetainedJson {
                    effective_type: diagnostic.effective_type,
                    role: diagnostic.role,
                    body: diagnostic.body,
                }),
                outputs: wire.outputs,
                pro_rejection: wire.pro_rejection,
            }))
        }
        TAG_MALFORMED_JSON => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::MalformedJson,
        )),
        TAG_MALFORMED_RESULT_JSON => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::MalformedResultJson,
        )),
        TAG_UNSUPPORTED_STORAGE => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::UnsupportedStorageClass,
        )),
        TAG_OVERSIZED => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::OversizedRetainedContent,
        )),
        TAG_MISSING_SESSION => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::MissingSession,
        )),
        TAG_MISSING_MESSAGE => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::MissingMessage,
        )),
        TAG_RELATIONSHIP_MISMATCH => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::SessionRelationshipMismatch,
        )),
        TAG_UNKNOWN_TYPE => Ok(OpenCodeJsonProjection::Rejected(
            OpenCodeNativeRejectionKind::UnknownRecordType,
        )),
        TAG_INVALID_TIMESTAMP => {
            let reason = serde_json::from_slice(payload).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "OpenCode timestamp rejection no longer decodes: {error}"
                ))
            })?;
            Ok(OpenCodeJsonProjection::RejectedWithReason(
                OpenCodeNativeRejectionKind::InvalidTimestamp,
                reason,
            ))
        }
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode NativePath JSON projection has an unknown tag",
        )),
    }
}

fn project_json_function(
    context: &Context<'_>,
    metrics: &OpenCodeJsonVisitorMetrics,
    profile: OpenCodeNativeProfile,
) -> rusqlite::Result<Vec<u8>> {
    let data = context.get::<String>(0)?;
    let column_type = context.get::<String>(1)?;
    let parent_data = context.get::<Option<String>>(2)?;
    let family_label = context.get::<String>(3)?;
    metrics.records.fetch_add(1, Ordering::Relaxed);
    let bytes = data
        .len()
        .saturating_add(parent_data.as_ref().map_or(0, String::len));
    metrics
        .bytes
        .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    Ok(project_json(
        &data,
        &column_type,
        parent_data.as_deref(),
        &family_label,
        profile,
    ))
}

fn project_json(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
    family_label: &str,
    profile: OpenCodeNativeProfile,
) -> Vec<u8> {
    let family = family_from_label(family_label);
    let direct_column_output = is_direct_output_token(column_type);
    let body = match audit_json(data) {
        Ok(value) => value,
        Err(()) => {
            return tag(if direct_column_output {
                TAG_MALFORMED_RESULT_JSON
            } else {
                TAG_MALFORMED_JSON
            });
        }
    };
    let parent = match parent_data.map(audit_json).transpose() {
        Ok(value) => value,
        Err(()) => {
            return tag(if direct_column_output {
                TAG_MALFORMED_RESULT_JSON
            } else {
                TAG_MALFORMED_JSON
            });
        }
    };
    if family != Some(OpenCodeNativeSchemaFamily::MessagePart) {
        if let Err(error) = opencode_event_time(&body.value, &OPENCODE_SQLITE_DIALECT) {
            return encode_rejection_reason(error.to_string());
        }
    }
    let body_type = object_text(&body.value, "type");
    let body_role = object_text(&body.value, "role");
    let parent_role = parent
        .as_ref()
        .and_then(|value| object_text(&value.value, "role"));
    let effective_type = effective_type(column_type, body_role, body_type, parent_role);
    let output = direct_column_output
        || is_direct_output_token(&effective_type)
        || body.forbidden_output
        || parent.as_ref().is_some_and(|value| value.forbidden_output);
    if output {
        if body.duplicate_key || parent.as_ref().is_some_and(|value| value.duplicate_key) {
            return encode_output(OutputWire {
                diagnostic: None,
                outputs: Vec::new(),
                pro_rejection: (profile == OpenCodeNativeProfile::CoreAndPro)
                    .then(|| "duplicate JSON key in an output-bearing native row".to_owned()),
            });
        }
        return project_output(&body.value, &effective_type, profile);
    }
    if body.duplicate_key || parent.as_ref().is_some_and(|value| value.duplicate_key) {
        return tag(TAG_MALFORMED_JSON);
    }
    if is_tool_token(&effective_type) {
        if tool_call_is_retained(&body.value) {
            // Continue below and retain the input-side projection.
        } else {
            return tag(TAG_EXCLUDED_OUTPUT);
        }
    } else if !is_retained_type(family, &effective_type) {
        return tag(TAG_UNKNOWN_TYPE);
    }
    let role = if family == Some(OpenCodeNativeSchemaFamily::MessagePart) {
        first_nonempty(&[parent_role, body_role])
    } else {
        first_nonempty(&[body_role, Some(effective_type.as_str()), parent_role])
    }
    .unwrap_or("assistant")
    .to_owned();
    let wire = RetainedWire {
        effective_type,
        role,
        body: body.value,
    };
    let mut encoded = vec![TAG_RETAINED];
    match serde_json::to_writer(&mut encoded, &wire) {
        Ok(()) => encoded,
        Err(_) => tag(TAG_MALFORMED_JSON),
    }
}

pub(super) fn encode_rejection_reason(reason: String) -> Vec<u8> {
    let mut encoded = vec![TAG_INVALID_TIMESTAMP];
    match serde_json::to_writer(&mut encoded, &reason) {
        Ok(()) => encoded,
        Err(_) => tag(TAG_MALFORMED_JSON),
    }
}

fn encode_output(wire: OutputWire) -> Vec<u8> {
    let mut encoded = vec![TAG_OUTPUT];
    match serde_json::to_writer(&mut encoded, &wire) {
        Ok(()) => encoded,
        Err(_) => tag(TAG_MALFORMED_RESULT_JSON),
    }
}

pub(super) fn encode_retained_projection(retained: &OpenCodeRetainedJson) -> Result<Vec<u8>> {
    let mut encoded = vec![TAG_RETAINED];
    serde_json::to_writer(
        &mut encoded,
        &RetainedWire {
            effective_type: retained.effective_type.clone(),
            role: retained.role.clone(),
            body: retained.body.clone(),
        },
    )
    .map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "OpenCode retained projection cannot be encoded: {error}"
        ))
    })?;
    Ok(encoded)
}

pub(super) fn excluded_output_projection() -> Vec<u8> {
    tag(TAG_EXCLUDED_OUTPUT)
}

fn project_output(body: &Value, effective_type: &str, profile: OpenCodeNativeProfile) -> Vec<u8> {
    let aggregate = collect_outcome(body);
    let Some(selected) = selected_output_subrecords(effective_type, body) else {
        return encode_output(OutputWire {
            diagnostic: output_diagnostic(body, effective_type, &aggregate, None),
            outputs: Vec::new(),
            pro_rejection: (profile == OpenCodeNativeProfile::CoreAndPro)
                .then(|| "output-bearing row has no eligible canonical result field".to_owned()),
        });
    };
    if selected.len() > MAX_OUTPUT_SUBRECORDS_PER_NATIVE_ROW {
        return encode_output(OutputWire {
            diagnostic: output_diagnostic(body, effective_type, &aggregate, None),
            outputs: Vec::new(),
            pro_rejection: (profile == OpenCodeNativeProfile::CoreAndPro).then(|| {
                format!(
                    "native output row has {} subrecords; maximum is {}",
                    selected.len(),
                    MAX_OUTPUT_SUBRECORDS_PER_NATIVE_ROW
                )
            }),
        });
    }
    let preview = selected
        .iter()
        .find(|value| matches!(collect_outcome(value).outcome(), 2 | 3))
        .or_else(|| selected.first())
        .map(normalized_output_value);
    let diagnostic = output_diagnostic(body, effective_type, &aggregate, preview.as_deref());
    let outputs = if profile == OpenCodeNativeProfile::CoreAndPro {
        selected
            .iter()
            .enumerate()
            .map(|(index, value)| output_draft(body, value, index, effective_type, &aggregate))
            .collect()
    } else {
        Vec::new()
    };
    encode_output(OutputWire {
        diagnostic,
        outputs,
        pro_rejection: None,
    })
}

fn output_diagnostic(
    body: &Value,
    effective_type: &str,
    aggregate: &OutcomeAggregate,
    preview: Option<&str>,
) -> Option<RetainedWire> {
    if !matches!(aggregate.outcome(), 2 | 3) {
        return None;
    }
    let mut diagnostic = Map::new();
    diagnostic.insert("type".to_owned(), Value::String(effective_type.to_owned()));
    diagnostic.insert("role".to_owned(), Value::String("tool".to_owned()));
    diagnostic.insert(
        "output_preview".to_owned(),
        Value::String(
            preview
                .unwrap_or_default()
                .chars()
                .take(OUTPUT_DIAGNOSTIC_PREVIEW_CHARS)
                .collect(),
        ),
    );
    diagnostic.insert(
        "result_outcome".to_owned(),
        Value::String("failure".to_owned()),
    );
    diagnostic.insert(
        "timed_out".to_owned(),
        Value::Bool(aggregate.outcome() == 3),
    );
    if let Some(exit_code) = aggregate.exit_code {
        diagnostic.insert("exit_code".to_owned(), Value::from(exit_code));
    }
    if let Some(duration_ms) = aggregate.duration_ms {
        diagnostic.insert("duration_ms".to_owned(), Value::from(duration_ms));
    }
    if let Some(call_id) = string_at(
        body,
        &[
            "/call_id",
            "/callId",
            "/callID",
            "/tool_call_id",
            "/state/call_id",
            "/state/callId",
            "/id",
        ],
    ) {
        diagnostic.insert("call_id".to_owned(), Value::String(call_id));
    }
    if let Some(tool) = string_at(body, &["/tool", "/tool_name", "/name"]) {
        diagnostic.insert("tool".to_owned(), Value::String(tool));
    }
    if let Some(command) = string_at(
        body,
        &[
            "/command",
            "/cmd",
            "/state/input/command",
            "/state/metadata/command",
        ],
    ) {
        diagnostic.insert("command".to_owned(), Value::String(command));
    }
    if let Some(cwd) = string_at(
        body,
        &[
            "/working_directory",
            "/workingDirectory",
            "/cwd",
            "/state/metadata/cwd",
        ],
    ) {
        diagnostic.insert("cwd".to_owned(), Value::String(cwd));
    }
    Some(RetainedWire {
        effective_type: effective_type.to_owned(),
        role: "tool".to_owned(),
        body: Value::Object(diagnostic),
    })
}

fn selected_output_subrecords(effective_type: &str, body: &Value) -> Option<Vec<Value>> {
    let candidates: &[&str] = if normalize_token(effective_type) == "shell" {
        &[
            "/output",
            "/state/output",
            "/stdout",
            "/stderr",
            "/result",
            "/content",
            "/text",
        ]
    } else {
        &[
            "/state/output",
            "/state/result",
            "/state/content",
            "/output",
            "/result",
            "/content",
            "/text",
        ]
    };
    let selected = candidates.iter().find_map(|pointer| body.pointer(pointer));
    match selected {
        Some(Value::Array(values)) if values.is_empty() => Some(vec![Value::Array(Vec::new())]),
        Some(Value::Array(values)) => Some(values.clone()),
        Some(value) => Some(vec![value.clone()]),
        None if is_direct_output_token(effective_type) || is_tool_token(effective_type) => {
            Some(vec![Value::String(String::new())])
        }
        None => None,
    }
}

fn normalized_output_value(value: &Value) -> String {
    if let Some(value) = value.as_str() {
        value.to_owned()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
}

fn output_draft(
    body: &Value,
    subrecord: &Value,
    index: usize,
    effective_type: &str,
    aggregate: &OutcomeAggregate,
) -> OpenCodeOutputDraft {
    let local = collect_outcome(subrecord);
    let selected_outcome = if local.has_signal() {
        &local
    } else {
        aggregate
    };
    let content_value = if subrecord.is_object() {
        [
            "/output", "/result", "/content", "/text", "/stdout", "/stderr",
        ]
        .iter()
        .find_map(|pointer| subrecord.pointer(pointer))
        .unwrap_or(subrecord)
    } else {
        subrecord
    };
    OpenCodeOutputDraft {
        subrecord_index: u32::try_from(index).unwrap_or(u32::MAX),
        kind: u8::from(normalize_token(effective_type) == "shell"),
        call_id: string_at(subrecord, &["/call_id", "/callId", "/tool_call_id", "/id"]).or_else(
            || {
                string_at(
                    body,
                    &[
                        "/call_id",
                        "/callId",
                        "/callID",
                        "/tool_call_id",
                        "/state/call_id",
                        "/state/callId",
                        "/id",
                    ],
                )
            },
        ),
        tool_name: string_at(body, &["/tool", "/tool_name", "/name"]),
        command: string_at(
            body,
            &[
                "/command",
                "/cmd",
                "/state/input/command",
                "/state/metadata/command",
            ],
        ),
        working_directory: string_at(
            body,
            &[
                "/working_directory",
                "/workingDirectory",
                "/cwd",
                "/state/metadata/cwd",
            ],
        ),
        outcome: selected_outcome.outcome(),
        exit_code: selected_outcome.exit_code,
        duration_ms: selected_outcome.duration_ms,
        content: normalized_output_value(content_value),
    }
}

fn tag(value: u8) -> Vec<u8> {
    vec![value]
}

fn hex_tag(value: u8) -> String {
    format!("{value:02x}")
}

fn output_predicate_sql(data: &str, column_type: &str, parent_data: Option<&str>) -> String {
    let direct_tokens = "'result','toolresult','toolresponse','commandresult','output',\
                         'tooloutput','commandoutput'";
    let normalized_column = normalized_sql(column_type);
    let value_predicate = |value: &str| {
        format!(
            "{normalized} in ({direct_tokens})
             or {normalized} like '%result'
             or {normalized} like '%output'",
            normalized = normalized_sql(value),
        )
    };
    let tree_predicate = |value: &str| {
        format!(
            "exists(
                 select 1 from json_tree({value}) jt
                 where (
                     ({key} in ({direct_tokens})
                       or {key} like '%result'
                       or {key} like '%output')
                     and not (
                         {key} = 'output'
                         and jt.type in ('integer', 'real')
                         and replace(jt.fullkey, '[', '.') like '%.tokens.output'
                     )
                 )
                 or (
                     {key} in ('type', 'role')
                     and ({atom})
                 )
             )",
            key = normalized_sql("coalesce(jt.key, '')"),
            atom = value_predicate("coalesce(jt.atom, '')"),
        )
    };
    let mut predicates = vec![value_predicate(&normalized_column), tree_predicate(data)];
    if let Some(parent) = parent_data {
        predicates.push(tree_predicate(parent));
    }
    predicates
        .into_iter()
        .map(|predicate| format!("({predicate})"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn failure_predicate_sql(data: &str, parent_data: Option<&str>) -> String {
    let predicate = |value: &str| {
        format!(
            "exists(
                 select 1 from json_tree({value}) jt
                 where (
                     {key} in ('timedout', 'timeout', 'iserror')
                     and (
                         (jt.type = 'true')
                         or {atom} in (
                             'timeout','timedout','failed','failure','error','errored',
                             'cancelled','canceled'
                         )
                     )
                 )
                 or (
                     {key} in ('status', 'outcome', 'state')
                     and {atom} in (
                         'timeout','timedout','failed','failure','error','errored',
                         'cancelled','canceled'
                     )
                 )
                 or (
                     {key} in ('exit', 'exitcode')
                     and jt.type = 'integer'
                     and cast(jt.atom as integer) <> 0
                 )
                 or (
                     {key} = 'success' and jt.type = 'false'
                 )
                 or (
                     {key} = 'error' and jt.type not in ('null', 'false')
                     and cast(jt.atom as text) <> ''
                 )
             )",
            key = normalized_sql("coalesce(jt.key, '')"),
            atom = normalized_sql("coalesce(cast(jt.atom as text), '')"),
        )
    };
    let mut predicates = vec![predicate(data)];
    if let Some(parent) = parent_data {
        predicates.push(predicate(parent));
    }
    predicates
        .into_iter()
        .map(|predicate| format!("({predicate})"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn normalized_sql(value: &str) -> String {
    format!("lower(replace(replace(replace(trim({value}), '_', ''), '-', ''), ' ', ''))")
}

fn family_from_label(label: &str) -> Option<OpenCodeNativeSchemaFamily> {
    match label {
        "session_message_seq" => Some(OpenCodeNativeSchemaFamily::SessionMessageSeq),
        "session_message_synthesized_seq" => {
            Some(OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq)
        }
        "session_entry" => Some(OpenCodeNativeSchemaFamily::SessionEntry),
        "legacy_message" => Some(OpenCodeNativeSchemaFamily::LegacyMessage),
        "message_part" => Some(OpenCodeNativeSchemaFamily::MessagePart),
        _ => None,
    }
}

fn effective_type(
    column_type: &str,
    body_role: Option<&str>,
    body_type: Option<&str>,
    parent_role: Option<&str>,
) -> String {
    let column = column_type.trim().to_ascii_lowercase();
    if !column.is_empty() && column != "message" && column != "part" {
        return column;
    }
    first_nonempty(&[body_role, body_type, parent_role])
        .unwrap_or(column.as_str())
        .trim()
        .to_ascii_lowercase()
}

fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .flatten()
        .copied()
        .find(|value| !value.trim().is_empty())
}

fn object_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn tool_call_is_retained(body: &Value) -> bool {
    let status = body
        .pointer("/state/status")
        .or_else(|| body.pointer("/state/outcome"))
        .or_else(|| body.get("status"))
        .or_else(|| body.get("outcome"))
        .and_then(Value::as_str)
        .map(normalize_token);
    let has_input = body.pointer("/state/input").is_some()
        || body.get("input").is_some()
        || body.get("arguments").is_some()
        || body.get("command").is_some()
        || body.get("toolCall").is_some()
        || body.get("tool_calls").is_some();
    has_input
        && status
            .as_deref()
            .is_none_or(|status| matches!(status, "pending" | "running"))
}

fn is_retained_type(family: Option<OpenCodeNativeSchemaFamily>, value: &str) -> bool {
    matches!(
        normalize_token(value).as_str(),
        "user"
            | "assistant"
            | "system"
            | "text"
            | "reasoning"
            | "summary"
            | "notice"
            | "patch"
            | "stepstart"
            | "stepfinish"
            | "snapshot"
            | "toolcall"
            | "tooluse"
            | "agentswitched"
            | "modelswitched"
            | "synthetic"
            | "compaction"
    ) || (family != Some(OpenCodeNativeSchemaFamily::MessagePart)
        && normalize_token(value) == "message")
}

fn is_tool_token(value: &str) -> bool {
    matches!(normalize_token(value).as_str(), "tool" | "shell")
}

fn is_direct_output_token(value: &str) -> bool {
    let value = normalize_token(value);
    matches!(
        value.as_str(),
        "result"
            | "toolresult"
            | "toolresponse"
            | "commandresult"
            | "output"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
}

fn is_output_key(value: &str, child: &Value, inside_tokens: bool) -> bool {
    let value = normalize_token(value);
    if inside_tokens && value == "output" && child.is_number() {
        return false;
    }
    matches!(
        value.as_str(),
        "output"
            | "result"
            | "stdout"
            | "stderr"
            | "toolresult"
            | "commandresult"
            | "tooloutput"
            | "commandoutput"
    ) || value.ends_with("result")
        || value.ends_with("output")
}

fn is_terminal_status(value: &str) -> bool {
    matches!(
        normalize_token(value).as_str(),
        "complete"
            | "completed"
            | "success"
            | "succeeded"
            | "ok"
            | "failed"
            | "failure"
            | "error"
            | "errored"
            | "timeout"
            | "timedout"
            | "cancelled"
            | "canceled"
    )
}

fn normalize_token(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Default)]
struct OutcomeAggregate {
    timeout: bool,
    failure: bool,
    success: bool,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

impl OutcomeAggregate {
    fn outcome(&self) -> u8 {
        if self.timeout {
            3
        } else if self.failure {
            2
        } else if self.success {
            1
        } else {
            0
        }
    }

    fn has_signal(&self) -> bool {
        self.timeout
            || self.failure
            || self.success
            || self.exit_code.is_some()
            || self.duration_ms.is_some()
    }
}

fn collect_outcome(value: &Value) -> OutcomeAggregate {
    let mut aggregate = OutcomeAggregate::default();
    collect_outcome_into(value, &mut aggregate);
    aggregate
}

fn collect_outcome_into(value: &Value, aggregate: &mut OutcomeAggregate) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_outcome_into(value, aggregate);
            }
        }
        Value::Object(object) => {
            aggregate.timeout |= ["timed_out", "timedOut", "timeout"]
                .iter()
                .any(|key| object.get(*key).and_then(Value::as_bool).unwrap_or(false));
            if let Some(success) = object.get("success").and_then(Value::as_bool) {
                aggregate.success |= success;
                aggregate.failure |= !success;
            }
            aggregate.failure |= object
                .get("isError")
                .or_else(|| object.get("is_error"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(code) = ["exit", "exit_code", "exitCode"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_i64))
            {
                aggregate.exit_code = i32::try_from(code).ok();
                aggregate.success |= code == 0;
                aggregate.failure |= code != 0;
            }
            if aggregate.duration_ms.is_none() {
                aggregate.duration_ms = ["duration_ms", "durationMs"]
                    .iter()
                    .find_map(|key| object.get(*key).and_then(Value::as_u64));
            }
            for key in ["status", "state", "outcome"] {
                if let Some(status) = object.get(key).and_then(Value::as_str) {
                    let status = normalize_token(status);
                    aggregate.timeout |= matches!(status.as_str(), "timeout" | "timedout");
                    aggregate.failure |= matches!(
                        status.as_str(),
                        "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
                    );
                    aggregate.success |= matches!(
                        status.as_str(),
                        "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
                    );
                }
            }
            aggregate.failure |= object.get("error").is_some_and(nonempty_error);
            for child in object.values() {
                collect_outcome_into(child, aggregate);
            }
        }
        _ => {}
    }
}

fn nonempty_error(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_none_or(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

fn string_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

struct AuditedValue {
    value: Value,
    duplicate_key: bool,
    forbidden_output: bool,
}

fn audit_json(raw: &str) -> std::result::Result<AuditedValue, ()> {
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let audited = AuditedSeed::ROOT
        .deserialize(&mut deserializer)
        .map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    Ok(audited)
}

#[derive(Clone, Copy)]
struct AuditedSeed {
    inside_tokens: bool,
}

impl AuditedSeed {
    const ROOT: Self = Self {
        inside_tokens: false,
    };
}

impl<'de> DeserializeSeed<'de> for AuditedSeed {
    type Value = AuditedValue;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(AuditedVisitor {
            inside_tokens: self.inside_tokens,
        })
    }
}

struct AuditedVisitor {
    inside_tokens: bool,
}

impl<'de> Visitor<'de> for AuditedVisitor {
    type Value = AuditedValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(audited_scalar)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        AuditedSeed {
            inside_tokens: self.inside_tokens,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        let mut duplicate_key = false;
        let mut forbidden_output = false;
        while let Some(value) = sequence.next_element_seed(AuditedSeed {
            inside_tokens: self.inside_tokens,
        })? {
            duplicate_key |= value.duplicate_key;
            forbidden_output |= value.forbidden_output;
            values.push(value.value);
        }
        Ok(AuditedValue {
            value: Value::Array(values),
            duplicate_key,
            forbidden_output,
        })
    }

    fn visit_map<A>(self, mut entries: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        let mut seen = BTreeSet::new();
        let mut duplicate_key = false;
        let mut forbidden_output = false;
        while let Some(key) = entries.next_key::<String>()? {
            let value = entries.next_value_seed(AuditedSeed {
                inside_tokens: normalize_token(&key) == "tokens",
            })?;
            duplicate_key |= value.duplicate_key || !seen.insert(key.clone());
            forbidden_output |=
                value.forbidden_output || is_output_key(&key, &value.value, self.inside_tokens);
            object.insert(key, value.value);
        }
        forbidden_output |= object_is_forbidden_output(&object);
        Ok(AuditedValue {
            value: Value::Object(object),
            duplicate_key,
            forbidden_output,
        })
    }
}

fn audited_scalar(value: Value) -> AuditedValue {
    AuditedValue {
        value,
        duplicate_key: false,
        forbidden_output: false,
    }
}

fn object_is_forbidden_output(object: &Map<String, Value>) -> bool {
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(is_direct_output_token)
        || object
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| normalize_token(role) == "tool")
    {
        return true;
    }
    let object_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(normalize_token);
    if !object_type
        .as_deref()
        .is_some_and(|value| matches!(value, "tool" | "shell"))
    {
        return false;
    }
    let state = object.get("state").and_then(Value::as_object);
    let status = state
        .and_then(|state| state.get("status").or_else(|| state.get("outcome")))
        .or_else(|| object.get("status"))
        .or_else(|| object.get("outcome"))
        .and_then(Value::as_str);
    if status.is_some_and(is_terminal_status) {
        return true;
    }
    let has_output = object.contains_key("content")
        || object.contains_key("structured")
        || state
            .is_some_and(|state| state.contains_key("content") || state.contains_key("structured"));
    status.is_some_and(|status| normalize_token(status) == "running") && has_output
}
