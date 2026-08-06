use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{SourceKey, StableEntityId};
use ctx_history_index::{
    current_semantic_generation_policy, CoreEventRecord, SemanticGenerationPolicy,
    SourceCoreRecordAggregate, SourceEventCursor, VerifiedIndex, LEXICAL_SCHEMA_VERSION,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use ctx_semantic_model::{semantic_model_contract_descriptor, SEMANTIC_DIMENSIONS};
use sha2::{Digest, Sha256};
#[cfg(test)]
use uuid::Uuid;

use super::control::FULL_REBUILD_STATE;
use super::flat_segments::{
    FlatActiveEventLookup, FlatEventMetadataUpdate, FlatSourceHash, FlatSourceReceiptInput,
    FlatSourceStageResume, PinnedFlatGeneration,
};
use super::{SemanticChunkDocument, SemanticVectorStore};
use crate::{
    indexing::{semantic_chunks_for_document, semantic_document_hash, semantic_source_text},
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
    SemanticEventDocument,
};

mod manifest;
mod state;

#[cfg(test)]
use manifest::SOURCE_CONTRACT_VERSION;
use manifest::{
    source_contract_fingerprint_with_authority, validate_generation, validate_page,
    validate_resolved_document, SourceProjectionFrontier, SourceTraversalPhase,
    SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
};
use state::{
    clear_active_source, commit_frontier_after_flat, source_projection_states,
    source_receipt_matches, SourceProjectionStates,
};

const SEARCH_DIRECTORY: &str = "search";
const SEMANTIC_DIRECTORY: &str = "semantic";

pub fn source_backed_semantic_vector_path(data_root: &Path) -> PathBuf {
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
    contract_fingerprint: String,
    model_descriptor: String,
    semantic_policy: SemanticGenerationPolicy,
    sources: Vec<SourceBackedSemanticSource>,
}

impl SourceBackedSemanticGeneration {
    /// Binds semantic catch-up to one verified schema-v15 Core manifest and
    /// mirrors its exact per-source Core commitments.
    pub(super) fn from_verified_index(index: &VerifiedIndex) -> Result<Self> {
        Self::from_verified_index_with_policy(index, current_semantic_generation_policy())
    }

    fn from_verified_index_with_policy(
        index: &VerifiedIndex,
        semantic_policy: SemanticGenerationPolicy,
    ) -> Result<Self> {
        Self::from_verified_index_with_authority(
            index,
            semantic_policy,
            semantic_model_contract_descriptor(),
        )
    }

    fn from_verified_index_with_authority(
        index: &VerifiedIndex,
        semantic_policy: SemanticGenerationPolicy,
        model_descriptor: String,
    ) -> Result<Self> {
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
        let semantic_policy_fingerprint = semantic_policy.canonical_sha256()?;
        let contract_fingerprint = source_contract_fingerprint_with_authority(
            &semantic_policy_fingerprint,
            &model_descriptor,
        )?;
        let generation = Self {
            core_generation_id: manifest_generation_id,
            semantic_policy_fingerprint,
            contract_fingerprint,
            model_descriptor,
            semantic_policy,
            sources,
        };
        validate_generation(&generation)?;
        Ok(generation)
    }

    fn includes(&self, record: &CoreEventRecord) -> bool {
        self.semantic_policy
            .includes_event(&record.event.event_type, record.event.role.as_deref())
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

pub trait SemanticDocumentBuilder {
    /// Builds one semantic document exclusively from complete records in the
    /// same pinned Core generation. `None` is an intentional policy filter.
    fn build_document(&mut self, record: &CoreEventRecord)
        -> Result<Option<SemanticEventDocument>>;
}

pub trait SemanticBatchEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceBackedSemanticOutcome {
    /// Exact number of complete Core records decoded from changed-source pages.
    pub(crate) records_decoded: usize,
    /// Exact stored Core JSON bytes decoded from changed-source pages.
    pub(crate) record_bytes_decoded: u64,
    pub(crate) records_embedded: usize,
    pub(crate) records_reused: usize,
    pub(crate) records_filtered: usize,
    pub(crate) invalidated_chunks: usize,
    pub(crate) deleted_chunks: usize,
    pub(crate) vectors_touched: u64,
    pub(crate) vector_bytes_touched: u64,
    pub(crate) metadata_records_touched: u64,
    pub(crate) ready: bool,
    pub(crate) work_remaining: bool,
}

impl SourceBackedSemanticOutcome {
    pub fn records_decoded(&self) -> usize {
        self.records_decoded
    }

    pub fn record_bytes_decoded(&self) -> u64 {
        self.record_bytes_decoded
    }

    pub fn records_embedded(&self) -> usize {
        self.records_embedded
    }

    pub fn records_reused(&self) -> usize {
        self.records_reused
    }

    pub fn records_filtered(&self) -> usize {
        self.records_filtered
    }

    pub fn invalidated_chunks(&self) -> usize {
        self.invalidated_chunks
    }

    pub fn deleted_chunks(&self) -> usize {
        self.deleted_chunks
    }

    pub fn vectors_touched(&self) -> u64 {
        self.vectors_touched
    }

    pub fn vector_bytes_touched(&self) -> u64 {
        self.vector_bytes_touched
    }

    pub fn metadata_records_touched(&self) -> u64 {
        self.metadata_records_touched
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn work_remaining(&self) -> bool {
        self.work_remaining
    }
}

fn merge_outcome(total: &mut SourceBackedSemanticOutcome, next: SourceBackedSemanticOutcome) {
    total.records_decoded = total.records_decoded.saturating_add(next.records_decoded);
    total.record_bytes_decoded = total
        .record_bytes_decoded
        .saturating_add(next.record_bytes_decoded);
    total.records_embedded = total.records_embedded.saturating_add(next.records_embedded);
    total.records_reused = total.records_reused.saturating_add(next.records_reused);
    total.records_filtered = total.records_filtered.saturating_add(next.records_filtered);
    total.invalidated_chunks = total
        .invalidated_chunks
        .saturating_add(next.invalidated_chunks);
    total.deleted_chunks = total.deleted_chunks.saturating_add(next.deleted_chunks);
    total.vectors_touched = total.vectors_touched.saturating_add(next.vectors_touched);
    total.vector_bytes_touched = total
        .vector_bytes_touched
        .saturating_add(next.vector_bytes_touched);
    total.metadata_records_touched = total
        .metadata_records_touched
        .saturating_add(next.metadata_records_touched);
    total.ready |= next.ready;
    total.work_remaining |= next.work_remaining;
}

pub enum SourceBackedGenerationPin {
    NotReady,
    ReadyEmpty,
    Ready(PinnedFlatGeneration),
}

#[derive(Debug)]
struct ResolvedSourceDocument {
    event_id: StableEntityId,
    stable_identity: Vec<u8>,
    source_text_sha256: String,
    seq: u64,
}

impl SemanticVectorStore {
    pub fn reconcile_source_backed_index(
        &mut self,
        index: &VerifiedIndex,
        builder: &mut dyn SemanticDocumentBuilder,
        embedder: &mut dyn SemanticBatchEmbedder,
    ) -> Result<SourceBackedSemanticOutcome> {
        semantic_owned_sidecar_result((|| {
            let work_before = self.flat.work_stats();
            let generation = SourceBackedSemanticGeneration::from_verified_index(index)?;
            let mut outcome =
                self.reconcile_source_backed_generation(index, &generation, builder, embedder)?;
            let work = self.flat.work_since(work_before);
            outcome.vectors_touched = work.vectors_touched;
            outcome.vector_bytes_touched = work.vector_bytes_touched;
            outcome.metadata_records_touched = work.metadata_records_touched;
            Ok(outcome)
        })())
    }

    fn reconcile_source_backed_generation(
        &mut self,
        index: &VerifiedIndex,
        generation: &SourceBackedSemanticGeneration,
        builder: &mut dyn SemanticDocumentBuilder,
        embedder: &mut dyn SemanticBatchEmbedder,
    ) -> Result<SourceBackedSemanticOutcome> {
        validate_generation(generation)?;
        if generation.core_generation_id != index.generation_id() {
            return Err(anyhow!(
                "source-backed semantic target does not match its pinned Core index"
            ));
        }
        if let Some(outcome) = self.reconcile_pending_full_rebuild()? {
            return Ok(outcome);
        }
        self.flat
            .begin_source_generation_view()
            .map_err(anyhow::Error::new)?;
        let result = (|| {
            self.recover_lost_flat_publication()?;
            if self
                .acknowledged_source_projection(
                    &generation.core_generation_id,
                    None,
                    Some(&generation.contract_fingerprint),
                    Some(&generation.semantic_policy_fingerprint),
                    false,
                )?
                .is_some()
            {
                return Ok(SourceBackedSemanticOutcome {
                    ready: true,
                    ..SourceBackedSemanticOutcome::default()
                });
            }

            let mut frontier = self.begin_or_resume_source_generation(generation)?;
            let mut states =
                source_projection_states(self.flat.source_states().map_err(anyhow::Error::new)?);
            let mut total = SourceBackedSemanticOutcome::default();
            loop {
                if let Some(source_identity_digest) = frontier.active_source_identity_digest.clone()
                {
                    let next = if frontier.removing_source {
                        self.reconcile_removed_source(
                            &mut frontier,
                            &source_identity_digest,
                            &mut states,
                        )?
                    } else {
                        let source =
                            generation.source(&source_identity_digest).ok_or_else(|| {
                                SemanticVectorStoreError::reset_required(
                            "active semantic source is absent from its pinned Core generation",
                        )
                            })?;
                        if frontier.source_scan_complete {
                            self.finish_active_source(&mut frontier, source, &mut states)?
                        } else {
                            self.reconcile_source_page(
                                index,
                                &mut frontier,
                                source,
                                generation,
                                builder,
                                embedder,
                            )?
                        }
                    };
                    merge_outcome(&mut total, next);
                    continue;
                }

                let next = match frontier.source_traversal_phase {
                    SourceTraversalPhase::RemovingStaleSources => {
                        self.reconcile_next_stale_source(&mut frontier, generation, &mut states)?
                    }
                    SourceTraversalPhase::ReconcilingSources => self.reconcile_next_target_source(
                        index,
                        &mut frontier,
                        generation,
                        builder,
                        embedder,
                        &mut states,
                    )?,
                    SourceTraversalPhase::Finalizing => {
                        let finished =
                            self.finish_source_generation(&frontier, generation, &states)?;
                        merge_outcome(&mut total, finished);
                        total.work_remaining = false;
                        return Ok(total);
                    }
                };
                merge_outcome(&mut total, next);
            }
        })();
        let end = self
            .flat
            .end_source_generation_view()
            .map_err(anyhow::Error::new);
        match (result, end) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(outcome), Ok(())) => Ok(outcome),
        }
    }

    fn reconcile_next_stale_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
        states: &mut SourceProjectionStates,
    ) -> Result<SourceBackedSemanticOutcome> {
        let mut after = frontier.source_traversal_after_identity_digest.clone();
        loop {
            let source_identity_digest = states
                .keys()
                .find(|identity| {
                    after
                        .as_deref()
                        .is_none_or(|after| identity.as_str() > after)
                })
                .cloned();
            let Some(source_identity_digest) = source_identity_digest else {
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
                return self.reconcile_removed_source(frontier, &source_identity_digest, states);
            }
            after = Some(source_identity_digest);
        }
    }

    fn reconcile_next_target_source(
        &mut self,
        index: &VerifiedIndex,
        frontier: &mut SourceProjectionFrontier,
        generation: &SourceBackedSemanticGeneration,
        builder: &mut dyn SemanticDocumentBuilder,
        embedder: &mut dyn SemanticBatchEmbedder,
        states: &mut SourceProjectionStates,
    ) -> Result<SourceBackedSemanticOutcome> {
        for source in
            generation.sources_after(frontier.source_traversal_after_identity_digest.as_deref())
        {
            let source_identity_digest = source.aggregate.source_identity_digest();
            let receipt = states.get(source_identity_digest).and_then(Option::as_ref);
            let vector_reuse_allowed = receipt.is_some_and(|receipt| {
                receipt.contract_fingerprint == generation.contract_fingerprint
                    && receipt.semantic_policy_fingerprint == generation.semantic_policy_fingerprint
            });
            if receipt.is_none_or(|receipt| {
                !source_receipt_matches(
                    receipt,
                    source,
                    &generation.contract_fingerprint,
                    &generation.semantic_policy_fingerprint,
                )
            }) {
                self.start_source_reconciliation(
                    frontier,
                    source,
                    generation,
                    vector_reuse_allowed,
                )?;
                return self
                    .reconcile_source_page(index, frontier, source, generation, builder, embedder);
            }
            frontier.source_traversal_after_identity_digest =
                Some(source_identity_digest.to_owned());
        }
        frontier.source_traversal_phase = SourceTraversalPhase::Finalizing;
        self.store_source_frontier(frontier)?;
        Ok(SourceBackedSemanticOutcome {
            work_remaining: true,
            ..SourceBackedSemanticOutcome::default()
        })
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

    fn reconcile_source_page(
        &mut self,
        index: &VerifiedIndex,
        frontier: &mut SourceProjectionFrontier,
        source: &SourceBackedSemanticSource,
        generation: &SourceBackedSemanticGeneration,
        builder: &mut dyn SemanticDocumentBuilder,
        embedder: &mut dyn SemanticBatchEmbedder,
    ) -> Result<SourceBackedSemanticOutcome> {
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
        let record_bytes_decoded = u64::try_from(core_page.encoded_core_bytes)?;
        let page = SourceBackedSemanticPage {
            core_generation_id: core_page.generation_id,
            source_identity_digest: source.aggregate.source_identity_digest().to_owned(),
            after,
            records: core_page.items,
            terminal: core_page.terminal,
        };
        validate_page(frontier, &page)?;
        let records_decoded = page.records.len();
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

        let reconciliation_id = frontier
            .active_source_reconciliation_id
            .clone()
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "active semantic source has no reconciliation identity",
                )
            })?;
        let resume = self
            .flat
            .begin_source_reconciliation_view(
                source.aggregate.source_identity_digest(),
                &reconciliation_id,
                &frontier.flat_publication,
                frontier.flat_staging.as_ref(),
            )
            .map_err(anyhow::Error::new)?;
        if resume == FlatSourceStageResume::Restarted {
            frontier.processed_source_documents = 0;
            frontier.processed_source_semantic_documents = 0;
            frontier.after_identity = None;
            frontier.source_scan_complete = false;
            frontier.flat_staging = None;
            self.store_source_frontier(frontier)?;
            return Ok(SourceBackedSemanticOutcome {
                records_decoded,
                record_bytes_decoded,
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }
        let eligible_event_ids = page
            .records
            .iter()
            .filter(|record| generation.includes(record))
            .map(|record| record.event_id.as_uuid())
            .collect::<Vec<_>>();
        let existing_events = self
            .flat
            .source_reconciliation_events(&eligible_event_ids)
            .map_err(anyhow::Error::new)?
            .into_iter()
            .zip(eligible_event_ids)
            .filter_map(|(event, event_id)| event.map(|event| (event_id, event)))
            .collect::<HashMap<_, _>>();
        let existing_lookup =
            FlatActiveEventLookup::from_events(existing_events.values().cloned().collect());
        let mut outcome = SourceBackedSemanticOutcome {
            records_decoded,
            record_bytes_decoded,
            ..SourceBackedSemanticOutcome::default()
        };
        let mut semantic_records = 0_u64;
        let mut replacements = Vec::new();
        let mut resolved = Vec::new();
        let mut retire = Vec::new();

        for record in &page.records {
            if !generation.includes(record) {
                continue;
            }
            semantic_records = semantic_records.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic candidate count overflowed",
                )
            })?;
            let stable_identity = record.event_id.encode_canonical()?.to_vec();
            let stable_identity_hash = Sha256::digest(&stable_identity);
            if let Some(prior) = existing_events.get(&record.event_id.as_uuid()) {
                if (prior.stable_identity_hash != [0; 32]
                    && prior.stable_identity_hash != stable_identity_hash.as_slice())
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
            let reusable = frontier.vector_reuse_allowed
                && existing_events
                    .get(&document.event_id)
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
                seq: document.seq,
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
        let metadata_updates = resolved
            .iter()
            .map(|document| {
                let stable_identity_hash = Sha256::digest(&document.stable_identity);
                let mut stable_identity = [0_u8; 32];
                stable_identity.copy_from_slice(&stable_identity_hash);
                Ok(FlatEventMetadataUpdate {
                    event_id: document.event_id.as_uuid(),
                    seq: document.seq,
                    source_text_hash: FlatSourceHash::parse_hex(&document.source_text_sha256)
                        .map_err(anyhow::Error::new)?,
                    stable_identity_hash: stable_identity,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        outcome.invalidated_chunks = retire.iter().try_fold(0_usize, |total, event_id| {
            total
                .checked_add(
                    existing_events
                        .get(event_id)
                        .map_or(0, |event| event.chunk_count as usize),
                )
                .ok_or_else(|| {
                    anyhow::Error::new(SemanticVectorStoreError::reset_required(
                        "semantic invalidated chunk count overflowed",
                    ))
                })
        })?;
        let publication =
            self.publish_source_page(&replacements, &metadata_updates, &retire, &existing_lookup)?;
        frontier.processed_source_documents = processed_documents;
        frontier.processed_source_semantic_documents = processed_semantic_documents;
        if let Some(last) = page.records.last() {
            frontier.after_identity = Some(last.event_id.encode_canonical()?.to_vec());
        }
        frontier.source_scan_complete = page.terminal;
        frontier.last_failure = None;
        frontier.flat_staging = Some(publication.staging.clone());

        let transaction = self.conn.transaction()?;
        commit_frontier_after_flat(&transaction, frontier, None)?;
        transaction.commit()?;
        #[cfg(test)]
        if self.flat.take_source_frontier_commit_failure() {
            return Err(anyhow!(
                "injected failure after semantic source frontier commit"
            ));
        }

        outcome.work_remaining = true;
        Ok(outcome)
    }

    fn finish_active_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        source: &SourceBackedSemanticSource,
        states: &mut SourceProjectionStates,
    ) -> Result<SourceBackedSemanticOutcome> {
        let reconciliation_id = frontier
            .active_source_reconciliation_id
            .as_deref()
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "completed semantic source has no reconciliation identity",
                )
            })?;
        let resume = self
            .flat
            .begin_source_reconciliation_view(
                source.aggregate.source_identity_digest(),
                reconciliation_id,
                &frontier.flat_publication,
                frontier.flat_staging.as_ref(),
            )
            .map_err(anyhow::Error::new)?;
        if resume == FlatSourceStageResume::Restarted {
            frontier.processed_source_documents = 0;
            frontier.processed_source_semantic_documents = 0;
            frontier.after_identity = None;
            frontier.source_scan_complete = false;
            frontier.flat_staging = None;
            self.store_source_frontier(frontier)?;
            return Ok(SourceBackedSemanticOutcome {
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }

        let finalization = self
            .flat
            .finish_source_reconciliation_view(Some(FlatSourceReceiptInput {
                source_identity_digest: source.aggregate.source_identity_digest().to_owned(),
                source_reconciliation_id: reconciliation_id.to_owned(),
                indexed_documents: source.aggregate.indexed_documents(),
                semantic_eligible_documents: frontier.processed_source_semantic_documents,
                core_record_accumulator: source.aggregate.core_record_accumulator().to_owned(),
                contract_fingerprint: frontier.contract_fingerprint.clone(),
                semantic_policy_fingerprint: frontier.semantic_policy_fingerprint.clone(),
            }))
            .map_err(anyhow::Error::new)?;
        #[cfg(test)]
        if self.flat.take_source_finalization_failure() {
            return Err(anyhow!(
                "injected failure after semantic source finalization"
            ));
        }
        let receipt = finalization.receipt.ok_or_else(|| {
            SemanticVectorStoreError::reset_required(
                "completed semantic source has no Flat-owned receipt",
            )
        })?;
        let source_identity_digest = source.aggregate.source_identity_digest().to_owned();
        states.insert(source_identity_digest.clone(), Some(receipt));
        clear_active_source(frontier);
        frontier.source_traversal_after_identity_digest = Some(source_identity_digest);
        let transaction = self.conn.transaction()?;
        commit_frontier_after_flat(&transaction, frontier, Some(&finalization.publication))?;
        transaction.commit()?;
        #[cfg(test)]
        if self.flat.take_source_publication_commit_failure() {
            return Err(anyhow!(
                "injected failure after published semantic source frontier commit before staging acknowledgement"
            ));
        }
        self.flat
            .acknowledge_source_staging(&finalization.publication.token())
            .map_err(anyhow::Error::new)?;
        Ok(SourceBackedSemanticOutcome {
            deleted_chunks: usize::try_from(finalization.deleted_chunks).unwrap_or(usize::MAX),
            work_remaining: true,
            ..SourceBackedSemanticOutcome::default()
        })
    }

    fn reconcile_removed_source(
        &mut self,
        frontier: &mut SourceProjectionFrontier,
        source_identity_digest: &str,
        states: &mut SourceProjectionStates,
    ) -> Result<SourceBackedSemanticOutcome> {
        let removal_reconciliation_id = format!(
            "remove-{}-{source_identity_digest}",
            frontier.core_generation_id
        );
        let resume = self
            .flat
            .begin_source_reconciliation_view(
                source_identity_digest,
                &removal_reconciliation_id,
                &frontier.flat_publication,
                frontier.flat_staging.as_ref(),
            )
            .map_err(anyhow::Error::new)?;
        if resume == FlatSourceStageResume::Restarted {
            frontier.flat_staging = None;
            self.store_source_frontier(frontier)?;
            return Ok(SourceBackedSemanticOutcome {
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            });
        }
        let finalization = self
            .flat
            .finish_source_reconciliation_view(None)
            .map_err(anyhow::Error::new)?;
        #[cfg(test)]
        if self.flat.take_source_finalization_failure() {
            return Err(anyhow!(
                "injected failure after semantic source finalization"
            ));
        }
        states.remove(source_identity_digest);
        frontier.source_traversal_after_identity_digest = Some(source_identity_digest.to_owned());
        clear_active_source(frontier);
        let transaction = self.conn.transaction()?;
        commit_frontier_after_flat(&transaction, frontier, Some(&finalization.publication))?;
        transaction.commit()?;
        #[cfg(test)]
        if self.flat.take_source_publication_commit_failure() {
            return Err(anyhow!(
                "injected failure after published semantic source frontier commit before staging acknowledgement"
            ));
        }
        self.flat
            .acknowledge_source_staging(&finalization.publication.token())
            .map_err(anyhow::Error::new)?;
        Ok(SourceBackedSemanticOutcome {
            deleted_chunks: usize::try_from(finalization.deleted_chunks).unwrap_or(usize::MAX),
            work_remaining: true,
            ..SourceBackedSemanticOutcome::default()
        })
    }
}
/// Control-message exclusion is applied only after complete normalized Core
/// content has crossed the generation pin.
pub fn semantic_core_content_is_control(text: &str) -> bool {
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
