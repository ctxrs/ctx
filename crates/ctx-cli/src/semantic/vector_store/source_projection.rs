use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::StableEntityId;
use ctx_history_index::{
    CoreEventRecord, SemanticEventCursor, VerifiedIndex, LEXICAL_SCHEMA_VERSION,
    MAX_SEMANTIC_EVENT_PAGE_ITEMS,
};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    SourceProjectionFrontier, SOURCE_ACKNOWLEDGEMENT_STATE, SOURCE_CONTRACT_VERSION,
    SOURCE_FRONTIER_STATE, SOURCE_INPUT_LEXICAL_SCHEMA_VERSION,
};

const SEARCH_DIRECTORY: &str = "search";
const SEMANTIC_DIRECTORY: &str = "semantic";

pub(in crate::semantic) fn source_backed_semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(SEMANTIC_DIRECTORY)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceBackedSemanticGeneration {
    pub(super) core_generation_id: String,
    pub(super) semantic_policy_fingerprint: String,
    /// Count of semantic-eligible records in this pinned Core generation.
    pub(super) semantic_documents: u64,
}

impl SourceBackedSemanticGeneration {
    /// Binds semantic catch-up to one verified schema-v4 Core manifest.
    ///
    /// `semantic_documents` is supplied by the stable semantic-record feed
    /// because a Core manifest counts all indexed event records, including
    /// records that are not semantic document anchors.
    pub(super) fn from_verified_index(
        index: &VerifiedIndex,
        semantic_documents: u64,
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
        if semantic_documents > manifest.indexed_documents {
            return Err(anyhow!(
                "source-backed semantic document count exceeds its Core manifest"
            ));
        }
        let manifest_generation_id = manifest.generation_id()?;
        if manifest_generation_id != index.generation_id() {
            return Err(anyhow!(
                "source-backed semantic manifest identity does not match its verified index"
            ));
        }
        let generation = Self {
            core_generation_id: manifest_generation_id,
            semantic_policy_fingerprint: semantic_policy_fingerprint()?,
            semantic_documents,
        };
        validate_generation(&generation)?;
        Ok(generation)
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBackedSemanticPage {
    pub(super) core_generation_id: String,
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
    pub(in crate::semantic) records_scanned: usize,
    pub(in crate::semantic) records_embedded: usize,
    pub(in crate::semantic) records_reused: usize,
    pub(in crate::semantic) records_filtered: usize,
    pub(in crate::semantic) invalidated_chunks: usize,
    pub(in crate::semantic) deleted_chunks: usize,
    pub(in crate::semantic) ready: bool,
    pub(in crate::semantic) work_remaining: bool,
}

#[derive(Debug)]
struct StoredSourceDocument {
    stable_event_identity: Vec<u8>,
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
            let semantic_documents = index.semantic_eligible_event_count()?;
            let generation =
                SourceBackedSemanticGeneration::from_verified_index(index, semantic_documents)?;
            if self
                .acknowledged_source_projection(
                    &generation.core_generation_id,
                    Some(generation.semantic_documents),
                )?
                .is_some()
            {
                return Ok(SourceBackedSemanticOutcome {
                    ready: true,
                    ..SourceBackedSemanticOutcome::default()
                });
            }
            let frontier = self.begin_or_resume_source_generation(&generation)?;
            let after = frontier
                .after_identity
                .as_deref()
                .map(StableEntityId::decode_canonical)
                .transpose()?;
            let cursor = after.map(|after| {
                SemanticEventCursor::new(generation.core_generation_id.clone(), after)
            });
            let page =
                index.core_semantic_event_page(cursor.as_ref(), MAX_SEMANTIC_EVENT_PAGE_ITEMS)?;
            if page.generation_id != generation.core_generation_id
                || page.eligible_total != generation.semantic_documents
            {
                return Err(SemanticVectorStoreError::reset_required(
                    "pinned semantic event page does not match its verified generation",
                )
                .into());
            }
            self.reconcile_source_backed_page(
                &generation,
                SourceBackedSemanticPage {
                    core_generation_id: page.generation_id,
                    after,
                    records: page.items,
                    terminal: page.terminal,
                },
                builder,
                embedder,
            )
        })())
    }

    pub(super) fn reconcile_source_backed_page<B, E>(
        &mut self,
        generation: &SourceBackedSemanticGeneration,
        page: SourceBackedSemanticPage,
        builder: &mut B,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        B: SourceBackedSemanticDocumentBuilder,
        E: SourceBackedSemanticEmbedder,
    {
        semantic_owned_sidecar_result((|| {
            validate_generation(generation)?;
            let mut frontier = self.begin_or_resume_source_generation(generation)?;
            validate_page(&frontier, &page)?;

            let mut outcome = SourceBackedSemanticOutcome::default();
            for record in page.records {
                if frontier.processed_documents >= frontier.semantic_documents {
                    return Err(SemanticVectorStoreError::reset_required(
                        "source-backed semantic page exceeds its manifest-backed document count",
                    )
                    .into());
                }
                outcome.records_scanned = outcome.records_scanned.saturating_add(1);
                let stable_identity = record.event_id.encode_canonical()?.to_vec();
                let prior = self.stored_source_document(record.event_id.as_uuid())?;
                if prior.as_ref().is_some_and(|prior| {
                    prior.stable_event_identity.as_slice() != stable_identity.as_slice()
                }) {
                    return Err(SemanticVectorStoreError::storage_conflict(format!(
                        "source-backed semantic compact identity collision at {}",
                        record.event_id.as_uuid()
                    ))
                    .into());
                }

                let Some(document) = builder.build_document(&record)? else {
                    outcome.invalidated_chunks = outcome
                        .invalidated_chunks
                        .saturating_add(self.invalidate_source_event(record.event_id.as_uuid())?);
                    outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                    frontier.processed_documents =
                        frontier.processed_documents.checked_add(1).ok_or_else(|| {
                            SemanticVectorStoreError::reset_required(
                                "source-backed semantic frontier document count overflowed",
                            )
                        })?;
                    frontier.after_identity = Some(stable_identity);
                    frontier.last_failure = None;
                    self.store_source_frontier(&frontier)?;
                    continue;
                };
                validate_resolved_document(&record, &document)?;

                let source_text = semantic_source_text(&document.text);
                if semantic_core_content_is_control(&source_text) {
                    outcome.invalidated_chunks = outcome
                        .invalidated_chunks
                        .saturating_add(self.invalidate_source_event(record.event_id.as_uuid())?);
                    outcome.records_filtered = outcome.records_filtered.saturating_add(1);
                    frontier.processed_documents =
                        frontier.processed_documents.checked_add(1).ok_or_else(|| {
                            SemanticVectorStoreError::reset_required(
                                "source-backed semantic frontier document count overflowed",
                            )
                        })?;
                    frontier.after_identity = Some(stable_identity);
                    frontier.last_failure = None;
                    self.store_source_frontier(&frontier)?;
                    continue;
                }
                let source_text_sha256 = semantic_document_hash(
                    &document,
                    &source_text,
                    &generation.semantic_policy_fingerprint,
                );
                let existing_hash = self
                    .existing_hashes_for_event_ids(&[document.event_id])?
                    .remove(&document.event_id);
                let reusable = existing_hash.as_deref() == Some(source_text_sha256.as_str());
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
                    let items = chunks.into_iter().zip(embeddings).collect::<Vec<_>>();
                    self.upsert_chunk_embeddings(&items)?;
                    outcome.records_embedded = outcome.records_embedded.saturating_add(1);
                }

                self.store_resolved_source_document(
                    record.event_id,
                    &stable_identity,
                    &source_text_sha256,
                    &frontier,
                )?;
                frontier.processed_documents =
                    frontier.processed_documents.checked_add(1).ok_or_else(|| {
                        SemanticVectorStoreError::reset_required(
                            "source-backed semantic frontier document count overflowed",
                        )
                    })?;
                frontier.after_identity = Some(stable_identity);
                frontier.last_failure = None;
                self.store_source_frontier(&frontier)?;
            }

            if page.terminal {
                if frontier.processed_documents != frontier.semantic_documents {
                    return Err(SemanticVectorStoreError::reset_required(format!(
                        "source-backed semantic terminal count mismatch: processed {}, expected {}",
                        frontier.processed_documents, frontier.semantic_documents
                    ))
                    .into());
                }
                outcome.deleted_chunks = self.finish_source_generation(&frontier)?;
                outcome.ready = true;
            } else {
                outcome.work_remaining = true;
            }
            Ok(outcome)
        })())
    }

    #[cfg(test)]
    pub(in crate::semantic) fn source_backed_generation_ready(
        &self,
        core_generation_id: &str,
    ) -> Result<bool> {
        semantic_owned_sidecar_result(
            self.acknowledged_source_projection(core_generation_id, None)
                .map(|projection| projection.is_some()),
        )
    }

    pub(in crate::semantic) fn pin_source_backed_generation(
        &self,
        core_generation_id: &str,
        semantic_documents: u64,
    ) -> Result<Option<PinnedFlatGeneration>> {
        semantic_owned_sidecar_result(
            self.acknowledged_source_projection(core_generation_id, Some(semantic_documents))
                .map(|projection| projection.and_then(|projection| projection.flat)),
        )
    }

    pub(in crate::semantic) fn source_backed_generation_ready_exact(
        &self,
        core_generation_id: &str,
        semantic_documents: u64,
    ) -> Result<bool> {
        semantic_owned_sidecar_result(
            self.acknowledged_source_projection(core_generation_id, Some(semantic_documents))
                .map(|projection| projection.is_some()),
        )
    }

    fn acknowledged_source_projection(
        &self,
        core_generation_id: &str,
        expected_semantic_documents: Option<u64>,
    ) -> Result<Option<AcknowledgedSourceProjection>> {
        (|| {
            validate_generation_id(core_generation_id)?;
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
                || acknowledgement.semantic_policy_fingerprint != semantic_policy_fingerprint()?
                || acknowledgement.consumer_build_id
                    != source_consumer_build_id(&fingerprint, core_generation_id)
                || expected_semantic_documents
                    .is_some_and(|expected| acknowledgement.semantic_documents != expected)
            {
                return Ok(None);
            }
            let stored_documents: u64 = self.conn.query_row(
                "SELECT COUNT(*) FROM semantic_source_documents
                 WHERE core_generation_id = ?1",
                [core_generation_id],
                |row| row.get(0),
            )?;
            if acknowledgement.projected_documents != stored_documents
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
                            && acknowledgement.flat_active_events
                                == pinned.stats().active_events as u64
                            && acknowledgement.flat_active_chunks
                                == pinned.stats().active_chunks as u64
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
                return Ok(flat_matches.then_some(AcknowledgedSourceProjection { flat: pinned }));
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
            Ok(matches.then_some(AcknowledgedSourceProjection { flat: Some(pinned) }))
        })()
    }

    fn begin_or_resume_source_generation(
        &self,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<SourceProjectionFrontier> {
        let fingerprint = source_contract_fingerprint()?;
        if let Some(frontier) = self.source_frontier()? {
            if frontier.contract_version == SOURCE_CONTRACT_VERSION
                && frontier.contract_fingerprint == fingerprint
                && frontier.core_generation_id == generation.core_generation_id
                && frontier.semantic_policy_fingerprint == generation.semantic_policy_fingerprint
                && frontier.semantic_documents == generation.semantic_documents
            {
                return Ok(frontier);
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
            processed_documents: 0,
            after_identity: None,
            last_failure: None,
        };
        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SOURCE_FRONTIER_STATE, serde_json::to_string(&frontier)?],
        )?;
        transaction.execute(
            "DELETE FROM semantic_maintenance_state WHERE key = ?1",
            [SOURCE_ACKNOWLEDGEMENT_STATE],
        )?;
        transaction.commit()?;
        Ok(frontier)
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
        self.store_maintenance_json(SOURCE_FRONTIER_STATE, frontier)
    }

    fn store_maintenance_json<T>(&self, key: &str, value: &T) -> Result<()>
    where
        T: Serialize,
    {
        self.conn.execute(
            "INSERT INTO semantic_maintenance_state(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    fn stored_source_document(&self, event_id: Uuid) -> Result<Option<StoredSourceDocument>> {
        self.conn
            .query_row(
                "SELECT stable_event_identity, source_text_sha256, core_generation_id
                 FROM semantic_source_documents WHERE event_id = ?1",
                [event_id.to_string()],
                |row| {
                    let stable_event_identity = row.get(0)?;
                    let _source_text_sha256 = row.get::<_, String>(1)?;
                    let _core_generation_id = row.get::<_, String>(2)?;
                    Ok(StoredSourceDocument {
                        stable_event_identity,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn store_resolved_source_document(
        &self,
        event_id: StableEntityId,
        stable_identity: &[u8],
        source_text_sha256: &str,
        frontier: &SourceProjectionFrontier,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO semantic_source_documents
             (event_id, stable_event_identity, source_text_sha256, core_generation_id,
              consumer_build_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(event_id) DO UPDATE SET
                stable_event_identity = excluded.stable_event_identity,
                source_text_sha256 = excluded.source_text_sha256,
                core_generation_id = excluded.core_generation_id,
                consumer_build_id = excluded.consumer_build_id",
            params![
                event_id.as_uuid().to_string(),
                stable_identity,
                source_text_sha256,
                frontier.core_generation_id,
                frontier.consumer_build_id,
            ],
        )?;
        Ok(())
    }

    fn invalidate_source_event(&mut self, event_id: Uuid) -> Result<usize> {
        self.delete_events(&[event_id])
    }

    fn finish_source_generation(&mut self, frontier: &SourceProjectionFrontier) -> Result<usize> {
        let mut statement = self.conn.prepare(
            "SELECT event_id, source_text_sha256
             FROM semantic_source_documents WHERE core_generation_id = ?1",
        )?;
        let rows = statement.query_map([&frontier.core_generation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let current = rows
            .map(|row| {
                let (value, source_text_sha256) = row?;
                Uuid::parse_str(&value)
                    .map_err(|_| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(SemanticVectorStoreError::reset_required(format!(
                                "invalid source-backed semantic event id: {value}"
                            ))),
                        )
                    })
                    .map(|event_id| (event_id, source_text_sha256))
            })
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        drop(statement);
        let retired = self
            .flat_active_events()?
            .into_iter()
            .map(|event| event.event_id)
            .filter(|event_id| !current.contains_key(event_id))
            .collect::<Vec<_>>();
        let deleted = self.delete_events(&retired)?;
        let pinned = self.flat_pin_generation()?;
        let projected_documents = validate_flat_projection(frontier, &current, pinned.as_ref())?;

        let transaction = self.conn.transaction()?;
        transaction.execute(
            "DELETE FROM semantic_source_documents WHERE core_generation_id != ?1",
            [&frontier.core_generation_id],
        )?;
        let acknowledgement = SourceProjectionAcknowledgement {
            contract_version: frontier.contract_version,
            contract_fingerprint: frontier.contract_fingerprint.clone(),
            core_generation_id: frontier.core_generation_id.clone(),
            semantic_policy_fingerprint: frontier.semantic_policy_fingerprint.clone(),
            consumer_build_id: frontier.consumer_build_id.clone(),
            semantic_documents: frontier.semantic_documents,
            projected_documents,
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
        Ok(deleted)
    }
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
