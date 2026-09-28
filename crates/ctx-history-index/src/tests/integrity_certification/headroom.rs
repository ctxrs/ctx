use super::*;

fn clone_bytes(root: &Path) -> (u64, u64) {
    let generation = active_generation_path(root);
    let mut logical = 0;
    let mut controls = 0;
    for entry in fs::read_dir(generation).unwrap() {
        let entry = entry.unwrap();
        let bytes = entry.metadata().unwrap().len();
        logical += bytes;
        if matches!(
            entry.file_name().to_str(),
            Some("meta.json" | ".managed.json")
        ) {
            controls += bytes;
        }
    }
    (logical, controls)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn incremental_append_needs_controls_and_writer_scratch_not_a_second_base() {
    let (temp, source, baseline) = published_fixture("incremental-headroom.jsonl");
    let pinned = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let (logical, controls) = clone_bytes(temp.path());
    let available = WriterOptions::default().memory_bytes as u64 + 16 * 1024 * 1024 + controls;
    assert!(logical > controls);
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            available_bytes: Some(available),
            rechecked_available_bytes: Some(available),
            ..Default::default()
        },
        |_, _| Ok(()),
    );
    let successor = append_one_record(temp.path(), &source).unwrap();
    let metrics = guard.metrics();
    drop(guard);
    assert!(metrics.required_headroom <= available);
    assert_ne!(successor.generation_id, baseline.generation_id);
    assert_eq!(pinned.count_term("body").unwrap(), 1);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
    assert_eq!(
        crate::publication::candidate_clone_metrics().retained_copied_bytes,
        0
    );
}

#[test]
fn portable_append_reserves_one_copy_plus_writer_scratch() {
    let (temp, source, baseline) = published_fixture("portable-headroom.jsonl");
    let (logical, _) = clone_bytes(temp.path());
    let available = logical + WriterOptions::default().memory_bytes as u64 + 16 * 1024 * 1024;
    let guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions {
            available_bytes: Some(available),
            rechecked_available_bytes: Some(available),
        },
        |_, _| Ok(()),
    );
    let successor = append_one_record(temp.path(), &source).unwrap();
    assert!(guard.metrics().required_headroom <= available);
    drop(guard);
    assert_ne!(successor.generation_id, baseline.generation_id);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn copy_fallback_append_reserves_one_copy_plus_writer_scratch() {
    let (temp, source, _) = published_fixture("copy-headroom.jsonl");
    let (logical, _) = clone_bytes(temp.path());
    let available = logical + WriterOptions::default().memory_bytes as u64 + 16 * 1024 * 1024;
    let guard = CloneTestHookGuard::set(
        CloneTestOptions {
            force_reflink_fallback: true,
            force_hardlink_fallback: true,
            available_bytes: Some(available),
            rechecked_available_bytes: Some(available),
            ..Default::default()
        },
        |_, _| Ok(()),
    );
    append_one_record(temp.path(), &source).unwrap();
    drop(guard);
    assert!(crate::publication::candidate_clone_metrics().retained_copied_bytes > 0);
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        2
    );
}

#[test]
fn low_space_preflight_reclaims_only_obsolete_unleased_generations() {
    let (temp, source, _) = published_fixture("low-space-reclamation.jsonl");
    let pinned = VerifiedIndex::open_pinned(temp.path()).unwrap();
    let oldest = active_generation_path(temp.path());
    append_one_record(temp.path(), &source).unwrap();
    append_record(temp.path(), &source, 3).unwrap();
    assert!(oldest.exists(), "reader lease must survive publication GC");
    let pointer = fs::read(temp.path().join("active-generation.json")).unwrap();
    let guard = PortableCloneTestGuard::set(
        PortableCloneTestOptions {
            available_bytes: Some(0),
            rechecked_available_bytes: Some(0),
        },
        |_, _| Ok(()),
    );
    assert!(matches!(
        append_record(temp.path(), &source, 4),
        Err(IndexError::CurrentRepublishInsufficientHeadroom { .. })
    ));
    assert!(oldest.exists());
    assert_eq!(pinned.count_term("body").unwrap(), 1);
    drop(pinned);
    assert!(matches!(
        append_record(temp.path(), &source, 4),
        Err(IndexError::CurrentRepublishInsufficientHeadroom { .. })
    ));
    drop(guard);
    assert!(
        !oldest.exists(),
        "writer preflight retries GC after the reader leaves"
    );
    assert_eq!(
        fs::read(temp.path().join("active-generation.json")).unwrap(),
        pointer
    );
    assert_eq!(
        VerifiedIndex::open_pinned(temp.path())
            .unwrap()
            .count_term("body")
            .unwrap(),
        3
    );
}
