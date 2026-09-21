use super::*;

#[test]
fn explicit_pinned_generation_composition_is_query_and_cursor_neutral() {
    let (_, _, projected) = referenced_commit_projection(8, true);
    let target = ResourceId(projected[0].subject.graph_id().expect("commit graph ID"));
    let directory = tempfile::tempdir().expect("pinned seam directory");

    let receipt = synthetic_receipt();
    let segment = write_segment(directory.path(), 0x69, projected, Vec::new());
    publish(directory.path(), receipt.clone(), vec![segment], None);

    let explicit = pin_graph(directory.path());
    let compatibility =
        SegmentGraph::open(directory.path(), None).expect("test compatibility opener");

    assert_eq!(
        explicit.graph_generation(),
        compatibility.graph_generation()
    );
    assert_eq!(explicit.completed_receipt(), &receipt);
    assert_eq!(
        explicit.completed_receipt(),
        compatibility.completed_receipt()
    );
    assert_eq!(
        served_commit_facts(&explicit, &target),
        served_commit_facts(&compatibility, &target)
    );
    assert_eq!(explicit.generation_id(), compatibility.generation_id());
}

#[test]
fn active_manifest_replacement_after_selection_preserves_the_selected_generation() {
    let (_, _, old_records) = referenced_commit_projection(1, true);
    let (_, _, new_records) = referenced_commit_projection(2, true);
    let directory = tempfile::tempdir().expect("manifest selection race directory");
    let root = directory.path();

    let old_segment = write_segment(root, 0x67, old_records, Vec::new());
    publish(root, synthetic_receipt(), vec![old_segment], None);

    let store = SegmentStore::new(root);
    let old_manifest = store
        .load_active()
        .expect("load old manifest")
        .expect("old manifest");
    let mut new_segment = write_segment(root, 0x68, new_records, Vec::new());
    new_segment.publication_generation = 2;
    let receipt = synthetic_receipt();
    let next_manifest = SegmentManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        generation_id: random_generation_id().expect("next manifest generation"),
        prior_generation_id: Some(old_manifest.generation_id.clone()),
        graph_generation: 2,
        materializer_identity: receipt.materializer_revision.clone(),
        core_receipt: receipt.clone(),
        schema_identity: SEGMENT_SCHEMA_IDENTITY.to_owned(),
        evidence_identity: SEGMENT_EVIDENCE_IDENTITY.to_owned(),
        ordering_identity: SEGMENT_ORDERING_IDENTITY.to_owned(),
        segments: vec![new_segment],
        predecessor_segments: old_manifest.segments,
    };
    let candidate = store
        .stage_manifest(&next_manifest)
        .expect("stage next manifest");
    let publication_root = root.to_owned();
    let pinned = FlatStore::new(root)
        .open_active_after_selection_for_test(SegmentGraph::flat_open_policy(), move || {
            SegmentStore::new(publication_root)
                .publish_candidate(candidate)
                .expect("replace active after selection");
        })
        .expect("pin selected descriptors");
    let graph = SegmentGraph::from_pinned(pinned, None);
    assert_eq!(graph.graph_generation(), 1);
    assert_eq!(pin_graph(root).graph_generation(), 2);
    assert_eq!(graph.completed_receipt(), &receipt);
}

#[test]
fn pinned_generation_survives_active_manifest_replacement_without_reopening() {
    let (_, _, old_records) = referenced_commit_projection(1, true);
    let target = ResourceId(old_records[0].subject.graph_id().expect("commit graph ID"));
    let (_, _, new_records) = referenced_commit_projection(2, true);
    let directory = tempfile::tempdir().expect("generation replacement directory");

    let old_segment = write_segment(directory.path(), 0x6b, old_records, Vec::new());
    #[cfg(unix)]
    let old_segment_path = directory.path().join(&old_segment.file_name);
    publish_synthetic(directory.path(), vec![old_segment]);
    let old_graph = pin_graph(directory.path());

    let mut new_segment = write_segment(directory.path(), 0x6c, new_records, Vec::new());
    new_segment.publication_generation = 2;
    publish_synthetic(directory.path(), vec![new_segment]);
    let new_graph = pin_graph(directory.path());

    assert_eq!(old_graph.graph_generation(), 1);
    assert_eq!(new_graph.graph_generation(), 2);
    #[cfg(unix)]
    std::fs::remove_file(old_segment_path).expect("unlink path held by pinned descriptor");
    assert_eq!(served_commit_facts(&old_graph, &target).len(), 1);
    assert_eq!(served_commit_facts(&new_graph, &target).len(), 2);
}

#[cfg(unix)]
#[test]
fn pinned_flat_opener_rejects_insecure_linked_and_indirect_storage() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = |generation_byte: u8| {
        let directory = tempfile::tempdir().expect("private Flat fixture directory");

        let segment = write_segment(
            directory.path(),
            generation_byte.wrapping_add(1),
            Vec::new(),
            Vec::new(),
        );
        let segment_path = directory.path().join(&segment.file_name);
        publish_synthetic(directory.path(), vec![segment]);
        (directory, segment_path)
    };

    let (insecure_directory, insecure_segment) = fixture(0x70);
    std::fs::set_permissions(&insecure_segment, std::fs::Permissions::from_mode(0o644))
        .expect("make Flat segment insecure");
    assert_pin_rejected(insecure_directory.path());

    let (linked_directory, linked_segment) = fixture(0x72);
    std::fs::hard_link(&linked_segment, linked_directory.path().join("second-link"))
        .expect("hard-link Flat segment");
    assert_pin_rejected(linked_directory.path());

    let (indirect_directory, indirect_segment) = fixture(0x74);
    let indirect_target = indirect_directory.path().join("symlink-target");
    std::fs::rename(&indirect_segment, &indirect_target).expect("move Flat segment behind link");
    symlink(&indirect_target, &indirect_segment).expect("replace Flat segment with symlink");
    assert_pin_rejected(indirect_directory.path());
}
