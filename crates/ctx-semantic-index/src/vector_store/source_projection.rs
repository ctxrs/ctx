use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{SourceKey, StableEntityId};
use ctx_history_index::{
    policy::semantic_generation_policy, CoreEventRecord, SemanticGenerationPolicy,
    SourceCoreRecordAggregate, SourceEventCursor, VerifiedIndex, LEXICAL_SCHEMA_VERSION,
    MAX_SOURCE_EVENT_PAGE_ITEMS,
};
use ctx_semantic_model::SemanticModelContract;
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
mod outcome;
mod state;

#[cfg(test)]
use manifest::SOURCE_CONTRACT_VERSION;
use manifest::{
    source_contract_fingerprint, source_contract_fingerprint_with_authority, validate_generation,
    validate_page, validate_resolved_document, SourceProjectionFrontier, SourceTraversalPhase,
    SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
};
use outcome::merge_outcome;
pub use outcome::SourceBackedSemanticOutcome;
use state::{
    clear_active_source, commit_frontier_after_flat, source_projection_states,
    source_receipt_allows_vector_reuse, source_receipt_matches, SourceProjectionStates,
};

const SEARCH_DIRECTORY: &str = "search";
const SEMANTIC_DIRECTORY: &str = "semantic";
pub fn source_backed_semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(SEMANTIC_DIRECTORY)
}

fn external_embedding_chunk_limit(model_contract: &SemanticModelContract) -> Option<usize> {
    model_contract
        .external_space()
        .map(|space| space.max_inputs_per_request())
}

fn source_event_page_limit(model_contract: &SemanticModelContract) -> usize {
    external_embedding_chunk_limit(model_contract).unwrap_or(MAX_SOURCE_EVENT_PAGE_ITEMS)
}

/// Returns persisted projection identity for one vector space, excluding
/// executor, runtime, accelerator, and artifact identity.
pub fn source_backed_semantic_contract_fingerprint(
    model_contract: &SemanticModelContract,
) -> Result<String> {
    source_contract_fingerprint(model_contract)
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
    trusted_legacy_contract_fingerprint: Option<String>,
    model_descriptor: String,
    semantic_policy: SemanticGenerationPolicy,
    sources: Vec<SourceBackedSemanticSource>,
}

impl SourceBackedSemanticGeneration {
    /// Binds semantic catch-up to one verified current-schema Core manifest and
    /// mirrors its exact per-source Core commitments.
    pub(super) fn from_verified_index(
        index: &VerifiedIndex,
        model_contract: &SemanticModelContract,
    ) -> Result<Self> {
        Self::from_verified_index_with_policy(
            index,
            semantic_generation_policy(model_contract),
            model_contract,
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
        // `VerifiedIndex` owns the authenticated publication identity. A compact
        // delta descriptor materializes into a full logical manifest whose
        // reserialized digest intentionally differs from that identity.
        let core_generation_id = index.generation_id().to_owned();
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
            core_generation_id,
            semantic_policy_fingerprint,
            contract_fingerprint,
            trusted_legacy_contract_fingerprint: None,
            model_descriptor,
            semantic_policy,
            sources,
        };
        validate_generation(&generation)?;
        Ok(generation)
    }

    fn includes(&self, record: &CoreEventRecord) -> bool {
        record.core_record.content.is_discovery_eligible()
            && self
                .semantic_policy
                .includes_provider_native_event_copy(record.core_record.event_copy.is_some())
            && self
                .semantic_policy
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
    chunks: Vec<SemanticChunkDocument>,
}

fn embed_chunks_in_bounded_batches(
    embedder: &mut dyn SemanticBatchEmbedder,
    chunks: Vec<SemanticChunkDocument>,
    dimensions: usize,
    batch_limit: Option<usize>,
) -> Result<Vec<(SemanticChunkDocument, Vec<f32>)>> {
    let batch_limit = batch_limit.unwrap_or(chunks.len()).max(1);
    let mut chunks = chunks.into_iter();
    let mut replacements = Vec::new();
    loop {
        let batch = chunks.by_ref().take(batch_limit).collect::<Vec<_>>();
        if batch.is_empty() {
            return Ok(replacements);
        }
        let embeddings = embedder.embed_chunks(&batch)?;
        if embeddings.len() != batch.len()
            || embeddings
                .iter()
                .any(|embedding| embedding.len() != dimensions)
        {
            return Err(SemanticVectorStoreError::unavailable(
                "source-backed semantic embedder returned an invalid batch",
            )
            .into());
        }
        replacements.extend(batch.into_iter().zip(embeddings));
    }
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
            let generation =
                SourceBackedSemanticGeneration::from_verified_index(index, self.contract())?;
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
        self.flat
            .begin_source_generation_view()
            .map_err(anyhow::Error::new)?;
        let result = (|| {
            if let Some(outcome) = self.reconcile_pending_full_rebuild()? {
                return Ok(outcome);
            }
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
            let vector_reuse_allowed = receipt
                .is_some_and(|receipt| source_receipt_allows_vector_reuse(receipt, generation));
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
            let deleted_chunks = self.delete_events_coordinated(&event_ids)?;
            return Ok(Some(SourceBackedSemanticOutcome {
                deleted_chunks,
                work_remaining: true,
                ..SourceBackedSemanticOutcome::default()
            }));
        }
        self.flat
            .finish_reconciliation_view_coordinated()
            .map_err(anyhow::Error::new)?;
        self.conn.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [FULL_REBUILD_STATE],
        )?;
        // End this coordinated generation view before beginning a new source
        // projection. Final compaction can replace the active Flat manifest,
        // so source staging must pin that post-compaction generation afresh.
        Ok(Some(SourceBackedSemanticOutcome {
            work_remaining: true,
            ..SourceBackedSemanticOutcome::default()
        }))
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
        let model_contract = self.contract().clone();
        let embedding_chunk_limit = external_embedding_chunk_limit(&model_contract);
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
            source_event_page_limit(&model_contract),
        )?;
        let record_bytes_decoded = u64::try_from(core_page.encoded_core_bytes)?;
        let mut page = SourceBackedSemanticPage {
            core_generation_id: core_page.generation_id,
            source_identity_digest: source.aggregate.source_identity_digest().to_owned(),
            after,
            records: core_page.items,
            terminal: core_page.terminal,
        };
        validate_page(frontier, &page)?;
        let records_decoded = page.records.len();

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
            frontier.processed_source_filtered_documents = 0;
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
        let mut outcome = SourceBackedSemanticOutcome {
            records_decoded,
            record_bytes_decoded,
            ..SourceBackedSemanticOutcome::default()
        };
        let mut semantic_records = 0_u64;
        let mut filtered_records = 0_u64;
        let mut replacements = Vec::new();
        let mut resolved = Vec::new();
        let mut retire = Vec::new();
        let mut projected_chunks = 0_usize;
        let mut processed_page_records = 0_usize;

        for record in &page.records {
            if !generation.includes(record) {
                processed_page_records = processed_page_records.saturating_add(1);
                continue;
            }
            // Stop on a record boundary once this external work unit has filled
            // its embedding budget. A first record is always admitted below so
            // one valid document that expands past the limit still progresses.
            if embedding_chunk_limit.is_some_and(|limit| projected_chunks >= limit) {
                break;
            }
            let next_semantic_records = semantic_records.checked_add(1).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic candidate count overflowed",
                )
            })?;
            let Some(document) = builder.build_document(record)? else {
                semantic_records = next_semantic_records;
                retire.push(record.event_id.as_uuid());
                filtered_records = filtered_records.checked_add(1).ok_or_else(|| {
                    SemanticVectorStoreError::reset_required(
                        "source-backed semantic filtered count overflowed",
                    )
                })?;
                outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                processed_page_records = processed_page_records.saturating_add(1);
                continue;
            };
            validate_resolved_document(record, &document)?;
            let source_text = semantic_source_text(&document.text);
            if semantic_core_content_is_control(&source_text) {
                semantic_records = next_semantic_records;
                retire.push(record.event_id.as_uuid());
                filtered_records = filtered_records.checked_add(1).ok_or_else(|| {
                    SemanticVectorStoreError::reset_required(
                        "source-backed semantic filtered count overflowed",
                    )
                })?;
                outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                processed_page_records = processed_page_records.saturating_add(1);
                continue;
            }
            let source_text_sha256 = semantic_document_hash(
                &model_contract,
                &document,
                &source_text,
                &frontier.semantic_policy_fingerprint,
            );
            let chunks = semantic_chunks_for_document(&document, &source_text, &source_text_sha256);
            if chunks.is_empty() {
                return Err(anyhow!(
                    "Core semantic projection produced an empty document for {}",
                    record.event_id
                ));
            }
            let generated_chunks = projected_chunks.checked_add(chunks.len()).ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic embedding chunk count overflowed",
                )
            })?;
            if projected_chunks != 0
                && embedding_chunk_limit.is_some_and(|limit| generated_chunks > limit)
            {
                break;
            }
            projected_chunks = generated_chunks;
            semantic_records = next_semantic_records;
            resolved.push(ResolvedSourceDocument {
                event_id: record.event_id,
                stable_identity: record.event_id.encode_canonical()?.to_vec(),
                source_text_sha256,
                seq: document.seq,
                chunks,
            });
            processed_page_records = processed_page_records.saturating_add(1);
        }

        let page_was_truncated = processed_page_records < page.records.len();
        page.records.truncate(processed_page_records);
        if page_was_truncated {
            page.terminal = false;
        }
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
        for record in page
            .records
            .iter()
            .filter(|record| generation.includes(record))
        {
            let stable_identity = record.event_id.encode_canonical()?;
            let stable_identity_hash = Sha256::digest(stable_identity);
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
        }
        let existing_lookup = FlatActiveEventLookup::from_events(
            page.records
                .iter()
                .filter_map(|record| existing_events.get(&record.event_id.as_uuid()).cloned())
                .collect(),
        );
        let mut pending_chunks = Vec::new();
        let mut pending_documents = 0_usize;
        for document in &mut resolved {
            let reusable = frontier.vector_reuse_allowed
                && existing_events
                    .get(&document.event_id.as_uuid())
                    .is_some_and(|event| {
                        event.source_text_hash.to_hex() == document.source_text_sha256
                    });
            if reusable {
                outcome.records_reused = outcome.records_reused.saturating_add(1);
            } else {
                pending_chunks.append(&mut document.chunks);
                pending_documents = pending_documents.saturating_add(1);
            }
        }

        let processed_semantic_documents = frontier
            .processed_source_semantic_documents
            .checked_add(semantic_records)
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic source candidate count overflowed",
                )
            })?;
        let processed_filtered_documents = frontier
            .processed_source_filtered_documents
            .checked_add(filtered_records)
            .ok_or_else(|| {
                SemanticVectorStoreError::reset_required(
                    "source-backed semantic filtered count overflowed",
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
        if !pending_chunks.is_empty() {
            replacements = embed_chunks_in_bounded_batches(
                embedder,
                pending_chunks,
                model_contract.dimensions(),
                embedding_chunk_limit,
            )?;
            outcome.records_embedded = outcome.records_embedded.saturating_add(pending_documents);
        }
        let publication =
            self.publish_source_page(&replacements, &metadata_updates, &retire, &existing_lookup)?;
        frontier.processed_source_documents = processed_documents;
        frontier.processed_source_semantic_documents = processed_semantic_documents;
        frontier.processed_source_filtered_documents = processed_filtered_documents;
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
            frontier.processed_source_filtered_documents = 0;
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
                filtered_event_count: frontier.processed_source_filtered_documents,
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
