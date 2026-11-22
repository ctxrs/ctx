use super::*;

fn restore_version(
    working: &Path,
    root: &Path,
    revision: usize,
    records: &[CoreRecord],
    previous: Option<&SessionMember>,
) -> (RestoreReceipt, SessionMember) {
    let source = working.join(format!("source-{revision}"));
    fs::create_dir(&source).unwrap();
    let input = archive(&source, records);
    let mut restore_options = options();
    if let Some(previous) = previous {
        restore_options
            .expected_predecessors
            .insert(previous.path.clone(), previous.sha256.clone());
    }
    let receipt = restore(&input, root, &restore_options).unwrap();
    (receipt, members(&input).into_iter().next().unwrap())
}

#[test]
fn pruning_keeps_the_recovery_and_reader_generations_and_preserves_exports() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("archive-root");
    let lexical = root.join("search/lexical");
    let retained = root.join("archives");
    let source = source();
    let mut events = vec![record(&source, "growing", 1, "original")];
    let (old, first_member) = restore_version(temp.path(), &root, 1, &events, None);
    let reader = VerifiedIndex::open_pinned(&lexical).unwrap();

    events.push(record(&source, "growing", 2, "second"));
    let (previous, second_member) =
        restore_version(temp.path(), &root, 2, &events, Some(&first_member));
    assert_eq!(prune(&root, false).unwrap().eligible_snapshots, 0);

    events.push(record(&source, "growing", 3, "third"));
    let (current, _) = restore_version(temp.path(), &root, 3, &events, Some(&second_member));
    let deferred = prune(&root, true).unwrap();
    assert!(deferred.deferred_generations);
    assert!(retained.join(&old.snapshot_id).is_dir());
    let pinned_export = temp.path().join("pinned-export");
    crate::retained::export_pinned(
        &root,
        &reader,
        &pinned_export,
        identity(),
        &crate::selection::SessionSelection::explicit(&Selection::default()),
    )
    .unwrap();
    assert_eq!(archive_records(&pinned_export), events[..1]);
    drop(reader);

    let before_path = temp.path().join("before");
    let before = export_data_root(&root, &before_path, identity(), &Selection::default()).unwrap();
    let preview = prune(&root, false).unwrap();
    assert_eq!(preview.eligible_snapshots, 1);
    assert_eq!(preview.reclaimed_snapshots, 0);
    assert!(retained.join(&old.snapshot_id).is_dir());
    let removal = prune(&root, true).unwrap();
    assert_eq!(removal.reclaimed_snapshots, 1);
    assert_eq!(removal.reclaimed_bytes, preview.eligible_bytes);
    assert!(!retained.join(&old.snapshot_id).exists());
    assert!(retained.join(&previous.snapshot_id).is_dir());
    assert!(retained.join(&current.snapshot_id).is_dir());
    let after_path = temp.path().join("after");
    let after = export_data_root(&root, &after_path, identity(), &Selection::default()).unwrap();
    assert_eq!(
        after.manifest.inventory_sha256,
        before.manifest.inventory_sha256
    );
    assert_eq!(archive_records(&after_path), archive_records(&before_path));
    assert_eq!(prune(&root, true).unwrap().reclaimed_snapshots, 0);
}

#[test]
fn pruning_preserves_unreferenced_records_that_do_not_exist_in_the_current_view() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("archive-root");
    let source = source();
    let old = record(&source, "edited", 1, "content unique to the old revision");
    let (first, member) = restore_version(temp.path(), &root, 1, std::slice::from_ref(&old), None);
    let mut edited = old;
    edited.content.normalized_body = Some("different content".into());
    let (_, edited_member) = restore_version(
        temp.path(),
        &root,
        2,
        std::slice::from_ref(&edited),
        Some(&member),
    );
    let events = [edited, record(&source, "edited", 2, "new event")];
    restore_version(temp.path(), &root, 3, &events, Some(&edited_member));
    assert_eq!(prune(&root, true).unwrap().reclaimed_snapshots, 0);
    assert!(root.join("archives").join(first.snapshot_id).is_dir());
}

#[test]
fn an_interrupted_staged_prune_resumes_without_changing_the_active_export() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("archive-root");
    let archives = root.join("archives");
    let source = source();
    let mut events = vec![record(&source, "growing", 1, "one")];
    let (old, first) = restore_version(temp.path(), &root, 1, &events, None);
    events.push(record(&source, "growing", 2, "two"));
    let (_, second) = restore_version(temp.path(), &root, 2, &events, Some(&first));
    events.push(record(&source, "growing", 3, "three"));
    restore_version(temp.path(), &root, 3, &events, Some(&second));
    assert_eq!(prune(&root, false).unwrap().eligible_snapshots, 1);

    let staged = archives.join(".prune");
    ctx_history_platform::platform_security::ensure_private_directory(&staged).unwrap();
    let pending = staged.join(&old.snapshot_id);
    fs::rename(archives.join(&old.snapshot_id), &pending).unwrap();
    fs::remove_file(pending.join("inventory.jsonl")).unwrap();
    assert_eq!(prune(&root, true).unwrap().reclaimed_snapshots, 1);
    assert!(!staged.exists());
    let exported = temp.path().join("reexport");
    export_data_root(&root, &exported, identity(), &Selection::default()).unwrap();
    events.sort_by_key(|event| event.event_id.digest());
    assert_eq!(archive_records(&exported), events);
}

#[test]
fn native_roots_are_refused_before_a_prune_directory_is_created() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("missing-root");
    assert!(prune(&path, true).is_err());
    assert!(!path.exists());
}
