//! Provider-owned verified-content coordinates and pure SQLite snapshot recovery.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::complete_content::{CompleteContentBodyDigest, COMPLETE_CONTENT_MAX_LOCATOR_BYTES};
use crate::{CaptureError, Result};

use super::message::{deepagents_messages_from_blob, DeepAgentsMessage};
use super::native_path::{
    deepagents_message_identity, deepagents_native_event, DeepAgentsNativeEvent,
    DeepAgentsParsedMessage,
};
use super::source::DeepAgentsWriteKey;

pub(crate) const DEEPAGENTS_CONTENT_LOCATOR_KIND: &str = "deepagents-write-message-v1";
const DEEPAGENTS_CONTENT_LOCATOR_VERSION: u8 = 1;
const DEEPAGENTS_CONTENT_LOCATOR_FIXED_BYTES: usize = 1 + 2 + 2 + 2 + 8 + 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeepAgentsContentAddress {
    pub(crate) thread_id: String,
    pub(crate) checkpoint_id: String,
    pub(crate) task_id: String,
    pub(crate) write_idx: i64,
    pub(crate) message_offset: u32,
}

impl DeepAgentsContentAddress {
    pub(super) fn from_write(key: &DeepAgentsWriteKey, message_offset: usize) -> Option<Self> {
        Some(Self {
            thread_id: key.thread_id.clone(),
            checkpoint_id: key.checkpoint_id.clone(),
            task_id: key.task_id.clone(),
            write_idx: key.idx,
            message_offset: u32::try_from(message_offset).ok()?,
        })
    }

    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        let thread = self.thread_id.as_bytes();
        let checkpoint = self.checkpoint_id.as_bytes();
        let task = self.task_id.as_bytes();
        let thread_len = u16::try_from(thread.len()).ok()?;
        let checkpoint_len = u16::try_from(checkpoint.len()).ok()?;
        let task_len = u16::try_from(task.len()).ok()?;
        let capacity = DEEPAGENTS_CONTENT_LOCATOR_FIXED_BYTES
            .checked_add(thread.len())?
            .checked_add(checkpoint.len())?
            .checked_add(task.len())?;
        if capacity > COMPLETE_CONTENT_MAX_LOCATOR_BYTES {
            return None;
        }
        let mut encoded = Vec::with_capacity(capacity);
        encoded.push(DEEPAGENTS_CONTENT_LOCATOR_VERSION);
        encoded.extend_from_slice(&thread_len.to_be_bytes());
        encoded.extend_from_slice(thread);
        encoded.extend_from_slice(&checkpoint_len.to_be_bytes());
        encoded.extend_from_slice(checkpoint);
        encoded.extend_from_slice(&task_len.to_be_bytes());
        encoded.extend_from_slice(task);
        encoded.extend_from_slice(&self.write_idx.to_be_bytes());
        encoded.extend_from_slice(&self.message_offset.to_be_bytes());
        Some(encoded)
    }
}

pub(crate) fn decode_deepagents_content_address(value: &[u8]) -> Option<DeepAgentsContentAddress> {
    let (&version, mut remaining) = value.split_first()?;
    if version != DEEPAGENTS_CONTENT_LOCATOR_VERSION {
        return None;
    }
    let thread_id = take_string(&mut remaining)?;
    let checkpoint_id = take_string(&mut remaining)?;
    let task_id = take_string(&mut remaining)?;
    let write_idx = i64::from_be_bytes(take_exact::<8>(&mut remaining)?);
    let message_offset = u32::from_be_bytes(take_exact::<4>(&mut remaining)?);
    if !remaining.is_empty() {
        return None;
    }
    Some(DeepAgentsContentAddress {
        thread_id,
        checkpoint_id,
        task_id,
        write_idx,
        message_offset,
    })
}

fn take_string(bytes: &mut &[u8]) -> Option<String> {
    let len = usize::from(u16::from_be_bytes(take_exact::<2>(bytes)?));
    if bytes.len() < len {
        return None;
    }
    let (value, remaining) = bytes.split_at(len);
    *bytes = remaining;
    String::from_utf8(value.to_vec()).ok()
}

fn take_exact<const N: usize>(bytes: &mut &[u8]) -> Option<[u8; N]> {
    if bytes.len() < N {
        return None;
    }
    let (value, remaining) = bytes.split_at(N);
    *bytes = remaining;
    value.try_into().ok()
}

pub(crate) fn deepagents_write_record_digest(
    key: &DeepAgentsWriteKey,
    value_type: Option<&str>,
    value: &[u8],
) -> CompleteContentBodyDigest {
    const DOMAIN: &[u8] = b"ctx-deepagents-write-record-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    update_digest_string(&mut digest, &key.thread_id);
    update_digest_string(&mut digest, &key.checkpoint_id);
    update_digest_string(&mut digest, &key.task_id);
    digest.update(key.idx.to_be_bytes());
    match value_type {
        Some(value_type) => {
            digest.update([1]);
            update_digest_string(&mut digest, value_type);
        }
        None => digest.update([0]),
    }
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
        .expect("SHA-256 formatter must return a valid digest")
}

fn update_digest_string(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

#[derive(Debug)]
pub(crate) struct DeepAgentsResolvedContent {
    pub(crate) text: String,
    pub(crate) event: DeepAgentsNativeEvent,
    pub(crate) record_digest: CompleteContentBodyDigest,
}

pub(crate) fn validate_deepagents_content_schema(conn: &Connection) -> Result<()> {
    use crate::provider::sqlite::{
        ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    };

    if !sqlite_table_exists(conn, "checkpoints")? || !sqlite_table_exists(conn, "writes")? {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents content source is missing required tables".to_owned(),
        ));
    }
    ensure_sqlite_table_columns(
        &sqlite_table_columns(conn, "checkpoints")?,
        "Deep Agents checkpoints table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "checkpoint",
            "metadata",
        ],
    )?;
    ensure_sqlite_table_columns(
        &sqlite_table_columns(conn, "writes")?,
        "Deep Agents writes table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "task_id",
            "idx",
            "channel",
            "type",
            "value",
        ],
    )
}

/// Resolve one content address against a caller-owned frozen, query-only SQLite snapshot.
/// This function never opens a path and never mutates provider state.
pub(crate) fn resolve_deepagents_content(
    conn: &Connection,
    address: &DeepAgentsContentAddress,
) -> Result<Option<DeepAgentsResolvedContent>> {
    let mut statement = conn.prepare(
        "select type, value from writes \
         where checkpoint_ns = '' and channel = 'messages' \
           and thread_id = ?1 and checkpoint_id = ?2 and task_id = ?3 and idx = ?4 \
         limit 2",
    )?;
    let mut rows = statement.query(rusqlite::params![
        address.thread_id,
        address.checkpoint_id,
        address.task_id,
        address.write_idx,
    ])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let value_type = row.get::<_, Option<String>>(0)?;
    let value = row.get::<_, Vec<u8>>(1)?;
    if rows.next()?.is_some() {
        return Err(CaptureError::InvalidPayload(
            "Deep Agents content address matched multiple writes".to_owned(),
        ));
    }
    let messages = deepagents_messages_from_blob(value_type.as_deref(), &value)?;
    let offset = usize::try_from(address.message_offset).map_err(|_| {
        CaptureError::InvalidPayload(
            "Deep Agents message offset exceeds platform limits".to_owned(),
        )
    })?;
    let Some(message) = messages.get(offset).cloned() else {
        return Ok(None);
    };
    let text = message.text.clone();
    let key = DeepAgentsWriteKey {
        thread_id: address.thread_id.clone(),
        checkpoint_id: address.checkpoint_id.clone(),
        task_id: address.task_id.clone(),
        idx: address.write_idx,
    };
    let event = resolved_event(address, message, &key);
    Ok(Some(DeepAgentsResolvedContent {
        text,
        event,
        record_digest: deepagents_write_record_digest(&key, value_type.as_deref(), &value),
    }))
}

fn resolved_event(
    address: &DeepAgentsContentAddress,
    message: DeepAgentsMessage,
    key: &DeepAgentsWriteKey,
) -> DeepAgentsNativeEvent {
    let cursor = format!(
        "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
        address.thread_id,
        address.checkpoint_id,
        address.task_id,
        address.write_idx,
        address.message_offset,
    );
    let identity = message
        .message_id
        .as_deref()
        .map(|message_id| deepagents_message_identity(&address.thread_id, message_id));
    let provider_event_hash = identity
        .as_ref()
        .map(|identity| identity.payload_hash.as_str())
        .unwrap_or(&cursor);
    deepagents_native_event(
        key,
        &DeepAgentsParsedMessage {
            offset: address.message_offset as usize,
            provider_event_index: identity
                .as_ref()
                .map_or(0, |identity| identity.provider_index),
            message,
        },
        DateTime::<Utc>::UNIX_EPOCH,
        provider_event_hash,
        identity.as_ref().map(|identity| identity.provider_index),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_address_round_trips_and_rejects_suffixes() {
        let address = DeepAgentsContentAddress {
            thread_id: "thread/🦀".to_owned(),
            checkpoint_id: "checkpoint-a".to_owned(),
            task_id: "task-a".to_owned(),
            write_idx: -7,
            message_offset: 12,
        };
        let encoded = address.encode().unwrap();
        assert_eq!(decode_deepagents_content_address(&encoded), Some(address));
        let mut suffixed = encoded;
        suffixed.push(0);
        assert!(decode_deepagents_content_address(&suffixed).is_none());

        let oversized = DeepAgentsContentAddress {
            thread_id: "t".repeat(COMPLETE_CONTENT_MAX_LOCATOR_BYTES),
            checkpoint_id: "checkpoint".to_owned(),
            task_id: "task".to_owned(),
            write_idx: 0,
            message_offset: 0,
        };
        assert!(oversized.encode().is_none());
    }
}
