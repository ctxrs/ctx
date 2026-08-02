use std::{collections::HashSet, fs, path::PathBuf, time::Instant};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, EventRole, EventType, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, StableEntityId, TypedKey,
};
use ctx_history_index::{CoreEventRecord, GenerationWriter, VerifiedIndex, WriterOptions};
use tempfile::TempDir;

use super::*;
use crate::semantic::vector_store_search::scan_exact_generation;

const TAIL_TOKEN: &str = "semantic-tail-token-7f0d";

#[derive(Default)]
struct CoreBuilder {
    calls: Vec<Uuid>,
    fail_on: HashSet<Uuid>,
}

impl SourceBackedSemanticDocumentBuilder for CoreBuilder {
    fn build_document(
        &mut self,
        record: &CoreEventRecord,
    ) -> Result<Option<SemanticEventDocument>> {
        self.calls.push(record.event_id.as_uuid());
        if self.fail_on.contains(&record.event_id.as_uuid()) {
            return Err(anyhow!("forced Core projection interruption"));
        }
        let text = record.core_record.content.meaningful_text().to_owned();
        if text.is_empty() {
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
            agent_type: None,
            session_is_primary: Some(record.is_primary),
            cwd: record.cwd.clone(),
            record_title: None,
            record_kind: Some("message".to_owned()),
            record_workspace: record.workspace.clone(),
            text,
        }))
    }
}

#[derive(Default)]
struct MarkerEmbedder {
    chunks: usize,
}

impl SourceBackedSemanticEmbedder for MarkerEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
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
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
        let mut sources = Vec::new();
        for source_index in 0..source_count {
            let anchor = u8::try_from(source_index + 1)?;
            let source = SourceKey::derive(
                "codex",
                "codex_session_jsonl_tree",
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
        let fixture_source = &self.sources[source_index];
        let event_id = derive_event_id(EventIdentityInput {
            source: &fixture_source.source,
            session_id: fixture_source.session_id,
            logical_item_kind: "message",
            native_item_key: &NativeItemKey::native_id("message", TypedKey::U64(sequence))?,
            subrecord_selector: None,
        })?;
        let mut record = CoreRecord::new_selected(
            event_id,
            fixture_source.session_id,
            fixture_source.session_id,
            fixture_source.source.clone(),
            sequence,
            "message",
            "primary",
            true,
            "semantic-source-projection-test-v1",
            body,
        )?;
        record.provider_session_id = Some(format!("fixture-session-{source_index}"));
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.role = Some("user".to_owned());
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.workspace = Some("/workspace".to_owned());
        record.cwd = Some("/workspace".to_owned());
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
        let mut writer = GenerationWriter::open(&root, WriterOptions::default())?;
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
    total.records_read = total.records_read.saturating_add(next.records_read);
    total.records_scanned = total.records_scanned.saturating_add(next.records_scanned);
    total.records_embedded = total.records_embedded.saturating_add(next.records_embedded);
    total.records_reused = total.records_reused.saturating_add(next.records_reused);
    total.records_filtered = total.records_filtered.saturating_add(next.records_filtered);
    total.invalidated_chunks = total
        .invalidated_chunks
        .saturating_add(next.invalidated_chunks);
    total.deleted_chunks = total.deleted_chunks.saturating_add(next.deleted_chunks);
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
        let outcome =
            store.reconcile_source_backed_generation(index, generation, builder, embedder)?;
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
    let mut statement = store.conn.prepare(
        "SELECT event_id, source_text_sha256, source_reconciliation_id
         FROM semantic_source_documents WHERE source_identity_digest = ?1 ORDER BY event_id",
    )?;
    let rows = statement.query_map([digest], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[test]
fn semantic_generation_mirrors_exact_per_source_core_aggregates() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let index = fixture.publish(
        "aggregate",
        &[(0, bodies("stable", 3)), (1, bodies("changed", 2))],
    )?;
    let generation = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    assert_eq!(SOURCE_CONTRACT_VERSION, 7);
    assert_eq!(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION, 15);
    assert_eq!(generation.semantic_documents, 5);
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
fn complexity_oracle_reads_only_changed_source_records() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let stable_large = bodies("large-stable", 100);
    let stable_small = bodies("small", 3);
    let initial = fixture.publish(
        "complexity-a",
        &[(0, stable_large.clone()), (1, stable_small.clone())],
    )?;
    let unchanged = fixture.publish(
        "complexity-unchanged",
        &[(0, stable_large.clone()), (1, stable_small)],
    )?;
    let target = fixture.publish(
        "complexity-b",
        &[(0, stable_large), (1, bodies("small-appended", 4))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    assert_eq!(
        reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?.records_read,
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
    assert_eq!(no_op.records_read, 0);
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
    assert_eq!(outcome.records_read, 4);
    assert_eq!(outcome.records_scanned, 4);
    assert_eq!(builder.calls.len(), 4);
    assert_eq!(source_rows(&store, &stable_digest)?, stable_rows);
    assert_eq!(store.flat_active_event_snapshot_count(), 0);
    assert_eq!(active_events(&store)?, 104);
    assert_eq!(
        store.flat_pin_generation()?.unwrap().generation(),
        flat_generation + 1,
        "one changed page must publish one flat mutation"
    );
    assert!(matches!(
        store.source_backed_generation_pin_exact(target.generation_id(), 104)?,
        SourceBackedGenerationPin::Ready(_)
    ));
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
    assert_eq!(appended.records_read, 4);
    assert_eq!(appended.records_reused, 3);
    assert_eq!(appended.records_embedded, 1);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 9);

    let rewritten = reconcile_all(&mut store, &rewrite, &mut builder, &mut embedder)?;
    assert_eq!(rewritten.records_read, 2);
    assert_eq!(rewritten.records_embedded, 2);
    assert!(rewritten.deleted_chunks >= 2);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 7);

    let removed_outcome = reconcile_all(&mut store, &removed, &mut builder, &mut embedder)?;
    assert_eq!(removed_outcome.records_read, 0);
    assert!(removed_outcome.deleted_chunks >= 2);
    assert_eq!(source_rows(&store, &retained_digest)?, retained_rows);
    assert_eq!(active_events(&store)?, 5);
    Ok(())
}

#[test]
fn crash_restart_replays_flat_publication_gap_idempotently() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = fixture.publish("crash", &[(0, bodies("crash", 6))])?;
    let mut builder = CoreBuilder::default();
    builder.fail_on.insert(fixture.event_id(0, 4)?);
    let mut embedder = MarkerEmbedder::default();
    let published_generation = {
        let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
        let transition =
            store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
        assert_eq!(transition.records_read, 0);
        assert!(transition.work_remaining);
        let error = store
            .reconcile_source_backed_index(&index, &mut builder, &mut embedder)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("forced Core projection interruption"));
        assert_eq!(active_events(&store)?, 0);

        // Simulate a crash after the page's flat generation is durable but
        // before ownership rows and the source frontier commit together.
        let generation = SourceBackedSemanticGeneration::from_verified_index(&index)?;
        let source = &generation.sources[0];
        let page =
            index.core_source_event_page(&source.source, None, MAX_SOURCE_EVENT_PAGE_ITEMS)?;
        let mut page_builder = CoreBuilder::default();
        let mut page_embedder = MarkerEmbedder::default();
        let mut replacements = Vec::new();
        for record in &page.items {
            let document = page_builder
                .build_document(record)?
                .ok_or_else(|| anyhow!("fixture record was unexpectedly filtered"))?;
            let source_text = semantic_source_text(&document.text);
            let source_text_sha256 = semantic_document_hash(
                &document,
                &source_text,
                &generation.semantic_policy_fingerprint,
            );
            let chunks = semantic_chunks_for_document(&document, &source_text, &source_text_sha256);
            let embeddings = page_embedder.embed_chunks(&chunks)?;
            replacements.extend(chunks.into_iter().zip(embeddings));
        }
        store.publish_chunk_replacements(&replacements, &[])?;
        assert_eq!(active_events(&store)?, 6);
        store.flat_pin_generation()?.unwrap().generation()
    };

    builder.fail_on.clear();
    builder.calls.clear();
    let mut restarted = SemanticVectorStore::open(&fixture.semantic_path)?;
    let outcome = reconcile_all(&mut restarted, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_read, 6);
    assert_eq!(outcome.records_reused, 6);
    assert_eq!(outcome.records_embedded, 0);
    assert_eq!(builder.calls.len(), 6);
    assert_eq!(active_events(&restarted)?, 6);
    assert_eq!(
        restarted.flat_pin_generation()?.unwrap().generation(),
        published_generation
    );
    let no_op = restarted.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert!(no_op.ready);
    assert_eq!(no_op.records_read, 0);
    Ok(())
}

#[test]
fn core_advance_mid_catch_up_never_pins_mixed_generation() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let initial = fixture.publish(
        "advance-a",
        &[(0, bodies("stable", 2)), (1, vec!["version a".to_owned()])],
    )?;
    let middle = fixture.publish(
        "advance-b",
        &[(0, bodies("stable", 2)), (1, vec!["version b".to_owned()])],
    )?;
    let newest = fixture.publish(
        "advance-c",
        &[
            (0, bodies("stable-new", 2)),
            (1, vec!["version a".to_owned()]),
        ],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

    let transition = store.reconcile_source_backed_index(&middle, &mut builder, &mut embedder)?;
    assert_eq!(transition.records_read, 0);
    let partial = store.reconcile_source_backed_index(&middle, &mut builder, &mut embedder)?;
    assert_eq!(partial.records_read, 1);
    assert!(matches!(
        store.source_backed_generation_pin_exact(initial.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(middle.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));

    let newer_transition =
        store.reconcile_source_backed_index(&newest, &mut builder, &mut embedder)?;
    assert_eq!(newer_transition.records_read, 0);
    let newer_partial =
        store.reconcile_source_backed_index(&newest, &mut builder, &mut embedder)?;
    assert_eq!(newer_partial.records_read, 2);
    assert!(matches!(
        store.source_backed_generation_pin_exact(newest.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));
    let completed = reconcile_all(&mut store, &newest, &mut builder, &mut embedder)?;
    assert_eq!(completed.records_read, 1);
    assert!(matches!(
        store.source_backed_generation_pin_exact(newest.generation_id(), 3)?,
        SourceBackedGenerationPin::Ready(_)
    ));
    assert!(matches!(
        store.source_backed_generation_pin_exact(middle.generation_id(), 3)?,
        SourceBackedGenerationPin::NotReady
    ));
    Ok(())
}

#[test]
fn policy_model_or_chunk_revision_forces_full_rebuild() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let index = fixture.publish(
        "revision",
        &[(0, bodies("first", 3)), (1, bodies("second", 2))],
    )?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    let chunks_before = embedder.chunks;

    let mut revised = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    revised.semantic_policy_fingerprint = "f".repeat(64);
    builder.calls.clear();
    let rebuilt = reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_read, 5);
    assert_eq!(rebuilt.records_embedded, 5);
    assert_eq!(builder.calls.len(), 5);
    assert!(embedder.chunks > chunks_before);
    Ok(())
}

#[test]
fn policy_rebuild_persists_linear_source_traversal_across_restart() -> Result<()> {
    let fixture = Fixture::new(8)?;
    let specs = (0..8)
        .map(|source| (source, bodies(&format!("source-{source}"), 1)))
        .collect::<Vec<_>>();
    let index = fixture.publish("linear-rebuild", &specs)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    reconcile_all(
        &mut store,
        &index,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;

    let mut revised = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    revised.semantic_policy_fingerprint = "e".repeat(64);
    let first_digest = revised.sources[0]
        .aggregate
        .source_identity_digest()
        .to_owned();
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();

    let transition =
        store.reconcile_source_backed_generation(&index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(transition.records_read, 0);
    let first_page =
        store.reconcile_source_backed_generation(&index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(first_page.records_read, 1);
    let first_finish =
        store.reconcile_source_backed_generation(&index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(first_finish.records_read, 0);
    let frontier = store
        .source_frontier()?
        .ok_or_else(|| anyhow!("policy rebuild lost its traversal frontier"))?;
    assert_eq!(
        frontier.source_traversal_phase,
        SourceTraversalPhase::ReconcilingSources
    );
    assert_eq!(
        frontier.source_traversal_after_identity_digest.as_deref(),
        Some(first_digest.as_str())
    );

    drop(store);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let resumed =
        store.reconcile_source_backed_generation(&index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(resumed.records_read, 1);
    assert_eq!(builder.calls.len(), 2);
    assert_ne!(builder.calls[0], builder.calls[1]);

    let remainder =
        reconcile_generation(&mut store, &index, &revised, &mut builder, &mut embedder)?;
    assert_eq!(remainder.records_read, 6);
    assert_eq!(builder.calls.len(), 8);
    assert_eq!(
        builder.calls.iter().copied().collect::<HashSet<_>>().len(),
        8
    );
    Ok(())
}

#[test]
fn control_reset_retires_unowned_flat_vectors_before_rebuild() -> Result<()> {
    let fixture = Fixture::new(2)?;
    let initial = fixture.publish(
        "reset-a",
        &[(0, bodies("retained", 3)), (1, bodies("removed", 2))],
    )?;
    let target = fixture.publish("reset-b", &[(0, bodies("retained", 3))])?;
    let removed_event = fixture.event_id(1, 1)?;
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    reconcile_all(&mut store, &initial, &mut builder, &mut embedder)?;

    drop(store);
    let control = rusqlite::Connection::open(fixture.semantic_path.join("state.sqlite"))?;
    control.pragma_update(None, "user_version", 2)?;
    drop(control);
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;

    let cleanup = store.reconcile_source_backed_index(
        &target,
        &mut CoreBuilder::default(),
        &mut MarkerEmbedder::default(),
    )?;
    assert_eq!(cleanup.records_read, 0);
    assert_eq!(cleanup.deleted_chunks, 5);
    assert!(cleanup.work_remaining);

    builder.calls.clear();
    let rebuilt = reconcile_all(&mut store, &target, &mut builder, &mut embedder)?;
    assert_eq!(rebuilt.records_read, 3);
    assert_eq!(rebuilt.records_embedded, 3);
    assert_eq!(active_events(&store)?, 3);
    assert!(store
        .flat_pin_generation()?
        .unwrap()
        .active_events()
        .iter()
        .all(|event| event.event_id != removed_event));
    Ok(())
}

#[test]
fn control_filter_and_full_tail_remain_generation_exact() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let body = format!("{} {TAIL_TOKEN}", "prefix ".repeat(2_500));
    let index = fixture.publish(
        "content",
        &[(
            0,
            vec![
                "<environment_context>control</environment_context>".to_owned(),
                body,
            ],
        )],
    )?;
    let page = index.core_semantic_event_page(None, 2)?;
    let tail_event = page
        .items
        .iter()
        .find(|item| {
            item.core_record
                .content
                .meaningful_text()
                .ends_with(TAIL_TOKEN)
        })
        .ok_or_else(|| anyhow!("missing complete tail content"))?
        .event_id
        .as_uuid();
    let mut store = SemanticVectorStore::open(&fixture.semantic_path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = reconcile_all(&mut store, &index, &mut builder, &mut embedder)?;
    assert_eq!(outcome.records_filtered, 1);
    assert_eq!(active_events(&store)?, 1);
    let pin = match store.source_backed_generation_pin_exact(index.generation_id(), 2)? {
        SourceBackedGenerationPin::Ready(pin) => pin,
        SourceBackedGenerationPin::NotReady | SourceBackedGenerationPin::ReadyEmpty => {
            return Err(anyhow!("nonempty reconciled generation was not pinned"));
        }
    };
    let mut query = vec![0.0; SEMANTIC_DIMENSIONS];
    query[0] = 1.0;
    let search = scan_exact_generation(&pin, &query, 1, None, Instant::now())?;
    assert_eq!(search.hits[0].event_id, tail_event);
    for directory in [
        fixture.semantic_path.clone(),
        fixture.semantic_path.join("flat_segments"),
    ] {
        if directory.exists() {
            for entry in fs::read_dir(directory)? {
                let path = entry?.path();
                if path.is_file() {
                    let bytes = fs::read(path)?;
                    assert!(!bytes
                        .windows(TAIL_TOKEN.len())
                        .any(|window| window == TAIL_TOKEN.as_bytes()));
                }
            }
        }
    }
    Ok(())
}
