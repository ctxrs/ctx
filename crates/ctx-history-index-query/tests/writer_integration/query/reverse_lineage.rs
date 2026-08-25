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
    parent_session_id: Option<StableEntityId>,
    claimed_root_session_id: Option<StableEntityId>,
    relationship: Option<ProviderNativeSessionRelationship>,
    ancestor: &CoreRecord,
    proof: ProviderNativeCopyProof,
) -> CoreRecord {
    event.parent_session_id = parent_session_id;
    event.root_session_id = claimed_root_session_id;
    event.session_relationship = relationship;
    event.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: ancestor.session_id,
        ancestor_event_id: ancestor.event_id,
        proof,
    });
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

#[test]
fn direct_copy_returns_exact_child_claim_and_resolved_target() {
    let source = source("reverse-lineage-direct.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copy = copied_event(
        session_event(&source, "child", 2, "copied"),
        Some(root.session_id),
        Some(root.session_id),
        Some(ProviderNativeSessionRelationship::Forked),
        &root,
        ProviderNativeCopyProof::NativeCopiedFromField,
    );
    let (_temp, index) = publish_records(&source, &[root.clone(), copy.clone()]);

    let result = index
        .copied_event_lineage(root.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(result.generation_id, index.generation_id());
    assert_eq!(result.selected_event_id, root.event_id.as_uuid());
    assert_eq!(result.selected_session_id, Some(root.session_id));
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Resolved {
            event_id: root.event_id,
            session_id: root.session_id,
        }
    );
    assert_eq!(result.selected_depth, 0);
    assert_eq!(result.exact_observed_count(), Some(1));
    assert_eq!(
        result.occurrences,
        vec![CopiedEventLineageOccurrence {
            event_id: copy.event_id,
            session_id: copy.session_id,
            copied_from_event_id: root.event_id,
            copied_from_session_id: root.session_id,
            parent_session_id: Some(root.session_id),
            claimed_root_session_id: Some(root.session_id),
            session_relationship: Some(ProviderNativeSessionRelationship::Forked),
            copy_proof: ProviderNativeCopyProof::NativeCopiedFromField,
            depth: 1,
        }]
    );
}

#[test]
fn selected_copy_resolves_only_its_direct_target() {
    let source = source("reverse-lineage-direct-only.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let middle = copied_event(
        session_event(&source, "middle", 2, "middle"),
        Some(root.session_id),
        Some(root.session_id),
        Some(ProviderNativeSessionRelationship::Delegated),
        &root,
        ProviderNativeCopyProof::NativeEventIdentity,
    );
    let leaf = copied_event(
        session_event(&source, "leaf", 3, "leaf"),
        Some(middle.session_id),
        Some(root.session_id),
        Some(ProviderNativeSessionRelationship::ResumedFrom),
        &middle,
        ProviderNativeCopyProof::NativeCallResultIdentity,
    );
    let (_temp, index) = publish_records(&source, &[leaf.clone(), root, middle.clone()]);

    let result = index
        .copied_event_lineage(leaf.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Resolved {
            event_id: middle.event_id,
            session_id: middle.session_id,
        }
    );
    assert_eq!(result.selected_depth, 1);
    assert_eq!(result.exact_observed_count(), Some(0));
    assert!(result.occurrences.is_empty());
}

#[test]
fn absent_direct_target_is_unresolved_without_inference() {
    let source = source("reverse-lineage-absent-target.jsonl");
    let absent = session_event(&source, "absent", 1, "not published");
    let copy = copied_event(
        session_event(&source, "child", 2, "copied"),
        None,
        None,
        None,
        &absent,
        ProviderNativeCopyProof::NativeEventIdentity,
    );
    let (_temp, index) = publish_records(&source, std::slice::from_ref(&copy));

    let result = index
        .copied_event_lineage(copy.event_id.as_uuid(), SHOW_COPIED_EVENT_LINEAGE_POLICY)
        .unwrap();
    assert_eq!(result.selected_session_id, Some(copy.session_id));
    assert_eq!(
        result.resolution,
        CopiedEventLineageResolution::Unresolved {
            event_id: absent.event_id.as_uuid(),
            session_id: Some(absent.session_id),
        }
    );
    assert_eq!(result.selected_depth, 1);
    assert_eq!(result.exact_observed_count(), Some(0));
}

#[test]
fn reverse_preview_preserves_optional_relationships_and_exact_counts() {
    let source = source("reverse-lineage-bounds.jsonl");
    let root = session_event(&source, "root", 1, "canonical");
    let copies = (2..=5)
        .map(|sequence| {
            copied_event(
                session_event(&source, &format!("child-{sequence}"), sequence, "copy"),
                None,
                None,
                (sequence % 2 == 0).then_some(ProviderNativeSessionRelationship::Forked),
                &root,
                ProviderNativeCopyProof::NativeEventIdentity,
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![root.clone()];
    records.extend(copies);
    let (_temp, index) = publish_records(&source, &records);

    let preview = index
        .copied_event_lineage(
            root.event_id.as_uuid(),
            CopiedEventLineagePolicy::new(3, 64),
        )
        .unwrap();
    assert_eq!(preview.returned, 3);
    assert_eq!(preview.exact_observed_count(), Some(4));
    assert!(!preview.truncated);
    assert_eq!(preview.relationship_counts.len(), 2);
    assert_eq!(
        preview
            .relationship_counts
            .iter()
            .find(|count| count.session_relationship.is_none())
            .map(|count| count.observed_count),
        Some(2)
    );

    let bounded = index
        .copied_event_lineage(
            root.event_id.as_uuid(),
            CopiedEventLineagePolicy::new(20, 1),
        )
        .unwrap();
    assert_eq!(bounded.observed_count, 1);
    assert!(bounded.truncated);
    assert_eq!(bounded.exact_observed_count(), None);
}
