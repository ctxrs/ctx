use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Result};
use ctx_history_core::{
    EventHydrationRequest, HydrationFailure, HydrationFailureKind, StableEntityId,
    StableEntityKind, IDENTITY_VERSION,
};
use ctx_history_index::{
    EventRecord, SemanticEventCursor, VerifiedIndex, LEXICAL_SCHEMA_VERSION,
    MAX_SEMANTIC_EVENT_PAGE_ITEMS,
};
use ctx_history_store::EventEmbeddingDocument;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::flat_segments::PinnedFlatGeneration;
use super::{SemanticChunkDocument, SemanticVectorStore};
use crate::semantic::{
    indexing::{semantic_chunks_for_document, semantic_document_hash, semantic_source_text},
    model_contract::{semantic_model_key, SEMANTIC_DIMENSIONS},
    vector_store_schema::{semantic_owned_sidecar_result, SemanticVectorStoreError},
};

const SOURCE_FRONTIER_STATE: &str = "source_backed_semantic_frontier_v1";
const SOURCE_ACKNOWLEDGEMENT_STATE: &str = "source_backed_semantic_acknowledgement_v1";
const SOURCE_VECTOR_DIRECTORY: &str = "source-backed-semantic-flat-f32-v0";
const SOURCE_CONTRACT_VERSION: u16 = 3;
const SOURCE_CONTRACT_DOMAIN: &[u8] = b"ctx-source-backed-semantic-contract-v1\0";
const SOURCE_BUILD_DOMAIN: &[u8] = b"ctx-source-backed-semantic-build-v1\0";
const SOURCE_INPUT_LEXICAL_SCHEMA_VERSION: u32 = 4;
const SHA256_HEX_BYTES: usize = 64;

pub(in crate::semantic) fn source_backed_semantic_vector_path(data_root: &Path) -> PathBuf {
    data_root.join(SOURCE_VECTOR_DIRECTORY)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceBackedSemanticGeneration {
    pub(super) core_generation_id: String,
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
    pub(super) records: Vec<EventRecord>,
    pub(super) terminal: bool,
}

pub(in crate::semantic) trait SourceBackedSemanticResolver {
    /// Rereads and verifies exact provider content for one committed typed
    /// locator. The returned document may combine provider-native records
    /// according to the semantic document contract, but it must not come from
    /// a canonical body copy.
    fn resolve_document(
        &mut self,
        event: &EventRecord,
        request: &EventHydrationRequest,
    ) -> std::result::Result<EventEmbeddingDocument, HydrationFailure>;
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
    pub(in crate::semantic) unavailable: Option<HydrationFailureKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceProjectionFrontier {
    contract_version: u16,
    contract_fingerprint: String,
    core_generation_id: String,
    consumer_build_id: String,
    semantic_documents: u64,
    processed_documents: u64,
    after_identity: Option<Vec<u8>>,
    last_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceProjectionAcknowledgement {
    contract_version: u16,
    contract_fingerprint: String,
    core_generation_id: String,
    consumer_build_id: String,
    semantic_documents: u64,
    projected_documents: u64,
    #[serde(default)]
    flat_generation: u64,
    #[serde(default)]
    flat_generation_hash: String,
    #[serde(default)]
    flat_active_events: u64,
    #[serde(default)]
    flat_active_chunks: u64,
}

#[derive(Debug)]
struct StoredSourceDocument {
    stable_event_identity: Vec<u8>,
    locator_json: Vec<u8>,
    source_text_sha256: String,
    core_generation_id: String,
}

struct AcknowledgedSourceProjection {
    flat: Option<PinnedFlatGeneration>,
}

impl SemanticVectorStore {
    pub(in crate::semantic) fn reconcile_source_backed_index<R, E>(
        &mut self,
        index: &VerifiedIndex,
        resolver: &mut R,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        R: SourceBackedSemanticResolver,
        E: SourceBackedSemanticEmbedder,
    {
        semantic_owned_sidecar_result((|| {
            let semantic_documents = index.semantic_eligible_event_count()?;
            let generation =
                SourceBackedSemanticGeneration::from_verified_index(index, semantic_documents)?;
            let frontier = self.begin_or_resume_source_generation(&generation)?;
            let after = frontier
                .after_identity
                .as_deref()
                .map(StableEntityId::decode_canonical)
                .transpose()?;
            let cursor = after.map(|after| {
                SemanticEventCursor::new(generation.core_generation_id.clone(), after)
            });
            let page = index.semantic_event_page(cursor.as_ref(), MAX_SEMANTIC_EVENT_PAGE_ITEMS)?;
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
                resolver,
                embedder,
            )
        })())
    }

    pub(super) fn reconcile_source_backed_page<R, E>(
        &mut self,
        generation: &SourceBackedSemanticGeneration,
        page: SourceBackedSemanticPage,
        resolver: &mut R,
        embedder: &mut E,
    ) -> Result<SourceBackedSemanticOutcome>
    where
        R: SourceBackedSemanticResolver,
        E: SourceBackedSemanticEmbedder,
    {
        semantic_owned_sidecar_result((|| {
            validate_generation(generation)?;
            let mut frontier = self.begin_or_resume_source_generation(generation)?;
            validate_page(&frontier, &page)?;

            let mut outcome = SourceBackedSemanticOutcome::default();
            for event in page.records {
                if frontier.processed_documents >= frontier.semantic_documents {
                    return Err(SemanticVectorStoreError::reset_required(
                        "source-backed semantic page exceeds its manifest-backed document count",
                    )
                    .into());
                }
                outcome.records_scanned = outcome.records_scanned.saturating_add(1);
                let stable_identity = event.event_id.encode_canonical()?.to_vec();
                let locator_json = serde_json::to_vec(&event.locator)?;
                let prior = self.stored_source_document(event.event_id.as_uuid())?;
                if prior.as_ref().is_some_and(|prior| {
                    prior.stable_event_identity.as_slice() != stable_identity.as_slice()
                }) {
                    return Err(SemanticVectorStoreError::storage_conflict(format!(
                        "source-backed semantic compact identity collision at {}",
                        event.event_id.as_uuid()
                    ))
                    .into());
                }

                let evidence_changed = prior
                    .as_ref()
                    .is_some_and(|prior| prior.locator_json != locator_json);
                if evidence_changed {
                    outcome.invalidated_chunks = outcome
                        .invalidated_chunks
                        .saturating_add(self.invalidate_source_event(event.event_id.as_uuid())?);
                }

                let request = EventHydrationRequest::new(event.event_id, event.locator.clone())?;
                let document = match resolver.resolve_document(&event, &request) {
                    Ok(document) => document,
                    Err(failure) => {
                        if evidence_changed || hydration_failure_invalidates(failure.kind) {
                            outcome.invalidated_chunks = outcome.invalidated_chunks.saturating_add(
                                self.invalidate_source_event(event.event_id.as_uuid())?,
                            );
                        }
                        frontier.last_failure =
                            Some(hydration_failure_name(failure.kind).to_owned());
                        self.store_source_frontier(&frontier)?;
                        outcome.unavailable = Some(failure.kind);
                        outcome.work_remaining = true;
                        return Ok(outcome);
                    }
                };
                validate_resolved_document(&event, &document)?;

                let source_text = semantic_source_text(&document.text);
                if semantic_hydrated_source_is_control(&source_text) {
                    outcome.invalidated_chunks = outcome
                        .invalidated_chunks
                        .saturating_add(self.invalidate_source_event(event.event_id.as_uuid())?);
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
                let source_text_sha256 = semantic_document_hash(&document, &source_text);
                let existing_hash = self
                    .existing_hashes_for_event_ids(&[document.event_id])?
                    .remove(&document.event_id);
                let reusable = !evidence_changed
                    && existing_hash.as_deref() == Some(source_text_sha256.as_str());
                if reusable {
                    outcome.records_reused = outcome.records_reused.saturating_add(1);
                } else {
                    let chunks =
                        semantic_chunks_for_document(&document, &source_text, &source_text_sha256);
                    if chunks.is_empty() {
                        return Err(anyhow!(
                            "source-backed semantic resolver returned an empty document for {}",
                            event.event_id
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
                    event.event_id,
                    &stable_identity,
                    &locator_json,
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
            let fingerprint = source_contract_fingerprint();
            if acknowledgement.contract_version != SOURCE_CONTRACT_VERSION
                || acknowledgement.contract_fingerprint != fingerprint
                || acknowledgement.core_generation_id != core_generation_id
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

    pub(super) fn source_backed_frontier_generation(&self) -> Result<Option<String>> {
        semantic_owned_sidecar_result(
            self.source_frontier()
                .map(|frontier| frontier.map(|frontier| frontier.core_generation_id)),
        )
    }

    pub(super) fn source_backed_hashes_for_generation(
        &self,
        core_generation_id: &str,
        event_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, String>> {
        semantic_owned_sidecar_result((|| {
            validate_generation_id(core_generation_id)?;
            if event_ids.is_empty() || !self.source_backed_generation_ready(core_generation_id)? {
                return Ok(HashMap::new());
            }
            let mut hashes = HashMap::new();
            let mut statement = self.conn.prepare(
                "SELECT stable_event_identity, source_text_sha256
                 FROM semantic_source_documents
                 WHERE event_id = ?1 AND core_generation_id = ?2",
            )?;
            for event_id in event_ids {
                let row = statement
                    .query_row(params![event_id.to_string(), core_generation_id], |row| {
                        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
                    })
                    .optional()?;
                let Some((identity, hash)) = row else {
                    continue;
                };
                let stable = StableEntityId::decode_canonical(&identity)?;
                if stable.as_uuid() != *event_id || stable.entity_kind() != StableEntityKind::Event
                {
                    return Err(SemanticVectorStoreError::reset_required(
                        "source-backed semantic metadata contains an invalid stable identity",
                    )
                    .into());
                }
                hashes.insert(*event_id, hash);
            }
            Ok(hashes)
        })())
    }

    fn begin_or_resume_source_generation(
        &self,
        generation: &SourceBackedSemanticGeneration,
    ) -> Result<SourceProjectionFrontier> {
        let fingerprint = source_contract_fingerprint();
        if let Some(frontier) = self.source_frontier()? {
            if frontier.contract_version == SOURCE_CONTRACT_VERSION
                && frontier.contract_fingerprint == fingerprint
                && frontier.core_generation_id == generation.core_generation_id
                && frontier.semantic_documents == generation.semantic_documents
            {
                return Ok(frontier);
            }
        }
        let frontier = SourceProjectionFrontier {
            contract_version: SOURCE_CONTRACT_VERSION,
            contract_fingerprint: fingerprint.clone(),
            core_generation_id: generation.core_generation_id.clone(),
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
                "SELECT stable_event_identity, locator_json, source_text_sha256, core_generation_id
                 FROM semantic_source_documents WHERE event_id = ?1",
                [event_id.to_string()],
                |row| {
                    Ok(StoredSourceDocument {
                        stable_event_identity: row.get(0)?,
                        locator_json: row.get(1)?,
                        source_text_sha256: row.get(2)?,
                        core_generation_id: row.get(3)?,
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
        locator_json: &[u8],
        source_text_sha256: &str,
        frontier: &SourceProjectionFrontier,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO semantic_source_documents
             (event_id, stable_event_identity, locator_json, source_text_sha256,
              core_generation_id, consumer_build_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(event_id) DO UPDATE SET
                stable_event_identity = excluded.stable_event_identity,
                locator_json = excluded.locator_json,
                source_text_sha256 = excluded.source_text_sha256,
                core_generation_id = excluded.core_generation_id,
                consumer_build_id = excluded.consumer_build_id",
            params![
                event_id.as_uuid().to_string(),
                stable_identity,
                locator_json,
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

fn validate_flat_projection(
    frontier: &SourceProjectionFrontier,
    source_documents: &HashMap<Uuid, String>,
    pinned: Option<&PinnedFlatGeneration>,
) -> Result<u64> {
    let source_document_count = u64::try_from(source_documents.len())?;
    if source_document_count > frontier.semantic_documents {
        return Err(SemanticVectorStoreError::reset_required(format!(
            "source-backed semantic completion has {source_document_count} projected documents, but only {} metadata-eligible records",
            frontier.semantic_documents
        ))
        .into());
    }
    if source_document_count == 0 {
        if pinned.is_some_and(|pinned| {
            pinned.stats().active_events != 0 || pinned.stats().active_chunks != 0
        }) {
            return Err(SemanticVectorStoreError::reset_required(
                "empty source-backed semantic generation has active flat F32 records",
            )
            .into());
        }
        return Ok(0);
    }
    let pinned = pinned.ok_or_else(|| {
        SemanticVectorStoreError::reset_required(
            "source-backed semantic completion has no flat F32 generation",
        )
    })?;
    if pinned.stats().active_events as u64 != source_document_count
        || pinned.active_events().len() != source_documents.len()
    {
        return Err(SemanticVectorStoreError::reset_required(
            "source-backed semantic source-document count does not match flat F32 events",
        )
        .into());
    }
    for event in pinned.active_events() {
        if event.chunk_count == 0
            || source_documents
                .get(&event.event_id)
                .is_none_or(|hash| hash != &event.source_text_hash.to_hex())
        {
            return Err(SemanticVectorStoreError::reset_required(
                "source-backed semantic source documents do not match flat F32 event metadata",
            )
            .into());
        }
    }
    Ok(source_document_count)
}

/// The metadata feed deliberately does not inspect indexed or stored body
/// text. Control-message exclusion happens only after exact provider content
/// has been hydrated for the generation-bound locator.
pub(in crate::semantic) fn semantic_hydrated_source_is_control(text: &str) -> bool {
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

fn validate_generation(generation: &SourceBackedSemanticGeneration) -> Result<()> {
    validate_generation_id(&generation.core_generation_id)
}

fn validate_generation_id(generation_id: &str) -> Result<()> {
    if generation_id.len() != SHA256_HEX_BYTES
        || !generation_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(anyhow!(
            "source-backed semantic generation ID is not a lowercase SHA-256 digest"
        ));
    }
    Ok(())
}

fn validate_page(
    frontier: &SourceProjectionFrontier,
    page: &SourceBackedSemanticPage,
) -> Result<()> {
    if page.core_generation_id != frontier.core_generation_id {
        return Err(anyhow!(
            "source-backed semantic page generation does not match its durable frontier"
        ));
    }
    let requested_after = page
        .after
        .map(|identity| identity.encode_canonical().map(|value| value.to_vec()))
        .transpose()?;
    if requested_after != frontier.after_identity {
        return Err(anyhow!(
            "source-backed semantic page cursor does not match its durable frontier"
        ));
    }
    let mut previous = frontier.after_identity.clone();
    for event in &page.records {
        event.event_id.validate_contract()?;
        event.locator.validate_contract()?;
        if event.event_id.entity_kind() != StableEntityKind::Event
            || event.event_id.source_digest() != event.locator.source().identity().digest()
            || event.event_id.source_descriptor_digest()
                != event.locator.source().exact_descriptor_digest()
        {
            return Err(anyhow!(
                "source-backed semantic page contains mismatched identity and locator evidence"
            ));
        }
        let encoded = event.event_id.encode_canonical()?;
        if previous
            .as_deref()
            .is_some_and(|previous| previous >= encoded.as_slice())
        {
            return Err(anyhow!(
                "source-backed semantic records are not in strict stable-identity order"
            ));
        }
        previous = Some(encoded.to_vec());
    }
    Ok(())
}

fn validate_resolved_document(
    event: &EventRecord,
    document: &EventEmbeddingDocument,
) -> Result<()> {
    if document.event_id != event.event_id.as_uuid()
        || document.seq != event.event_sequence
        || document.text.trim().is_empty()
    {
        return Err(anyhow!(
            "source-backed semantic resolver returned a document that does not match {}",
            event.event_id
        ));
    }
    Ok(())
}

fn hydration_failure_invalidates(kind: HydrationFailureKind) -> bool {
    matches!(
        kind,
        HydrationFailureKind::ConfirmedDeleted
            | HydrationFailureKind::StaleSourceEvidence
            | HydrationFailureKind::StaleRecordEvidence
            | HydrationFailureKind::MissingRecord
            | HydrationFailureKind::InvalidLocator
    )
}

fn hydration_failure_name(kind: HydrationFailureKind) -> &'static str {
    match kind {
        HydrationFailureKind::TemporarilyUnavailable => "temporarily_unavailable",
        HydrationFailureKind::ConfirmedDeleted => "confirmed_deleted",
        HydrationFailureKind::StaleSourceEvidence => "stale_source_evidence",
        HydrationFailureKind::StaleRecordEvidence => "stale_record_evidence",
        HydrationFailureKind::MissingRecord => "missing_record",
        HydrationFailureKind::UnsupportedParserRevision => "unsupported_parser_revision",
        HydrationFailureKind::InvalidLocator => "invalid_locator",
    }
}

fn source_contract_fingerprint() -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE_CONTRACT_DOMAIN);
    digest.update(SOURCE_CONTRACT_VERSION.to_be_bytes());
    digest.update(IDENTITY_VERSION.to_be_bytes());
    digest.update(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION.to_be_bytes());
    digest.update(semantic_model_key().as_bytes());
    hex(&digest.finalize())
}

fn source_consumer_build_id(contract_fingerprint: &str, core_generation_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE_BUILD_DOMAIN);
    digest.update(contract_fingerprint.as_bytes());
    digest.update(core_generation_id.as_bytes());
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ctx_history_core::{
        derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, EventRole,
        EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
        SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator, TypedKey,
    };
    use tempfile::TempDir;

    use super::*;

    struct FakeResolver {
        texts: HashMap<Uuid, String>,
        failures: HashMap<Uuid, HydrationFailureKind>,
        calls: Vec<Uuid>,
    }

    impl FakeResolver {
        fn available(records: &[EventRecord]) -> Self {
            Self {
                texts: records
                    .iter()
                    .map(|record| {
                        (
                            record.event_id.as_uuid(),
                            format!("exact provider text for {}", record.event_sequence),
                        )
                    })
                    .collect(),
                failures: HashMap::new(),
                calls: Vec::new(),
            }
        }
    }

    impl SourceBackedSemanticResolver for FakeResolver {
        fn resolve_document(
            &mut self,
            event: &EventRecord,
            request: &EventHydrationRequest,
        ) -> std::result::Result<EventEmbeddingDocument, HydrationFailure> {
            assert_eq!(request.event_id(), event.event_id);
            assert_eq!(request.locator(), &event.locator);
            self.calls.push(event.event_id.as_uuid());
            if let Some(kind) = self.failures.get(&event.event_id.as_uuid()).copied() {
                return Err(HydrationFailure {
                    kind,
                    detail: "fixture source unavailable".to_owned(),
                });
            }
            let text = self
                .texts
                .get(&event.event_id.as_uuid())
                .cloned()
                .ok_or_else(|| HydrationFailure {
                    kind: HydrationFailureKind::MissingRecord,
                    detail: "fixture record missing".to_owned(),
                })?;
            Ok(EventEmbeddingDocument {
                event_id: event.event_id.as_uuid(),
                history_record_id: None,
                session_id: Some(event.session_id.as_uuid()),
                seq: event.event_sequence,
                occurred_at_ms: event.occurred_at_unix_ms.unwrap_or_default(),
                anchor_occurred_at_ms: event.occurred_at_unix_ms.unwrap_or_default(),
                event_type: EventType::Message,
                role: Some(EventRole::User),
                rank_bucket: "source_backed_event".to_owned(),
                provider: Some(CaptureProvider::Codex),
                source_format: Some(event.source_format.clone()),
                agent_type: None,
                session_is_primary: Some(true),
                cwd: event.cwd.clone(),
                raw_source_path: None,
                record_title: None,
                record_kind: Some("message".to_owned()),
                record_workspace: event.workspace.clone(),
                text,
            })
        }
    }

    #[derive(Default)]
    struct FakeEmbedder {
        calls: usize,
    }

    impl SourceBackedSemanticEmbedder for FakeEmbedder {
        fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
            self.calls = self.calls.saturating_add(chunks.len());
            Ok(chunks
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
                    embedding[index % SEMANTIC_DIMENSIONS] = 1.0;
                    embedding
                })
                .collect())
        }
    }

    struct Fixture {
        _temp: TempDir,
        path: std::path::PathBuf,
        source: SourceKey,
        session_id: StableEntityId,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let temp = tempfile::tempdir()?;
            let source = SourceKey::derive(
                "codex",
                "codex_session_jsonl_tree",
                "session",
                1,
                SourceAnchor::CatalogLineage([7; 32]),
            )?;
            let session_key =
                NativeSessionKey::native_id("session", TypedKey::utf8("fixture-session")?)?;
            let session_id = derive_session_id(SessionIdentityInput {
                source: &source,
                logical_session_kind: "thread",
                native_session_key: &session_key,
            })?;
            Ok(Self {
                path: temp.path().join("semantic-vectors"),
                _temp: temp,
                source,
                session_id,
            })
        }

        fn event(&self, sequence: u64, record_digest: u8) -> Result<EventRecord> {
            let native_item_key = NativeItemKey::native_id("message", TypedKey::U64(sequence))?;
            let event_id = derive_event_id(EventIdentityInput {
                source: &self.source,
                session_id: self.session_id,
                logical_item_kind: "message",
                native_item_key: &native_item_key,
                subrecord_selector: None,
            })?;
            let locator = SourceRecordLocator::new(
                self.source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: sequence * 100,
                    byte_length: 50,
                    physical_ordinal: sequence,
                    native_session_key: Some(TypedKey::utf8("fixture-session")?),
                    native_event_key: Some(TypedKey::U64(sequence)),
                },
                LocatorRevisionPolicy::ExactSourceRevision,
                Some([record_digest; 32]),
                [record_digest; 32],
            )?;
            Ok(EventRecord {
                event_id,
                session_id: self.session_id,
                parent_session_id: None,
                root_session_id: self.session_id,
                locator,
                provider: "codex".to_owned(),
                source_format: "codex_session_jsonl_tree".to_owned(),
                provider_session_id: Some("fixture-session".to_owned()),
                branch: Some("main".to_owned()),
                source_path: None,
                agent_type: "primary".to_owned(),
                is_primary: true,
                event_sequence: sequence,
                occurred_at_unix_ms: Some(sequence as i64),
                event_type: "message".to_owned(),
                role: Some("user".to_owned()),
                preview: String::new(),
                workspace: Some("/workspace".to_owned()),
                cwd: Some("/workspace".to_owned()),
                touched_files: Vec::new(),
            })
        }
    }

    fn generation(id: u8, semantic_documents: u64) -> SourceBackedSemanticGeneration {
        SourceBackedSemanticGeneration {
            core_generation_id: format!("{id:064x}"),
            semantic_documents,
        }
    }

    fn stable_identity_order(records: &mut [EventRecord]) {
        records.sort_by_key(|record| record.event_id.encode_canonical().unwrap());
    }

    #[test]
    fn new_install_catch_up_resumes_from_its_own_stable_identity_frontier() -> Result<()> {
        let fixture = Fixture::new()?;
        let mut records = vec![fixture.event(1, 1)?, fixture.event(2, 2)?];
        stable_identity_order(&mut records);
        let first = records[0].clone();
        let second = records[1].clone();
        let target = generation(1, 2);
        let mut resolver = FakeResolver::available(&[first.clone(), second.clone()]);
        let mut embedder = FakeEmbedder::default();

        {
            let mut store = SemanticVectorStore::open(&fixture.path)?;
            let outcome = store.reconcile_source_backed_page(
                &target,
                SourceBackedSemanticPage {
                    core_generation_id: target.core_generation_id.clone(),
                    after: None,
                    records: vec![first.clone()],
                    terminal: false,
                },
                &mut resolver,
                &mut embedder,
            )?;
            assert_eq!(outcome.records_embedded, 1);
            assert!(outcome.work_remaining);
            assert!(!outcome.ready);
            assert!(!store.source_backed_generation_ready(&target.core_generation_id)?);
        }

        let mut store = SemanticVectorStore::open(&fixture.path)?;
        assert_eq!(
            store.source_backed_frontier_generation()?.as_deref(),
            Some(target.core_generation_id.as_str())
        );
        let outcome = store.reconcile_source_backed_page(
            &target,
            SourceBackedSemanticPage {
                core_generation_id: target.core_generation_id.clone(),
                after: Some(first.event_id),
                records: vec![second.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;
        assert!(outcome.ready);
        assert!(!outcome.work_remaining);
        assert!(store.source_backed_generation_ready(&target.core_generation_id)?);
        assert_eq!(store.cached_or_exact_stats()?.embedded_items, 2);
        assert_eq!(
            store
                .source_backed_hashes_for_generation(
                    &target.core_generation_id,
                    &[first.event_id.as_uuid(), second.event_id.as_uuid()],
                )?
                .len(),
            2
        );
        let pinned = store
            .pin_source_backed_generation(&target.core_generation_id, 2)?
            .expect("exact flat generation");
        assert_eq!(pinned.stats().active_events, 2);
        store.delete_events(&[first.event_id.as_uuid()])?;
        assert!(!store.source_backed_generation_ready_exact(&target.core_generation_id, 2)?);
        assert!(store
            .pin_source_backed_generation(&target.core_generation_id, 2)?
            .is_none());
        Ok(())
    }

    #[test]
    fn metadata_eligible_control_record_is_filtered_only_after_exact_hydration() -> Result<()> {
        let fixture = Fixture::new()?;
        let event = fixture.event(1, 1)?;
        let target = generation(9, 1);
        let mut resolver = FakeResolver::available(std::slice::from_ref(&event));
        resolver.texts.insert(
            event.event_id.as_uuid(),
            "<environment_context>exact provider control record</environment_context>".to_owned(),
        );
        let mut embedder = FakeEmbedder::default();
        let mut store = SemanticVectorStore::open(&fixture.path)?;

        let outcome = store.reconcile_source_backed_page(
            &target,
            SourceBackedSemanticPage {
                core_generation_id: target.core_generation_id.clone(),
                after: None,
                records: vec![event.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;

        assert_eq!(resolver.calls, vec![event.event_id.as_uuid()]);
        assert_eq!(outcome.records_filtered, 1);
        assert_eq!(outcome.records_embedded, 0);
        assert_eq!(embedder.calls, 0);
        assert!(outcome.ready);
        assert!(store.source_backed_generation_ready_exact(&target.core_generation_id, 1)?);
        if let Some(pinned) = store.pin_source_backed_generation(&target.core_generation_id, 1)? {
            assert_eq!(pinned.stats().active_events, 0);
            assert_eq!(pinned.stats().active_chunks, 0);
        }
        Ok(())
    }

    #[test]
    fn same_id_rewrite_reembeds_and_complete_generation_retires_deletions() -> Result<()> {
        let fixture = Fixture::new()?;
        let original = fixture.event(1, 1)?;
        let deleted = fixture.event(2, 2)?;
        let first_generation = generation(2, 2);
        let mut resolver = FakeResolver::available(&[original.clone(), deleted.clone()]);
        let mut embedder = FakeEmbedder::default();
        let mut store = SemanticVectorStore::open(&fixture.path)?;
        let mut initial_records = vec![original.clone(), deleted.clone()];
        stable_identity_order(&mut initial_records);
        assert!(
            store
                .reconcile_source_backed_page(
                    &first_generation,
                    SourceBackedSemanticPage {
                        core_generation_id: first_generation.core_generation_id.clone(),
                        after: None,
                        records: initial_records,
                        terminal: true,
                    },
                    &mut resolver,
                    &mut embedder,
                )?
                .ready
        );
        let original_hash = store
            .existing_hashes_for_event_ids(&[original.event_id.as_uuid()])?
            .remove(&original.event_id.as_uuid())
            .expect("original hash");

        let rewritten = fixture.event(1, 9)?;
        let second_generation = generation(3, 1);
        resolver.texts.insert(
            rewritten.event_id.as_uuid(),
            "rewritten exact provider text".to_owned(),
        );
        let outcome = store.reconcile_source_backed_page(
            &second_generation,
            SourceBackedSemanticPage {
                core_generation_id: second_generation.core_generation_id.clone(),
                after: None,
                records: vec![rewritten.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;
        assert_eq!(outcome.invalidated_chunks, 1);
        assert_eq!(outcome.deleted_chunks, 1);
        assert!(outcome.ready);
        let hashes = store.existing_hashes_for_event_ids(&[
            rewritten.event_id.as_uuid(),
            deleted.event_id.as_uuid(),
        ])?;
        assert_ne!(
            hashes.get(&rewritten.event_id.as_uuid()),
            Some(&original_hash)
        );
        assert!(!hashes.contains_key(&deleted.event_id.as_uuid()));
        assert_eq!(store.cached_or_exact_stats()?.embedded_items, 1);
        Ok(())
    }

    #[test]
    fn unavailable_source_never_advances_or_exposes_the_new_core_generation() -> Result<()> {
        let fixture = Fixture::new()?;
        let event = fixture.event(1, 1)?;
        let initial = generation(4, 1);
        let mut resolver = FakeResolver::available(std::slice::from_ref(&event));
        let mut embedder = FakeEmbedder::default();
        let mut store = SemanticVectorStore::open(&fixture.path)?;
        assert!(
            store
                .reconcile_source_backed_page(
                    &initial,
                    SourceBackedSemanticPage {
                        core_generation_id: initial.core_generation_id.clone(),
                        after: None,
                        records: vec![event.clone()],
                        terminal: true,
                    },
                    &mut resolver,
                    &mut embedder,
                )?
                .ready
        );

        let core_receipt = generation(5, 1);
        resolver.failures.insert(
            event.event_id.as_uuid(),
            HydrationFailureKind::TemporarilyUnavailable,
        );
        let outcome = store.reconcile_source_backed_page(
            &core_receipt,
            SourceBackedSemanticPage {
                core_generation_id: core_receipt.core_generation_id.clone(),
                after: None,
                records: vec![event.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;
        assert_eq!(
            outcome.unavailable,
            Some(HydrationFailureKind::TemporarilyUnavailable)
        );
        assert!(outcome.work_remaining);
        assert!(!outcome.ready);
        assert_eq!(
            core_receipt.core_generation_id,
            format!("{:064x}", 5),
            "semantic failure cannot mutate or roll back Core publication"
        );
        assert!(!store.source_backed_generation_ready(&core_receipt.core_generation_id)?);
        assert!(store
            .source_backed_hashes_for_generation(
                &core_receipt.core_generation_id,
                &[event.event_id.as_uuid()],
            )?
            .is_empty());
        assert!(!store.source_backed_generation_ready(&initial.core_generation_id)?);
        assert_eq!(store.cached_or_exact_stats()?.embedded_items, 1);
        Ok(())
    }

    #[test]
    fn rewritten_locator_is_invalidated_even_when_the_source_is_unavailable() -> Result<()> {
        let fixture = Fixture::new()?;
        let original = fixture.event(1, 1)?;
        let initial = generation(6, 1);
        let mut resolver = FakeResolver::available(std::slice::from_ref(&original));
        let mut embedder = FakeEmbedder::default();
        let mut store = SemanticVectorStore::open(&fixture.path)?;
        store.reconcile_source_backed_page(
            &initial,
            SourceBackedSemanticPage {
                core_generation_id: initial.core_generation_id.clone(),
                after: None,
                records: vec![original.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;

        let rewritten = fixture.event(1, 8)?;
        resolver.failures.insert(
            rewritten.event_id.as_uuid(),
            HydrationFailureKind::TemporarilyUnavailable,
        );
        let target = generation(7, 1);
        let outcome = store.reconcile_source_backed_page(
            &target,
            SourceBackedSemanticPage {
                core_generation_id: target.core_generation_id.clone(),
                after: None,
                records: vec![rewritten.clone()],
                terminal: true,
            },
            &mut resolver,
            &mut embedder,
        )?;
        assert_eq!(outcome.invalidated_chunks, 1);
        let stats = store.cached_or_exact_stats()?;
        assert_eq!(stats.embedded_items, 0);
        assert_eq!(stats.embedded_chunks, 0);
        assert!(!store.source_backed_generation_ready(&target.core_generation_id)?);
        Ok(())
    }
}
