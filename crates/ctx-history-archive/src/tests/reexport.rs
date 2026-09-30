use std::collections::BTreeSet;

use super::*;

fn backup(root: &Path, destination: &Path) -> ExportReceipt {
    export_data_root(root, destination, identity(), &Selection::default()).unwrap()
}

#[test]
fn root_marker_observation_is_read_only_and_rejects_present_invalid_markers() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("root");
    assert!(!is_archive_root(&root).unwrap());
    assert!(!root.exists());
    fs::create_dir(&root).unwrap();
    fs::write(root.join("native-file"), b"untouched").unwrap();
    assert!(!is_archive_root(&root).unwrap());
    for (bytes, valid) in [
        (br#"{"archive_root_version":1}"#.as_slice(), true),
        (br#"{"archive_root_version":2}"#.as_slice(), false),
        (b"{".as_slice(), false),
    ] {
        fs::write(root.join("archive-root.json"), bytes).unwrap();
        let found = is_archive_root(&root);
        if valid {
            assert!(found.unwrap());
        } else {
            assert!(found.is_err());
        }
        assert_eq!(fs::read(root.join("archive-root.json")).unwrap(), bytes);
        assert!(!root.join("restore.lock").exists());
        assert_eq!(fs::read(root.join("native-file")).unwrap(), b"untouched");
    }
    #[cfg(unix)]
    {
        fs::remove_file(root.join("archive-root.json")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("absent"), root.join("archive-root.json"))
            .unwrap();
        assert!(
            is_archive_root(&root).is_err(),
            "a dangling marker is present, not native ownership"
        );
    }
}

#[test]
fn native_backup_restore_reexport_preserves_original_records_and_reimport_is_unchanged() {
    let temp = tempdir().unwrap();
    let native = temp.path().join("native");
    let original = temp.path().join("original");
    let parent = record(&source(), "parent", 1, "original parent");
    let mut child = record(&source(), "child", 2, "original child");
    child.parent_session_id = Some(parent.session_id);
    child.root_session_id = Some(parent.session_id);
    child.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    child.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: parent.session_id,
        ancestor_event_id: parent.event_id,
        proof: ProviderNativeCopyProof::NativeCopiedFromField,
    });
    let mut expected = vec![parent, child];
    publish(&native.join("search/lexical"), &expected, 1);
    let native_export = backup(&native, &original);
    assert_eq!(native_export.manifest.members, 2);
    assert!(native_export.excluded_identities.is_empty());
    assert!(!native.join("archive-root.json").exists());
    expected.sort_by_key(|r| r.event_id.digest());
    assert_eq!(archive_records(&original), expected);

    let root = temp.path().join("restored");
    let first = restore(&original, &root, &options()).unwrap();
    fs::remove_dir_all(&native).unwrap();
    fs::remove_dir_all(&original).unwrap();
    let reexport = temp.path().join("reexport");
    let exported = backup(&root, &reexport);
    assert_eq!(
        exported.manifest.inventory_sha256,
        native_export.manifest.inventory_sha256
    );
    assert_eq!(exported.manifest.identity, identity());
    assert!(exported.excluded_identities.is_empty());
    assert_eq!(archive_records(&reexport), expected);
    let repeated = restore(&reexport, &root, &options()).unwrap();
    assert_eq!(repeated.generation_id, first.generation_id);
    assert_eq!(repeated.imported_members, 0);
    assert_eq!(repeated.unchanged_members, 2);
    assert_eq!(
        VerifiedIndex::open_pinned(root.join("search/lexical"))
            .unwrap()
            .document_count(),
        2
    );
    let second_root = temp.path().join("second-restore");
    restore(&reexport, &second_root, &options()).unwrap();
    let final_export = temp.path().join("final-export");
    backup(&second_root, &final_export);
    assert_eq!(archive_records(&final_export), expected);
}

#[test]
fn only_committed_revisions_are_reexported_and_missing_sessions_remain_additive() {
    let temp = tempdir().unwrap();
    let original = record(&source(), "corrected", 1, "before");
    let sibling = record(&source(), "sibling", 2, "retained sibling");
    let archive = archive(temp.path(), &[original.clone(), sibling.clone()]);
    let root = temp.path().join("restored");
    restore(&archive, &root, &options()).unwrap();
    let old = members(&archive)
        .into_iter()
        .find(|m| m.session_id == original.session_id)
        .unwrap();
    let mut corrected = original.clone();
    corrected.content.normalized_body = Some("after".into());
    publish(
        &temp.path().join("producer-index"),
        std::slice::from_ref(&corrected),
        2,
    );
    let update = temp.path().join("update");
    export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &update,
        identity(),
        &Selection::default(),
    )
    .unwrap();
    assert!(matches!(
        restore(&update, &root, &options()),
        Err(ArchiveError::Conflict { .. })
    ));
    assert_eq!(
        fs::read_dir(root.join("archives")).unwrap().count(),
        2,
        "the rejected successor left a real owned copy, not committed membership"
    );
    let rejected_export = temp.path().join("after-rejection");
    backup(&root, &rejected_export);
    assert_eq!(archive_records(&rejected_export), archive_records(&archive));
    let mut authorized = options();
    authorized
        .expected_predecessors
        .insert(old.path, old.sha256);
    restore(&update, &root, &authorized).unwrap();
    let current = temp.path().join("current");
    backup(&root, &current);
    let mut expected = vec![corrected, sibling];
    expected.sort_by_key(|r| r.event_id.digest());
    assert_eq!(archive_records(&current), expected);
    assert_eq!(
        restore(&current, &root, &options())
            .unwrap()
            .unchanged_members,
        2
    );
}

#[test]
fn mixed_origins_export_explicitly_without_relabeling_or_changing_either_import() {
    let temp = tempdir().unwrap();
    let records = [record(&source(), "session", 1, "retained")];
    let a = archive(temp.path(), &records);
    let b = temp.path().join("other-origin");
    let mut other = identity();
    other.origin = "synthetic-machine-b".into();
    export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &b,
        other.clone(),
        &Selection::default(),
    )
    .unwrap();
    let root = temp.path().join("root");
    restore(&a, &root, &options()).unwrap();
    let mut other_options = options();
    other_options.binding.identity = other.clone();
    restore(&b, &root, &other_options).unwrap();
    let selected_a = backup(&root, &temp.path().join("selected-a"));
    assert_eq!(selected_a.excluded_identities, vec![other.clone()]);
    assert!(selected_a.manifest.selected_subset);
    assert_eq!(selected_a.manifest.members, 1);
    let selected_b = export_data_root(
        &root,
        &temp.path().join("selected-b"),
        other.clone(),
        &Selection::default(),
    )
    .unwrap();
    assert_eq!(selected_b.manifest.identity, other);
    assert_eq!(selected_b.excluded_identities, vec![identity()]);
    let mut nonexistent = identity();
    nonexistent.view = "unimported-view".into();
    let relabeled = temp.path().join("relabeled");
    assert!(export_data_root(&root, &relabeled, nonexistent, &Selection::default()).is_err());
    assert!(!relabeled.exists());
    assert_eq!(restore(&a, &root, &options()).unwrap().unchanged_members, 1);
    assert_eq!(
        restore(&b, &root, &other_options)
            .unwrap()
            .unchanged_members,
        1
    );
    assert_eq!(
        VerifiedIndex::open_pinned(root.join("search/lexical"))
            .unwrap()
            .document_count(),
        2
    );
}

#[test]
fn namespaces_coalesce_identical_members_but_conflicts_require_local_core_selection() {
    let temp = tempdir().unwrap();
    let original = record(&source(), "session", 1, "first version");
    let archive = archive(temp.path(), std::slice::from_ref(&original));
    let old = members(&archive).pop().unwrap();
    let root = temp.path().join("root");
    restore(&archive, &root, &options()).unwrap();
    let mut second = options();
    // A supported maximum-length binding must not overflow Core's observation
    // metadata bound: the committed pointer does not duplicate the binding.
    second.binding.namespace = "n".repeat(4096);
    restore(&archive, &root, &second).unwrap();
    assert_eq!(
        backup(&root, &temp.path().join("coalesced"))
            .manifest
            .members,
        1
    );
    let mut corrected = original.clone();
    corrected.content.normalized_body = Some("second version".into());
    publish(
        &temp.path().join("producer-index"),
        std::slice::from_ref(&corrected),
        2,
    );
    let update = temp.path().join("update");
    export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &update,
        identity(),
        &Selection::default(),
    )
    .unwrap();
    second
        .expected_predecessors
        .insert(old.path.clone(), old.sha256.clone());
    restore(&update, &root, &second).unwrap();
    let ambiguous = temp.path().join("ambiguous");
    let error = export_data_root(&root, &ambiguous, identity(), &Selection::default()).unwrap_err();
    assert!(error.to_string().contains("--source"));
    assert!(!ambiguous.exists());
    for (name, binding, expected) in [
        ("first", options().binding, original),
        ("second", second.binding.clone(), corrected.clone()),
    ] {
        let source = mapped_source(&binding, &old).unwrap();
        let selection = Selection {
            sources: [hex(&source.identity().digest())].into_iter().collect(),
            sessions: BTreeSet::new(),
        };
        let selected = temp.path().join(name);
        export_data_root(&root, &selected, identity(), &selection).unwrap();
        assert_eq!(archive_records(&selected), vec![expected]);
    }
    let selected = temp.path().join("by-local-session");
    let selection = Selection {
        sources: BTreeSet::new(),
        sessions: [hex(&mapped_session(&second.binding, old.session_id)
            .unwrap()
            .digest())]
        .into_iter()
        .collect(),
    };
    export_data_root(&root, &selected, identity(), &selection).unwrap();
    assert_eq!(archive_records(&selected), vec![corrected]);
    assert_eq!(
        VerifiedIndex::open_pinned(root.join("search/lexical"))
            .unwrap()
            .document_count(),
        2
    );
}

#[test]
fn missing_or_corrupt_committed_original_never_falls_back_to_the_projection() {
    let temp = tempdir().unwrap();
    let archive = archive(temp.path(), &[record(&source(), "session", 1, "original")]);
    let member = members(&archive).pop().unwrap();
    let root = temp.path().join("restored");
    let receipt = restore(&archive, &root, &options()).unwrap();
    let original = root
        .join("archives")
        .join(receipt.snapshot_id)
        .join(&member.path);
    let bytes = fs::read(&original).unwrap();
    fs::remove_file(&original).unwrap();
    let output = temp.path().join("export");
    assert!(export_data_root(&root, &output, identity(), &Selection::default()).is_err());
    assert!(!output.exists());
    let mut damaged = bytes.clone();
    damaged[0] ^= 1;
    fs::write(&original, damaged).unwrap();
    assert!(export_data_root(&root, &output, identity(), &Selection::default()).is_err());
    assert!(!output.exists());
    fs::write(&original, bytes).unwrap();
    backup(&root, &output);
    assert_eq!(archive_records(&output), archive_records(&archive));
}

#[test]
fn committed_pointers_require_bounded_owned_paths_and_original_revision_proof() {
    let temp = tempdir().unwrap();
    let original = record(&source(), "session", 1, "retained original");
    let archive = archive(temp.path(), std::slice::from_ref(&original));
    let member = members(&archive).pop().unwrap();
    let root = temp.path().join("restored");
    let receipt = restore(&archive, &root, &options()).unwrap();
    let index = VerifiedIndex::open_pinned(root.join("search/lexical")).unwrap();
    let certificate = index.manifest().sources[0].clone();
    let mapped = map_record(&options().binding, &member, original).unwrap();
    let output = temp.path().join("backup");

    // Losing the root marker must not select the lossy native-export branch.
    let marker = fs::read(root.join("archive-root.json")).unwrap();
    fs::remove_file(root.join("archive-root.json")).unwrap();
    assert!(export_data_root(&root, &output, identity(), &Selection::default()).is_err());
    assert!(!output.exists());
    fs::write(root.join("archive-root.json"), marker).unwrap();

    let pointer = |snapshot: &str, path: &str| {
        serde_json::to_vec(&crate::retained::OwnedMember {
            snapshot: snapshot.to_owned(),
            member: path.to_owned(),
        })
        .unwrap()
    };
    let commit = |kind: &str, revision: Vec<u8>, digest: [u8; 32]| {
        let mut writer = GenerationWriter::open(root.join("search/lexical"), writer_options())
            .unwrap()
            .into_writer()
            .unwrap();
        writer.begin_source(mapped.source.clone()).unwrap();
        writer.add_core_record(mapped.clone()).unwrap();
        let observation = SourceObservation::new(mapped.source.clone(), kind, revision).unwrap();
        writer
            .certify_source(
                CertifiedSource::certify(
                    observation.clone(),
                    observation,
                    certificate.parser_revision(),
                    digest,
                    certificate.counts(),
                )
                .unwrap(),
            )
            .unwrap();
        writer.commit(|_| true).unwrap();
    };
    let owned_kind = crate::retained::OBSERVATION_KIND;
    let digest = *certificate.content_digest();
    for (kind, revision, digest) in [
        ("synthetic-missing-proof", vec![1], digest),
        (owned_kind, pointer("../outside", &member.path), digest),
        (
            owned_kind,
            pointer(&receipt.snapshot_id, "../outside.jsonl"),
            digest,
        ),
        (owned_kind, pointer(&"0".repeat(64), &member.path), digest),
        (
            owned_kind,
            pointer(
                &receipt.snapshot_id,
                &format!("members/{}.jsonl", "0".repeat(64)),
            ),
            digest,
        ),
        (
            owned_kind,
            pointer(&receipt.snapshot_id, &member.path),
            [0; 32],
        ),
    ] {
        commit(kind, revision, digest);
        assert!(export_data_root(&root, &output, identity(), &Selection::default()).is_err());
        assert!(!output.exists());
    }
    commit(
        owned_kind,
        certificate.observation().revision().to_vec(),
        digest,
    );
    backup(&root, &output);
    assert_eq!(archive_records(&output), archive_records(&archive));
}
