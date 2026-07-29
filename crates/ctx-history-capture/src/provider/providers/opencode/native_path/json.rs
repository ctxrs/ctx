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

use super::super::{normalization::opencode_event_time, schema::OpenCodeSqliteDialect};
use super::model::{OpenCodeNativeRejectionKind, OpenCodeNativeSchemaFamily};

mod audit;
mod output;

use audit::audit_json;
use output::*;

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

pub(super) const MISSING_SESSION_PROJECTION: &[u8] = &[TAG_MISSING_SESSION];
pub(super) const MISSING_MESSAGE_PROJECTION: &[u8] = &[TAG_MISSING_MESSAGE];
pub(super) const RELATIONSHIP_MISMATCH_PROJECTION: &[u8] = &[TAG_RELATIONSHIP_MISMATCH];

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeRetainedJson {
    pub(super) effective_type: String,
    pub(super) role: String,
    pub(super) body: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OpenCodeOutputJson {
    pub(super) diagnostic: Option<OpenCodeRetainedJson>,
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

#[derive(Serialize, Deserialize)]
struct RetainedWire {
    effective_type: String,
    role: String,
    body: Value,
}

#[derive(Serialize, Deserialize)]
struct OutputWire {
    diagnostic: Option<RetainedWire>,
}

pub(super) fn register_projection_function(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Arc<OpenCodeJsonVisitorMetrics>> {
    let metrics = Arc::new(OpenCodeJsonVisitorMetrics::default());
    let function_metrics = Arc::clone(&metrics);
    let projection_dialect = dialect.clone();
    conn.create_scalar_function(
        PROJECT_JSON_FUNCTION,
        4,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| {
            project_json_function(context, &function_metrics, projection_dialect.clone())
        },
    )?;
    let timestamp_dialect = dialect.clone();
    conn.create_scalar_function(
        PROJECT_TIMESTAMP_FUNCTION,
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| timestamp_rejection_function(context, timestamp_dialect.clone()),
    )?;
    Ok(metrics)
}

pub(super) fn projection_sql(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
    family: OpenCodeNativeSchemaFamily,
    max_bytes_parameter: &str,
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
    let output = output_predicate_sql(data, column_type, parent_data);
    let failure = failure_predicate_sql(data, parent_data);
    let core_output_guard = format!(
        "when json_valid({data}) and {parent_valid}
               and ({output}) and not ({failure})
         then X'{excluded}'",
        parent_valid = parent_data
            .map(|parent| format!("json_valid({parent})"))
            .unwrap_or_else(|| "1".to_owned()),
        excluded = hex_tag(TAG_EXCLUDED_OUTPUT),
    );
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

fn timestamp_rejection_function(
    context: &Context<'_>,
    dialect: OpenCodeSqliteDialect,
) -> rusqlite::Result<Option<Vec<u8>>> {
    let json_type = context.get::<String>(0)?;
    let error = if json_type == "integer" {
        let millis = context.get::<i64>(1)?;
        provider_required_event_timestamp(millis, dialect).err()
    } else {
        Some(CaptureError::InvalidPayload(format!(
            "{} event time.created must be integer millis",
            dialect.display_name
        )))
    };
    Ok(error.map(|error| encode_rejection_reason(error.to_string())))
}

fn provider_required_event_timestamp(millis: i64, dialect: OpenCodeSqliteDialect) -> Result<()> {
    crate::provider::normalization::provider_required_timestamp_millis(
        millis,
        dialect.event_time_created_field,
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
    dialect: OpenCodeSqliteDialect,
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
        dialect,
    ))
}

fn project_json(
    data: &str,
    column_type: &str,
    parent_data: Option<&str>,
    family_label: &str,
    dialect: OpenCodeSqliteDialect,
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
        if let Err(error) = opencode_event_time(&body.value, &dialect) {
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
            return encode_output(OutputWire { diagnostic: None });
        }
        return project_output(&body.value, &effective_type);
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
