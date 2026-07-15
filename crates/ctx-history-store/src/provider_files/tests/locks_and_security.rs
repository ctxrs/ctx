#[cfg(unix)]
#[test]
fn existing_owner_lock_directory_must_already_be_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let lock_dir = temp.path().join("owner-locks");
    std::fs::create_dir(&lock_dir).unwrap();
    std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

    let error = create_or_validate_private_lock_dir(&lock_dir).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

#[cfg(windows)]
#[test]
fn windows_private_paths_validate_exact_acl_and_reject_wrong_object_type() {
    let temp = tempdir().unwrap();
    let directory = temp.path().join("private-directory");
    create_private_staging_dir(&directory).unwrap();
    validate_existing_private_windows_path(&directory, true).unwrap();
    assert!(validate_existing_private_windows_path(&directory, false).is_err());

    let file_path = directory.join("private-file");
    drop(create_private_staging_file(&file_path).unwrap());
    validate_existing_private_windows_path(&file_path, false).unwrap();
    assert!(validate_existing_private_windows_path(&file_path, true).is_err());
}

#[cfg(unix)]
#[test]
fn unix_owner_lock_rejects_symlink_hardlink_permissive_owner_and_inode_swap() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let temp = tempdir().unwrap();
    let target = temp.path().join("target.lock");
    std::fs::write(&target, b"").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    let symlink_path = temp.path().join("symlink.lock");
    symlink(&target, &symlink_path).unwrap();
    assert!(open_private_owner_lock_file(&symlink_path).is_err());

    let hardlink_path = temp.path().join("hardlink.lock");
    std::fs::hard_link(&target, &hardlink_path).unwrap();
    assert!(open_private_owner_lock_file(&target).is_err());
    std::fs::remove_file(&hardlink_path).unwrap();

    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(open_private_owner_lock_file(&target).is_err());
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    if unsafe { libc::geteuid() } == 0 {
        let changed = unsafe {
            libc::chown(
                std::ffi::CString::new(target.as_os_str().as_encoded_bytes())
                    .unwrap()
                    .as_ptr(),
                65_534,
                65_534,
            )
        };
        assert_eq!(changed, 0);
        assert!(open_private_owner_lock_file(&target).is_err());
        let _ = unsafe {
            libc::chown(
                std::ffi::CString::new(target.as_os_str().as_encoded_bytes())
                    .unwrap()
                    .as_ptr(),
                0,
                0,
            )
        };
    }

    let opened = open_private_owner_lock_file(&target).unwrap();
    let displaced = temp.path().join("displaced.lock");
    std::fs::rename(&target, &displaced).unwrap();
    std::fs::write(&target, b"").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(validate_open_private_owner_lock_file(&opened, &target).is_err());
}

#[cfg(unix)]
#[test]
fn non_utf8_private_root_does_not_block_main_database_staging() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let file = source_file(20, 100);
    let retained_event = Uuid::from_u128(69_900);
    let generation = {
        let store = Store::open(&path).unwrap();
        let generation = store
            .allocate_source_import_inventory_generation(file.provider, &file.source_root)
            .unwrap();
        store
            .upsert_source_import_files(generation, std::slice::from_ref(&file))
            .unwrap();
        let source = Uuid::from_u128(69_901);
        insert_capture_source(&store, source, PATH_A, "non-utf8-staging-root");
        insert_raw_event(&store, retained_event, 1, source, "retained visible event");
        generation
    };
    let invalid_temp_root = temp
        .path()
        .join(OsString::from_vec(b"non-utf8-\xff".to_vec()));
    std::fs::create_dir(&invalid_temp_root).unwrap();

    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--exact")
        .arg("provider_files::tests::provider_file_subprocess_helper")
        .arg("--test-threads=1")
        .env("CTX_PROVIDER_FILE_HELPER_ACTION", "non-utf8-private-root")
        .env("CTX_PROVIDER_FILE_HELPER_STORE", &path)
        .env(
            "CTX_PROVIDER_FILE_HELPER_GENERATION",
            generation.to_string(),
        )
        .env("TMPDIR", &invalid_temp_root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "subprocess failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let reopened = Store::open(&path).unwrap();
    assert!(!reopened.has_pending_provider_file_publications().unwrap());
    assert!(row_exists(&reopened, "events", retained_event));
    assert_eq!(reopened.list_events().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn canonical_store_lock_identity_survives_rename_hardlink_and_symlink_aliases() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let original = temp.path().join("store.sqlite");
    std::fs::write(&original, b"identity").unwrap();
    let hardlink = temp.path().join("hardlink.sqlite");
    std::fs::hard_link(&original, &hardlink).unwrap();
    let symlink_path = temp.path().join("symlink.sqlite");
    symlink(&hardlink, &symlink_path).unwrap();
    let first =
        crate::store_identity::CanonicalStoreIdentity::open_target(&original, false).unwrap();
    let hardlinked =
        crate::store_identity::CanonicalStoreIdentity::open_target(&hardlink, false).unwrap();
    let symlinked =
        crate::store_identity::CanonicalStoreIdentity::open_target(&symlink_path, false).unwrap();
    assert_eq!(first.digest(), hardlinked.digest());
    assert_eq!(first.digest(), symlinked.digest());
    assert_eq!(first.private_root(), hardlinked.private_root());
    let renamed = temp.path().join("renamed.sqlite");
    std::fs::rename(&original, &renamed).unwrap();
    let moved =
        crate::store_identity::CanonicalStoreIdentity::open_target(&renamed, false).unwrap();
    assert_eq!(first.digest(), moved.digest());
}

#[cfg(unix)]
#[test]
fn subprocess_owner_lock_excludes_aliases_and_releases_on_process_exit() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let original = temp.path().join("store.sqlite");
    std::fs::write(&original, b"identity").unwrap();
    let hardlink = temp.path().join("hardlink.sqlite");
    std::fs::hard_link(&original, &hardlink).unwrap();
    let symlink_path = temp.path().join("symlink.sqlite");
    symlink(&hardlink, &symlink_path).unwrap();
    let ready = temp.path().join("ready");
    let release = temp.path().join("release");
    let mut holder =
        spawn_provider_file_helper("hold-lock", &original, Some(&ready), Some(&release), None);
    wait_for_path(&ready);
    let renamed = temp.path().join("renamed.sqlite");
    std::fs::rename(&original, &renamed).unwrap();
    for alias in [&hardlink, &symlink_path, &renamed] {
        let status = spawn_provider_file_helper("try-lock", alias, None, None, None)
            .wait()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(23),
            "alias {} escaped the lock",
            alias.display()
        );
    }
    std::fs::write(&release, b"release").unwrap();
    assert!(holder.wait().unwrap().success());
    assert!(
        spawn_provider_file_helper("try-lock", &renamed, None, None, None)
            .wait()
            .unwrap()
            .success()
    );

    let bootstrap = temp.path().join("bootstrap.sqlite");
    let first_digest = temp.path().join("first-digest");
    let second_digest = temp.path().join("second-digest");
    let mut first_creator = spawn_provider_file_helper(
        "create-identity",
        &bootstrap,
        Some(&first_digest),
        None,
        None,
    );
    let mut second_creator = spawn_provider_file_helper(
        "create-identity",
        &bootstrap,
        Some(&second_digest),
        None,
        None,
    );
    assert!(first_creator.wait().unwrap().success());
    assert!(second_creator.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(first_digest).unwrap(),
        std::fs::read_to_string(second_digest).unwrap()
    );
}

#[test]
fn subprocess_partial_cleanup_supersession_stays_hidden_and_is_adopted() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("work.sqlite");
    let store = Store::open(&path).unwrap();
    let original = source_file(20, 100);
    let generation = store
        .allocate_source_import_inventory_generation(original.provider, &original.source_root)
        .unwrap();
    store
        .upsert_source_import_files(generation, std::slice::from_ref(&original))
        .unwrap();
    let source = Uuid::from_u128(44_000);
    let first = Uuid::from_u128(44_001);
    let second = Uuid::from_u128(44_002);
    insert_capture_source(&store, source, PATH_A, "subprocess-partial");
    insert_raw_event(&store, first, 1, source, "first stale event");
    insert_raw_event(&store, second, 2, source, "second stale event");
    store.rebuild_search_projection().unwrap();
    drop(store);

    let ready = temp.path().join("partial-ready");
    let status = spawn_provider_file_helper(
        "partial-crash",
        &path,
        Some(&ready),
        None,
        Some((generation, first)),
    )
    .wait()
    .unwrap();
    assert_eq!(status.code(), Some(29));
    wait_for_path(&ready);

    let store = Store::open(&path).unwrap();
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(!row_exists(&store, "events", first));
    assert!(row_exists(&store, "events", second));
    assert!(store.list_events().unwrap().is_empty());
    assert!(store.search_event_hits("stale", 10).unwrap().is_empty());

    let rewritten = source_file(30, 130);
    let newest_generation = store
        .allocate_source_import_inventory_generation(rewritten.provider, &rewritten.source_root)
        .unwrap();
    store
        .upsert_source_import_files(newest_generation, std::slice::from_ref(&rewritten))
        .unwrap();
    assert!(store.has_pending_provider_file_publications().unwrap());
    assert!(store.list_events().unwrap().is_empty());
    let outcome = source_outcome(&rewritten, newest_generation, 140);
    let adopted = store
        .begin_provider_file_publication(
            rewritten.provider,
            outcome.observation,
            MATERIAL_FORMAT,
            ProviderFilePublicationKind::Incremental,
            135,
        )
        .unwrap();
    assert_eq!(adopted.kind(), ProviderFilePublicationKind::Replacement);
    reconcile_all(&store, &adopted, 1);
    store
        .finalize_provider_file_publication(
            adopted,
            outcome,
            ProviderFilePublicationCommit::Replacement(None),
        )
        .unwrap();
    assert!(!store.has_pending_provider_file_publications().unwrap());
    assert!(store.list_events().unwrap().is_empty());
}
