use std::collections::BTreeMap;

use anyhow::Result;
use ctx_history_index::{SourceCoreRecordAggregate, MAX_SOURCE_EVENT_PAGE_ITEMS};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::manifest::{
    source_contract_fingerprint, SourceProjectionAcknowledgement, SourceProjectionFrontier,
    SOURCE_ACKNOWLEDGEMENT_STATE, SOURCE_FRONTIER_STATE,
};
use super::{
    SemanticVectorStore, SourceBackedSemanticGeneration, SourceBackedSemanticOutcome,
    SourceBackedSemanticSource,
};
use crate::semantic::vector_store_schema::SemanticVectorStoreError;

const SOURCE_RECONCILIATION_DOMAIN: &[u8] = b"ctx-semantic-source-reconciliation-v1\0";
const SOURCE_RECEIPT_DOMAIN: &[u8] = b"ctx-semantic-source-receipt-v1\0";
const RECEIPT_SET_DOMAIN: &[u8] = b"ctx-semantic-source-receipt-set-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SourceProjectionReceipt {
    pub(super) source_identity_digest: String,
    pub(super) indexed_documents: u64,
    pub(super) semantic_eligible_documents: u64,
    pub(super) core_record_accumulator: String,
    pub(super) contract_fingerprint: String,
    pub(super) semantic_policy_fingerprint: String,
    pub(super) owned_event_count: u64,
    pub(super) owned_event_ids_hash: String,
}

impl SourceProjectionReceipt {
    pub(super) fn matches(
        &self,
        source: &SourceBackedSemanticSource,
        contract_fingerprint: &str,
        policy_fingerprint: &str,
    ) -> bool {
        self.source_identity_digest == source.aggregate.source_identity_digest()
            && self.indexed_documents == source.aggregate.indexed_documents()
            && self.semantic_eligible_documents == source.aggregate.semantic_eligible_documents()
            && self.core_record_accumulator == source.aggregate.core_record_accumulator()
            && self.contract_fingerprint == contract_fingerprint
            && self.semantic_policy_fingerprint == policy_fingerprint
            && self.owned_event_count <= self.semantic_eligible_documents
    }
}

impl SemanticVectorStore {
    pub(super) fn source_frontier(&self) -> Result<Option<SourceProjectionFrontier>> {
        self.maintenance_json(SOURCE_FRONTIER_STATE)
    }

    pub(super) fn source_acknowledgement(&self) -> Result<Option<SourceProjectionAcknowledgement>> {
        self.maintenance_json(SOURCE_ACKNOWLEDGEMENT_STATE)
    }

    pub(super) fn maintenance_json<T>(&self, key: &str) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    SemanticVectorStoreError::reset_required(format!(
                        "semantic vector store has invalid {key} state: {error}"
                    ))
                    .into()
                })
            })
            .transpose()
    }

    pub(super) fn store_source_frontier(&self, frontier: &SourceProjectionFrontier) -> Result<()> {
        self.conn.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SOURCE_FRONTIER_STATE, serde_json::to_string(frontier)?],
        )?;
        Ok(())
    }

    pub(super) fn source_document_ids(
        &self,
        source_identity_digest: &str,
        except_reconciliation_id: Option<&str>,
    ) -> Result<Vec<Uuid>> {
        self.flat
            .source_event_ids_except_reconciliation(
                source_identity_digest,
                except_reconciliation_id,
                MAX_SOURCE_EVENT_PAGE_ITEMS,
            )
            .map_err(anyhow::Error::new)
    }

    pub(super) fn source_receipts(&self) -> Result<BTreeMap<String, SourceProjectionReceipt>> {
        let mut statement = self.conn.prepare(
            "SELECT source_identity_digest, indexed_documents,
                    semantic_eligible_documents, core_record_accumulator,
                    contract_fingerprint, semantic_policy_fingerprint,
                    owned_event_count, owned_event_ids_hash
             FROM semantic_source_receipts ORDER BY source_identity_digest",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SourceProjectionReceipt {
                source_identity_digest: row.get(0)?,
                indexed_documents: row.get(1)?,
                semantic_eligible_documents: row.get(2)?,
                core_record_accumulator: row.get(3)?,
                contract_fingerprint: row.get(4)?,
                semantic_policy_fingerprint: row.get(5)?,
                owned_event_count: row.get(6)?,
                owned_event_ids_hash: row.get(7)?,
            })
        })?;
        rows.map(|row| row.map(|receipt| (receipt.source_identity_digest.clone(), receipt)))
            .collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }

    pub(super) fn source_receipt(
        &self,
        source_identity_digest: &str,
    ) -> Result<Option<SourceProjectionReceipt>> {
        self.conn
            .query_row(
                "SELECT source_identity_digest, indexed_documents,
                        semantic_eligible_documents, core_record_accumulator,
                        contract_fingerprint, semantic_policy_fingerprint,
                        owned_event_count, owned_event_ids_hash
                 FROM semantic_source_receipts WHERE source_identity_digest = ?1",
                [source_identity_digest],
                |row| {
                    Ok(SourceProjectionReceipt {
                        source_identity_digest: row.get(0)?,
                        indexed_documents: row.get(1)?,
                        semantic_eligible_documents: row.get(2)?,
                        core_record_accumulator: row.get(3)?,
                        contract_fingerprint: row.get(4)?,
                        semantic_policy_fingerprint: row.get(5)?,
                        owned_event_count: row.get(6)?,
                        owned_event_ids_hash: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn next_source_receipt_identity(
        &self,
        after: Option<&str>,
    ) -> Result<Option<String>> {
        let value = match after {
            Some(after) => self
                .conn
                .query_row(
                    "SELECT source_identity_digest FROM semantic_source_receipts
                     WHERE source_identity_digest > ?1
                     ORDER BY source_identity_digest LIMIT 1",
                    [after],
                    |row| row.get(0),
                )
                .optional()?,
            None => self
                .conn
                .query_row(
                    "SELECT source_identity_digest FROM semantic_source_receipts
                     ORDER BY source_identity_digest LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?,
        };
        Ok(value)
    }

    pub(super) fn build_source_receipt(
        &self,
        source: &SourceBackedSemanticSource,
        contract_fingerprint: &str,
        policy_fingerprint: &str,
        reconciliation_id: &str,
    ) -> Result<SourceProjectionReceipt> {
        let lookup = self
            .flat
            .source_event_lookup(source.aggregate.source_identity_digest())
            .map_err(anyhow::Error::new)?;
        let mut count = 0_u64;
        let mut digest = Sha256::new();
        digest.update(SOURCE_RECEIPT_DOMAIN);
        for event in lookup.events() {
            if event.source_reconciliation_id != reconciliation_id {
                return Err(SemanticVectorStoreError::reset_required(
                    "semantic source retained stale ownership after source completion",
                )
                .into());
            }
            digest.update(event.event_id.as_bytes());
            digest.update([0]);
            digest.update(event.seq.to_be_bytes());
            digest.update(event.source_text_hash.as_bytes());
            digest.update(event.stable_identity_hash);
            digest.update([0]);
            count = count.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic source ownership count overflowed",
                )
            })?;
        }
        if count > source.aggregate.semantic_eligible_documents() {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic source owns more vectors than its Core aggregate permits",
            )
            .into());
        }
        Ok(SourceProjectionReceipt {
            source_identity_digest: source.aggregate.source_identity_digest().to_owned(),
            indexed_documents: source.aggregate.indexed_documents(),
            semantic_eligible_documents: source.aggregate.semantic_eligible_documents(),
            core_record_accumulator: source.aggregate.core_record_accumulator().to_owned(),
            contract_fingerprint: contract_fingerprint.to_owned(),
            semantic_policy_fingerprint: policy_fingerprint.to_owned(),
            owned_event_count: count,
            owned_event_ids_hash: hex(&digest.finalize()),
        })
    }

    pub(super) fn source_receipts_summary(&self) -> Result<(u64, String, u64)> {
        let receipts = self.source_receipts()?;
        let mut digest = Sha256::new();
        digest.update(RECEIPT_SET_DOMAIN);
        let mut projected_documents = 0_u64;
        for receipt in receipts.values() {
            digest.update(serde_json::to_vec(receipt)?);
            digest.update([0]);
            projected_documents = projected_documents
                .checked_add(receipt.owned_event_count)
                .ok_or_else(|| {
                    SemanticVectorStoreError::reset_required(
                        "semantic source receipt count overflowed",
                    )
                })?;
        }
        Ok((
            u64::try_from(receipts.len())?,
            hex(&digest.finalize()),
            projected_documents,
        ))
    }

    pub(super) fn finish_source_generation(
        &mut self,
        frontier: &SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<SourceBackedSemanticOutcome> {
        let receipts = self.source_receipts()?;
        let contract_fingerprint = source_contract_fingerprint()?;
        if receipts.len() != generation.sources.len()
            || generation.sources.iter().any(|source| {
                receipts
                    .get(source.aggregate.source_identity_digest())
                    .is_none_or(|receipt| {
                        !receipt.matches(
                            source,
                            &contract_fingerprint,
                            &generation.semantic_policy_fingerprint,
                        )
                    })
            })
        {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic generation receipts do not match target Core source aggregates",
            )
            .into());
        }

        let (source_receipt_count, source_receipts_hash, receipt_documents) =
            self.source_receipts_summary()?;
        if receipt_documents > frontier.semantic_documents {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic source receipts exceed metadata-eligible Core records",
            )
            .into());
        }
        self.flat
            .finish_reconciliation_view()
            .map_err(anyhow::Error::new)?;
        let stats = self.flat.active_stats().map_err(anyhow::Error::new)?;
        if stats.active_events as u64 != receipt_documents
            || (receipt_documents == 0 && stats.active_chunks != 0)
            || (receipt_documents != 0 && stats.active_chunks < stats.active_events)
        {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic source receipts do not match flat manifest counters",
            )
            .into());
        }
        let acknowledgement = SourceProjectionAcknowledgement {
            contract_version: frontier.contract_version,
            contract_fingerprint: frontier.contract_fingerprint.clone(),
            core_generation_id: frontier.core_generation_id.clone(),
            semantic_policy_fingerprint: frontier.semantic_policy_fingerprint.clone(),
            consumer_build_id: frontier.consumer_build_id.clone(),
            semantic_documents: frontier.semantic_documents,
            projected_documents: receipt_documents,
            source_receipt_count,
            source_receipts_hash,
            flat_generation: stats.generation,
            flat_generation_hash: stats.generation_hash.unwrap_or_default(),
            flat_active_events: stats.active_events as u64,
            flat_active_chunks: stats.active_chunks as u64,
        };
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![
                SOURCE_ACKNOWLEDGEMENT_STATE,
                serde_json::to_string(&acknowledgement)?
            ],
        )?;
        transaction.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [SOURCE_FRONTIER_STATE],
        )?;
        transaction.commit()?;
        Ok(SourceBackedSemanticOutcome {
            ready: true,
            ..SourceBackedSemanticOutcome::default()
        })
    }
}

pub(super) fn store_frontier(
    transaction: &Transaction<'_>,
    frontier: &SourceProjectionFrontier,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SOURCE_FRONTIER_STATE, serde_json::to_string(frontier)?],
    )?;
    Ok(())
}

pub(super) fn store_source_receipt(
    transaction: &Transaction<'_>,
    receipt: &SourceProjectionReceipt,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO semantic_source_receipts
         (source_identity_digest, indexed_documents, semantic_eligible_documents,
          core_record_accumulator, contract_fingerprint, semantic_policy_fingerprint,
          owned_event_count, owned_event_ids_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_identity_digest) DO UPDATE SET
            indexed_documents = excluded.indexed_documents,
            semantic_eligible_documents = excluded.semantic_eligible_documents,
            core_record_accumulator = excluded.core_record_accumulator,
            contract_fingerprint = excluded.contract_fingerprint,
            semantic_policy_fingerprint = excluded.semantic_policy_fingerprint,
            owned_event_count = excluded.owned_event_count,
            owned_event_ids_hash = excluded.owned_event_ids_hash",
        params![
            receipt.source_identity_digest,
            receipt.indexed_documents,
            receipt.semantic_eligible_documents,
            receipt.core_record_accumulator,
            receipt.contract_fingerprint,
            receipt.semantic_policy_fingerprint,
            receipt.owned_event_count,
            receipt.owned_event_ids_hash,
        ],
    )?;
    Ok(())
}

pub(super) fn clear_active_source(frontier: &mut SourceProjectionFrontier) {
    frontier.active_source_identity_digest = None;
    frontier.active_source_reconciliation_id = None;
    frontier.active_source_indexed_documents = 0;
    frontier.active_source_semantic_documents = 0;
    frontier.processed_source_documents = 0;
    frontier.processed_source_semantic_documents = 0;
    frontier.after_identity = None;
    frontier.source_scan_complete = false;
    frontier.removing_source = false;
    frontier.last_failure = None;
}

pub(super) fn source_reconciliation_id(
    contract_fingerprint: &str,
    semantic_policy_fingerprint: &str,
    aggregate: &SourceCoreRecordAggregate,
) -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE_RECONCILIATION_DOMAIN);
    digest.update(contract_fingerprint.as_bytes());
    digest.update(semantic_policy_fingerprint.as_bytes());
    digest.update(aggregate.source_identity_digest().as_bytes());
    digest.update(aggregate.indexed_documents().to_be_bytes());
    digest.update(aggregate.semantic_eligible_documents().to_be_bytes());
    digest.update(aggregate.core_record_accumulator().as_bytes());
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}
