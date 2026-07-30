//! ForgeCode's source-backed complete-content boundary.
//!
//! The global SQLite resolver predates NativePath and owns its logical-value
//! digest contract. These functions preserve that established read-only
//! boundary. NativePath uses only the bounded locator attachment below and
//! never constructs an ingestion page.

use std::collections::BTreeSet;

use ctx_history_core::{CaptureProvider, ContentRef, EventType};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
    },
    native_source::{NativeLocator, NativeSqliteValue},
    provider::sqlite::{
        ensure_sqlite_table_columns, optional_column_expr, sqlite_table_columns,
        sqlite_table_exists,
    },
    CaptureError, Result,
};

use super::event::{
    forgecode_event_type, forgecode_message_parts, forgecode_message_text, ForgeCodeNativeEvent,
};
use crate::FORGECODE_SQLITE_SOURCE_FORMAT;

const FORGECODE_LOCATOR_KIND: &str = "forgecode-conversation-row-v1";

pub(super) struct ForgeCodeCompleteContentDigest {
    locator: NativeLocator,
    record_digest: [u8; 32],
    canonical_record_bytes: u64,
}

impl ForgeCodeCompleteContentDigest {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        rowid: i64,
        conversation_id: &str,
        title: Option<&str>,
        workspace_id: i64,
        context: Option<&str>,
        created_at: &str,
        updated_at: Option<&str>,
        metrics: Option<&str>,
    ) -> Result<Self> {
        let locator = NativeLocator::new(FORGECODE_LOCATOR_KIND, rowid.to_be_bytes().to_vec())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let values = vec![
            NativeSqliteValue::Integer(rowid),
            NativeSqliteValue::Text(conversation_id.to_owned()),
            native_text(title),
            NativeSqliteValue::Integer(workspace_id),
            native_text(context),
            NativeSqliteValue::Text(created_at.to_owned()),
            native_text(updated_at),
            native_text(metrics),
        ];
        let record_digest = forgecode_logical_record_digest(&values);
        let canonical_record_bytes = forgecode_logical_record_bytes(&values)?;
        Ok(Self {
            locator,
            record_digest,
            canonical_record_bytes,
        })
    }

    pub(super) fn record_digest(&self) -> [u8; 32] {
        self.record_digest
    }

    pub(super) fn canonical_record_bytes(&self) -> u64 {
        self.canonical_record_bytes
    }

    pub(super) fn attach_message(
        &self,
        event: &mut ForgeCodeNativeEvent,
        complete_text: impl FnOnce() -> String,
    ) -> Result<()> {
        if event.event_type != EventType::Message
            || event
                .payload
                .pointer("/text_retention/truncated")
                .and_then(Value::as_bool)
                != Some(true)
        {
            return Ok(());
        }
        let complete_text = complete_text();
        let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
            CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
        )?;
        let profile = verified_content_profile(
            CaptureProvider::ForgeCode,
            FORGECODE_SQLITE_SOURCE_FORMAT,
            CompleteContentSourceFamily::Sqlite,
            VerifiedContentRole::MessageBody,
        )
        .ok_or(CaptureError::SystemInvariant(
            "supported SQLite message route must have a verified-content profile",
        ))?;
        let native_record_id = event
            .provider_event_hash
            .clone()
            .unwrap_or_else(|| event.cursor.clone());
        let persisted = VerifiedContentLocatorV1::new(
            VerifiedContentRole::MessageBody,
            profile,
            content_ref,
            CompleteContentSourceFamily::Sqlite,
            self.locator.kind(),
            self.locator.value(),
            native_record_id,
            complete_content_digest(self.record_digest)?,
        )
        .ok_or(CaptureError::SystemInvariant(
            "SQLite complete-content locator exceeds the bounded canonical schema",
        ))?;
        attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
            CaptureError::SystemInvariant("verified-content locator collection is malformed"),
        )?;
        Ok(())
    }
}

pub(in crate::provider::providers::forgecode) fn forgecode_logical_record_digest(
    values: &[NativeSqliteValue],
) -> [u8; 32] {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    // SQLite rowid is acquisition-only; logical evidence starts with the
    // provider-native conversation values.
    let values = values.get(1..).unwrap_or_default();
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    digest.finalize().into()
}

fn complete_content_digest(digest: [u8; 32]) -> Result<CompleteContentBodyDigest> {
    CompleteContentBodyDigest::parse(hex_digest(&digest)).ok_or(CaptureError::SystemInvariant(
        "ForgeCode logical-row digest is not canonical SHA-256",
    ))
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn forgecode_logical_record_bytes(values: &[NativeSqliteValue]) -> Result<u64> {
    values.iter().try_fold(8_u64, |total, value| {
        let value_bytes = match value {
            NativeSqliteValue::Null => 1,
            NativeSqliteValue::Integer(_) | NativeSqliteValue::RealBits(_) => 9,
            NativeSqliteValue::Text(value) => canonical_variable_value_bytes(value.len())?,
            NativeSqliteValue::Blob(value) => canonical_variable_value_bytes(value.len())?,
        };
        total
            .checked_add(value_bytes)
            .ok_or(CaptureError::SystemInvariant(
                "ForgeCode canonical logical-row length overflowed",
            ))
    })
}

fn canonical_variable_value_bytes(length: usize) -> Result<u64> {
    u64::try_from(length)
        .ok()
        .and_then(|length| length.checked_add(9))
        .ok_or(CaptureError::SystemInvariant(
            "ForgeCode canonical logical-row value length overflowed",
        ))
}

pub(crate) fn forgecode_complete_message(
    values: &[NativeSqliteValue],
    subrecord_index: u32,
) -> Result<(String, String, String)> {
    let (conversation_id, context) = conversation_identity_and_context(values)?;
    let entry = conversation_message(context.as_deref(), subrecord_index, "message")?;
    let parts = forgecode_message_parts(&entry);
    Ok((
        conversation_id,
        crate::compute_payload_hash(&entry)?,
        forgecode_message_text(parts, forgecode_event_type(parts)),
    ))
}

pub(crate) fn load_forgecode_conversation_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    values_at_rowid(conn, rowid)?.ok_or(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows))
}

fn values_at_rowid(conn: &Connection, rowid: i64) -> Result<Option<Vec<NativeSqliteValue>>> {
    let columns = conversation_columns(conn)?;
    let title = optional_column_expr(&columns, "title", "NULL");
    let context = optional_column_expr(&columns, "context", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let metrics = optional_column_expr(&columns, "metrics", "NULL");
    let sql = format!(
        "select rowid, cast(conversation_id as blob), cast({title} as blob), \
         workspace_id, cast({context} as blob), cast(created_at as blob), \
         cast({updated_at} as blob), cast({metrics} as blob) \
         from conversations where rowid = ?1"
    );
    conn.query_row(&sql, [rowid], |row| {
        Ok(HydratedConversation {
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
    .optional()?
    .map(HydratedConversation::into_values)
    .transpose()
}

struct HydratedConversation {
    rowid: i64,
    conversation_id: Vec<u8>,
    title: Option<Vec<u8>>,
    workspace_id: i64,
    context: Option<Vec<u8>>,
    created_at: Vec<u8>,
    updated_at: Option<Vec<u8>>,
    metrics: Option<Vec<u8>>,
}

impl HydratedConversation {
    fn into_values(self) -> Result<Vec<NativeSqliteValue>> {
        Ok(vec![
            NativeSqliteValue::Integer(self.rowid),
            required_text(self.conversation_id, "conversation_id")?,
            optional_text(self.title, "title")?,
            NativeSqliteValue::Integer(self.workspace_id),
            optional_text(self.context, "context")?,
            required_text(self.created_at, "created_at")?,
            optional_text(self.updated_at, "updated_at")?,
            optional_text(self.metrics, "metrics")?,
        ])
    }
}

fn conversation_identity_and_context(
    values: &[NativeSqliteValue],
) -> Result<(String, Option<String>)> {
    let [NativeSqliteValue::Integer(_), NativeSqliteValue::Text(conversation_id), _, NativeSqliteValue::Integer(_), context, NativeSqliteValue::Text(_), _, _] =
        values
    else {
        return Err(CaptureError::SystemInvariant(
            "ForgeCode complete-content row has an invalid value shape",
        ));
    };
    Ok((conversation_id.clone(), optional_native_text(context)?))
}

fn conversation_message(context: Option<&str>, subrecord_index: u32, kind: &str) -> Result<Value> {
    let context = context
        .filter(|raw| !raw.trim().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidPayload("ForgeCode conversation has no context".into())
        })?;
    let value: Value = serde_json::from_str(context)?;
    value
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.get(subrecord_index as usize))
        .cloned()
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!("ForgeCode {kind} subrecord is missing"))
        })
}

fn conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode .forge.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "ForgeCode conversations table",
        &["conversation_id", "workspace_id", "created_at"],
    )?;
    Ok(columns)
}

fn required_text(value: Vec<u8>, field: &str) -> Result<NativeSqliteValue> {
    String::from_utf8(value)
        .map(NativeSqliteValue::Text)
        .map_err(|_| {
            CaptureError::InvalidPayload(format!(
                "ForgeCode conversations.{field} is not valid UTF-8"
            ))
        })
}

fn optional_text(value: Option<Vec<u8>>, field: &str) -> Result<NativeSqliteValue> {
    value.map_or(Ok(NativeSqliteValue::Null), |value| {
        required_text(value, field)
    })
}

fn optional_native_text(value: &NativeSqliteValue) -> Result<Option<String>> {
    match value {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::SystemInvariant(
            "ForgeCode complete-content row has an invalid optional text value",
        )),
    }
}

fn native_text(value: Option<&str>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, |value| {
        NativeSqliteValue::Text(value.to_owned())
    })
}
