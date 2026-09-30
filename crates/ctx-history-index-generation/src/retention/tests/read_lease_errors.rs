use super::*;

#[test]
fn missing_delta_base_is_distinct_from_an_unretained_generation() {
    let root = tempdir().unwrap();
    let base = create_slot(root.path(), 'a');
    let delta_bytes = format!(
        r#"{{"storage_format":"ctx-manifest-flat-delta-v1","base_generation_id":"{}"}}"#,
        base.generation_id()
    );
    let previous = create_slot_with_bytes(root.path(), 'b', delta_bytes.as_bytes());
    let active = create_slot(root.path(), 'c');
    let pointer = ActiveGenerationPointer::new(active.clone(), Some(previous.clone())).unwrap();
    publish_active_generation_pointer(root.path(), &pointer).unwrap();

    // A delta may depend on a manifest outside the pointer pair. Ordinary
    // reclamation must retain that dependency before any corruption is injected.
    let retained = vec![
        active.generation_id().to_owned(),
        previous.generation_id().to_owned(),
    ];
    reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
    assert!(manifest_path(root.path(), base.generation_id()).is_file());
    drop(acquire_generation_read_lease(root.path(), previous.generation_id()).unwrap());
    assert!(matches!(
        acquire_generation_read_lease(root.path(), base.generation_id()),
        Err(IndexError::GenerationRetentionLeaseTargetNotRetained { requested_generation_id })
            if requested_generation_id == base.generation_id()
    ));

    fs::remove_file(manifest_path(root.path(), base.generation_id())).unwrap();
    assert!(matches!(
        acquire_generation_read_lease(root.path(), previous.generation_id()),
        Err(IndexError::MissingManifest(generation_id)) if generation_id == base.generation_id()
    ));
    let active_lease = acquire_generation_read_lease(root.path(), active.generation_id()).unwrap();
    assert_eq!(active_lease.generation_id(), active.generation_id());
    assert_eq!(
        load_active_generation_pointer(root.path()).unwrap(),
        Some(pointer)
    );
    assert!(!manifest_path(root.path(), base.generation_id()).exists());
    assert!(manifest_path(root.path(), previous.generation_id()).is_file());
}

#[test]
fn generation_read_lease_preserves_manifest_open_errors() {
    let root = tempdir().unwrap();
    let active = create_slot(root.path(), 'a');
    let pointer = ActiveGenerationPointer::new(active.clone(), None).unwrap();
    publish_active_generation_pointer(root.path(), &pointer).unwrap();
    let manifest = manifest_path(root.path(), active.generation_id());
    fs::remove_file(&manifest).unwrap();
    fs::create_dir(&manifest).unwrap();

    assert!(matches!(
        acquire_generation_read_lease(root.path(), active.generation_id()),
        Err(IndexError::Io(error)) if error.kind() != std::io::ErrorKind::NotFound
    ));
    assert!(manifest.is_dir());
}

#[test]
fn generation_read_lease_preserves_directory_open_errors() {
    let root = tempdir().unwrap();
    let active = create_slot(root.path(), 'a');
    let pointer = ActiveGenerationPointer::new(active.clone(), None).unwrap();
    publish_active_generation_pointer(root.path(), &pointer).unwrap();
    let directory = slot_path(root.path(), &active);
    fs::remove_dir(&directory).unwrap();
    fs::write(&directory, b"not a generation directory").unwrap();

    assert!(matches!(
        acquire_generation_read_lease(root.path(), active.generation_id()),
        Err(IndexError::Io(error)) if error.kind() != std::io::ErrorKind::NotFound
    ));
    assert_eq!(fs::read(directory).unwrap(), b"not a generation directory");
}
