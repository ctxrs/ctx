use super::*;

pub(super) fn event_source_sql(
    schema: &OpenCodeNativeSchema,
    profile: OpenCodeNativeProfile,
) -> String {
    match schema.family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            row_event_source_sql(schema, "session_message", true, profile)
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            row_event_source_sql(schema, "session_message", false, profile)
        }
        OpenCodeNativeSchemaFamily::SessionEntry => {
            row_event_source_sql(schema, "session_entry", false, profile)
        }
        OpenCodeNativeSchemaFamily::LegacyMessage => {
            row_event_source_sql(schema, "message", false, profile)
        }
        OpenCodeNativeSchemaFamily::MessagePart => part_event_source_sql(schema, profile),
    }
}

pub(super) fn row_event_source_sql(
    schema: &OpenCodeNativeSchema,
    table: &str,
    explicit_sequence: bool,
    profile: OpenCodeNativeProfile,
) -> String {
    let type_column = type_expression(schema.event_has_type, "x");
    let projection = projection_sql("x.data", &type_column, None, schema.family, "?1", profile);
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
                x.rowid
         from {table} x
         left join session s on s.id = x.session_id",
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
    )
}

pub(super) fn part_event_source_sql(
    schema: &OpenCodeNativeSchema,
    profile: OpenCodeNativeProfile,
) -> String {
    let type_column = type_expression(schema.event_has_type, "p");
    let projection = projection_sql(
        "p.data",
        &type_column,
        Some("m.data"),
        schema.family,
        "?1",
        profile,
    );
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
                p.rowid
         from part p
         left join message m on m.id = p.message_id
         left join session s on s.id = p.session_id",
        missing_message = hex_bytes(MISSING_MESSAGE_PROJECTION),
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
        relationship_mismatch = hex_bytes(RELATIONSHIP_MISMATCH_PROJECTION),
    )
}

pub(super) fn type_expression(has_type: bool, alias: &str) -> String {
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
            "OpenCode snapshot index contains an unknown order tag",
        )),
    }
}

pub(super) fn event_digest(
    family: OpenCodeNativeSchemaFamily,
    native_identity: &str,
    native_order: &OpenCodeNativeOrder,
    time_created: i64,
    time_updated: i64,
    retained: &OpenCodeRetainedJson,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-retained-event-v2\0");
    hash_str(&mut hasher, family.label());
    hash_str(&mut hasher, native_identity);
    hash_order(&mut hasher, native_order);
    hash_str(&mut hasher, &retained.effective_type);
    hash_str(&mut hasher, &retained.role);
    hasher.update(time_created.to_le_bytes());
    hasher.update(time_updated.to_le_bytes());
    let canonical = serde_json::to_vec(&retained.body).map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "OpenCode retained projection cannot be hashed: {error}"
        ))
    })?;
    hasher.update((canonical.len() as u64).to_le_bytes());
    hasher.update(canonical);
    Ok(super::super::schema::hex_digest(hasher.finalize().into()))
}

pub(super) fn native_order_digest(order: &OpenCodeNativeOrder) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-order-v1\0");
    hash_order(&mut hasher, order);
    super::super::schema::hex_digest(hasher.finalize().into())
}

pub(super) fn hash_order(hasher: &mut Sha256, order: &OpenCodeNativeOrder) {
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

pub(super) fn hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
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

pub(super) fn stable_native_event_index(
    session_identity: &str,
    native_record_identity: &str,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-family-native-event-index-v1\0");
    hash_str(&mut hasher, session_identity);
    hash_str(&mut hasher, native_record_identity);
    let digest: [u8; 32] = hasher.finalize().into();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix has eight bytes"),
    )
}

pub(super) fn ordered_source_rowid(rowid: i64) -> u64 {
    (rowid as u64) ^ (1_u64 << 63)
}

pub(super) fn session_digest(values: [&str; 6], time_created: i64, time_updated: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-session-v1\0");
    for value in values {
        hash_str(&mut hasher, value);
    }
    hasher.update(time_created.to_le_bytes());
    hasher.update(time_updated.to_le_bytes());
    super::super::schema::hex_digest(hasher.finalize().into())
}

pub(super) fn optional_session_text(
    columns: &std::collections::BTreeSet<String>,
    column: &str,
) -> String {
    if columns.contains(column) {
        format!("case when typeof({column}) = 'text' then cast({column} as text) else '' end")
    } else {
        "''".to_owned()
    }
}

pub(super) fn session_metadata_preflight_sql(
    columns: &std::collections::BTreeSet<String>,
) -> String {
    let optional_lengths = ["parent_id", "title", "directory", "model", "agent"]
        .into_iter()
        .filter(|column| columns.contains(*column))
        .map(|column| {
            format!("case when typeof({column}) = 'text' then octet_length({column}) else 0 end")
        })
        .collect::<Vec<_>>();
    let total = std::iter::once("octet_length(id)".to_owned())
        .chain(optional_lengths)
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "select exists(
             select 1 from session
             where typeof(id) <> 'text'
                or octet_length(id) > ?1
                or ({total}) + {SESSION_INDEX_FIXED_BYTES} > ?1
             limit 1
         )"
    )
}

pub(super) fn session_metadata_bytes(
    identity: &str,
    parent: &str,
    title: &str,
    directory: &str,
    model: &str,
    agent: &str,
) -> Result<i64> {
    let bytes = identity
        .len()
        .checked_add(parent.len())
        .and_then(|bytes| bytes.checked_add(title.len()))
        .and_then(|bytes| bytes.checked_add(directory.len()))
        .and_then(|bytes| bytes.checked_add(model.len()))
        .and_then(|bytes| bytes.checked_add(agent.len()))
        .and_then(|bytes| bytes.checked_add(SESSION_INDEX_FIXED_BYTES as usize))
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode session metadata byte count overflowed",
        ))?;
    i64::try_from(bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode session metadata bytes exceed SQLite integer".to_owned(),
        )
    })
}

pub(super) fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

pub(super) fn table_count(conn: &Connection, table: &str) -> Result<u64> {
    let sql = format!("select count(*) from {table}");
    let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    u64::try_from(count).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "OpenCode snapshot index table {table} has a negative row count"
        ))
    })
}

pub(super) fn i64_limit(limit: usize) -> Result<i64> {
    i64::try_from(limit)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode page limit exceeds i64"))
}

pub(super) fn i64_from_u64(value: u64, label: &'static str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| CaptureError::InvalidPayload(format!("{label} exceed SQLite integer")))
}

pub(super) fn native_shape_from_family(
    family: OpenCodeNativeSchemaFamily,
) -> OpenCodeCapturedShape {
    match family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq
        | OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            OpenCodeCapturedShape::SessionMessage
        }
        OpenCodeNativeSchemaFamily::SessionEntry => OpenCodeCapturedShape::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage => OpenCodeCapturedShape::Message,
        OpenCodeNativeSchemaFamily::MessagePart => OpenCodeCapturedShape::MessagePart,
    }
}

pub(super) fn native_locator(
    shape: OpenCodeCapturedShape,
    rowid: i64,
) -> Result<OpenCodeNativeLocator> {
    let locator = opencode_message_locator(shape, rowid)?;
    Ok(OpenCodeNativeLocator {
        version: 1,
        kind: locator.kind().to_owned(),
        payload: locator.value().to_vec(),
    })
}

pub(super) fn output_unit_bytes(
    native_identity: &str,
    output: &OpenCodeOutputDraft,
) -> Result<u64> {
    let variable = [
        native_identity.len(),
        output.call_id.as_ref().map_or(0, String::len),
        output.tool_name.as_ref().map_or(0, String::len),
        output.command.as_ref().map_or(0, String::len),
        output.working_directory.as_ref().map_or(0, String::len),
        output.content.len(),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .ok_or(CaptureError::SystemInvariant(
        "OpenCode output byte accounting overflowed",
    ))?;
    // Includes frontier, locator, source/session/message associations, option/length prefixes,
    // fixed scalar fields, and the maximum validated native identity relationship envelope.
    let bytes = variable
        .checked_add(48 * 1024)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode output byte accounting overflowed",
        ))?;
    u64::try_from(bytes)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode output bytes exceed u64"))
}

pub(super) fn rejection_unit_bytes(native_identity: &str, reason: &str) -> Result<u64> {
    let bytes = native_identity
        .len()
        .checked_add(reason.len())
        .and_then(|bytes| bytes.checked_add(48 * 1024))
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode rejection byte accounting overflowed",
        ))?;
    u64::try_from(bytes)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode rejection bytes exceed u64"))
}

pub(super) fn sqlite_nonnegative_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

pub(super) fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
