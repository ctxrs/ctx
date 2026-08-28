use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CaptureProvider, CertifiedSource, CoreActivity,
    CoreDiscoveryExclusion, CoreRecord, EventIdentityInput, EventRole, EventType, LiteralFactKind,
    NativeItemKey, NativeSessionKey, ProviderDeclaredFact, ProviderNativeCopyProof,
    ProviderNativeEventCopy, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{
    current_semantic_generation_policy, CoreEventRecord, GenerationWriter, SourceEventRole,
    VerifiedIndex, WriterOptions,
};
use tempfile::TempDir;

use super::*;
use crate::vector_store_search::scan_exact_generation;

mod content;
mod filter_accounting;
mod policy_rebuild;
mod proportionality;
mod provider_native;
mod recovery;

const TAIL_TOKEN: &str = "semantic-tail-token-7f0d";
const EMPTY_DOCUMENT_TOKEN: &str = "semantic-empty-document-fixture-7f0d";

#[derive(Default)]
struct CoreBuilder {
    calls: Vec<Uuid>,
    fail_on: HashSet<Uuid>,
    fail_after: Option<usize>,
}

impl SemanticDocumentBuilder for CoreBuilder {
    fn build_document(
        &mut self,
        record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        self.calls.push(record.event_id.as_uuid());
        if self.fail_on.contains(&record.event_id.as_uuid())
            || self
                .fail_after
                .is_some_and(|limit| self.calls.len() > limit)
        {
            return Err(anyhow!("forced Core projection interruption"));
        }
        let text = ctx_history_index::project_body_search(record.core_record.content.clone())?
            .unwrap_or_default();
        if text.is_empty() || text.contains(EMPTY_DOCUMENT_TOKEN) {
            return Ok(None);
        }
        Ok(Some(SemanticEventDocument {
            event_id: record.event_id.as_uuid(),
            session_id: Some(record.session_id.as_uuid()),
            seq: record.event_sequence,
            occurred_at_ms: record.occurred_at_unix_ms.unwrap_or_default(),
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: "core_event".to_owned(),
            provider: Some(CaptureProvider::Codex),
            source_format: Some(record.source_format.clone()),
            agent_scope: record.core_record.agent_scope,
            literal_facts: record
                .core_record
                .content
                .activity
                .as_ref()
                .map_or_else(Vec::new, |activity| activity.facts.clone()),
            text,
        }))
    }
}

#[derive(Default)]
struct MarkerEmbedder {
    chunks: usize,
    calls: usize,
}

impl SemanticBatchEmbedder for MarkerEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        self.calls = self.calls.saturating_add(1);
        self.chunks = self.chunks.saturating_add(chunks.len());
        Ok(chunks
            .iter()
            .map(|chunk| {
                let mut embedding = vec![0.0; SEMANTIC_DIMENSIONS];
                embedding[usize::from(!chunk.text.contains(TAIL_TOKEN))] = 1.0;
                embedding
            })
            .collect())
    }
}

struct FixtureSource {
    source: SourceKey,
    session_id: StableEntityId,
}

struct Fixture {
    _temp: TempDir,
    data_root: PathBuf,
    semantic_path: PathBuf,
    sources: Vec<FixtureSource>,
}

impl Fixture {
    fn new(source_count: usize) -> Result<Self> {
        Self::new_with_source_format(source_count, "codex_session_jsonl_tree")
    }

    fn new_with_source_format(source_count: usize, source_format: &str) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let mut sources = Vec::new();
        for source_index in 0..source_count {
            let anchor = u8::try_from(source_index + 1)?;
            let source = SourceKey::derive(
                "codex",
                source_format,
                "session",
                1,
                SourceAnchor::CatalogLineage([anchor; 32]),
            )?;
            let native_session_key = NativeSessionKey::native_id(
                "session",
                TypedKey::utf8(format!("fixture-session-{source_index}"))?,
            )?;
            let session_id = derive_session_id(SessionIdentityInput {
                source: &source,
                logical_session_kind: "thread",
                native_session_key: &native_session_key,
            })?;
            sources.push(FixtureSource { source, session_id });
        }
        Ok(Self {
            semantic_path: source_backed_semantic_vector_path(&data_root),
            data_root,
            _temp: temp,
            sources,
        })
    }

    fn record(&self, source_index: usize, sequence: u64, body: &str) -> Result<CoreRecord> {
        self.record_with_event_sequence(source_index, sequence, sequence, body)
    }

    fn record_with_role(
        &self,
        source_index: usize,
        sequence: u64,
        body: &str,
        role: EventRole,
    ) -> Result<CoreRecord> {
        let mut record = self.record(source_index, sequence, body)?;
        record.role = Some(role.as_str().to_owned());
        record.validate_contract()?;
        Ok(record)
    }

    fn record_with_event_sequence(
        &self,
        source_index: usize,
        identity_sequence: u64,
        event_sequence: u64,
        body: &str,
    ) -> Result<CoreRecord> {
        let fixture_source = &self.sources[source_index];
        let event_id = derive_event_id(EventIdentityInput {
            source: &fixture_source.source,
            session_id: fixture_source.session_id,
            logical_item_kind: "message",
            native_item_key: &NativeItemKey::native_id(
                "message",
                TypedKey::U64(identity_sequence),
            )?,
            subrecord_selector: None,
        })?;
        let mut record = CoreRecord::new_selected(
            event_id,
            fixture_source.session_id,
            fixture_source.source.clone(),
            event_sequence,
            "message",
            "semantic-source-projection-test-v1",
            body,
        )?;
        record.provider_session_id = Some(format!("fixture-session-{source_index}"));
        record.native_event_id = Some(TypedKey::U64(identity_sequence));
        record.role = Some("user".to_owned());
        record.occurred_at_unix_ms = Some(event_sequence as i64);
        record.agent_scope = Some(AgentScope::Primary);
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts: vec![
                ProviderDeclaredFact {
                    kind: LiteralFactKind::Workspace,
                    value: "/workspace".to_owned(),
                },
                ProviderDeclaredFact {
                    kind: LiteralFactKind::SessionCwd,
                    value: "/workspace".to_owned(),
                },
            ],
        });
        record.validate_contract()?;
        Ok(record)
    }

    fn event_id(&self, source_index: usize, sequence: u64) -> Result<Uuid> {
        Ok(self
            .record(source_index, sequence, "identity")?
            .event_id
            .as_uuid())
    }

    fn publish(&self, name: &str, specs: &[(usize, Vec<String>)]) -> Result<VerifiedIndex> {
        let root = self.data_root.join(format!("index-{name}"));
        self.publish_to_root(&root, name, specs)
    }

    fn publish_to_root(
        &self,
        root: &Path,
        name: &str,
        specs: &[(usize, Vec<String>)],
    ) -> Result<VerifiedIndex> {
        let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
        for (source_index, records) in specs {
            let fixture_source = &self.sources[*source_index];
            writer.begin_source(fixture_source.source.clone())?;
            for (offset, body) in records.iter().enumerate() {
                writer.add_core_record(self.record(
                    *source_index,
                    u64::try_from(offset + 1)?,
                    body,
                )?)?;
            }
            let observation = SourceObservation::new(
                fixture_source.source.clone(),
                format!("fixture-{name}"),
                name.as_bytes().to_vec(),
            )?;
            let count = u64::try_from(records.len())?;
            writer.certify_source(CertifiedSource::certify(
                observation.clone(),
                observation,
                "fixture-parser-v1",
                [u8::try_from(*source_index + 1)?; 32],
                ScannedSourceCounts {
                    complete_records: count,
                    retained_records: count,
                    indexed_documents: count,
                    certified_bytes: count.saturating_mul(50),
                    ..ScannedSourceCounts::default()
                },
            )?)?;
        }
        writer.commit(|_| true)?;
        Ok(VerifiedIndex::open(root)?)
    }

    fn publish_with_event_sequences(
        &self,
        name: &str,
        specs: &[(usize, Vec<(u64, String)>)],
    ) -> Result<VerifiedIndex> {
        let root = self.data_root.join(format!("index-{name}"));
        let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
        for (source_index, records) in specs {
            let fixture_source = &self.sources[*source_index];
            writer.begin_source(fixture_source.source.clone())?;
            for (offset, (event_sequence, body)) in records.iter().enumerate() {
                writer.add_core_record(self.record_with_event_sequence(
                    *source_index,
                    u64::try_from(offset + 1)?,
                    *event_sequence,
                    body,
                )?)?;
            }
            let observation = SourceObservation::new(
                fixture_source.source.clone(),
                format!("fixture-{name}"),
                name.as_bytes().to_vec(),
            )?;
            let count = u64::try_from(records.len())?;
            writer.certify_source(CertifiedSource::certify(
                observation.clone(),
                observation,
                "fixture-parser-v1",
                [u8::try_from(*source_index + 1)?; 32],
                ScannedSourceCounts {
                    complete_records: count,
                    retained_records: count,
                    indexed_documents: count,
                    certified_bytes: count.saturating_mul(50),
                    ..ScannedSourceCounts::default()
                },
            )?)?;
        }
        writer.commit(|_| true)?;
        Ok(VerifiedIndex::open(root)?)
    }

    fn source_digest(&self, index: &VerifiedIndex, source_index: usize) -> Result<String> {
        let generation = SourceBackedSemanticGeneration::from_verified_index(index)?;
        generation
            .sources
            .iter()
            .find(|source| {
                source
                    .source
                    .exact_descriptor_eq(&self.sources[source_index].source)
            })
            .map(|source| source.aggregate.source_identity_digest().to_owned())
            .ok_or_else(|| anyhow!("missing source aggregate"))
    }
}

fn bodies(prefix: &str, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("{prefix} record {index}"))
        .collect()
}

fn merge(total: &mut SourceBackedSemanticOutcome, next: SourceBackedSemanticOutcome) {
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
}

fn reconcile_generation(
    store: &mut SemanticVectorStore,
    index: &VerifiedIndex,
    generation: &SourceBackedSemanticGeneration,
    builder: &mut CoreBuilder,
    embedder: &mut MarkerEmbedder,
) -> Result<SourceBackedSemanticOutcome> {
    let mut total = SourceBackedSemanticOutcome::default();
    for _ in 0..128 {
        let work_before = store.flat.work_stats();
        let mut outcome =
            store.reconcile_source_backed_generation(index, generation, builder, embedder)?;
        let work = store.flat.work_since(work_before);
        outcome.vectors_touched = work.vectors_touched;
        outcome.vector_bytes_touched = work.vector_bytes_touched;
        outcome.metadata_records_touched = work.metadata_records_touched;
        let ready = outcome.ready;
        merge(&mut total, outcome);
        if ready {
            total.work_remaining = false;
            return Ok(total);
        }
    }
    Err(anyhow!("semantic fixture did not converge"))
}

fn reconcile_all(
    store: &mut SemanticVectorStore,
    index: &VerifiedIndex,
    builder: &mut CoreBuilder,
    embedder: &mut MarkerEmbedder,
) -> Result<SourceBackedSemanticOutcome> {
    reconcile_generation(
        store,
        index,
        &SourceBackedSemanticGeneration::from_verified_index(index)?,
        builder,
        embedder,
    )
}

fn active_events(store: &SemanticVectorStore) -> Result<usize> {
    Ok(store
        .flat_pin_generation()?
        .map_or(0, |pinned| pinned.stats().active_events))
}

fn source_rows(store: &SemanticVectorStore, digest: &str) -> Result<Vec<(String, String, String)>> {
    Ok(store
        .flat
        .source_event_lookup(digest)
        .map_err(anyhow::Error::new)?
        .events()
        .iter()
        .map(|event| {
            (
                event.event_id.to_string(),
                event.source_text_hash.to_hex(),
                event.source_reconciliation_id.clone(),
            )
        })
        .collect())
}

#[derive(Debug, PartialEq)]
struct ProjectionSnapshot {
    sources: Vec<(
        String,
        Option<super::super::flat_segments::FlatSourceReceipt>,
    )>,
    events: Vec<ProjectionEvent>,
    chunks: Vec<ProjectionChunk>,
}

type ProjectionEvent = (Uuid, u64, String, u32, String, String, [u8; 32]);
type ProjectionChunk = (Uuid, u64, String, u32, u32, u32, Vec<f32>);

fn projection_snapshot(store: &SemanticVectorStore) -> Result<ProjectionSnapshot> {
    let sources = store
        .flat
        .source_states()
        .map_err(anyhow::Error::new)?
        .into_iter()
        .map(|state| (state.source_identity_digest, state.receipt))
        .collect();
    let Some(pin) = store.flat_pin_generation()? else {
        return Ok(ProjectionSnapshot {
            sources,
            events: Vec::new(),
            chunks: Vec::new(),
        });
    };
    let events = pin
        .active_events()
        .iter()
        .map(|event| {
            (
                event.event_id,
                event.seq,
                event.source_text_hash.to_hex(),
                event.chunk_count,
                event.source_identity_digest.clone(),
                event.source_reconciliation_id.clone(),
                event.stable_identity_hash,
            )
        })
        .collect();
    let mut chunks = pin
        .scan_segments()
        .iter()
        .flat_map(|segment| segment.chunks())
        .map(|chunk| {
            (
                chunk.event_id,
                chunk.seq,
                chunk.source_text_hash.to_hex(),
                chunk.chunk_index,
                chunk.start_char,
                chunk.end_char,
                chunk.vector.to_vec(),
            )
        })
        .collect::<Vec<_>>();
    chunks.sort_by_key(|chunk| (chunk.0, chunk.3));
    Ok(ProjectionSnapshot {
        sources,
        events,
        chunks,
    })
}

#[test]
fn semantic_generation_uses_exact_per_source_core_aggregates_without_candidate_totals() -> Result<()>
{
    let fixture = Fixture::new(2)?;
    let index = fixture.publish(
        "aggregate",
        &[(0, bodies("stable", 3)), (1, bodies("changed", 2))],
    )?;
    let generation = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    assert_eq!(SOURCE_CONTRACT_VERSION, 13);
    assert_eq!(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION, 22);
    assert_eq!(index.semantic_eligible_event_count()?, 5);
    assert_eq!(generation.core_generation_id, index.generation_id());
    assert_eq!(generation.sources.len(), 2);
    assert_eq!(
        generation
            .sources
            .iter()
            .map(|source| source.aggregate.indexed_documents())
            .sum::<u64>(),
        5
    );
    Ok(())
}

#[test]
fn semantic_generation_uses_verified_delta_manifest_identity() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let root = fixture.data_root.join("index-delta-manifest-identity");
    drop(fixture.publish_to_root(&root, "delta-base", &[(0, bodies("base", 1))])?);
    let index = fixture.publish_to_root(&root, "delta-next", &[(0, bodies("next", 2))])?;

    assert_ne!(index.manifest().generation_id()?, index.generation_id());
    let generation = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    assert_eq!(generation.core_generation_id, index.generation_id());
    Ok(())
}

#[test]
fn retrieval_excluded_events_never_enter_the_source_backed_semantic_projection() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let mut excluded = fixture.record(0, 1, "retrieval payload must not embed")?;
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    excluded.validate_contract()?;

    let root = fixture.data_root.join("index-retrieval-semantic-exclusion");
    let fixture_source = &fixture.sources[0];
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(fixture_source.source.clone())?;
    writer.add_core_record(excluded.clone())?;
    let observation = SourceObservation::new(
        fixture_source.source.clone(),
        "fixture-retrieval-semantic-exclusion",
        b"retrieval-semantic-exclusion".to_vec(),
    )?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 50,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open(root)?;
    let source_digest = index.manifest().core_record_aggregates[0]
        .source_identity_digest()
        .to_owned();

    assert_eq!(index.manifest().indexed_documents, 1);
    assert_eq!(index.semantic_eligible_event_count()?, 0);
    let semantic = index.core_semantic_event_page(None, 1)?;
    assert!(semantic.terminal);
    assert!(semantic.items.is_empty());

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 1);
    assert_eq!(outcome.records_embedded, 0);
    assert!(builder.calls.is_empty());
    assert_eq!(embedder.chunks, 0);
    assert_eq!(active_events(&store)?, 0);
    assert!(source_rows(&store, &source_digest)?.is_empty());
    Ok(())
}

#[test]
fn mixed_core_roles_build_and_pin_only_the_semantic_candidate() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let user = fixture.record_with_role(0, 1, "eligible user question", EventRole::User)?;
    let assistant =
        fixture.record_with_role(0, 2, "ineligible assistant answer", EventRole::Assistant)?;
    let root = fixture.data_root.join("index-mixed-core-roles");
    let fixture_source = &fixture.sources[0];
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(fixture_source.source.clone())?;
    writer.add_core_record(user.clone())?;
    writer.add_core_record(assistant)?;
    let observation = SourceObservation::new(
        fixture_source.source.clone(),
        "fixture-mixed-core-roles",
        b"mixed-core-roles".to_vec(),
    )?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 2,
            indexed_documents: 2,
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open(root)?;

    assert_eq!(index.manifest().indexed_documents, 2);
    let source_aggregate = &index.manifest().core_record_aggregates[0];
    assert_eq!(source_aggregate.indexed_documents(), 2);
    assert_eq!(index.semantic_eligible_event_count()?, 1);
    let core_page = index.core_source_event_page(&fixture_source.source, None, 2)?;
    assert!(core_page.terminal);
    assert_eq!(core_page.items.len(), 2);
    let semantic_page = index.core_semantic_event_page(None, 2)?;
    assert!(semantic_page.terminal);
    assert_eq!(semantic_page.eligible_total, 1);
    assert_eq!(semantic_page.items.len(), 1);
    assert_eq!(semantic_page.items[0].event_id, user.event_id);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 2);
    assert_eq!(outcome.records_embedded, 1);
    assert_eq!(outcome.records_filtered, 0);
    assert_eq!(builder.calls, vec![user.event_id.as_uuid()]);
    assert_eq!(embedder.chunks, 1);

    let pin = match store.source_backed_generation_pin_exact(index.generation_id(), 1)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!(
                "mixed Core generation did not return its exact pin"
            ));
        }
    };
    assert_eq!(pin.stats().active_events, 1);
    assert_eq!(pin.active_events()[0].event_id, user.event_id.as_uuid());
    Ok(())
}

#[test]
fn page_embedding_batches_multiple_documents_in_one_embedder_call() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let first = fixture.record_with_role(0, 1, "first eligible user question", EventRole::User)?;
    let second_body = "second eligible user question ".repeat(50);
    let second = fixture.record_with_role(0, 2, &second_body, EventRole::User)?;
    let root = fixture.data_root.join("index-page-embedding-batch");
    let fixture_source = &fixture.sources[0];
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(fixture_source.source.clone())?;
    writer.add_core_record(first.clone())?;
    writer.add_core_record(second.clone())?;
    let observation = SourceObservation::new(
        fixture_source.source.clone(),
        "fixture-page-embedding-batch",
        b"page-embedding-batch".to_vec(),
    )?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 2,
            indexed_documents: 2,
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open(root)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_embedded, 2);
    assert_eq!(outcome.records_reused, 0);
    assert_eq!(embedder.calls, 1);
    assert_eq!(embedder.chunks, 3);
    assert_eq!(
        builder.calls,
        vec![first.event_id.as_uuid(), second.event_id.as_uuid()]
    );
    assert_eq!(active_events(&store)?, 2);
    Ok(())
}

#[test]
fn role_policy_transition_rebuilds_semantic_state_without_reingesting_core() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let user = fixture.record_with_role(0, 1, "eligible user question", EventRole::User)?;
    let assistant = fixture.record_with_role(
        0,
        2,
        "newly eligible assistant answer",
        EventRole::Assistant,
    )?;
    let root = fixture.data_root.join("index-role-policy-transition");
    let fixture_source = &fixture.sources[0];
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    writer.begin_source(fixture_source.source.clone())?;
    writer.add_core_record(user.clone())?;
    writer.add_core_record(assistant.clone())?;
    let observation = SourceObservation::new(
        fixture_source.source.clone(),
        "fixture-role-policy-transition",
        b"role-policy-transition".to_vec(),
    )?;
    writer.certify_source(CertifiedSource::certify(
        observation.clone(),
        observation,
        "fixture-parser-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 2,
            retained_records: 2,
            indexed_documents: 2,
            certified_bytes: 100,
            ..ScannedSourceCounts::default()
        },
    )?)?;
    writer.commit(|_| true)?;
    let index = VerifiedIndex::open(&root)?;
    let core_generation_id = index.generation_id().to_owned();

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(builder.calls, vec![user.event_id.as_uuid()]);

    let mut assistant_policy = current_semantic_generation_policy();
    assistant_policy.candidate_roles = [SourceEventRole::Assistant];
    let revised =
        SourceBackedSemanticGeneration::from_verified_index_with_policy(&index, assistant_policy)?;
    builder.calls.clear();
    let rebuilt = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_decoded, 2);
    assert_eq!(rebuilt.records_embedded, 1);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(builder.calls, vec![assistant.event_id.as_uuid()]);

    assert_eq!(
        store
            .source_acknowledgement()?
            .expect("revised semantic policy acknowledgement")
            .semantic_policy_fingerprint,
        revised.semantic_policy_fingerprint
    );
    let pin = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("revised semantic policy did not publish its Flat generation"))?;
    assert_eq!(
        pin.active_events()[0].event_id,
        assistant.event_id.as_uuid()
    );
    assert_eq!(index.generation_id(), core_generation_id);
    assert_eq!(
        VerifiedIndex::open(root)?.generation_id(),
        core_generation_id
    );
    Ok(())
}

#[test]
fn exact_generation_pin_distinguishes_not_ready_empty_and_pinned() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let empty = fixture.publish("pin-empty", &[(0, Vec::new())])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    assert!(matches!(
        store.source_backed_generation_pin_exact(empty.generation_id(), 0)?,
        SourceBackedGenerationPin::NotReady
    ));

    reconcile_all(
        &mut store,
        &empty,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert!(matches!(
        store.source_backed_generation_pin_exact(empty.generation_id(), 0)?,
        SourceBackedGenerationPin::ReadyEmpty
    ));

    let populated = fixture.publish("pin-populated", &[(0, bodies("present", 1))])?;
    reconcile_all(
        &mut store,
        &populated,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let pin = match store.source_backed_generation_pin_exact(populated.generation_id(), 1)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("populated generation did not return a flat pin"));
        }
    };
    assert_eq!(pin.stats().active_events, 1);
    Ok(())
}

#[test]
fn four_event_source_work_is_independent_of_740k_equivalent_corpus() -> Result<()> {
    const PRODUCTION_EQUIVALENT_CORPUS_EVENTS: u64 = 740_000;
    let fixture = Fixture::new(2)?;
    let stable_large = bodies("large-stable", 100);
    let stable_small = bodies("small", 3);
    let mut appended_small = stable_small.clone();
    appended_small.push("small appended record".to_owned());
    let initial = fixture.publish(
        "complexity-a",
        &[(0, stable_large.clone()), (1, stable_small.clone())],
    )?;
    let unchanged = fixture.publish(
        "complexity-unchanged",
        &[(0, stable_large.clone()), (1, stable_small)],
    )?;
    let target = fixture.publish("complexity-b", &[(0, stable_large), (1, appended_small)])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    assert_eq!(
        reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?.records_decoded,
        103
    );
    let stable_digest = fixture.source_digest(&initial, 0)?;
    let stable_rows = source_rows(&store, &stable_digest)?;
    let small_digest = fixture.source_digest(&initial, 1)?;
    let small_rows = source_rows(&store, &small_digest)?;
    let flat_generation = store.flat_pin_generation()?.unwrap().generation();
    let embedded_chunks = embedder.chunks;

    builder.calls.clear();
    store.reset_flat_active_event_snapshot_count();
    let no_op = reconcile_all(&mut store, &unchanged, &mut builder, &mut embedder)?;
    assert_eq!(no_op.records_decoded, 0);
    assert!(builder.calls.is_empty());
    assert_eq!(embedder.chunks, embedded_chunks);
    assert_eq!(source_rows(&store, &stable_digest)?, stable_rows);
    assert_eq!(source_rows(&store, &small_digest)?, small_rows);
    assert_eq!(store.flat_active_event_snapshot_count(), 0);
    assert_eq!(
        store.flat_pin_generation()?.unwrap().generation(),
        flat_generation
    );

    builder.calls.clear();
    store.reset_flat_active_event_snapshot_count();
    let outcome = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 4);
    assert_eq!(builder.calls.len(), 4);
    assert_eq!(outcome.vectors_touched, 1);
    assert_eq!(outcome.vector_bytes_touched, SEMANTIC_DIMENSIONS as u64 * 4);
    assert!(outcome.metadata_records_touched < 64);
    // The untouched source contributes zero records, vectors, and bytes to the
    // measured refresh. The same bound therefore applies at the production
    // 740k-event corpus size; only manifest segment descriptors are inspected.
    assert!(outcome.vectors_touched < PRODUCTION_EQUIVALENT_CORPUS_EVENTS);
    assert_eq!(source_rows(&store, &stable_digest)?, stable_rows);
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        0,
        "one changed source must not materialize the global event catalog"
    );
    assert_eq!(active_events(&store)?, 104);
    assert_eq!(
        store.flat_pin_generation()?.unwrap().generation(),
        flat_generation + 2,
        "one changed page publishes combined vector/authority state, then its source receipt"
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(target.generation_id(), 104)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    Ok(())
}

#[test]
fn sequence_only_core_change_updates_authority_without_embedding() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish_with_event_sequences(
        "sequence-a",
        &[(0, vec![(1, "same semantic body".to_owned())])],
    )?;
    let target = fixture.publish_with_event_sequences(
        "sequence-b",
        &[(0, vec![(91, "same semantic body".to_owned())])],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let embedded_before = embedder.chunks;

    let outcome = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, 1);
    assert_eq!(outcome.records_reused, 1);
    assert_eq!(outcome.records_embedded, 0);
    assert_eq!(outcome.vectors_touched, 0);
    assert_eq!(outcome.vector_bytes_touched, 0);
    assert!(outcome.metadata_records_touched < 32);
    assert_eq!(embedder.chunks, embedded_before);
    assert!(matches!(
        store.source_backed_generation_pin_exact(initial.generation_id(), 1)?,
        SourceBackedGenerationPin::NotReady
    ));
    let pin = match store.source_backed_generation_pin_exact(target.generation_id(), 1)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("sequence-only target did not produce an exact pin"));
        }
    };
    let chunk = pin
        .scan_segments()
        .iter()
        .flat_map(|segment| segment.chunks())
        .next()
        .ok_or_else(|| anyhow!("sequence-only target lost its vector"))?;
    assert_eq!(chunk.seq, 91);
    Ok(())
}

#[test]
fn multi_page_reconciliation_constructs_one_flat_view() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let record_count = MAX_SOURCE_EVENT_PAGE_ITEMS + 4;
    let initial = fixture.publish("view-a", &[(0, bodies("initial", record_count))])?;
    let target = fixture.publish("view-b", &[(0, bodies("rewritten", record_count))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let segments_before = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("initial reconciliation did not publish a flat generation"))?
        .stats()
        .segment_count;

    store.reset_flat_active_event_snapshot_count();
    let outcome = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_decoded, record_count);
    assert_eq!(outcome.records_embedded, record_count);
    assert_eq!(outcome.records_reused, 0);
    let pin = store
        .flat_pin_generation()?
        .ok_or_else(|| anyhow!("multi-page reconciliation did not publish a flat generation"))?;
    assert_eq!(pin.stats().active_events, record_count);
    assert!(
        pin.stats().segment_count
            <= segments_before + record_count.div_ceil(MAX_SOURCE_EVENT_PAGE_ITEMS),
        "one reconciliation may retain at most one durable delta per bounded Core page"
    );
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        0,
        "changed Core pages must not materialize the global event catalog"
    );
    assert_eq!(
        store.flat.active_generation_load_count(),
        1,
        "multi-page publication must load current stats only at its terminal boundary"
    );
    Ok(())
}

#[test]
fn multipage_source_scales_independently_of_hundreds_of_global_descriptors() -> Result<()> {
    const UNRELATED_SOURCES: usize = 128;
    let fixture = Fixture::new(UNRELATED_SOURCES + 1)?;
    let changed_records = MAX_SOURCE_EVENT_PAGE_ITEMS * 2 + 17;
    let mut initial_specs = (0..UNRELATED_SOURCES)
        .map(|source| (source, bodies(&format!("unrelated-{source}"), 1)))
        .collect::<Vec<_>>();
    initial_specs.push((UNRELATED_SOURCES, bodies("scaled-initial", changed_records)));
    let mut target_specs = initial_specs[..UNRELATED_SOURCES].to_vec();
    target_specs.push((
        UNRELATED_SOURCES,
        bodies("scaled-rewritten", changed_records),
    ));
    let initial = fixture.publish("scaled-global-a", &initial_specs)?;
    let target = fixture.publish("scaled-global-b", &target_specs)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(
        &mut store,
        &initial,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert!(
        store
            .flat
            .active_stats()
            .map_err(anyhow::Error::new)?
            .segment_count
            >= UNRELATED_SOURCES * 2,
        "fixture must independently scale the unrelated global descriptor set"
    );

    store.reset_flat_active_event_snapshot_count();
    let outcome = reconcile_all(
        &mut store,
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert_eq!(outcome.records_decoded, changed_records);
    assert_eq!(store.flat.global_manifest_parse_count(), 1);
    assert_eq!(store.flat.global_manifest_serialization_count(), 1);
    assert_eq!(store.flat.source_publication_count(), 1);
    assert_eq!(store.flat.global_segment_directory_scan_count(), 1);
    assert!(
        store.flat.staging_peak_event_records() <= u64::try_from(MAX_SOURCE_EVENT_PAGE_ITEMS)?,
        "source staging retained more than one bounded Core page"
    );
    Ok(())
}

#[test]
fn multi_page_restart_reconstructs_one_view_for_remaining_pages() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let record_count = MAX_SOURCE_EVENT_PAGE_ITEMS + 4;
    let index = fixture.publish("view-restart", &[(0, bodies("restart", record_count))])?;
    let mut clean = SemanticVectorStore::open(&fixture.data_root.join("semantic-clean-page"))?;
    reconcile_all(
        &mut clean,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let expected = projection_snapshot(&clean)?;
    let mut builder = CoreBuilder {
        fail_after: Some(MAX_SOURCE_EVENT_PAGE_ITEMS),
        ..CoreBuilder::default()
    };
    let mut embedder = MarkerEmbedder::default();
    {
        let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
        let error = store
            .reconcile_source_backed_index(&index, &mut builder, &mut embedder)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("forced Core projection interruption"));
        assert_eq!(active_events(&store)?, 0);
    }

    builder.fail_after = None;
    builder.calls.clear();
    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
    assert!(restarted.flat_pin_generation()?.is_none());
    restarted.reset_flat_active_event_snapshot_count();
    let resumed = reconcile_all(&mut restarted, &index, &mut builder, &mut embedder)?;
    assert_eq!(resumed.records_decoded, 4);
    assert_eq!(resumed.records_embedded, 4);
    assert_eq!(resumed.vectors_touched, 4);
    assert_eq!(
        resumed.vector_bytes_touched,
        4 * SEMANTIC_DIMENSIONS as u64 * 4
    );
    assert_eq!(active_events(&restarted)?, record_count);
    assert_eq!(
        restarted.flat_active_event_snapshot_count(),
        0,
        "restart must reconstruct source-local staging without a global snapshot"
    );
    assert_eq!(
        restarted.flat.active_generation_load_count(),
        1,
        "restart must perform one terminal current-generation load"
    );
    assert!(
        restarted
            .flat_pin_generation()?
            .ok_or_else(|| anyhow!("resumed reconciliation lost its flat generation"))?
            .stats()
            .segment_count
            <= record_count.div_ceil(MAX_SOURCE_EVENT_PAGE_ITEMS) + 1,
        "the staged pages and source receipt retain bounded scoped segments"
    );
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn restart_after_source_receipt_finalization_is_exact() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish("receipt-fault-a", &[(0, bodies("before", 3))])?;
    let target = fixture.publish("receipt-fault-b", &[(0, bodies("after", 4))])?;
    let mut clean = SemanticVectorStore::open(&fixture.data_root.join("semantic-clean-receipt"))?;
    reconcile_all(
        &mut clean,
        &initial,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    reconcile_all(
        &mut clean,
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let expected = projection_snapshot(&clean)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    store.flat.fail_after_source_finalization_once();
    let error = store
        .reconcile_source_backed_index(&target, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source finalization"));
    drop(store);

    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(&mut restarted, &target, &mut builder, &mut embedder)?;
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn restart_after_removed_source_finalization_is_exact() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let initial = fixture.publish(
        "remove-fault-a",
        &[(0, bodies("retained", 2)), (1, bodies("removed", 3))],
    )?;
    let target = fixture.publish("remove-fault-b", &[(0, bodies("retained", 2))])?;
    let mut clean = SemanticVectorStore::open(&fixture.data_root.join("semantic-clean-removal"))?;
    reconcile_all(
        &mut clean,
        &initial,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    reconcile_all(
        &mut clean,
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let expected = projection_snapshot(&clean)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    store.flat.fail_after_source_finalization_once();
    let error = store
        .reconcile_source_backed_index(&target, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source finalization"));
    drop(store);

    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(&mut restarted, &target, &mut builder, &mut embedder)?;
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn restart_after_final_acknowledgement_is_exact() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("ack-fault", &[(0, bodies("ack", 3))])?;
    let mut clean = SemanticVectorStore::open(&fixture.data_root.join("semantic-clean-ack"))?;
    reconcile_all(
        &mut clean,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    let expected = projection_snapshot(&clean)?;

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    store.flat.fail_after_source_acknowledgement_once();
    let error = store
        .reconcile_source_backed_index(
            &index,
            &mut CoreBuilder::default(),
            &mut MarkerEmbedder::default(),
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source acknowledgement"));
    drop(store);

    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(
        &mut restarted,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert_eq!(projection_snapshot(&restarted)?, expected);
    Ok(())
}

#[test]
fn threshold_compaction_rewrites_only_the_changed_source() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let stable = bodies("stable-threshold", 5);
    let initial = fixture.publish(
        "threshold-0",
        &[(0, stable.clone()), (1, bodies("mutable-threshold-0", 2))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let stable_digest = fixture.source_digest(&initial, 0)?;
    let stable_rows = source_rows(&store, &stable_digest)?;

    let mut threshold = SourceBackedSemanticOutcome::default();
    for revision in 1..=14 {
        let target = fixture.publish(
            &format!("threshold-{revision}"),
            &[
                (0, stable.clone()),
                (1, bodies(&format!("mutable-threshold-{revision}"), 2)),
            ],
        )?;
        threshold = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    }
    assert_eq!(threshold.records_decoded, 2);
    assert_eq!(threshold.records_embedded, 2);
    assert_eq!(
        threshold.vectors_touched, 4,
        "two embedded vectors plus two source-local compacted vectors"
    );
    assert_eq!(
        threshold.vector_bytes_touched,
        4 * SEMANTIC_DIMENSIONS as u64 * 4
    );
    assert_eq!(source_rows(&store, &stable_digest)?, stable_rows);
    assert_eq!(
        store
            .flat
            .active_stats()
            .map_err(anyhow::Error::new)?
            .segment_count,
        3,
        "stable source segments plus one compacted changed-source segment"
    );
    Ok(())
}

#[test]
fn append_rewrite_and_removal_touch_only_owned_source() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let initial = fixture.publish(
        "lifecycle-a",
        &[(0, bodies("retained", 5)), (1, bodies("mutable", 3))],
    )?;
    let append = fixture.publish(
        "lifecycle-b",
        &[(0, bodies("retained", 5)), (1, bodies("mutable", 4))],
    )?;
    let rewrite = fixture.publish(
        "lifecycle-c",
        &[
            (0, bodies("retained", 5)),
            (
                1,
                vec!["rewritten one".to_owned(), "rewritten two".to_owned()],
            ),
        ],
    )?;
    let removed = fixture.publish("lifecycle-d", &[(0, bodies("retained", 5))])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let retained_digest = fixture.source_digest(&initial, 0)?;
    let retained_rows = source_rows(&store, &retained_digest)?;

    let appended = reconcile_all(&mut store, &append, &mut builder, &mut embedder)?;
    assert_eq!(appended.records_decoded, 4);
    assert_eq!(appended.records_reused, 3);
    assert_eq!(appended.records_embedded, 1);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 9);

    let rewritten = reconcile_all(&mut store, &rewrite, &mut builder, &mut embedder)?;
    assert_eq!(rewritten.records_decoded, 2);
    assert_eq!(rewritten.records_embedded, 2);
    assert!(rewritten.deleted_chunks >= 2);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 7);

    let removed_outcome = reconcile_all(&mut store, &removed, &mut builder, &mut embedder)?;
    assert_eq!(removed_outcome.records_decoded, 0);
    assert!(removed_outcome.deleted_chunks >= 2);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 5);
    Ok(())
}

#[test]
fn large_multipage_lifecycle_replays_each_source_catalog_once() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let mutable_count = MAX_SOURCE_EVENT_PAGE_ITEMS * 2 + 17;
    let retained = bodies("large-retained", 3);
    let initial = fixture.publish(
        "large-linear-a",
        &[
            (0, retained.clone()),
            (1, bodies("large-mutable", mutable_count)),
        ],
    )?;
    let appended = fixture.publish(
        "large-linear-b",
        &[
            (0, retained.clone()),
            (1, bodies("large-mutable", mutable_count + 1)),
        ],
    )?;
    let rewritten = fixture.publish(
        "large-linear-c",
        &[
            (0, retained.clone()),
            (1, bodies("large-rewritten", mutable_count + 1)),
        ],
    )?;
    let removed = fixture.publish("large-linear-d", &[(0, retained)])?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let retained_digest = fixture.source_digest(&initial, 0)?;
    let retained_rows = source_rows(&store, &retained_digest)?;

    store.reset_flat_active_event_snapshot_count();
    let append = reconcile_all(&mut store, &appended, &mut builder, &mut embedder)?;
    assert_eq!(append.records_decoded, mutable_count + 1);
    assert_eq!(append.records_reused, mutable_count);
    assert_eq!(append.records_embedded, 1);
    assert_eq!(store.flat.source_catalog_load_count(), 0);
    assert_eq!(store.flat.source_catalog_records_replayed(), 0);
    assert_eq!(store.flat.source_publication_count(), 1);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);

    store.reset_flat_active_event_snapshot_count();
    let rewrite = reconcile_all(&mut store, &rewritten, &mut builder, &mut embedder)?;
    assert_eq!(rewrite.records_decoded, mutable_count + 1);
    assert_eq!(rewrite.records_reused, 0);
    assert_eq!(rewrite.records_embedded, mutable_count + 1);
    assert_eq!(store.flat.source_catalog_load_count(), 0);
    assert_eq!(store.flat.source_catalog_records_replayed(), 0);
    assert_eq!(store.flat.source_publication_count(), 1);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);

    store.reset_flat_active_event_snapshot_count();
    let removal = reconcile_all(&mut store, &removed, &mut builder, &mut embedder)?;
    assert_eq!(removal.records_decoded, 0);
    assert_eq!(removal.deleted_chunks, mutable_count + 1);
    assert_eq!(store.flat.source_catalog_load_count(), 0);
    assert_eq!(store.flat.source_catalog_records_replayed(), 0);
    assert_eq!(store.flat.source_publication_count(), 1);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 3);
    Ok(())
}

#[test]
fn frontier_commit_manifest_rollback_replays_sequence_and_same_id_rewrite() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let initial = fixture.publish_with_event_sequences(
        "rollback-initial",
        &[(0, vec![(1, "same semantic body".to_owned())])],
    )?;
    let sequence_only = fixture.publish_with_event_sequences(
        "rollback-sequence",
        &[(0, vec![(91, "same semantic body".to_owned())])],
    )?;
    let rewrite = fixture.publish_with_event_sequences(
        "rollback-rewrite",
        &[(0, vec![(92, "rewritten semantic body".to_owned())])],
    )?;
    let source_digest = fixture.source_digest(&initial, 0)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;
    let embedded_initial = embedder.chunks;
    store.flat.fail_after_source_frontier_commit_once();
    let error = store
        .reconcile_source_backed_index(&sequence_only, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source frontier commit"));
    assert_eq!(embedder.chunks, embedded_initial);
    let committed = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("sequence-only fault lost its committed frontier"))?
        .flat_publication;
    assert_eq!(store.flat.rollback_active_manifest()?, committed);
    drop(store);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let replayed = reconcile_all(&mut store, &sequence_only, &mut builder, &mut embedder)?;
    assert_eq!(replayed.records_decoded, 1);
    assert_eq!(replayed.records_reused, 0);
    assert_eq!(replayed.records_embedded, 1);
    assert_eq!(embedder.chunks, embedded_initial + 1);
    let sequence_pin =
        match store.source_backed_generation_pin_exact(sequence_only.generation_id(), 1)? {
            SourceBackedGenerationPin::Ready(pin) => pin,
            SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
                return Err(anyhow!(
                    "sequence-only rollback replay did not become ready"
                ));
            }
        };
    assert_eq!(
        sequence_pin
            .scan_segments()
            .iter()
            .flat_map(|segment| segment.chunks())
            .next()
            .ok_or_else(|| anyhow!("sequence-only rollback replay lost its vector"))?
            .seq,
        91
    );
    let sequence_rows = source_rows(&store, &source_digest)?;

    store.flat.fail_after_source_frontier_commit_once();
    let error = store
        .reconcile_source_backed_index(&rewrite, &mut builder, &mut embedder)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("injected failure after semantic source frontier commit"));
    assert_eq!(embedder.chunks, embedded_initial + 2);
    let committed = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("same-ID rewrite fault lost its committed frontier"))?
        .flat_publication;
    assert_eq!(store.flat.rollback_active_manifest()?, committed);
    drop(store);

    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let replayed = reconcile_all(&mut store, &rewrite, &mut builder, &mut embedder)?;
    assert_eq!(replayed.records_decoded, 1);
    assert_eq!(replayed.records_embedded, 1);
    assert_eq!(embedder.chunks, embedded_initial + 3);
    assert_ne!(source_rows(&store, &source_digest)?, sequence_rows);
    let rewrite_pin = match store.source_backed_generation_pin_exact(rewrite.generation_id(), 1)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("same-ID rollback replay did not become ready"));
        }
    };
    assert_eq!(
        rewrite_pin
            .scan_segments()
            .iter()
            .flat_map(|segment| segment.chunks())
            .next()
            .ok_or_else(|| anyhow!("same-ID rollback replay lost its vector"))?
            .seq,
        92
    );
    Ok(())
}

include!("tests/generation_recovery.rs");
