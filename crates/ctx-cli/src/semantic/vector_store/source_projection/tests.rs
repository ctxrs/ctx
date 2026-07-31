use std::{collections::HashSet, fs, time::Instant};

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
    EventRole, EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, SourceRecordLocator, TypedKey,
};
use ctx_history_index::{
    CoreEventRecord, EventRecord, GenerationWriter, LexicalDocument, VerifiedIndex, WriterOptions,
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
            history_record_id: None,
            session_id: Some(record.session_id.as_uuid()),
            seq: record.event_sequence,
            occurred_at_ms: record.occurred_at_unix_ms.unwrap_or_default(),
            anchor_occurred_at_ms: record.occurred_at_unix_ms.unwrap_or_default(),
            event_type: EventType::Message,
            role: Some(EventRole::User),
            rank_bucket: "core_event".to_owned(),
            provider: Some(CaptureProvider::Codex),
            source_format: Some(record.source_format.clone()),
            agent_type: None,
            session_is_primary: Some(record.is_primary),
            cwd: record.cwd.clone(),
            raw_source_path: None,
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

    fn document(&self, sequence: u64, body: impl Into<String>) -> Result<LexicalDocument> {
        let item = NativeItemKey::native_id("message", TypedKey::U64(sequence))?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "message",
            native_item_key: &item,
            subrecord_selector: None,
        })?;
        Ok(LexicalDocument {
            event_id,
            session_id: self.session_id,
            parent_session_id: None,
            root_session_id: self.session_id,
            source: self.source.clone(),
            locator: SourceRecordLocator::new(
                self.source.clone(),
                NativeRecordCoordinate::Jsonl {
                    byte_offset: sequence * 100,
                    byte_length: 50,
                    physical_ordinal: sequence,
                    native_session_key: Some(TypedKey::utf8("fixture-session")?),
                    native_event_key: Some(TypedKey::U64(sequence)),
                },
                LocatorRevisionPolicy::ExactSourceRevision,
                Some([9; 32]),
                [sequence as u8; 32],
            )?,
            provider_session_id: Some("fixture-session".to_owned()),
            branch: Some("main".to_owned()),
            source_path: Some(
                self.data_root
                    .join("provider-source-removed.jsonl")
                    .display()
                    .to_string(),
            ),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence: sequence,
            occurred_at_unix_ms: Some(sequence as i64),
            event_type: "message".to_owned(),
            role: Some("user".to_owned()),
            body: body.into(),
            workspace: Some("/workspace".to_owned()),
            cwd: Some("/workspace".to_owned()),
            touched_files: Vec::new(),
        })
    }

    fn record(&self, sequence: u64, body: impl Into<String>) -> Result<CoreEventRecord> {
        let document = self.document(sequence, body)?;
        let core_record = document.to_core_record()?;
        let event = EventRecord {
            event_id: document.event_id,
            session_id: document.session_id,
            parent_session_id: document.parent_session_id,
            root_session_id: document.root_session_id,
            locator: document.locator,
            provider: document.source.provider().to_owned(),
            source_format: document.source.source_format().to_owned(),
            provider_session_id: document.provider_session_id,
            branch: document.branch,
            source_path: document.source_path,
            agent_type: document.agent_type,
            is_primary: document.is_primary,
            event_sequence: document.event_sequence,
            occurred_at_unix_ms: document.occurred_at_unix_ms,
            event_type: document.event_type,
            role: document.role,
            workspace: document.workspace,
            cwd: document.cwd,
            touched_files: document.touched_files,
        };
        Ok(CoreEventRecord { event, core_record })
    }

    fn publish(&self, documents: Vec<LexicalDocument>) -> Result<VerifiedIndex> {
        let count = documents.len() as u64;
        let mut writer = GenerationWriter::open(&self.index_root, WriterOptions::default())?;
        writer.begin_source(self.source.clone())?;
        for document in documents {
            writer.add_document(document)?;
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
    let index = fixture.publish(vec![fixture.document(1, body)?])?;
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
fn no_op_and_policy_receipt_mismatch_are_automatic_and_bounded() -> Result<()> {
    let fixture = Fixture::new()?;
    let documents = (0..65)
        .map(|sequence| fixture.document(sequence + 1, format!("record {sequence}")))
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
    while !store
        .reconcile_source_backed_index(&index, &mut builder, &mut embedder)?
        .ready
    {}
    assert!(store.source_backed_generation_ready_exact(index.generation_id(), 65)?);
    assert!(builder.calls.len() > calls);
    Ok(())
}
