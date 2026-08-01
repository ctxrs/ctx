use crate::{CaptureError, Result};

use super::super::{
    json::{
        projection_sql, MISSING_MESSAGE_PROJECTION, MISSING_SESSION_PROJECTION,
        RELATIONSHIP_MISMATCH_PROJECTION,
    },
    model::{OpenCodeNativeOrder, OpenCodeNativeSchemaFamily},
    schema::OpenCodeNativeSchema,
};
const JSON_HINT_BYTES: usize = 256;

pub(super) fn source_backed_event_source_sql(schema: &OpenCodeNativeSchema) -> String {
    match schema.family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            row_event_source_sql(schema, "session_message", true)
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            row_event_source_sql(schema, "session_message", false)
        }
        OpenCodeNativeSchemaFamily::SessionEntry => {
            row_event_source_sql(schema, "session_entry", false)
        }
        OpenCodeNativeSchemaFamily::LegacyMessage => row_event_source_sql(schema, "message", false),
        OpenCodeNativeSchemaFamily::MessagePart => part_event_source_sql(schema),
    }
}

fn row_event_source_sql(
    schema: &OpenCodeNativeSchema,
    table: &str,
    explicit_sequence: bool,
) -> String {
    let type_column = type_expression(schema.event_has_type, "x");
    let projection = projection_sql("x.data", &type_column, None, schema.family, "?1");
    let order_a = if explicit_sequence {
        "cast(x.seq as integer)"
    } else {
        "cast(x.time_created as integer)"
    };
    let order_tag = if explicit_sequence { 1 } else { 2 };
    format!(
        "select cast(x.id as text), cast(x.id as text), cast(x.session_id as text),
                {order_tag}, {order_a}, 0,
                case
                    when typeof(x.data) = 'text' and json_valid(x.data)
                         and json_type(x.data, '$.time.created') = 'integer'
                    then cast(json_extract(x.data, '$.time.created') as integer)
                    else cast(x.time_created as integer)
                end,
                cast(x.time_updated as integer),
                case when typeof(x.data) in ('text', 'blob')
                     then octet_length(x.data) else 0 end,
                case when s.id is null
                     then X'{missing_session}'
                     else {projection}
                end,
                case
                    when typeof(x.data) = 'text' and json_valid(x.data)
                         and json_type(x.data, '$.time.created') is not null
                    then 1 else 0
                end,
                x.rowid, x.data
         from {table} x
         left join session s on s.id = x.session_id",
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
    )
}

fn part_event_source_sql(schema: &OpenCodeNativeSchema) -> String {
    let type_column = type_expression(schema.event_has_type, "p");
    let projection = projection_sql("p.data", &type_column, Some("m.data"), schema.family, "?1");
    format!(
        "select cast(p.id as text), cast(p.message_id as text),
                cast(p.session_id as text), 3,
                coalesce(cast(m.time_created as integer), cast(p.time_created as integer)),
                cast(p.time_created as integer),
                cast(p.time_created as integer),
                cast(p.time_updated as integer),
                case when typeof(p.data) in ('text', 'blob')
                     then octet_length(p.data) else 0 end,
                case
                    when m.id is null then X'{missing_message}'
                    when s.id is null then X'{missing_session}'
                    when cast(m.session_id as text) <> cast(p.session_id as text)
                        then X'{relationship_mismatch}'
                    else {projection}
                end,
                0,
                p.rowid, p.data
         from part p
         left join message m on m.id = p.message_id
         left join session s on s.id = p.session_id",
        missing_message = hex_bytes(MISSING_MESSAGE_PROJECTION),
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
        relationship_mismatch = hex_bytes(RELATIONSHIP_MISMATCH_PROJECTION),
    )
}

fn type_expression(has_type: bool, alias: &str) -> String {
    if has_type {
        format!(
            "case when typeof({alias}.type) = 'text'
                  then lower(substr(trim({alias}.type), 1, {JSON_HINT_BYTES}))
                  else '' end"
        )
    } else {
        "'message'".to_owned()
    }
}

pub(super) fn decode_order(
    order_tag: i64,
    session_identity: &str,
    message_identity: &str,
    native_identity: &str,
    order_a: i64,
    order_b: i64,
) -> Result<OpenCodeNativeOrder> {
    match order_tag {
        1 => Ok(OpenCodeNativeOrder::ExplicitSequence {
            session_id: session_identity.to_owned(),
            sequence: order_a,
            message_id: message_identity.to_owned(),
        }),
        2 => Ok(OpenCodeNativeOrder::SynthesizedSequence {
            session_id: session_identity.to_owned(),
            time_created: order_a,
            message_id: message_identity.to_owned(),
        }),
        3 => Ok(OpenCodeNativeOrder::MessagePart {
            session_id: session_identity.to_owned(),
            message_time_created: order_a,
            message_id: message_identity.to_owned(),
            part_time_created: order_b,
            part_id: native_identity.to_owned(),
        }),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode source-backed index contains an unknown order tag",
        )),
    }
}

pub(super) fn native_record_identity(
    family: OpenCodeNativeSchemaFamily,
    message_identity: &str,
    native_identity: &str,
) -> String {
    if family == OpenCodeNativeSchemaFamily::MessagePart {
        format!("{message_identity}:{native_identity}")
    } else {
        native_identity.to_owned()
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
