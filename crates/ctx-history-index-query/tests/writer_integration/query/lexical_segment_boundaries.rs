use super::*;

const QUERY: &str = "segmentboundaryneedle";

fn publish_unmerged_segments(segment_count: usize) -> (TempDir, VerifiedIndex, Vec<[u8; 32]>) {
    let temp = tempdir().unwrap();
    let source = source(&format!("lexical-segments-{segment_count}.jsonl"));
    let records = (0..segment_count)
        .map(|index| {
            document_for_session(
                &source,
                &format!("lexical-segment-session-{index}"),
                1,
                QUERY,
            )
        })
        .collect::<Vec<_>>();
    let mut exact = records
        .iter()
        .map(|record| record.event_id.digest())
        .collect::<Vec<_>>();
    exact.sort();

    let initial = publish_records(&temp, &source, records.clone());
    drop(initial);
    let (searcher, manifest) = open_unverified_generation(temp.path());
    drop(searcher);
    let directory = DurableMmapDirectory::open(active_generation_path(temp.path())).unwrap();
    let tantivy_index = Index::open(directory).unwrap();
    let fields = fields_from_schema(&tantivy_index.schema()).unwrap();
    let generation_id = manifest.generation_id().unwrap();
    let mut writer = tantivy_index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    writer.delete_all_documents().unwrap();
    writer.commit().unwrap();

    for (index, record) in records.into_iter().enumerate() {
        let authority = ctx_history_index_format::SessionAuthorityKey::exact(
            record.session_id,
            record.source.identity(),
        )
        .unwrap();
        let mut document = indexed_document(record);
        document.add_bytes(fields.session_authority, authority.as_bytes());
        writer.add_document(document).unwrap();
        if index + 1 == segment_count {
            let mut prepared = writer.prepare_commit().unwrap();
            prepared.set_payload(
                &serde_json::to_string(&CommitPayload {
                    version: COMMIT_PAYLOAD_VERSION,
                    generation_id: generation_id.clone(),
                    publication_metadata: None,
                })
                .unwrap(),
            );
            prepared.commit().unwrap();
        } else {
            writer.commit().unwrap();
        }
    }
    writer.wait_merging_threads().unwrap();
    let metas = tantivy_index.load_metas().unwrap();
    assert_eq!(metas.segments.len(), segment_count);
    assert!(metas.segments.iter().all(|segment| segment.num_docs() == 1));

    let pointer = load_active_generation_pointer(temp.path())
        .unwrap()
        .unwrap();
    let generation_path = active_generation_path(temp.path());
    let digest =
        physical_integrity_digest(&tantivy_index, &generation_path, Some(&pointer)).unwrap();
    let active = GenerationSlot::new(
        generation_id,
        pointer.active().directory().to_owned(),
        digest,
    )
    .unwrap();
    publish_active_generation_pointer(
        temp.path(),
        &ActiveGenerationPointer::new(active, None).unwrap(),
    )
    .unwrap();

    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index, exact)
}

#[test]
fn real_segment_topology_limits_preserve_exact_prefixes() {
    for segment_count in [1_usize, 64, 65, 512] {
        let (_temp, index, exact) = publish_unmerged_segments(segment_count);
        for limit in [1, 10, 20, 200] {
            let batch =
                lexical_search_batch(&index, &[QUERY], &EventSearchFilters::default(), limit)
                    .unwrap();
            assert!(
                batch.complete,
                "segment_count={segment_count} limit={limit}"
            );
            assert_eq!(batch.counters.segments, segment_count as u64);
            assert_eq!(
                batch
                    .candidates
                    .iter()
                    .map(|candidate| candidate.event.event_identity_digest)
                    .collect::<Vec<_>>(),
                exact[..exact.len().min(limit)],
                "segment_count={segment_count} limit={limit}"
            );
            assert_eq!(batch.candidate_set_exhaustive, exact.len() <= limit);
        }
    }

    let (_temp, index, _) = publish_unmerged_segments(513);
    let exhausted =
        lexical_search_batch(&index, &[QUERY], &EventSearchFilters::default(), 200).unwrap();
    assert!(!exhausted.complete);
    assert!(!exhausted.candidate_set_exhaustive);
    assert!(exhausted.candidates.is_empty());
    assert_eq!(
        exhausted.counters,
        ctx_history_index_query::LexicalWorkCounters {
            analyzed_tokens: 1,
            exact_filter_terms: 1,
            ..ctx_history_index_query::LexicalWorkCounters::default()
        }
    );
    let exhaustion = exhausted.exhaustion.unwrap();
    assert_eq!(
        exhaustion.counter,
        ctx_history_index_query::LexicalWorkCounter::Segments
    );
    assert_eq!(exhaustion.used, 0);
    assert_eq!(exhaustion.limit, 512);
    assert!(exhaustion.segment.is_none());
    assert!(exhaustion.next_doc.is_none());
}
