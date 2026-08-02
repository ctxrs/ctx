use crate::{CaptureError, Result};

use super::super::{
    model::{OpenCodeNativeOrder, OpenCodeNativeSchemaFamily},
    schema::OpenCodeNativeSchema,
};
const JSON_HINT_BYTES: usize = 256;
const MAX_NATIVE_IDENTITY_BYTES: usize = 4 * 1024;

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

#[cfg(test)]
pub(super) fn source_backed_event_order_sql(schema: &OpenCodeNativeSchema) -> &'static str {
    match schema.family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            " order by x.session_id collate binary, x.seq"
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq
        | OpenCodeNativeSchemaFamily::SessionEntry
        | OpenCodeNativeSchemaFamily::LegacyMessage => {
            " order by x.session_id collate binary, x.time_created, x.id collate binary"
        }
        OpenCodeNativeSchemaFamily::MessagePart if schema.message_part_indexed_streaming => {
            " order by m.session_id collate binary, m.time_created,
                       m.id collate binary, p.time_created, p.id collate binary"
        }
        OpenCodeNativeSchemaFamily::MessagePart => {
            " order by p.session_id collate binary,
                       coalesce(m.time_created, p.time_created),
                       p.message_id collate binary,
                       p.time_created, p.id collate binary"
        }
    }
}

fn row_event_source_sql(
    schema: &OpenCodeNativeSchema,
    table: &str,
    explicit_sequence: bool,
) -> String {
    let type_column = type_expression(schema.event_has_type, "x");
    let order_a = if explicit_sequence {
        "cast(x.seq as integer)"
    } else {
        "cast(x.time_created as integer)"
    };
    let order_tag = if explicit_sequence { 1 } else { 2 };
    let ordering_invalid = if explicit_sequence {
        "typeof(x.seq) <> 'integer' or x.seq < 0
         or typeof(x.time_created) <> 'integer'
         or typeof(x.time_updated) <> 'integer'"
    } else {
        "typeof(x.time_created) <> 'integer'
         or typeof(x.time_updated) <> 'integer'"
    };
    format!(
        "select cast(x.id as text), cast(x.id as text), cast(x.session_id as text),
                {order_tag}, {order_a}, 0,
                cast(x.time_created as integer),
                cast(x.time_updated as integer),
                case when typeof(x.data) in ('text', 'blob')
                     then octet_length(x.data) else 0 end,
                {type_column},
                0,
                x.rowid, x.data,
                case
                    when typeof(x.id) <> 'text' or trim(x.id) = ''
                         or octet_length(x.id) > {MAX_NATIVE_IDENTITY_BYTES}
                         or typeof(x.session_id) <> 'text' or trim(x.session_id) = ''
                         or octet_length(x.session_id) > {MAX_NATIVE_IDENTITY_BYTES}
                    then 1
                    when {ordering_invalid} then 3
                    else 0
                end,
                null,
                case when s.id is null then 1 else 0 end
         from {table} x
         left join session s on s.id = x.session_id",
    )
}

fn part_event_source_sql(schema: &OpenCodeNativeSchema) -> String {
    part_event_source_sql_with_payload(schema, schema.message_part_indexed_streaming, false)
}

pub(super) fn source_backed_fallback_sort_key_sql(schema: &OpenCodeNativeSchema) -> String {
    match schema.family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            row_sort_key_sql("session_message", "x.seq")
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            row_sort_key_sql("session_message", "x.time_created")
        }
        OpenCodeNativeSchemaFamily::SessionEntry => {
            row_sort_key_sql("session_entry", "x.time_created")
        }
        OpenCodeNativeSchemaFamily::LegacyMessage => row_sort_key_sql("message", "x.time_created"),
        OpenCodeNativeSchemaFamily::MessagePart if schema.message_part_indexed_streaming => {
            "select p.rowid, m.session_id, m.time_created,
                m.id, p.time_created, p.id,
                (case when typeof(p.data) in ('text', 'blob')
                      then octet_length(p.data) else 0 end)
                + (case when typeof(m.data) in ('text', 'blob')
                        then octet_length(m.data) else 0 end)
           from message m
           cross join part p on p.message_id = m.id"
                .to_owned()
        }
        OpenCodeNativeSchemaFamily::MessagePart => "select p.rowid, p.session_id,
                coalesce(m.time_created, p.time_created),
                p.message_id, p.time_created, p.id,
                (case when typeof(p.data) in ('text', 'blob')
                      then octet_length(p.data) else 0 end)
                + (case when typeof(m.data) in ('text', 'blob')
                        then octet_length(m.data) else 0 end)
           from part p
           left join message m on m.id = p.message_id"
            .to_owned(),
    }
}

fn row_sort_key_sql(table: &str, order: &str) -> String {
    format!(
        "select x.rowid, x.session_id, {order}, x.id, 0, '',
                case when typeof(x.data) in ('text', 'blob')
                     then octet_length(x.data) else 0 end
           from {table} x"
    )
}

pub(super) fn source_backed_fallback_events_by_rowids_sql(
    schema: &OpenCodeNativeSchema,
    rows: usize,
) -> String {
    let placeholders = (1..=rows)
        .map(|parameter| format!("?{parameter}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = match schema.family {
        OpenCodeNativeSchemaFamily::MessagePart => {
            part_event_source_sql_with_payload(schema, true, true)
        }
        _ => source_backed_event_source_sql(schema),
    };
    let alias = if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        "p"
    } else {
        "x"
    };
    sql.push_str(&format!(" where {alias}.rowid in ({placeholders})"));
    sql
}

fn part_event_source_sql_with_payload(
    schema: &OpenCodeNativeSchema,
    include_payload: bool,
    hydrate_by_rowid: bool,
) -> String {
    let type_column = type_expression(schema.event_has_type, "p");
    let (source, source_locator, source_data, parent_source_data) =
        if schema.message_part_indexed_streaming && !hydrate_by_rowid {
            (
                "from message m
                 cross join part p on p.message_id = m.id
                 left join session s on s.id = p.session_id",
                "p.rowid",
                "p.data",
                "m.data",
            )
        } else if include_payload {
            (
                "from part p
                 left join message m on m.id = p.message_id
                 left join session s on s.id = p.session_id",
                "p.rowid",
                "p.data",
                "m.data",
            )
        } else {
            // The compatibility path can require a full ORDER BY when provider
            // indexes are absent. Keep payloads out of that sorter and hydrate
            // the ordered identities through their required primary keys.
            (
                "from part p
                 left join message m on m.id = p.message_id
                 left join session s on s.id = p.session_id",
                "0",
                "null",
                "null",
            )
        };
    format!(
        "select cast(p.id as text), cast(p.message_id as text),
                cast(p.session_id as text), 3,
                coalesce(cast(m.time_created as integer), cast(p.time_created as integer)),
                cast(p.time_created as integer),
                cast(p.time_created as integer),
                cast(p.time_updated as integer),
                case when typeof(p.data) in ('text', 'blob')
                     then octet_length(p.data) else 0 end,
                {type_column},
                0,
                {source_locator}, {source_data},
                case
                    when typeof(p.id) <> 'text' or trim(p.id) = ''
                         or octet_length(p.id) > {MAX_NATIVE_IDENTITY_BYTES}
                         or typeof(p.session_id) <> 'text' or trim(p.session_id) = ''
                         or octet_length(p.session_id) > {MAX_NATIVE_IDENTITY_BYTES}
                    then 1
                    when typeof(p.message_id) <> 'text' or trim(p.message_id) = ''
                         or octet_length(p.message_id) > {MAX_NATIVE_IDENTITY_BYTES}
                    then 2
                    when typeof(p.time_created) <> 'integer'
                         or typeof(p.time_updated) <> 'integer'
                    then 3
                    when m.id is not null and (
                         typeof(m.id) <> 'text' or trim(m.id) = ''
                         or octet_length(m.id) > {MAX_NATIVE_IDENTITY_BYTES}
                         or typeof(m.session_id) <> 'text' or trim(m.session_id) = ''
                         or octet_length(m.session_id) > {MAX_NATIVE_IDENTITY_BYTES}
                         or typeof(m.time_created) <> 'integer'
                         or typeof(m.time_updated) <> 'integer')
                    then 4
                    else 0
                end,
                {parent_source_data},
                case
                    when m.id is null then 2
                    when s.id is null then 1
                    when cast(m.session_id as text) <> cast(p.session_id as text) then 3
                    else 0
                end
         {source}",
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
