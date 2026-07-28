use rusqlite::{Connection, OptionalExtension};

use ctx_history_core::{CaptureProvider, ContentRef};

use crate::{
    complete_content::CompleteContentBodyDigest,
    native_source::{NativeLocator, NativeSqliteValue},
    CaptureError, Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::{
    normalization::{
        goose_event_payload_hash, normalize_goose_native_message, GooseNativeEventKind,
    },
    position::goose_message_locator,
    schema::{self, GooseNativeSchema},
    stream::{goose_native_message_identity_at, GooseRetainedContentClass, GooseRetainedMessage},
};

pub(super) fn goose_logical_row_digest(values: &[NativeSqliteValue]) -> Result<[u8; 32]> {
    let digest = crate::complete_content::sqlite::sqlite_logical_record_digest(values);
    let bytes = digest.as_str().as_bytes();
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        decoded[index] = decode_hex_nibble(pair[0])
            .and_then(|high| decode_hex_nibble(pair[1]).map(|low| (high << 4) | low))
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Goose exact-row resolver returned an invalid logical-row digest".to_owned(),
                )
            })?;
    }
    Ok(decoded)
}

fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

pub(super) fn load_schema(conn: &Connection) -> Result<()> {
    GooseNativeSchema::probe(conn)?;
    Ok(())
}

pub(super) fn load_message_values(conn: &Connection, rowid: i64) -> Result<Vec<NativeSqliteValue>> {
    message_values_at_rowid(conn, rowid)?.ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Goose message row {rowid} is missing"))
    })
}

pub(super) fn complete_message_with_normalized_hash(
    conn: &Connection,
    values: &[NativeSqliteValue],
) -> Result<(String, String, String, String)> {
    let (parent_rowid, message) = schema::decode_goose_message_record(values)?;
    if parent_rowid.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Goose message parent is missing".into(),
        ));
    }
    let schema = GooseNativeSchema::probe(conn)?;
    let identity = goose_native_message_identity_at(conn, &schema, message.rowid, message.id)?;
    let event = normalize_goose_native_message(GooseRetainedMessage {
        sqlite_rowid: message.rowid,
        native_order: message.id,
        native_identity: identity.native_identity,
        provider_message_identity: identity.provider_message_identity,
        identity_degraded: identity.identity_degraded,
        session_identity: message.session_id,
        role: message.role,
        retained_class: GooseRetainedContentClass::Message,
        content_bytes: message.content_json.len() as u64,
        content_json: message.content_json,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp,
        tokens_json: message.tokens,
        metadata_json: message.metadata_json,
        logical_row_digest: goose_logical_row_digest(values)?,
    })?;
    if event.kind != GooseNativeEventKind::Message {
        return Err(CaptureError::InvalidPayload(
            "Goose complete-content row is no longer a retained message".to_owned(),
        ));
    }
    let text = super::normalization::goose_complete_content_text(&event.content)
        .unwrap_or_else(|| event.searchable_text.clone());
    let normalized_hash = goose_event_payload_hash(&event);
    Ok((
        event.session_identity,
        event.provider_message_identity,
        normalized_hash,
        text,
    ))
}

pub(super) fn attach_message_locator(
    rowid: i64,
    native_record_id: &str,
    payload: &serde_json::Value,
    metadata: &mut serde_json::Value,
    logical_row_digest: [u8; 32],
    complete_text: String,
) -> Result<()> {
    let (kind, value) = goose_message_locator(rowid);
    let locator = NativeLocator::new(kind, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let record_digest = CompleteContentBodyDigest::parse(goose_hex_digest(logical_row_digest))
        .ok_or(CaptureError::SystemInvariant(
            "Goose logical-row digest must be valid SHA-256",
        ))?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Goose complete content exceeds ContentRef bounds"),
    )?;
    crate::complete_content::sqlite::attach_sqlite_complete_content_locator_with_ref(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        native_record_id,
        payload,
        metadata,
        &locator,
        record_digest,
        content_ref,
    )
}

fn goose_hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn message_values_at_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let columns = schema::goose_message_columns(conn)?;
    let expressions = schema::goose_message_expressions(&columns, "m");
    let select = expressions.hydration.join(", ");
    conn.query_row(
        &format!(
            "select s.rowid, {select} from messages m \
             left join sessions s on s.id = m.session_id where m.rowid = ?1"
        ),
        [rowid],
        |row| {
            let mut values = vec![row
                .get::<_, Option<i64>>(0)?
                .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)];
            values.extend(schema::goose_message_values_at(row, 1)?);
            Ok(values)
        },
    )
    .optional()
    .map_err(CaptureError::from)
}
