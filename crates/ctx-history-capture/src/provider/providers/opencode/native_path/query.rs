use crate::Result;

use super::{
    json::OpenCodeRetainedJson,
    model::{OpenCodeNativeOrder, OpenCodeNativeSchemaFamily},
    schema::OpenCodeNativeSchema,
};

mod sql;

pub(super) fn source_backed_event_sql(schema: &OpenCodeNativeSchema) -> String {
    sql::source_backed_event_source_sql(schema)
}

pub(super) fn source_backed_decode_order(
    order_tag: i64,
    session_identity: &str,
    message_identity: &str,
    native_identity: &str,
    order_a: i64,
    order_b: i64,
) -> Result<OpenCodeNativeOrder> {
    sql::decode_order(
        order_tag,
        session_identity,
        message_identity,
        native_identity,
        order_a,
        order_b,
    )
}

pub(super) fn source_backed_event_digest(
    family: OpenCodeNativeSchemaFamily,
    native_identity: &str,
    native_order: &OpenCodeNativeOrder,
    time_created: i64,
    time_updated: i64,
    retained: &OpenCodeRetainedJson,
) -> Result<String> {
    sql::event_digest(
        family,
        native_identity,
        native_order,
        time_created,
        time_updated,
        retained,
    )
}

pub(super) fn source_backed_native_record_identity(
    family: OpenCodeNativeSchemaFamily,
    message_identity: &str,
    native_identity: &str,
) -> String {
    sql::native_record_identity(family, message_identity, native_identity)
}
