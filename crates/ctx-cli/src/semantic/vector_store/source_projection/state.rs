use std::collections::BTreeMap;

use anyhow::Result;
use ctx_history_index::SourceCoreRecordAggregate;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::manifest::{
    semantic_policy_fingerprint, source_consumer_build_id, source_contract_fingerprint,
    validate_generation_id, AcknowledgedSourceProjection, SourceProjectionAcknowledgement,
    SourceProjectionFrontier, SourceTraversalPhase, SOURCE_ACKNOWLEDGEMENT_STATE,
    SOURCE_CONTRACT_VERSION, SOURCE_FRONTIER_STATE, SOURCE_REPLAY_FRONTIER_STATE,
};
use super::{
    SemanticVectorStore, SourceBackedGenerationPin, SourceBackedSemanticGeneration,
    SourceBackedSemanticOutcome, SourceBackedSemanticSource,
};
use crate::semantic::{
    vector_store::control::FULL_REBUILD_STATE,
    vector_store::flat_segments::{
        FlatPublicationToken, FlatPublishOutcome, FlatSourceReceipt, FlatSourceState,
    },
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

const SOURCE_RECONCILIATION_DOMAIN: &[u8] = b"ctx-semantic-source-reconciliation-v1\0";
const RECEIPT_SET_DOMAIN: &[u8] = b"ctx-semantic-source-receipt-set-v1\0";

pub(super) type SourceProjectionStates = BTreeMap<String, Option<FlatSourceReceipt>>;

pub(super) fn source_projection_states(states: Vec<FlatSourceState>) -> SourceProjectionStates {
    states
        .into_iter()
        .map(|state| (state.source_identity_digest, state.receipt))
        .collect()
}

pub(super) fn source_receipt_matches(
    receipt: &FlatSourceReceipt,
    source: &SourceBackedSemanticSource,
    contract_fingerprint: &str,
    policy_fingerprint: &str,
) -> bool {
    receipt.source_identity_digest == source.aggregate.source_identity_digest()
        && receipt.indexed_documents == source.aggregate.indexed_documents()
        && receipt.semantic_eligible_documents == source.aggregate.semantic_eligible_documents()
        && receipt.core_record_accumulator == source.aggregate.core_record_accumulator()
        && receipt.contract_fingerprint == contract_fingerprint
        && receipt.semantic_policy_fingerprint == policy_fingerprint
        && receipt.owned_event_count <= receipt.semantic_eligible_documents
}

impl SemanticVectorStore {
    pub(super) fn source_frontier(&self) -> Result<Option<SourceProjectionFrontier>> {
        self.maintenance_json(SOURCE_FRONTIER_STATE)
    }

    pub(super) fn source_acknowledgement(&self) -> Result<Option<SourceProjectionAcknowledgement>> {
        self.maintenance_json(SOURCE_ACKNOWLEDGEMENT_STATE)
    }

    pub(super) fn recover_lost_flat_publication(&self) -> Result<()> {
        let current = self
            .flat
            .active_publication_token()
            .map_err(anyhow::Error::new)?;
        if let Some(frontier) = self.source_frontier()? {
            if publication_is_lost(&frontier.flat_publication, &current)? {
                let replay = self
                    .maintenance_json::<SourceProjectionFrontier>(SOURCE_REPLAY_FRONTIER_STATE)?
                    .ok_or_else(|| {
                        SemanticVectorStoreError::reset_required(
                            "semantic Flat publication was lost without a replay frontier",
                        )
                    })?;
                if publication_is_lost(&replay.flat_publication, &current)? {
                    return Err(SemanticVectorStoreError::reset_required(
                        "semantic Flat rollback predates its replay frontier",
                    )
                    .into());
                }
                let transaction = self.conn.unchecked_transaction()?;
                store_frontier(&transaction, &replay)?;
                transaction.execute(
                    "DELETE FROM semantic_maintenance_state WHERE key = ?1",
                    [SOURCE_ACKNOWLEDGEMENT_STATE],
                )?;
                transaction.commit()?;
            }
            return Ok(());
        }

        if let Some(acknowledgement) = self.source_acknowledgement()? {
            let acknowledged = FlatPublicationToken {
                generation: acknowledgement.flat_generation,
                generation_hash: (!acknowledgement.flat_generation_hash.is_empty())
                    .then_some(acknowledgement.flat_generation_hash),
            };
            if publication_is_lost(&acknowledged, &current)? {
                let replay = self
                    .maintenance_json::<SourceProjectionFrontier>(SOURCE_REPLAY_FRONTIER_STATE)?
                    .ok_or_else(|| {
                        SemanticVectorStoreError::reset_required(
                            "acknowledged semantic Flat publication was lost without replay state",
                        )
                    })?;
                if publication_is_lost(&replay.flat_publication, &current)? {
                    return Err(SemanticVectorStoreError::reset_required(
                        "acknowledged semantic Flat rollback predates its replay frontier",
                    )
                    .into());
                }
                let transaction = self.conn.unchecked_transaction()?;
                store_frontier(&transaction, &replay)?;
                transaction.execute(
                    "DELETE FROM semantic_maintenance_state WHERE key = ?1",
                    [SOURCE_ACKNOWLEDGEMENT_STATE],
                )?;
                transaction.commit()?;
            }
        }
        Ok(())
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

    pub(super) fn source_receipts_summary(
        &self,
        states: &SourceProjectionStates,
    ) -> Result<(u64, String, u64)> {
        let mut digest = Sha256::new();
        digest.update(RECEIPT_SET_DOMAIN);
        let mut projected_documents = 0_u64;
        let mut receipt_count = 0_u64;
        for receipt in states.values() {
            let receipt = receipt.as_ref().ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic Flat source state is incomplete at generation finalization",
                )
            })?;
            digest.update(serde_json::to_vec(receipt)?);
            digest.update([0]);
            receipt_count = receipt_count.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required("semantic source receipt count overflowed")
            })?;
            projected_documents = projected_documents
                .checked_add(receipt.owned_event_count)
                .ok_or_else(|| {
                    SemanticVectorStoreError::reset_required(
                        "semantic source receipt count overflowed",
                    )
                })?;
        }
        Ok((receipt_count, hex(&digest.finalize()), projected_documents))
    }

    pub(super) fn finish_source_generation(
        &mut self,
        frontier: &SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
        states: &SourceProjectionStates,
    ) -> Result<SourceBackedSemanticOutcome> {
        let contract_fingerprint = source_contract_fingerprint()?;
        if states.len() != generation.sources.len()
            || generation.sources.iter().any(|source| {
                states
                    .get(source.aggregate.source_identity_digest())
                    .and_then(Option::as_ref)
                    .is_none_or(|receipt| {
                        !source_receipt_matches(
                            receipt,
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
            self.source_receipts_summary(states)?;
        if receipt_documents > frontier.semantic_documents {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic source receipts exceed metadata-eligible Core records",
            )
            .into());
        }
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

    pub(in crate::semantic) fn source_backed_generation_pin_exact(
        &self,
        core_generation_id: &str,
        semantic_documents: u64,
    ) -> Result<SourceBackedGenerationPin> {
        semantic_owned_sidecar_result((|| {
            let Some(projection) = self.acknowledged_source_projection(
                core_generation_id,
                Some(semantic_documents),
                None,
                true,
            )?
            else {
                return Ok(SourceBackedGenerationPin::NotReady);
            };
            if projection.projected_documents == 0 {
                return Ok(SourceBackedGenerationPin::ReadyEmpty);
            }
            projection
                .flat
                .map(SourceBackedGenerationPin::Ready)
                .ok_or_else(|| {
                    SemanticVectorStoreError::reset_required(
                        "nonempty acknowledged semantic generation has no flat pin",
                    )
                    .into()
                })
        })())
    }

    pub(super) fn acknowledged_source_projection(
        &self,
        core_generation_id: &str,
        expected_semantic_documents: Option<u64>,
        expected_semantic_policy_fingerprint: Option<&str>,
        require_pin: bool,
    ) -> Result<Option<AcknowledgedSourceProjection>> {
        validate_generation_id(core_generation_id)?;
        let full_rebuild_pending = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM semantic_maintenance_state WHERE key = ?1
             )",
            [FULL_REBUILD_STATE],
            |row| row.get::<_, bool>(0),
        )?;
        if full_rebuild_pending {
            return Ok(None);
        }
        let Some(acknowledgement) = self.source_acknowledgement()? else {
            return Ok(None);
        };
        if self.source_frontier()?.is_some() {
            return Ok(None);
        }
        let fingerprint = source_contract_fingerprint()?;
        if acknowledgement.contract_version != SOURCE_CONTRACT_VERSION
            || acknowledgement.contract_fingerprint != fingerprint
            || acknowledgement.core_generation_id != core_generation_id
            || acknowledgement.semantic_policy_fingerprint
                != expected_semantic_policy_fingerprint
                    .map(str::to_owned)
                    .unwrap_or(semantic_policy_fingerprint()?)
            || acknowledgement.consumer_build_id
                != source_consumer_build_id(&fingerprint, core_generation_id)
            || expected_semantic_documents
                .is_some_and(|expected| acknowledgement.semantic_documents != expected)
        {
            return Ok(None);
        }
        let states =
            source_projection_states(self.flat.source_states().map_err(anyhow::Error::new)?);
        if states.values().any(Option::is_none) {
            return Ok(None);
        }
        let (receipt_count, receipt_hash, projected_documents) =
            self.source_receipts_summary(&states)?;
        if acknowledgement.source_receipt_count != receipt_count
            || acknowledgement.source_receipts_hash != receipt_hash
            || acknowledgement.projected_documents != projected_documents
            || acknowledgement.projected_documents > acknowledgement.semantic_documents
        {
            return Ok(None);
        }
        let manifest_stats = self.flat.active_stats().map_err(anyhow::Error::new)?;
        let manifest_matches = acknowledgement.flat_generation == manifest_stats.generation
            && acknowledgement.flat_generation_hash
                == manifest_stats
                    .generation_hash
                    .as_deref()
                    .unwrap_or_default()
            && acknowledgement.flat_active_events == manifest_stats.active_events as u64
            && acknowledgement.flat_active_chunks == manifest_stats.active_chunks as u64
            && acknowledgement.flat_active_events == acknowledgement.projected_documents
            && (acknowledgement.projected_documents != 0
                || acknowledgement.flat_active_chunks == 0);
        if !manifest_matches {
            return Ok(None);
        }
        let flat = if require_pin && acknowledgement.projected_documents != 0 {
            let Some(pinned) = self.flat_pin_generation()? else {
                return Ok(None);
            };
            if pinned.generation() != acknowledgement.flat_generation
                || pinned.generation_hash() != acknowledgement.flat_generation_hash
                || pinned.stats().active_events as u64 != acknowledgement.flat_active_events
                || pinned.stats().active_chunks as u64 != acknowledgement.flat_active_chunks
            {
                return Ok(None);
            }
            Some(pinned)
        } else {
            None
        };
        Ok(Some(AcknowledgedSourceProjection {
            flat,
            projected_documents: acknowledgement.projected_documents,
        }))
    }

    pub(super) fn begin_or_resume_source_generation(
        &self,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<SourceProjectionFrontier> {
        let fingerprint = source_contract_fingerprint()?;
        let previous_frontier = self.source_frontier()?;
        if let Some(frontier) = previous_frontier.as_ref() {
            if frontier.contract_version == SOURCE_CONTRACT_VERSION
                && frontier.contract_fingerprint == fingerprint
                && frontier.core_generation_id == generation.core_generation_id
                && frontier.semantic_policy_fingerprint == generation.semantic_policy_fingerprint
                && frontier.semantic_documents == generation.semantic_documents
            {
                return Ok(frontier.clone());
            }
        }
        let frontier = SourceProjectionFrontier {
            contract_version: SOURCE_CONTRACT_VERSION,
            contract_fingerprint: fingerprint.clone(),
            core_generation_id: generation.core_generation_id.clone(),
            semantic_policy_fingerprint: generation.semantic_policy_fingerprint.clone(),
            consumer_build_id: source_consumer_build_id(
                &fingerprint,
                &generation.core_generation_id,
            ),
            semantic_documents: generation.semantic_documents,
            source_traversal_phase: SourceTraversalPhase::RemovingStaleSources,
            source_traversal_after_identity_digest: None,
            active_source_identity_digest: None,
            active_source_reconciliation_id: None,
            active_source_indexed_documents: 0,
            active_source_semantic_documents: 0,
            processed_source_documents: 0,
            processed_source_semantic_documents: 0,
            after_identity: None,
            source_scan_complete: false,
            removing_source: false,
            last_failure: None,
            flat_publication: self
                .flat
                .active_publication_token()
                .map_err(anyhow::Error::new)?,
        };
        let transaction = self.conn.unchecked_transaction()?;
        store_frontier(&transaction, &frontier)?;
        transaction.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [SOURCE_ACKNOWLEDGEMENT_STATE],
        )?;
        transaction.commit()?;
        Ok(frontier)
    }

    pub(super) fn start_source_reconciliation(
        &self,
        frontier: &mut SourceProjectionFrontier,
        source: &SourceBackedSemanticSource,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<()> {
        let source_identity_digest = source.aggregate.source_identity_digest();
        frontier.active_source_identity_digest = Some(source_identity_digest.to_owned());
        frontier.active_source_reconciliation_id = Some(source_reconciliation_id(
            &frontier.contract_fingerprint,
            &generation.semantic_policy_fingerprint,
            &source.aggregate,
        ));
        frontier.active_source_indexed_documents = source.aggregate.indexed_documents();
        frontier.active_source_semantic_documents = source.aggregate.semantic_eligible_documents();
        frontier.processed_source_documents = 0;
        frontier.processed_source_semantic_documents = 0;
        frontier.after_identity = None;
        frontier.source_scan_complete = false;
        frontier.removing_source = false;
        frontier.last_failure = None;
        self.store_source_frontier(frontier)
    }

    pub(super) fn start_source_removal(
        &self,
        frontier: &mut SourceProjectionFrontier,
        source_identity_digest: &str,
    ) -> Result<()> {
        frontier.active_source_identity_digest = Some(source_identity_digest.to_owned());
        frontier.active_source_reconciliation_id = None;
        frontier.active_source_indexed_documents = 0;
        frontier.active_source_semantic_documents = 0;
        frontier.processed_source_documents = 0;
        frontier.processed_source_semantic_documents = 0;
        frontier.after_identity = None;
        frontier.source_scan_complete = true;
        frontier.removing_source = true;
        frontier.last_failure = None;
        self.store_source_frontier(frontier)
    }
}

pub(super) fn commit_frontier_after_flat(
    transaction: &Transaction<'_>,
    replay: &SourceProjectionFrontier,
    frontier: &mut SourceProjectionFrontier,
    publication: &FlatPublishOutcome,
) -> Result<()> {
    frontier.flat_publication = publication.token();
    transaction.execute(
        "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SOURCE_REPLAY_FRONTIER_STATE, serde_json::to_string(replay)?],
    )?;
    store_frontier(transaction, frontier)
}

fn publication_is_lost(
    expected: &FlatPublicationToken,
    current: &FlatPublicationToken,
) -> Result<bool> {
    if current.generation > expected.generation {
        return Ok(false);
    }
    if current.generation < expected.generation {
        return Ok(true);
    }
    if current.generation_hash == expected.generation_hash {
        return Ok(false);
    }
    Err(SemanticVectorStoreError::reset_required(
        "semantic Flat publication generation has a different manifest hash",
    )
    .into())
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
