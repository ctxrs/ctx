use ctx_history_core::{CoreRecord, StableEntityId, StableEntityKind};

use crate::{IndexError, Result};

pub const SESSION_EVENT_ORDER_SESSION_PREFIX_LEN: usize = StableEntityId::CANONICAL_LEN;
pub const SESSION_EVENT_ORDER_KEY_LEN: usize = SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 8 + 9 + 16;
const SESSION_EVENT_ORDER_SEQUENCE_OFFSET: usize = SESSION_EVENT_ORDER_SESSION_PREFIX_LEN;
const SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET: usize = SESSION_EVENT_ORDER_SEQUENCE_OFFSET + 8;
const SESSION_EVENT_ORDER_EVENT_ID_OFFSET: usize = SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 9;
const SESSION_EVENT_ORDER_FIELD: &str = "session_event_order";

/// Exact session-coordinate term used for bounded forward traversal.
///
/// Big-endian encoding preserves the existing deterministic session order:
/// sequence, `None` before `Some(timestamp)`, signed timestamp, then compact
/// event UUID. The full canonical session identity is the range prefix, so a
/// compact UUID collision can never mix session ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionEventOrderKey(pub(super) [u8; SESSION_EVENT_ORDER_KEY_LEN]);

impl SessionEventOrderKey {
    pub fn for_core_record(record: &CoreRecord) -> Result<Self> {
        Self::from_parts(
            record.session_id,
            record.event_sequence,
            record.occurred_at_unix_ms,
            record.event_id.as_uuid(),
        )
    }

    pub(super) fn from_parts(
        session_id: StableEntityId,
        event_sequence: u64,
        occurred_at_unix_ms: Option<i64>,
        event_id: uuid::Uuid,
    ) -> Result<Self> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::WriterInvariant(
                "session event order requires a session identity",
            ));
        }
        let mut key = [0_u8; SESSION_EVENT_ORDER_KEY_LEN];
        key[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN]
            .copy_from_slice(&session_id.encode_canonical()?);
        key[SESSION_EVENT_ORDER_SEQUENCE_OFFSET..SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET]
            .copy_from_slice(&event_sequence.to_be_bytes());
        if let Some(occurred_at_unix_ms) = occurred_at_unix_ms {
            key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] = 1;
            let sortable = (occurred_at_unix_ms as u64) ^ (1_u64 << 63);
            key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                .copy_from_slice(&sortable.to_be_bytes());
        }
        key[SESSION_EVENT_ORDER_EVENT_ID_OFFSET..].copy_from_slice(event_id.as_bytes());
        Ok(Self(key))
    }

    pub fn decode_for_session(session_id: StableEntityId, encoded: &[u8]) -> Result<Self> {
        let key: [u8; SESSION_EVENT_ORDER_KEY_LEN] = encoded
            .try_into()
            .map_err(|_| IndexError::InvalidStoredDocumentField(SESSION_EVENT_ORDER_FIELD))?;
        let expected_prefix = Self::session_prefix(session_id)?;
        if key[..SESSION_EVENT_ORDER_SESSION_PREFIX_LEN] != expected_prefix {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        if key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] > 1
            || (key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] == 0
                && key[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1
                    ..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                    .iter()
                    .any(|byte| *byte != 0))
        {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        Ok(Self(key))
    }

    pub fn session_prefix(
        session_id: StableEntityId,
    ) -> Result<[u8; SESSION_EVENT_ORDER_SESSION_PREFIX_LEN]> {
        if session_id.entity_kind() != StableEntityKind::Session {
            return Err(IndexError::InvalidStoredDocumentField(
                SESSION_EVENT_ORDER_FIELD,
            ));
        }
        Ok(session_id.encode_canonical()?)
    }

    pub fn session_range_end(session_id: StableEntityId) -> Result<Vec<u8>> {
        let mut bound = Vec::with_capacity(SESSION_EVENT_ORDER_KEY_LEN + 1);
        bound.extend_from_slice(&Self::session_prefix(session_id)?);
        bound.extend(std::iter::repeat_n(
            u8::MAX,
            SESSION_EVENT_ORDER_KEY_LEN - SESSION_EVENT_ORDER_SESSION_PREFIX_LEN + 1,
        ));
        Ok(bound)
    }

    pub fn event_sequence(self) -> u64 {
        u64::from_be_bytes(
            self.0[SESSION_EVENT_ORDER_SEQUENCE_OFFSET..SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET]
                .try_into()
                .expect("fixed session event order sequence layout"),
        )
    }

    pub fn occurred_at_unix_ms(self) -> Option<i64> {
        (self.0[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET] == 1).then(|| {
            let sortable = u64::from_be_bytes(
                self.0[SESSION_EVENT_ORDER_OCCURRED_AT_OFFSET + 1
                    ..SESSION_EVENT_ORDER_EVENT_ID_OFFSET]
                    .try_into()
                    .expect("fixed session event order timestamp layout"),
            );
            (sortable ^ (1_u64 << 63)) as i64
        })
    }

    pub fn event_id(self) -> uuid::Uuid {
        uuid::Uuid::from_bytes(
            self.0[SESSION_EVENT_ORDER_EVENT_ID_OFFSET..]
                .try_into()
                .expect("fixed session event order UUID layout"),
        )
    }

    pub fn as_bytes(&self) -> &[u8; SESSION_EVENT_ORDER_KEY_LEN] {
        &self.0
    }

    pub fn into_bytes(self) -> [u8; SESSION_EVENT_ORDER_KEY_LEN] {
        self.0
    }
}
