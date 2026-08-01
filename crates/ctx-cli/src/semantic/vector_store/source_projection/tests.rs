use std::{collections::HashSet, fs, time::Instant};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CoreRecord,
    EventIdentityInput, EventRole, EventType, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{
    CoreEventRecord, EventRecord, GenerationWriter, VerifiedIndex, WriterOptions,
};
use tempfile::TempDir;

use super::*;
use crate::semantic::vector_store_search::scan_exact_generation;

const TAIL_TOKEN: &str = "semantic-tail-token-7f0d";

fn active_counts(store: &SemanticVectorStore) -> Result<(usize, usize)> {
    Ok(store.flat_pin_generation()?.map_or((0, 0), |pinned| {
        (pinned.stats().active_events, pinned.stats().active_chunks)
    }))
}

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
    maximum_batch: usize,
}

impl SourceBackedSemanticEmbedder for MarkerEmbedder {
    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        self.chunks = self.chunks.saturating_add(chunks.len());
        self.maximum_batch = self.maximum_batch.max(chunks.len());
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

struct Fixture {
    _temp: TempDir,
    data_root: std::path::PathBuf,
    index_root: std::path::PathBuf,
    path: std::path::PathBuf,
    source: SourceKey,
    session_id: StableEntityId,
}

impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let data_root = temp.path().join("data");
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
            index_root: data_root.join("search").join("lexical"),
            path: source_backed_semantic_vector_path(&data_root),
            data_root,
            _temp: temp,
            source,
            session_id,
        })
    }

    fn core_record(&self, sequence: u64, body: impl Into<String>) -> Result<CoreRecord> {
        let item = NativeItemKey::native_id("message", TypedKey::U64(sequence))?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })?;
        let mut record = CoreRecord::new_selected(
            event_id,
            self.session_id,
            self.session_id,
            self.source.clone(),
            sequence,
            "message",
            "primary",
            true,
            "semantic-source-projection-test-v1",
            body,
        )?;
        record.provider_session_id = Some("fixture-session".to_owned());
        record.native_event_id = Some(TypedKey::U64(sequence));
        record.branch = Some("main".to_owned());
        record.occurred_at_unix_ms = Some(sequence as i64);
        record.role = Some("user".to_owned());
        record.workspace = Some("/workspace".to_owned());
        record.cwd = Some("/workspace".to_owned());
        record.validate_contract()?;
        Ok(record)
    }

    fn record(&self, sequence: u64, body: impl Into<String>) -> Result<CoreEventRecord> {
        let core_record = self.core_record(sequence, body)?;
        let event = EventRecord {
            event_id: core_record.event_id,
            session_id: core_record.session_id,
            parent_session_id: core_record.parent_session_id,
            root_session_id: core_record.root_session_id,
            source: core_record.source.clone(),
            provider: core_record.source.provider().to_owned(),
            source_format: core_record.source.source_format().to_owned(),
            provider_session_id: core_record.provider_session_id.clone(),
            native_event_id: core_record.native_event_id.clone(),
            branch: core_record.branch.clone(),
            agent_type: core_record.agent_type.clone(),
            is_primary: core_record.is_primary,
            event_sequence: core_record.event_sequence,
            occurred_at_unix_ms: core_record.occurred_at_unix_ms,
            event_type: core_record.event_type.clone(),
            role: core_record.role.clone(),
            workspace: core_record.workspace.clone(),
            cwd: core_record.cwd.clone(),
            touched_files: Vec::new(),
        };
        Ok(CoreEventRecord { event, core_record })
    }

    fn publish(&self, records: Vec<CoreRecord>) -> Result<VerifiedIndex> {
        let count = records.len() as u64;
        let mut writer = GenerationWriter::open(&self.index_root, WriterOptions::default())?;
        writer.begin_source(self.source.clone())?;
        for record in records {
            writer.add_core_record(record)?;
        }
        let observation = SourceObservation::new(self.source.clone(), "fixture-v1", vec![1])?;
        writer.certify_source(CertifiedSource::certify(
            observation.clone(),
            observation,
            "fixture-parser-v1",
            [1; 32],
            ScannedSourceCounts {
                complete_records: count,
                retained_records: count,
                indexed_documents: count,
                certified_bytes: count * 50,
                ..ScannedSourceCounts::default()
            },
        )?)?;
        writer.commit(|_| true)?;
        Ok(VerifiedIndex::open(&self.index_root)?)
    }
}

fn generation(id: u8, semantic_documents: u64) -> SourceBackedSemanticGeneration {
    SourceBackedSemanticGeneration {
        core_generation_id: format!("{id:064x}"),
        semantic_policy_fingerprint: semantic_policy_fingerprint().unwrap(),
        semantic_documents,
    }
}

fn stable_identity_order(records: &mut [CoreEventRecord]) {
    records.sort_by_key(|record| record.event_id.encode_canonical().unwrap());
}

#[test]
fn semantic_generation_mirrors_the_persisted_core_eligible_count() -> Result<()> {
    let fixture = Fixture::new()?;
    let eligible = fixture.core_record(1, "eligible user message")?;
    let mut ineligible = fixture.core_record(2, "ineligible assistant message")?;
    ineligible.role = Some("assistant".to_owned());
    let index = fixture.publish(vec![eligible, ineligible])?;

    let generation = SourceBackedSemanticGeneration::from_verified_index(&index)?;
    assert_eq!(SOURCE_CONTRACT_VERSION, 5);
    assert_eq!(SOURCE_INPUT_LEXICAL_SCHEMA_VERSION, 15);
    assert_eq!(index.manifest().semantic_eligible_documents, 1);
    assert_eq!(generation.semantic_documents, 1);
    assert_eq!(generation.core_generation_id, index.generation_id());
    Ok(())
}

#[test]
fn catch_up_resumes_after_restart_from_core_identity_frontier() -> Result<()> {
    let fixture = Fixture::new()?;
    let mut records = vec![fixture.record(1, "first")?, fixture.record(2, "second")?];
    stable_identity_order(&mut records);
    let first = records[0].clone();
    let second = records[1].clone();
    let target = generation(1, 2);
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();

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
            &mut builder,
            &mut embedder,
        )?;
        assert!(outcome.work_remaining);
        assert!(!outcome.ready);
    }

    let mut store = SemanticVectorStore::open(&fixture.path)?;
    let outcome = store.reconcile_source_backed_page(
        &target,
        SourceBackedSemanticPage {
            core_generation_id: target.core_generation_id.clone(),
            after: Some(first.event_id),
            records: vec![second],
            terminal: true,
        },
        &mut builder,
        &mut embedder,
    )?;
    assert!(outcome.ready);
    assert_eq!(active_counts(&store)?.0, 2);
    assert!(store.source_backed_generation_ready_exact(&target.core_generation_id, 2)?);
    Ok(())
}

#[test]
fn complete_core_control_record_is_filtered_without_embedding() -> Result<()> {
    let fixture = Fixture::new()?;
    let record = fixture.record(
        1,
        "<environment_context>Core control record</environment_context>",
    )?;
    let target = generation(2, 1);
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let mut store = SemanticVectorStore::open(&fixture.path)?;

    let outcome = store.reconcile_source_backed_page(
        &target,
        SourceBackedSemanticPage {
            core_generation_id: target.core_generation_id.clone(),
            after: None,
            records: vec![record],
            terminal: true,
        },
        &mut builder,
        &mut embedder,
    )?;

    assert_eq!(outcome.records_filtered, 1);
    assert_eq!(embedder.chunks, 0);
    assert!(outcome.ready);
    assert_eq!(active_counts(&store)?, (0, 0));
    Ok(())
}

#[test]
fn generation_mismatch_rebuilds_and_failed_rebuild_keeps_flat_state_coherent() -> Result<()> {
    let fixture = Fixture::new()?;
    let record = fixture.record(1, "stable Core body")?;
    let initial = generation(3, 1);
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let mut store = SemanticVectorStore::open(&fixture.path)?;
    assert!(
        store
            .reconcile_source_backed_page(
                &initial,
                SourceBackedSemanticPage {
                    core_generation_id: initial.core_generation_id.clone(),
                    after: None,
                    records: vec![record.clone()],
                    terminal: true,
                },
                &mut builder,
                &mut embedder,
            )?
            .ready
    );
    let before = store.flat_pin_generation()?.unwrap();
    let before_hash = before.generation_hash().to_owned();
    drop(before);

    let target = generation(4, 1);
    builder.fail_on.insert(record.event_id.as_uuid());
    let error = store
        .reconcile_source_backed_page(
            &target,
            SourceBackedSemanticPage {
                core_generation_id: target.core_generation_id.clone(),
                after: None,
                records: vec![record],
                terminal: true,
            },
            &mut builder,
            &mut embedder,
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("forced Core projection interruption"));
    assert!(!store.source_backed_generation_ready(&target.core_generation_id)?);
    assert_eq!(
        store.flat_pin_generation()?.unwrap().generation_hash(),
        before_hash
    );
    assert_eq!(active_counts(&store)?.0, 1);
    Ok(())
}

#[test]
fn tail_beyond_sixteen_kib_is_paged_embedded_searchable_and_never_stored_plaintext() -> Result<()> {
    let fixture = Fixture::new()?;
    let body = format!("{} {TAIL_TOKEN}", "prefix ".repeat(2_500));
    assert!(body.len() > 16 * 1024);
    let index = fixture.publish(vec![fixture.core_record(1, body)?])?;
    assert!(!fixture
        .data_root
        .join("provider-source-removed.jsonl")
        .exists());
    let page = index.core_semantic_event_page(None, 1)?;
    assert!(page.items[0]
        .core_record
        .content
        .meaningful_text()
        .ends_with(TAIL_TOKEN));

    let mut store = SemanticVectorStore::open(&fixture.path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();
    let outcome = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert!(outcome.ready);
    assert!(embedder.chunks > 1);
    let pin = store
        .pin_source_backed_generation(index.generation_id(), 1)?
        .unwrap();
    let mut query = vec![0.0; SEMANTIC_DIMENSIONS];
    query[0] = 1.0;
    let search = scan_exact_generation(&pin, &query, 1, None, Instant::now())?;
    assert_eq!(search.hits[0].event_id, page.items[0].event_id.as_uuid());

    for directory in [fixture.path.clone(), fixture.path.join("flat_segments")] {
        if !directory.exists() {
            continue;
        }
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
    Ok(())
}

#[test]
fn no_op_and_policy_receipt_mismatch_reuse_one_page_lookup() -> Result<()> {
    let fixture = Fixture::new()?;
    let documents = (0..65)
        .map(|sequence| fixture.core_record(sequence + 1, format!("record {sequence}")))
        .collect::<Result<Vec<_>>>()?;
    let index = fixture.publish(documents)?;
    let mut store = SemanticVectorStore::open(&fixture.path)?;
    let mut builder = CoreBuilder::default();
    let mut embedder = MarkerEmbedder::default();

    let first = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert_eq!(first.records_scanned, MAX_SEMANTIC_EVENT_PAGE_ITEMS);
    assert!(first.work_remaining);
    let second = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert_eq!(second.records_scanned, 1);
    assert!(second.ready);
    assert!(embedder.maximum_batch <= 2);
    let calls = builder.calls.len();
    let no_op = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert!(no_op.ready);
    assert_eq!(no_op.records_scanned, 0);
    assert_eq!(builder.calls.len(), calls);

    let mut receipt: serde_json::Value = serde_json::from_str(&store.conn.query_row(
        "SELECT value FROM semantic_maintenance_state WHERE key = ?1",
        [SOURCE_ACKNOWLEDGEMENT_STATE],
        |row| row.get::<_, String>(0),
    )?)?;
    receipt["semantic_policy_fingerprint"] = serde_json::Value::String("0".repeat(64));
    store.conn.execute(
        "UPDATE semantic_maintenance_state SET value = ?1 WHERE key = ?2",
        params![
            serde_json::to_string(&receipt)?,
            SOURCE_ACKNOWLEDGEMENT_STATE
        ],
    )?;
    assert!(!store.source_backed_generation_ready_exact(index.generation_id(), 65)?);
    let embedded_chunks = embedder.chunks;
    store.reset_flat_active_event_snapshot_count();

    let replay_first = store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert_eq!(replay_first.records_scanned, MAX_SEMANTIC_EVENT_PAGE_ITEMS);
    assert_eq!(replay_first.records_reused, MAX_SEMANTIC_EVENT_PAGE_ITEMS);
    assert!(replay_first.work_remaining);
    assert_eq!(embedder.chunks, embedded_chunks);
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        0,
        "reuse must not materialize and scan all active events per record"
    );

    let replay_terminal =
        store.reconcile_source_backed_index(&index, &mut builder, &mut embedder)?;
    assert!(replay_terminal.ready);
    assert_eq!(replay_terminal.records_reused, 1);
    assert_eq!(embedder.chunks, embedded_chunks);
    assert_eq!(
        store.flat_active_event_snapshot_count(),
        1,
        "only terminal retirement should enumerate the active generation"
    );
    assert!(store.source_backed_generation_ready_exact(index.generation_id(), 65)?);
    assert!(builder.calls.len() > calls);
    Ok(())
}
