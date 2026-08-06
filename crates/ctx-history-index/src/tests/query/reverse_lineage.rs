use super::*;
use ctx_history_core::StableEntityId;

fn session_event(
    source: &SourceKey,
    native_session_id: &str,
    sequence: u64,
    body: &str,
) -> CoreRecord {
    super::super::document_for_session(source, native_session_id, sequence, body)
}

fn copied_event(
    mut event: CoreRecord,
    parent_session_id: StableEntityId,
    root_session_id: StableEntityId,
    relationship: SessionRelationshipKind,
    ancestor: &CoreRecord,
) -> CoreRecord {
    event
        .set_session_relationship(relationship, Some(parent_session_id), root_session_id)
        .unwrap();
    event.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(ancestor.session_id),
        ancestor_event_id: Box::new(ancestor.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    event.validate_contract().unwrap();
    event
}

fn publish_records(source: &SourceKey, records: &[CoreRecord]) -> (TempDir, VerifiedIndex) {
    let temp = tempdir().unwrap();
    let mut writer = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    writer
        .certify_source(super::super::certificate(source, 1, records.len() as u64))
        .unwrap();
    writer.commit(|_| true).unwrap();
    let index = VerifiedIndex::open(temp.path()).unwrap();
    (temp, index)
}

fn relationship_count(
    result: &CopiedEventLineage,
    relationship: SessionRelationshipKind,
) -> Option<u64> {
    result
        .relationship_counts
        .iter()
        .find(|count| count.session_relationship == relationship)
        .map(|count| count.observed_count)
}

#[test]
fn direct_copy_returns_full_stored_lineage_and_exact_total() {
    let source = source("reverse-lineage-direct.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copy = copied_event(
        session_event(&source, "child", 2, "copied"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let (_temp, index) = publish_records(&source, &[root.clone(), copy.clone()]);

    let result = index
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(result.generation_id, index.generation_id());
    assert_eq!(result.selected_event_id, root.event_id);
    assert_eq!(result.selected_session_id, root.session_id);
    assert_eq!(result.canonical_event_id, root.event_id);
    assert_eq!(result.canonical_session_id, root.session_id);
    assert_eq!(result.selected_depth, 0);
    assert_eq!(result.observed_count, 1);
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(result.returned, 1);
    assert!(!result.truncated);
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::Forked),
        Some(1)
    );
    assert_eq!(
        result.occurrences,
        vec![CopiedEventLineageOccurrence {
            event_id: copy.event_id,
            session_id: copy.session_id,
            copied_from_event_id: root.event_id,
            copied_from_session_id: root.session_id,
            parent_session_id: Some(root.session_id),
            root_session_id: root.session_id,
            session_relationship: SessionRelationshipKind::Forked,
            depth: 1,
        }]
    );
}

#[test]
fn selected_copy_resolves_forward_then_returns_breadth_first_inheritors() {
    let source = source("reverse-lineage-multihop.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let middle = copied_event(
        session_event(&source, "middle", 2, "middle"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Delegated,
        &root,
    );
    let leaf = copied_event(
        session_event(&source, "leaf", 3, "leaf"),
        middle.session_id,
        root.session_id,
        SessionRelationshipKind::ResumedFrom,
        &middle,
    );
    let (_temp, index) = publish_records(&source, &[leaf.clone(), root.clone(), middle.clone()]);

    let result = index
        .copied_event_lineage(leaf.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(result.selected_event_id, leaf.event_id);
    assert_eq!(result.selected_session_id, leaf.session_id);
    assert_eq!(result.canonical_event_id, root.event_id);
    assert_eq!(result.canonical_session_id, root.session_id);
    assert_eq!(result.selected_depth, 2);
    assert_eq!(result.observed_count, 2);
    assert_eq!(result.exact_observed_count(), Some(2));
    assert_eq!(
        result
            .occurrences
            .iter()
            .map(|occurrence| (occurrence.event_id, occurrence.depth))
            .collect::<Vec<_>>(),
        vec![(middle.event_id, 1), (leaf.event_id, 2)]
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::Delegated),
        Some(1)
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::ResumedFrom),
        Some(1)
    );
}

#[test]
fn duplicate_event_paths_deduplicate_sessions_deterministically() {
    let source = source("reverse-lineage-diamond.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let first_a = copied_event(
        session_event(&source, "branch-a", 2, "first A"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let second_a = copied_event(
        session_event(&source, "branch-a", 3, "second A"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let first_b = copied_event(
        session_event(&source, "branch-b", 4, "first B"),
        first_a.session_id,
        root.session_id,
        SessionRelationshipKind::ResumedFrom,
        &first_a,
    );
    let second_b = copied_event(
        session_event(&source, "branch-b", 5, "second B"),
        first_a.session_id,
        root.session_id,
        SessionRelationshipKind::ResumedFrom,
        &second_a,
    );
    let records = [
        second_b.clone(),
        second_a.clone(),
        root.clone(),
        first_b.clone(),
        first_a.clone(),
    ];
    let (_temp, index) = publish_records(&source, &records);

    let result = index
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(result.observed_count, 2);
    assert_eq!(result.returned, 2);
    assert_eq!(result.exact_observed_count(), Some(2));
    assert_eq!(
        result
            .occurrences
            .iter()
            .map(|occurrence| occurrence.event_id)
            .collect::<Vec<_>>(),
        vec![first_a.event_id, first_b.event_id]
    );
    assert_eq!(
        result
            .occurrences
            .iter()
            .map(|occurrence| occurrence.session_id)
            .collect::<HashSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::Forked),
        Some(1)
    );
    assert_eq!(
        relationship_count(&result, SessionRelationshipKind::ResumedFrom),
        Some(1)
    );
}

#[test]
fn preview_retention_preserves_exact_counts_while_posting_bounds_report_lower_bounds() {
    let fanout_source = source("reverse-lineage-bounds.jsonl");
    let root = session_event(&fanout_source, "root", 1, "canonical");
    let copies = (2..=5)
        .map(|sequence| {
            copied_event(
                session_event(
                    &fanout_source,
                    &format!("child-{sequence}"),
                    sequence,
                    "copy",
                ),
                root.session_id,
                root.session_id,
                SessionRelationshipKind::Forked,
                &root,
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![root.clone()];
    records.extend(copies);
    let (_temp, index) = publish_records(&fanout_source, &records);

    let capped = index
        .copied_event_lineage(root.event_id.as_uuid(), SEARCH_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(capped.returned, 3);
    assert_eq!(capped.observed_count, 4);
    assert!(!capped.truncated);
    assert_eq!(capped.exact_observed_count(), Some(4));
    assert_eq!(
        relationship_count(&capped, SessionRelationshipKind::Forked),
        Some(4)
    );

    let chain_source = source("reverse-lineage-posting-bound.jsonl");
    let chain_root = session_event(&chain_source, "root", 1, "canonical");
    let middle = copied_event(
        session_event(&chain_source, "middle", 2, "middle"),
        chain_root.session_id,
        chain_root.session_id,
        SessionRelationshipKind::Forked,
        &chain_root,
    );
    let leaf = copied_event(
        session_event(&chain_source, "leaf", 3, "leaf"),
        middle.session_id,
        chain_root.session_id,
        SessionRelationshipKind::Forked,
        &middle,
    );
    let (_chain_temp, chain_index) =
        publish_records(&chain_source, &[chain_root.clone(), middle.clone(), leaf]);
    let posting_capped = chain_index
        .copied_event_lineage(
            chain_root.event_id.as_uuid(),
            CopiedEventLineagePolicy::new(20, 1),
        )
        .unwrap()
        .unwrap();
    assert_eq!(posting_capped.returned, 1);
    assert_eq!(posting_capped.observed_count, 1);
    assert_eq!(posting_capped.occurrences[0].event_id, middle.event_id);
    assert!(posting_capped.truncated);
    assert_eq!(posting_capped.exact_observed_count(), None);
}

#[test]
fn invalid_caller_policies_are_rejected_before_lookup() {
    let source = source("reverse-lineage-invalid-policy.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let (_temp, index) = publish_records(&source, std::slice::from_ref(&root));

    assert!(matches!(
        index.copied_event_lineage(root.event_id.as_uuid(), CopiedEventLineagePolicy::new(0, 1)),
        Err(IndexError::InvalidCopiedEventLineageOccurrenceLimit { .. })
    ));
    assert!(matches!(
        index.copied_event_lineage(
            root.event_id.as_uuid(),
            CopiedEventLineagePolicy::new(1, MAX_COPIED_EVENT_LINEAGE_POSTING_VISITS + 1)
        ),
        Err(IndexError::InvalidCopiedEventLineagePostingVisitLimit { .. })
    ));
}

#[test]
fn deleted_exact_identity_postings_hit_a_typed_absolute_bound_before_not_found() {
    let source = source("reverse-lineage-deleted-identity-postings.jsonl");
    let ancestor = session_event(&source, "ancestor", 1, "ancestor");
    let selected = copied_event(
        session_event(&source, "selected", 2, "selected"),
        ancestor.session_id,
        ancestor.session_id,
        SessionRelationshipKind::Forked,
        &ancestor,
    );
    let survivor = session_event(&source, "survivor", 3, "survivor");
    let (_temp, baseline) = publish_records(
        &source,
        &[ancestor.clone(), selected.clone(), survivor.clone()],
    );
    let index = baseline.searcher.index().clone();
    let event_id = required_field(&index.schema(), "event_id").unwrap();

    let mut writer = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    writer.set_merge_policy(Box::<NoMergePolicy>::default());
    for _ in 0..=MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS {
        writer
            .add_document(super::super::indexed_document(ancestor.clone()))
            .unwrap();
    }
    writer
        .add_document(super::super::indexed_document(survivor))
        .unwrap();
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();

    let mut deleting = index
        .writer_with_num_threads::<TantivyDocument>(1, INDEX_MEMORY_MIN_PER_THREAD)
        .unwrap();
    deleting.set_merge_policy(Box::<NoMergePolicy>::default());
    deleting.delete_term(Term::from_field_text(
        event_id,
        &ancestor.event_id.to_string(),
    ));
    deleting.commit().unwrap();
    deleting.wait_merging_threads().unwrap();

    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .unwrap();
    let searcher = reader.searcher();
    assert!(
        searcher
            .segment_readers()
            .iter()
            .map(|segment| segment.num_deleted_docs() as usize)
            .sum::<usize>()
            > MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS
    );
    let deleted_heavy = VerifiedIndex {
        searcher,
        ..baseline
    };

    assert!(matches!(
        deleted_heavy.copied_event_lineage(
            selected.event_id.as_uuid(),
            SHOW_COPIED_EVENT_LINEAGE_POLICY
        ),
        Err(
            IndexError::CopiedEventLineageExactIdentityPostingVisitLimitExceeded {
                maximum: MAX_COPIED_EVENT_LINEAGE_EXACT_IDENTITY_POSTING_VISITS
            }
        )
    ));
}

#[test]
fn depth_1024_is_complete_and_a_deeper_edge_truncates_truthfully() {
    let source = source("reverse-lineage-depth-bound.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let mut records = vec![root.clone()];
    let mut previous = root.clone();
    for depth in 1..=MAX_COPIED_EVENT_LINEAGE_DEPTH {
        let next = copied_event(
            session_event(&source, &format!("depth-{depth}"), depth as u64 + 1, "copy"),
            previous.session_id,
            root.session_id,
            SessionRelationshipKind::Forked,
            &previous,
        );
        records.push(next.clone());
        previous = next;
    }
    let (temp, baseline) = publish_records(&source, std::slice::from_ref(&root));
    let index = baseline.searcher.index().clone();
    drop(baseline);
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(
            &source,
            2,
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 1,
        )])
        .unwrap(),
        std::slice::from_ref(&source),
        records
            .iter()
            .cloned()
            .map(super::super::indexed_document)
            .collect(),
    );
    let exact_depth = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let complete = exact_depth
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(
        complete.exact_observed_count(),
        Some(MAX_COPIED_EVENT_LINEAGE_DEPTH as u64)
    );
    assert_eq!(
        complete.returned,
        SHOW_COPIED_EVENT_LINEAGE_POLICY.maximum_occurrences
    );
    let selected_at_boundary = exact_depth
        .copied_event_lineage(
            previous.event_id.as_uuid(),
            SHOW_COPIED_EVENT_LINEAGE_POLICY,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        selected_at_boundary.selected_depth,
        MAX_COPIED_EVENT_LINEAGE_DEPTH
    );
    assert_eq!(selected_at_boundary.canonical_event_id, root.event_id);
    assert_eq!(
        selected_at_boundary.exact_observed_count(),
        Some(MAX_COPIED_EVENT_LINEAGE_DEPTH as u64)
    );
    drop(exact_depth);

    let beyond = copied_event(
        session_event(
            &source,
            "beyond-depth-bound",
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 2,
            "copy",
        ),
        previous.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &previous,
    );
    let mut forged_documents = records
        .into_iter()
        .map(super::super::indexed_document)
        .collect::<Vec<_>>();
    forged_documents.push(super::super::indexed_document(beyond));
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(
            &source,
            3,
            MAX_COPIED_EVENT_LINEAGE_DEPTH as u64 + 2,
        )])
        .unwrap(),
        std::slice::from_ref(&source),
        forged_documents,
    );
    let forged = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let truncated = forged
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert!(truncated.truncated);
    assert_eq!(
        truncated.observed_count,
        MAX_COPIED_EVENT_LINEAGE_DEPTH as u64
    );
    assert_eq!(truncated.exact_observed_count(), None);
}

#[test]
fn forged_inverse_digest_projection_fails_closed() {
    let source = source("reverse-lineage-forged-digest.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let other = session_event(&source, "root", 2, "other root event");
    let copy = copied_event(
        session_event(&source, "child", 3, "copy"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &other,
    );
    let (temp, pinned) = publish_records(&source, &[root.clone(), other.clone(), copy.clone()]);
    let fields = fields_from_schema(pinned.searcher.schema()).unwrap();
    let target = fields.origin_event_identity_digest;
    let complete = super::super::indexed_document(copy);
    let mut forged = TantivyDocument::default();
    for (field, value) in complete.field_values() {
        if field != target {
            forged.add_field_value(field, value);
        }
    }
    forged.add_text(target, crate::hex(&root.event_id.digest()));
    let index = pinned.searcher.index().clone();
    drop(pinned);
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(&source, 2, 3)]).unwrap(),
        std::slice::from_ref(&source),
        vec![
            super::super::indexed_document(root.clone()),
            super::super::indexed_document(other),
            forged,
        ],
    );
    let forged_index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert!(matches!(
        forged_index
            .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY),
        Err(IndexError::InvalidStoredDocumentField(
            "origin_event_identity_digest"
        ))
    ));
}

#[test]
fn copied_event_cycle_fails_closed() {
    let source = source("reverse-lineage-cycle.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let first = copied_event(
        session_event(&source, "first", 2, "first"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let second = copied_event(
        session_event(&source, "second", 3, "second"),
        first.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &first,
    );
    let (temp, pinned) = publish_records(&source, &[root.clone(), first.clone(), second.clone()]);
    let index = pinned.searcher.index().clone();
    drop(pinned);

    let mut cyclic_first = first;
    cyclic_first.event_origin = EventOrigin::CopiedFromAncestor {
        ancestor_session_id: Box::new(second.session_id),
        ancestor_event_id: Box::new(second.event_id),
        proof: EventCopyProofKind::NativeEventIdentity,
    };
    cyclic_first.validate_contract().unwrap();
    super::super::publish_unchecked_generation(
        temp.path(),
        &index,
        GenerationManifest::from_sources(vec![super::super::certificate(&source, 2, 3)]).unwrap(),
        std::slice::from_ref(&source),
        vec![
            super::super::indexed_document(root),
            super::super::indexed_document(cyclic_first),
            super::super::indexed_document(second.clone()),
        ],
    );
    let cyclic_index = VerifiedIndex::open_pinned(temp.path()).unwrap();

    assert!(matches!(
        cyclic_index
            .copied_event_lineage(second.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY),
        Err(IndexError::InvalidEventOriginGraph(
            "cycle in copied-event origin graph"
        ))
    ));
}

#[test]
fn open_reader_remains_generation_pinned_across_replacement() {
    let source = source("reverse-lineage-generation-pinned.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copy = copied_event(
        session_event(&source, "child", 2, "copy"),
        root.session_id,
        root.session_id,
        SessionRelationshipKind::Forked,
        &root,
    );
    let (temp, pinned) = publish_records(&source, &[root.clone(), copy]);
    let pinned_generation = pinned.generation_id().to_owned();

    let mut replacement = GenerationWriter::open(temp.path(), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    replacement.begin_source(source.clone()).unwrap();
    replacement.add_core_record(root.clone()).unwrap();
    replacement
        .certify_source(super::super::certificate(&source, 2, 1))
        .unwrap();
    replacement.commit(|_| true).unwrap();
    let active = VerifiedIndex::open_pinned(temp.path()).unwrap();

    let retained_result = pinned
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    let active_result = active
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap()
        .unwrap();
    assert_eq!(retained_result.generation_id, pinned_generation);
    assert_eq!(retained_result.exact_observed_count(), Some(1));
    assert_ne!(active_result.generation_id, pinned_generation);
    assert_eq!(active_result.exact_observed_count(), Some(0));
    assert!(active_result.occurrences.is_empty());
}
