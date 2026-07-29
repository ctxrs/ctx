use std::collections::HashMap;

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, EventIdentityInput, EventRole, EventType,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator, TypedKey,
};
use tempfile::TempDir;

use super::*;

fn active_counts(store: &SemanticVectorStore) -> Result<(usize, usize)> {
    let pinned = store
        .flat_pin_generation()?
        .expect("fixture must publish a flat generation");
    Ok((pinned.stats().active_events, pinned.stats().active_chunks))
}

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
    ) -> std::result::Result<SemanticEventDocument, HydrationFailure> {
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
        Ok(SemanticEventDocument {
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
            path: source_backed_semantic_vector_path(temp.path()),
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
    assert_eq!(active_counts(&store)?.0, 2);
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
    assert_eq!(active_counts(&store)?.0, 1);
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
    assert_eq!(active_counts(&store)?.0, 1);
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
    assert_eq!(active_counts(&store)?, (0, 0));
    assert!(!store.source_backed_generation_ready(&target.core_generation_id)?);
    Ok(())
}
