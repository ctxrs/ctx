use super::*;
use crate::{SemanticQueryPin, SourceBackedSemanticDocumentBuilder};
use ctx_history_index::{CompiledSearchFilter, EventSearchFilters};

const FINDING: &str = "The amber falcon owns relay 47";

fn publish_turn(fixture: &Fixture, records: Vec<CoreRecord>) -> Result<VerifiedIndex> {
    let root = fixture.data_root.join("index-lite-turn");
    let mut writer = GenerationWriter::open(&root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?;
    let source = &fixture.sources[0].source;
    writer.begin_source(source.clone())?;
    let count = records.len() as u64;
    for record in records {
        writer.add_core_record(record)?;
    }
    let observation = SourceObservation::new(source.clone(), "lite-turn", vec![1])?;
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
    Ok(VerifiedIndex::open_pinned(root)?)
}

// Deterministic embedding input coverage, not a model relevance benchmark.
struct FindingEmbedder;

impl SemanticBatchEmbedder for FindingEmbedder {
    fn document_fits(&mut self, _text: &str) -> Result<bool> {
        Ok(true)
    }

    fn embed_chunks(&mut self, chunks: &[SemanticChunkDocument]) -> Result<Vec<Vec<f32>>> {
        Ok(chunks
            .iter()
            .map(|chunk| {
                let mut vector = vec![0.0; semantic_model_contract().dimensions()];
                vector[usize::from(!chunk.text.contains(FINDING))] = 1.0;
                vector
            })
            .collect())
    }
}

#[test]
fn intermediate_assistant_is_searchable_with_exact_unicode_member_spans() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let mut excluded =
        fixture.record_with_role(0, 3, "excluded retrieval", EventRole::Assistant)?;
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    let index = publish_turn(
        &fixture,
        vec![
            fixture.record(0, 1, "Run check A")?,
            fixture.record_with_role(
                0,
                2,
                &format!("\u{3000}{FINDING} 🦅 \n"),
                EventRole::Assistant,
            )?,
            excluded,
            fixture.record_with_role(0, 4, " \t ", EventRole::Assistant)?,
            fixture.record_with_role(0, 5, "\tDone ✓ ", EventRole::Assistant)?,
            fixture.record(0, 6, "Continue")?,
            fixture.record_with_role(0, 7, "next turn only", EventRole::Assistant)?,
        ],
    )?;
    let mut store = open_store(&fixture.semantic_path)?;
    let generation =
        SourceBackedSemanticGeneration::from_verified_index(&index, semantic_model_contract())?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let outcome = reconcile_generation(
        &mut store,
        &index,
        &generation,
        &mut builder,
        &mut FindingEmbedder,
    )?;
    assert!(outcome.ready);
    let mut pin =
        SemanticQueryPin::preflight(&index, &fixture.data_root, semantic_model_contract())?;
    let mut query = vec![0.0; semantic_model_contract().dimensions()];
    query[0] = 1.0;
    let filter = CompiledSearchFilter::compile(EventSearchFilters::default())?;
    let (hits, _) = pin.search(&index, &filter, &[query], 1)?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].event.event_id, fixture.event_id(0, 1)?);
    assert!(
        hits[0].score > 0.99,
        "intermediate finding must enter the embedded input"
    );
    let passage = pin.resolve_passage(
        &index,
        semantic_model_contract(),
        &hits[0].event,
        hits[0]
            .semantic_evidence
            .as_ref()
            .expect("semantic evidence"),
    )?;
    assert_eq!(
        passage.text,
        format!("user:\nRun check A\n\nassistant:\n{FINDING} 🦅\n\nassistant:\nDone ✓")
    );
    assert_eq!(passage.members.len(), 3);
    for (member, (sequence, text, trim)) in passage.members.iter().zip([
        (1, "Run check A".to_owned(), 0),
        (2, format!("{FINDING} 🦅"), 1),
        (5, "Done ✓".to_owned(), 1),
    ]) {
        assert_eq!(
            member.event.event_id.as_uuid(),
            fixture.event_id(0, sequence)?
        );
        assert_eq!(&passage.text[member.byte_range.clone()], text);
        assert_eq!(member.content_start_char, trim);
    }
    Ok(())
}

#[test]
fn all_assistant_policy_rebuilds_unchanged_core_and_then_reuses_it() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let index = publish_turn(
        &fixture,
        vec![
            fixture.record(0, 1, "Run check A")?,
            fixture.record_with_role(0, 2, FINDING, EventRole::Assistant)?,
            fixture.record_with_role(0, 3, "Done", EventRole::Assistant)?,
            fixture.record(0, 4, "Continue")?,
        ],
    )?;
    let core_generation = index.generation_id().to_owned();
    let mut old_policy = current_semantic_generation_policy();
    old_policy.eligibility_revision = 6;
    assert_eq!(current_semantic_generation_policy().eligibility_revision, 7);
    let legacy = SourceBackedSemanticGeneration::from_verified_index_with_policy(
        &index,
        old_policy,
        semantic_model_contract(),
    )?;
    let current =
        SourceBackedSemanticGeneration::from_verified_index(&index, semantic_model_contract())?;
    assert_ne!(legacy.contract_fingerprint, current.contract_fingerprint);
    let mut store = open_store(&fixture.semantic_path)?;
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let initial = reconcile_generation(
        &mut store,
        &index,
        &legacy,
        &mut builder,
        &mut FindingEmbedder,
    )?;
    assert_eq!(initial.records_embedded, 2);
    drop(store);
    let mut store = open_store(&fixture.semantic_path)?;
    assert!(
        SemanticQueryPin::preflight(&index, &fixture.data_root, semantic_model_contract()).is_err()
    );
    let rebuilt = reconcile_generation(
        &mut store,
        &index,
        &current,
        &mut builder,
        &mut FindingEmbedder,
    )?;
    assert_eq!(rebuilt.records_embedded, 2);
    assert_eq!(rebuilt.records_reused, 0);
    assert_eq!(
        store
            .source_acknowledgement()?
            .unwrap()
            .semantic_policy_fingerprint,
        current.semantic_policy_fingerprint
    );
    let no_op = reconcile_generation(
        &mut store,
        &index,
        &current,
        &mut builder,
        &mut FindingEmbedder,
    )?;
    assert_eq!(no_op.records_decoded, 0);
    assert_eq!(no_op.records_embedded, 0);
    assert_eq!(
        VerifiedIndex::open_pinned(fixture.data_root.join("index-lite-turn"))?.generation_id(),
        core_generation
    );
    Ok(())
}

#[test]
fn all_assistants_keep_the_existing_unicode_source_cap() -> Result<()> {
    let fixture = Fixture::new(1)?;
    let first = "🦅".repeat(ctx_history_index::SEMANTIC_SOURCE_MAX_CHARS);
    let index = publish_turn(
        &fixture,
        vec![
            fixture.record(0, 1, "Run check A")?,
            fixture.record_with_role(0, 2, &first, EventRole::Assistant)?,
            fixture.record_with_role(0, 3, "Done", EventRole::Assistant)?,
        ],
    )?;
    let anchor = index.core_event_by_id(fixture.event_id(0, 1)?)?.unwrap();
    let mut builder = SourceBackedSemanticDocumentBuilder::new(&index);
    let document = builder.build_document(&anchor)?.unwrap();
    assert!(document.text.ends_with("assistant:\nDone"));
    let source = crate::indexing::semantic_source_text(&document.text);
    assert_eq!(source.chars().count(), 65_536);
    assert!(source.ends_with('🦅'));
    assert!(!source.contains("Done"));
    let chunks = crate::indexing::semantic_chunks_for_document(&document, &source, &"0".repeat(64));
    assert_eq!(chunks.last().unwrap().end_char, 65_536);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.end_char - chunk.start_char <= 1_200));
    Ok(())
}
