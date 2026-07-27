//! Cumulative-message identity, committed-event lookup, and deduplication state.

use std::collections::BTreeSet;

use ctx_history_core::CaptureProvider;
use ctx_history_store::{Store, StoreError};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::provider::importer::provider_scoped_source_uuid;
use crate::{CaptureError, Result, DEEPAGENTS_SQLITE_SOURCE_FORMAT};

use super::deepagents_oversize_limit;
use super::message::DeepAgentsMessage;

#[derive(Clone)]
pub(super) enum DeepAgentsWritePlan {
    UnknownThread,
    DecodeRejected,
    RejectedKey(String),
    Oversize {
        observed_bytes: u64,
    },
    Accepted {
        next_event_index: u64,
        accepted_offsets: Vec<u32>,
        accepted_event_indices: Vec<u64>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DeepAgentsMessageIdentity {
    pub(super) provider_index: u64,
    pub(super) payload_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DeepAgentsMessageDedupeKey([u8; 32]);

fn deepagents_message_dedupe_key(thread_id: &str, message_id: &str) -> DeepAgentsMessageDedupeKey {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-deepagents-in-memory-dedupe-v1");
    for component in [thread_id.as_bytes(), message_id.as_bytes()] {
        hasher.update(
            u64::try_from(component.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(component);
    }
    DeepAgentsMessageDedupeKey(hasher.finalize().into())
}

pub(super) struct DeepAgentsMessageLedger {
    committed_store: Option<Store>,
    raw_source_path: Option<String>,
    prior_row_keys: BTreeSet<DeepAgentsMessageDedupeKey>,
    current_row_keys: BTreeSet<DeepAgentsMessageDedupeKey>,
}

impl DeepAgentsMessageLedger {
    pub(super) fn new(committed_store: Option<Store>, raw_source_path: Option<String>) -> Self {
        Self {
            committed_store,
            raw_source_path,
            prior_row_keys: BTreeSet::new(),
            current_row_keys: BTreeSet::new(),
        }
    }

    pub(super) fn begin_row(&mut self) {
        self.prior_row_keys
            .extend(self.current_row_keys.iter().copied());
    }

    pub(super) fn reset_for_batch_request(&mut self) {
        self.prior_row_keys.clear();
        self.prior_row_keys
            .extend(self.current_row_keys.iter().copied());
    }

    #[cfg(test)]
    pub(super) fn retained_key_counts(&self) -> (usize, usize) {
        (self.prior_row_keys.len(), self.current_row_keys.len())
    }

    pub(super) fn plan_messages(
        &mut self,
        thread_id: &str,
        messages: &[DeepAgentsMessage],
        retained_before_offsets: u64,
        start_event_index: u64,
    ) -> Result<DeepAgentsWritePlan> {
        let mut next_event_index = start_event_index;
        let mut accepted_offsets = Vec::new();
        let mut accepted_event_indices = Vec::new();
        let mut current_row_keys = BTreeSet::new();
        for (offset, message) in messages.iter().enumerate() {
            if let Some(message_id) = message.message_id.as_deref() {
                let dedupe_key = deepagents_message_dedupe_key(thread_id, message_id);
                let first_in_current_row = current_row_keys.insert(dedupe_key);
                if self.prior_row_keys.contains(&dedupe_key) || !first_in_current_row {
                    continue;
                }
                if let Some(existing_index) = self.committed_event_index(thread_id, message_id)? {
                    next_event_index = next_event_index.max(existing_index.saturating_add(1));
                    continue;
                }
            }
            let offset = u32::try_from(offset).map_err(|_| {
                CaptureError::InvalidPayload(
                    "Deep Agents write contains too many decoded messages".to_owned(),
                )
            })?;
            accepted_offsets.push(offset);
            accepted_event_indices.push(next_event_index);
            next_event_index =
                next_event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Deep Agents event index overflowed",
                    ))?;
        }
        let offset_bytes = u64::try_from(accepted_offsets.len())
            .ok()
            .and_then(|count| count.checked_mul(12))
            .ok_or(CaptureError::SystemInvariant(
                "Deep Agents accepted-offset byte count overflowed",
            ))?;
        let observed_bytes = retained_before_offsets.checked_add(offset_bytes).ok_or(
            CaptureError::SystemInvariant("Deep Agents retained byte count overflowed"),
        )?;
        if observed_bytes > deepagents_oversize_limit()? {
            return Ok(DeepAgentsWritePlan::Oversize { observed_bytes });
        }
        // Deep Agents writes are cumulative message snapshots. Keep the complete current valid
        // row's fixed-size identities, including messages already seen in the prior row or Store.
        // At a raw-batch boundary this one-record-bounded carry is the prefix needed by the
        // following cumulative row; retaining only newly accepted identities makes partitioning
        // observable. A rejected row does not erase the last valid cumulative frontier.
        self.current_row_keys = current_row_keys;
        Ok(DeepAgentsWritePlan::Accepted {
            next_event_index,
            accepted_offsets,
            accepted_event_indices,
        })
    }

    fn committed_event_index(&self, thread_id: &str, message_id: &str) -> Result<Option<u64>> {
        let Some(store) = self.committed_store.as_ref() else {
            return Ok(None);
        };
        let identity = deepagents_message_identity(thread_id, message_id);
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::DeepAgents,
            thread_id,
            DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            self.raw_source_path.as_deref(),
        );
        let dedupe_key = Store::provider_source_event_dedupe_key(
            source_id,
            identity.provider_index,
            &identity.payload_hash,
        );
        let event_id = match store.event_id_by_dedupe_key(&dedupe_key) {
            Ok(event_id) => event_id,
            Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
            Err(error) => return Err(CaptureError::Store(error)),
        };
        let event = store.get_event(event_id)?;
        let event_index = event
            .payload
            .get("provider_event_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "committed Deep Agents event is missing its provider event index".to_owned(),
                )
            })?;
        Ok(Some(event_index))
    }
}

pub(super) fn deepagents_message_identity(
    thread_id: &str,
    message_id: &str,
) -> DeepAgentsMessageIdentity {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        b"ctx-deepagents-message-v1".as_slice(),
        thread_id.as_bytes(),
        message_id.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    DeepAgentsMessageIdentity {
        provider_index: hash,
        payload_hash: format!("fnv1a64:{hash:016x}"),
    }
}
