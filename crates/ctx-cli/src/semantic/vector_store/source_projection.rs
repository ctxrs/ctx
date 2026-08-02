use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{SourceKey, StableEntityId};
use ctx_history_index::{
    CoreEventRecord, SemanticEligibility, SourceCoreRecordAggregate, SourceEventCursor,
    VerifiedIndex, LEXICAL_SCHEMA_VERSION, MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::control::FULL_REBUILD_STATE;
use super::flat_segments::PinnedFlatGeneration;
use super::{SemanticChunkDocument, SemanticVectorStore};
use crate::semantic::{
    indexing::{semantic_chunks_for_document, semantic_document_hash, semantic_source_text},
    model_contract::SEMANTIC_DIMENSIONS,
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
    SemanticEventDocument,
};

mod manifest;

use manifest::{
    semantic_policy_fingerprint, source_consumer_build_id, source_contract_fingerprint,
    validate_flat_projection, validate_generation, validate_generation_id, validate_page,
    validate_resolved_document, AcknowledgedSourceProjection, SourceProjectionAcknowledgement,
    SourceProjectionFrontier, SourceTraversalPhase, SOURCE_ACKNOWLEDGEMENT_STATE,
    SOURCE_CONTRACT_VERSION, SOURCE_FRONTIER_STATE, SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
};

const SEARCH_DIRECTORY: &str = "search";
const SEMANTIC_DIRECTORY: &str = "semantic";
const SOURCE_RECONCILIATION_DOMAIN: &[u8] = b"ctx-semantic-source-reconciliation-v1\0";
const SOURCE_RECEIPT_DOMAIN: &[u8] = b"ctx-semantic-source-receipt-v1\0";
const RECEIPT_SET_DOMAIN: &[u8] = b"ctx-semantic-source-receipt-set-v1\0";

pub(in crate::semantic) fn source_backed_semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(SEMANTIC_DIRECTORY)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceBackedSemanticSource {
    source: SourceKey,
    aggregate: SourceCoreRecordAggregate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceBackedSemanticGeneration {
    pub(super) core_generation_id: String,
    pub(super) semantic_policy_fingerprint: String,
    /// Count of semantic-eligible records in this pinned Core generation.
    pub(super) semantic_documents: u64,
    sources: Vec<SourceBackedSemanticSource>,
}

impl SourceBackedSemanticGeneration {
    /// Binds semantic catch-up to one verified schema-v15 Core manifest and
    /// mirrors its exact per-source Core commitments.
    pub(super) fn from_verified_index(index: &VerifiedIndex) -> Result<Self> {
        let manifest = index.manifest();
        if LEXICAL_SCHEMA_VERSION != SOURCE_INPUT_LEXICAL_SCHEMA_VERSION
            || manifest.lexical_schema_version != SOURCE_INPUT_LEXICAL_SCHEMA_VERSION
        {
            return Err(anyhow!(
                "source-backed semantic input requires lexical schema v{}",
                SOURCE_INPUT_LEXICAL_SCHEMA_VERSION
            ));
        }
        if manifest.indexed_documents != index.document_count() {
            return Err(anyhow!(
                "source-backed semantic manifest count does not match its verified index"
            ));
        }
        if manifest.semantic_eligible_documents > manifest.indexed_documents {
            return Err(anyhow!(
                "source-backed semantic document count exceeds its Core manifest"
            ));
        }
        if manifest.sources.len() != manifest.core_record_aggregates.len() {
            return Err(anyhow!(
                "source-backed semantic manifest has mismatched source aggregates"
            ));
        }
        let manifest_generation_id = manifest.generation_id()?;
        if manifest_generation_id != index.generation_id() {
            return Err(anyhow!(
                "source-backed semantic manifest identity does not match its verified index"
            ));
        }
        let sources = manifest
            .sources
            .iter()
            .zip(&manifest.core_record_aggregates)
            .map(|(source, aggregate)| SourceBackedSemanticSource {
                source: source.observation().source().clone(),
                aggregate: aggregate.clone(),
            })
            .collect();
        let generation = Self {
            core_generation_id: manifest_generation_id,
            semantic_policy_fingerprint: semantic_policy_fingerprint()?,
            semantic_documents: manifest.semantic_eligible_documents,
            sources,
        };
        validate_generation(&generation)?;
        Ok(generation)
    }

    fn source(&self, source_identity_digest: &str) -> Option<&SourceBackedSemanticSource> {
        self.sources
            .binary_search_by(|source| {
                source
                    .aggregate
                    .source_identity_digest()
                    .cmp(source_identity_digest)
            })
            .ok()
            .map(|index| &self.sources[index])
    }

    fn sources_after(&self, source_identity_digest: Option<&str>) -> &[SourceBackedSemanticSource] {
        let start = source_identity_digest.map_or(0, |digest| {
            self.sources
                .partition_point(|source| source.aggregate.source_identity_digest() <= digest)
        });
        &self.sources[start..]
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedSemanticPage {
    pub(super) core_generation_id: String,
    pub(super) source_identity_digest: String,
    /// Exclusive keyset cursor used to request this page.
    pub(super) after: Option<StableEntityId>,
    pub(super) records: Vec<CoreEventRecord>,
    pub(super) terminal: bool,
}

pub(in crate::semantic) trait SourceBackedSemanticDocumentBuilder {
    /// Builds one semantic document exclusively from complete records in the
    /// same pinned Core generation. `None` is an intentional policy filter.
    fn build_document(&mut self, record: &CoreEventRecord)
        -> Result<Option<SemanticEventDocument>>;
}

pub(in crate::semantic) trait SourceBackedSemanticEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::semantic) struct SourceBackedSemanticOutcome {
    /// Exact number of complete Core records decoded from changed-source pages.
    pub(in crate::semantic) records_read: usize,
    /// Compatibility name retained for daemon progress output.
    pub(in crate::semantic) records_scanned: usize,
    pub(in crate::semantic) records_embedded: usize,
    pub(in crate::semantic) records_reused: usize,
    pub(in crate::semantic) records_filtered: usize,
    pub(in crate::semantic) invalidated_chunks: usize,
    pub(in crate::semantic) deleted_chunks: usize,
    pub(in crate::semantic) ready: bool,
    pub(in crate::semantic) work_remaining: bool,
}

pub(in crate::semantic) enum SourceBackedGenerationPin {
    NotReady,
    ReadyEmpty,
    Ready(PinnedFlatGeneration),
}

#[derive(Debug)]
struct StoredSourceDocument {
    stable_event_identity: Vec<u8>,
    source_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SourceProjectionReceipt {
    source_identity_digest: String,
    indexed_documents: u64,
    semantic_eligible_documents: u64,
    core_record_accumulator: String,
    contract_fingerprint: String,
    semantic_policy_fingerprint: String,
    owned_event_count: u64,
    owned_event_ids_hash: String,
}

impl SourceProjectionReceipt {
    fn matches(
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

#[derive(Debug)]
struct ResolvedSourceDocument {
    event_id: StableEntityId,
    stable_identity: Vec<u8>,
    source_text_sha256: String,
}

impl SemanticVectorStore {
    pub(in crate::semantic) fn reconcile_source_backed_index<B, E>(
        &mut self,
        index: &VerifiedIndex,
        builder: &mut B,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        B: SourceBackedSemanticDocumentBuilder,
        E: SourceBackedSemanticEmbedder,
    {
        semantic_owned_sidecar_result((|| {
            let generation = SourceBackedSemanticGeneration::from_verified_index(index)?;
            self.reconcile_source_backed_generation(index, &generation, builder, embedder)
        })())
    }

    fn reconcile_source_backed_generation<B, E>(
        &mut self,
        index: &VerifiedIndex,
        generation: &SourceBackedSemanticGeneration,
        builder: &mut B,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        B: SourceBackedSemanticDocumentBuilder,
        E: SourceBackedSemanticEmbedder,
    {
        validate_generation(generation)?;
        if generation.core_generation_id != index.generation_id() {
            return Err(anyhow!(
                "source-backed semantic target does not match its pinned Core index"
            ));
        }
        if let Some(outcome) = self.reconcile_pending_full_rebuild()? {
            return Ok(outcome);
        }
        if self
            .acknowledged_source_projection(
                &generation.core_generation_id,
                Some(generation.semantic_documents),
                Some(&generation.semantic_policy_fingerprint),
            )?
            .is_some()
        {
            return Ok(SourceBackedSemanticOutcome {
                ready: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }

        let mut frontier = self.begin_or_resume_source_generation(generation)?;
        if let Some(source_identity_digest) = frontier.active_source_identity_digest.clone() {
            if frontier.removing_source {
                return self.reconcile_removed_source(&mut frontier, &source_identity_digest);
            }
            let source = generation.source(&source_identity_digest).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "active semantic source is absent from its pinned Core generation",
                )
            })?;
            if frontier.source_scan_complete {
                return self.finish_active_source(&mut frontier, source);
            }
            return self.reconcile_source_page(index, &mut frontier, source, builder, embedder);
        }

        match frontier.source_traversal_phase {
            SourceTraversalPhase::RemovingStaleSources => {
                return self.reconcile_next_stale_source(&mut frontier, generation);
            }
            SourceTraversalPhase::ReconcilingSources => {
                return self.reconcile_next_target_source(
                    index,
                    &mut frontier,
                    generation,
                    builder,
                    embedder,
                );
            }
            SourceTraversalPhase::Finalizing => {}
        }

        self.finish_source_generation(&frontier, generation)
    }

    fn reconcile_next_stale_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<SourceBackedSemanticOutcome> {
        let mut after = frontier.source_traversal_after_identity_digest.clone();
        loop {
            let Some(source_identity_digest) =
                self.next_source_receipt_identity(after.as_deref())?
            else {
                frontier.source_traversal_phase = SourceTraversalPhase::ReconcilingSources;
                frontier.source_traversal_after_identity_digest = None;
                self.store_source_frontier(frontier)?;
                return Ok(SourceBackedSemanticOutcome {
                    work_remaining: true,
                    ..SourceBackedSemanticOutcome::default()
                });
            };
            if generation.source(&source_identity_digest).is_none() {
                frontier.source_traversal_after_identity_digest = after;
                self.start_source_removal(frontier, &source_identity_digest)?;
                return self.reconcile_removed_source(frontier, &source_identity_digest);
            }
            after = Some(source_identity_digest);
        }
    }

    fn reconcile_next_target_source<B, E>(
        &mut self,
        index: &VerifiedIndex,
        frontier: &mut SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
        builder: &mut B,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        B: SourceBackedSemanticDocumentBuilder,
        E: SourceBackedSemanticEmbedder,
    {
        let contract_fingerprint = source_contract_fingerprint()?;
        for source in
            generation.sources_after(frontier.source_traversal_after_identity_digest.as_deref())
        {
            let source_identity_digest = source.aggregate.source_identity_digest();
            let receipt = self.source_receipt(source_identity_digest)?;
            if receipt.as_ref().is_none_or(|receipt| {
                !receipt.matches(
                    source,
                    &contract_fingerprint,
                    &generation.semantic_policy_fingerprint,
                )
            }) {
                self.start_source_reconciliation(frontier, source, generation)?;
                return self.reconcile_source_page(index, frontier, source, builder, embedder);
            }
            frontier.source_traversal_after_identity_digest =
                Some(source_identity_digest.to_owned());
        }
        frontier.source_traversal_phase = SourceTraversalPhase::Finalizing;
        self.store_source_frontier(frontier)?;
        self.finish_source_generation(frontier, generation)
    }

    fn reconcile_pending_full_rebuild(&mut self) -> Result<Option<SourceBackedSemanticOutcome>> {
        let pending = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM semantic_maintenance_state WHERE key = ?1
             )",
            [FULL_REBUILD_STATE],
            |row| row.get::<_, bool>(0),
        )?;
        if !pending {
            return Ok(None);
        }
        self.flat
            .begin_reconciliation_view(FULL_REBUILD_STATE)
            .map_err(anyhow::Error::new)?;
        let event_ids = self
            .flat
            .reconciliation_event_ids(FULL_REBUILD_STATE, MAX_SOURCE_EVENT_PAGE_ITEMS)
            .map_err(anyhow::Error::new)?;
        if !event_ids.is_empty() {
            let deleted_chunks = self.delete_events(&event_ids)?;
            return Ok(Some(SourceBackedSemanticOutcome {
                deleted_chunks,
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            }));
        }
        self.flat
            .finish_reconciliation_view()
            .map_err(anyhow::Error::new)?;
        self.conn.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [FULL_REBUILD_STATE],
        )?;
        Ok(None)
    }

    fn reconcile_source_page<B, E>(
        &mut self,
        index: &VerifiedIndex,
        frontier: &mut SourceProjectionFrontier,
        source: &SourceBackedSemanticSource,
        builder: &mut B,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        B: SourceBackedSemanticDocumentBuilder,
        E: SourceBackedSemanticEmbedder,
    {
        let after = frontier
            .after_identity
            .as_deref()
            .map(StableEntityId::decode_canonical)
            .transpose()?;
        let cursor = after.map(|after| {
            SourceEventCursor::new(
                frontier.core_generation_id.clone(),
                source.source.clone(),
                after,
            )
        });
        let core_page = index.core_source_event_page(
            &source.source,
            cursor.as_ref(),
            MAX_SOURCE_EVENT_PAGE_ITEMS,
        )?;
        let page = SourceBackedSemanticPage {
            core_generation_id: core_page.generation_id,
            source_identity_digest: source.aggregate.source_identity_digest().to_owned(),
            after,
            records: core_page.items,
            terminal: core_page.terminal,
        };
        validate_page(frontier, &page)?;

        let page_documents = u64::try_from(page.records.len())?;
        let processed_documents = frontier
            .processed_source_documents
            .checked_add(page_documents)
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic source document count overflowed",
                )
            })?;
        if processed_documents > source.aggregate.indexed_documents()
            || (page.terminal && processed_documents != source.aggregate.indexed_documents())
        {
            return Err(SemanticVectorStoreError::reset_required(
                "source-backed semantic source page count disagrees with its Core aggregate",
            )
            .into());
        }

        self.flat
            .begin_reconciliation_view(&frontier.core_generation_id)
            .map_err(anyhow::Error::new)?;
        let existing_events = self.flat_active_event_lookup()?;
        let reconciliation_id = frontier
            .active_source_reconciliation_id
            .clone()
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "active semantic source has no reconciliation identity",
                )
            })?;
        let mut outcome = SourceBackedSemanticOutcome {
            records_read: page.records.len(),
            records_scanned: page.records.len(),
            ..SourceBackedSemanticOutcome::default()
        };
        let mut semantic_records = 0_u64;
        let mut replacements = Vec::new();
        let mut resolved = Vec::new();
        let mut retire = Vec::new();

        for record in &page.records {
            if !SemanticEligibility::CURRENT.includes(&record.event) {
                continue;
            }
            semantic_records = semantic_records.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic candidate count overflowed",
                )
            })?;
            let stable_identity = record.event_id.encode_canonical()?.to_vec();
            if let Some(prior) = self.stored_source_document(record.event_id.as_uuid())? {
                if prior.stable_event_identity != stable_identity
                    || prior.source_identity_digest != source.aggregate.source_identity_digest()
                {
                    return Err(SemanticVectorStoreError::storage_conflict(format!(
                        "source-backed semantic compact identity collision at {}",
                        record.event_id.as_uuid()
                    ))
                    .into());
                }
            }

            let Some(document) = builder.build_document(record)? else {
                retire.push(record.event_id.as_uuid());
                outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                continue;
            };
            validate_resolved_document(record, &document)?;
            let source_text = semantic_source_text(&document.text);
            if semantic_core_content_is_control(&source_text) {
                retire.push(record.event_id.as_uuid());
                outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                continue;
            }
            let source_text_sha256 = semantic_document_hash(
                &document,
                &source_text,
                &frontier.semantic_policy_fingerprint,
            );
            let reusable = existing_events
                .event(document.event_id)
                .is_some_and(|event| event.source_text_hash.to_hex() == source_text_sha256);
            if reusable {
                outcome.records_reused = outcome.records_reused.saturating_add(1);
            } else {
                let chunks =
                    semantic_chunks_for_document(&document, &source_text, &source_text_sha256);
                if chunks.is_empty() {
                    return Err(anyhow!(
                        "Core semantic projection produced an empty document for {}",
                        record.event_id
                    ));
                }
                let embeddings = embedder.embed_chunks(&chunks)?;
                if embeddings.len() != chunks.len()
                    || embeddings
                        .iter()
                        .any(|embedding| embedding.len() != SEMANTIC_DIMENSIONS)
                {
                    return Err(SemanticVectorStoreError::unavailable(
                        "source-backed semantic embedder returned an invalid batch",
                    )
                    .into());
                }
                replacements.extend(chunks.into_iter().zip(embeddings));
                outcome.records_embedded = outcome.records_embedded.saturating_add(1);
            }
            resolved.push(ResolvedSourceDocument {
                event_id: record.event_id,
                stable_identity,
                source_text_sha256,
            });
        }

        let processed_semantic_documents = frontier
            .processed_source_semantic_documents
            .checked_add(semantic_records)
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic source candidate count overflowed",
                )
            })?;
        if processed_semantic_documents > source.aggregate.semantic_eligible_documents()
            || (page.terminal
                && processed_semantic_documents != source.aggregate.semantic_eligible_documents())
        {
            return Err(SemanticVectorStoreError::reset_required(
                "source-backed semantic candidate count disagrees with its Core aggregate",
            )
            .into());
        }

        outcome.invalidated_chunks = self.publish_chunk_replacements(&replacements, &retire)?;
        frontier.processed_source_documents = processed_documents;
        frontier.processed_source_semantic_documents = processed_semantic_documents;
        if let Some(last) = page.records.last() {
            frontier.after_identity = Some(last.event_id.encode_canonical()?.to_vec());
        }
        frontier.source_scan_complete = page.terminal;
        frontier.last_failure = None;

        let transaction = self.conn.transaction()?;
        delete_source_documents(&transaction, &retire)?;
        store_resolved_source_documents(
            &transaction,
            &resolved,
            source.aggregate.source_identity_digest(),
            &reconciliation_id,
        )?;
        store_frontier(&transaction, frontier)?;
        transaction.commit()?;

        outcome.work_remaining = true;
        Ok(outcome)
    }

    fn finish_active_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        source: &SourceBackedSemanticSource,
    ) -> Result<SourceBackedSemanticOutcome> {
        self.flat
            .begin_reconciliation_view(&frontier.core_generation_id)
            .map_err(anyhow::Error::new)?;
        let reconciliation_id = frontier
            .active_source_reconciliation_id
            .as_deref()
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "completed semantic source has no reconciliation identity",
                )
            })?;
        let retired = self.source_document_ids(
            source.aggregate.source_identity_digest(),
            Some(reconciliation_id),
        )?;
        if !retired.is_empty() {
            let deleted_chunks = self.delete_events(&retired)?;
            return Ok(SourceBackedSemanticOutcome {
                deleted_chunks,
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }

        let receipt = self.build_source_receipt(
            source,
            &frontier.contract_fingerprint,
            &frontier.semantic_policy_fingerprint,
            reconciliation_id,
        )?;
        let source_identity_digest = source.aggregate.source_identity_digest().to_owned();
        clear_active_source(frontier);
        frontier.source_traversal_after_identity_digest = Some(source_identity_digest);
        let transaction = self.conn.transaction()?;
        store_source_receipt(&transaction, &receipt)?;
        store_frontier(&transaction, frontier)?;
        transaction.commit()?;
        Ok(SourceBackedSemanticOutcome {
            work_remaining: true,
            ..SourceBackedSemanticOutcome::default()
        })
    }

    fn reconcile_removed_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        source_identity_digest: &str,
    ) -> Result<SourceBackedSemanticOutcome> {
        self.flat
            .begin_reconciliation_view(&frontier.core_generation_id)
            .map_err(anyhow::Error::new)?;
        let removed = self.source_document_ids(source_identity_digest, None)?;
        if !removed.is_empty() {
            let deleted_chunks = self.delete_events(&removed)?;
            return Ok(SourceBackedSemanticOutcome {
                deleted_chunks,
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }
        frontier.source_traversal_after_identity_digest = Some(source_identity_digest.to_owned());
        clear_active_source(frontier);
        let transaction = self.conn.transaction()?;
        transaction.execute(
            "DELETE FROM semantic_source_receipts WHERE source_identity_digest = ?1",
            [source_identity_digest],
        )?;
        store_frontier(&transaction, frontier)?;
        transaction.commit()?;
        Ok(SourceBackedSemanticOutcome {
            work_remaining: true,
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

    fn acknowledged_source_projection(
        &self,
        core_generation_id: &str,
        expected_semantic_documents: Option<u64>,
        expected_semantic_policy_fingerprint: Option<&str>,
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
        let (receipt_count, receipt_hash, projected_documents) = self.source_receipts_summary()?;
        let stored_documents: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM semantic_source_documents",
            [],
            |row| row.get(0),
        )?;
        if acknowledgement.source_receipt_count != receipt_count
            || acknowledgement.source_receipts_hash != receipt_hash
            || acknowledgement.projected_documents != projected_documents
            || acknowledgement.projected_documents != stored_documents
            || acknowledgement.projected_documents > acknowledgement.semantic_documents
        {
            return Ok(None);
        }
        let pinned = self.flat_pin_generation()?;
        if acknowledgement.projected_documents == 0 {
            let flat_matches = match pinned.as_ref() {
                Some(pinned) => {
                    acknowledgement.flat_generation == pinned.generation()
                        && acknowledgement.flat_generation_hash == pinned.generation_hash()
                        && acknowledgement.flat_active_events == pinned.stats().active_events as u64
                        && acknowledgement.flat_active_chunks == pinned.stats().active_chunks as u64
                        && acknowledgement.flat_active_events == 0
                        && acknowledgement.flat_active_chunks == 0
                }
                None => {
                    acknowledgement.flat_generation == 0
                        && acknowledgement.flat_generation_hash.is_empty()
                        && acknowledgement.flat_active_events == 0
                        && acknowledgement.flat_active_chunks == 0
                }
            };
            return Ok(flat_matches.then_some(AcknowledgedSourceProjection {
                flat: pinned,
                projected_documents: acknowledgement.projected_documents,
            }));
        }
        let Some(pinned) = pinned else {
            return Ok(None);
        };
        let stats = pinned.stats();
        let matches = acknowledgement.flat_generation != 0
            && acknowledgement.flat_generation == pinned.generation()
            && acknowledgement.flat_generation_hash == pinned.generation_hash()
            && acknowledgement.flat_active_events == stats.active_events as u64
            && acknowledgement.flat_active_chunks == stats.active_chunks as u64
            && acknowledgement.flat_active_events == acknowledgement.projected_documents;
        Ok(matches.then_some(AcknowledgedSourceProjection {
            flat: Some(pinned),
            projected_documents: acknowledgement.projected_documents,
        }))
    }

    fn begin_or_resume_source_generation(
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
        };
        let transaction = self.conn.unchecked_transaction()?;
        if let Some(interrupted_source) = previous_frontier
            .as_ref()
            .and_then(|frontier| frontier.active_source_identity_digest.as_deref())
        {
            // A page from the interrupted generation may already have replaced
            // vectors and ownership rows. Its old receipt must not let a later
            // generation with the same aggregate skip repairing that source.
            let interrupted = previous_frontier.as_ref().ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic source frontier lost its interrupted source state",
                )
            })?;
            transaction.execute(
                "INSERT INTO semantic_source_receipts
                 (source_identity_digest, indexed_documents, semantic_eligible_documents,
                  core_record_accumulator, contract_fingerprint,
                  semantic_policy_fingerprint, owned_event_count, owned_event_ids_hash)
                 VALUES (?1, ?2, ?3, '', '', ?4, 0, '')
                 ON CONFLICT(source_identity_digest) DO UPDATE SET
                    contract_fingerprint = ''",
                params![
                    interrupted_source,
                    interrupted.active_source_indexed_documents,
                    interrupted.active_source_semantic_documents,
                    interrupted.semantic_policy_fingerprint,
                ],
            )?;
        }
        store_frontier(&transaction, &frontier)?;
        transaction.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [SOURCE_ACKNOWLEDGEMENT_STATE],
        )?;
        transaction.commit()?;
        Ok(frontier)
    }

    fn start_source_reconciliation(
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

    fn start_source_removal(
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

    fn source_frontier(&self) -> Result<Option<SourceProjectionFrontier>> {
        self.maintenance_json(SOURCE_FRONTIER_STATE)
    }

    fn source_acknowledgement(&self) -> Result<Option<SourceProjectionAcknowledgement>> {
        self.maintenance_json(SOURCE_ACKNOWLEDGEMENT_STATE)
    }

    fn maintenance_json<T>(&self, key: &str) -> Result<Option<T>>
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

    fn store_source_frontier(&self, frontier: &SourceProjectionFrontier) -> Result<()> {
        self.conn.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SOURCE_FRONTIER_STATE, serde_json::to_string(frontier)?],
        )?;
        Ok(())
    }

    fn stored_source_document(&self, event_id: Uuid) -> Result<Option<StoredSourceDocument>> {
        self.conn
            .query_row(
                "SELECT stable_event_identity, source_identity_digest
                 FROM semantic_source_documents WHERE event_id = ?1",
                [event_id.to_string()],
                |row| {
                    Ok(StoredSourceDocument {
                        stable_event_identity: row.get(0)?,
                        source_identity_digest: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn source_document_ids(
        &self,
        source_identity_digest: &str,
        except_reconciliation_id: Option<&str>,
    ) -> Result<Vec<Uuid>> {
        let limit = MAX_SOURCE_EVENT_PAGE_ITEMS as u64;
        let values = match except_reconciliation_id {
            Some(reconciliation_id) => {
                let mut statement = self.conn.prepare(
                    "SELECT event_id FROM semantic_source_documents
                     WHERE source_identity_digest = ?1 AND source_reconciliation_id != ?2
                     ORDER BY event_id LIMIT ?3",
                )?;
                let rows = statement.query_map(
                    params![source_identity_digest, reconciliation_id, limit],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            }
            None => {
                let mut statement = self.conn.prepare(
                    "SELECT event_id FROM semantic_source_documents
                     WHERE source_identity_digest = ?1 ORDER BY event_id LIMIT ?2",
                )?;
                let rows = statement.query_map(params![source_identity_digest, limit], |row| {
                    row.get::<_, String>(0)
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        values
            .into_iter()
            .map(|value| {
                Uuid::parse_str(&value).map_err(|_| {
                    anyhow::Error::new(SemanticVectorStoreError::reset_required(format!(
                        "invalid source-backed semantic event id: {value}"
                    )))
                })
            })
            .collect()
    }

    fn source_receipts(&self) -> Result<BTreeMap<String, SourceProjectionReceipt>> {
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

    fn source_receipt(
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

    fn next_source_receipt_identity(&self, after: Option<&str>) -> Result<Option<String>> {
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

    fn build_source_receipt(
        &self,
        source: &SourceBackedSemanticSource,
        contract_fingerprint: &str,
        policy_fingerprint: &str,
        reconciliation_id: &str,
    ) -> Result<SourceProjectionReceipt> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, source_text_sha256, source_reconciliation_id
             FROM semantic_source_documents WHERE source_identity_digest = ?1
             ORDER BY event_id",
        )?;
        let mut rows = statement.query([source.aggregate.source_identity_digest()])?;
        let mut count = 0_u64;
        let mut digest = Sha256::new();
        digest.update(SOURCE_RECEIPT_DOMAIN);
        while let Some(row) = rows.next()? {
            let event_id = row.get::<_, String>(0)?;
            let source_text_sha256 = row.get::<_, String>(1)?;
            let stored_reconciliation_id = row.get::<_, String>(2)?;
            if stored_reconciliation_id != reconciliation_id {
                return Err(SemanticVectorStoreError::reset_required(
                    "semantic source retained stale ownership after source completion",
                )
                .into());
            }
            digest.update(event_id.as_bytes());
            digest.update([0]);
            digest.update(source_text_sha256.as_bytes());
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

    fn source_receipts_summary(&self) -> Result<(u64, String, u64)> {
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

    fn validate_source_documents_against_flat(
        &self,
        pinned: Option<&PinnedFlatGeneration>,
    ) -> Result<()> {
        let active_events = pinned.map_or(&[][..], PinnedFlatGeneration::active_events);
        let mut statement = self.conn.prepare(
            "SELECT event_id, source_text_sha256
             FROM semantic_source_documents ORDER BY event_id",
        )?;
        let mut rows = statement.query([])?;
        let mut index = 0_usize;
        while let Some(row) = rows.next()? {
            let event_id = row.get::<_, String>(0)?;
            let source_text_sha256 = row.get::<_, String>(1)?;
            let event_uuid = Uuid::parse_str(&event_id).map_err(|_| {
                SemanticVectorStoreError::reset_required(format!(
                    "invalid source-backed semantic event id: {event_id}"
                ))
            })?;
            let Some(active_event) = active_events.get(index) else {
                return Err(SemanticVectorStoreError::reset_required(
                    "semantic source metadata exceeds flat active events",
                )
                .into());
            };
            if active_event.event_id != event_uuid
                || active_event.source_text_hash.to_hex() != source_text_sha256
            {
                return Err(SemanticVectorStoreError::reset_required(
                    "semantic source metadata does not match flat event metadata",
                )
                .into());
            }
            index = index.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "semantic source metadata count overflowed",
                )
            })?;
        }
        if index != active_events.len() {
            return Err(SemanticVectorStoreError::reset_required(
                "flat active events exceed semantic source metadata",
            )
            .into());
        }
        Ok(())
    }

    fn finish_source_generation(
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
        let stored_documents: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM semantic_source_documents",
            [],
            |row| row.get(0),
        )?;
        if receipt_documents != stored_documents {
            return Err(SemanticVectorStoreError::reset_required(
                "semantic source receipt totals do not match projected documents",
            )
            .into());
        }
        self.flat
            .finish_reconciliation_view()
            .map_err(anyhow::Error::new)?;
        let pinned = self.flat_pin_generation()?;
        self.validate_source_documents_against_flat(pinned.as_ref())?;
        let projected_documents =
            validate_flat_projection(frontier, stored_documents, pinned.as_ref())?;
        let acknowledgement = SourceProjectionAcknowledgement {
            contract_version: frontier.contract_version,
            contract_fingerprint: frontier.contract_fingerprint.clone(),
            core_generation_id: frontier.core_generation_id.clone(),
            semantic_policy_fingerprint: frontier.semantic_policy_fingerprint.clone(),
            consumer_build_id: frontier.consumer_build_id.clone(),
            semantic_documents: frontier.semantic_documents,
            projected_documents,
            source_receipt_count,
            source_receipts_hash,
            flat_generation: pinned.as_ref().map_or(0, PinnedFlatGeneration::generation),
            flat_generation_hash: pinned
                .as_ref()
                .map_or_else(String::new, |pinned| pinned.generation_hash().to_owned()),
            flat_active_events: pinned
                .as_ref()
                .map_or(0, |pinned| pinned.stats().active_events as u64),
            flat_active_chunks: pinned
                .as_ref()
                .map_or(0, |pinned| pinned.stats().active_chunks as u64),
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

fn store_frontier(
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

fn delete_source_documents(transaction: &Transaction<'_>, event_ids: &[Uuid]) -> Result<()> {
    if event_ids.is_empty() {
        return Ok(());
    }
    let mut statement =
        transaction.prepare("DELETE FROM semantic_source_documents WHERE event_id = ?1")?;
    for event_id in event_ids.iter().copied().collect::<HashSet<_>>() {
        statement.execute([event_id.to_string()])?;
    }
    Ok(())
}

fn store_resolved_source_documents(
    transaction: &Transaction<'_>,
    documents: &[ResolvedSourceDocument],
    source_identity_digest: &str,
    reconciliation_id: &str,
) -> Result<()> {
    if documents.is_empty() {
        return Ok(());
    }
    let mut statement = transaction.prepare(
        "INSERT INTO semantic_source_documents
         (event_id, stable_event_identity, source_text_sha256,
          source_identity_digest, source_reconciliation_id)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(event_id) DO UPDATE SET
            stable_event_identity = excluded.stable_event_identity,
            source_text_sha256 = excluded.source_text_sha256,
            source_identity_digest = excluded.source_identity_digest,
            source_reconciliation_id = excluded.source_reconciliation_id",
    )?;
    for document in documents {
        statement.execute(params![
            document.event_id.as_uuid().to_string(),
            document.stable_identity,
            document.source_text_sha256,
            source_identity_digest,
            reconciliation_id,
        ])?;
    }
    Ok(())
}

fn store_source_receipt(
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

fn clear_active_source(frontier: &mut SourceProjectionFrontier) {
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

fn source_reconciliation_id(
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

/// Control-message exclusion is applied only after complete normalized Core
/// content has crossed the generation pin.
pub(in crate::semantic) fn semantic_core_content_is_control(text: &str) -> bool {
    let user = text
        .strip_prefix("user:\n")
        .unwrap_or(text)
        .split_once("\n\nassistant:\n")
        .map_or(text.strip_prefix("user:\n").unwrap_or(text), |(user, _)| {
            user
        })
        .trim();
    user.starts_with("<environment_context>")
        || user.starts_with("<turn_aborted>")
        || user.starts_with("<subagent_notification>")
        || user.starts_with("Warning: The maximum number of unified exec processes")
}

#[cfg(test)]
mod tests;
