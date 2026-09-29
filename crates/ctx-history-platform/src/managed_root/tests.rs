use super::*;

const ID: &str = "96f2c8b7-4696-4c11-8941-7b97cf3f88a5";

fn root_with_identity(path: &Path) {
    security::create_private_directory_all(path).unwrap();
    let mut file = security::create_private_file_new(&path.join("install.json")).unwrap();
    write!(
        file,
        r#"{{"schema_version":1,"install_id":"{ID}","created_at":"2026-01-01T00:00:00Z"}}"#
    )
    .unwrap();
}

#[test]
fn absent_locator_is_read_only_and_defaults_to_platform_home() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(resolve_in(home.path()).unwrap(), home.path().join(".ctx"));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn activation_preserves_identity_and_rejects_unavailable_or_foreign_destination() {
    let home = tempfile::tempdir().unwrap();
    let destination = home.path().join("volume with spaces/data");
    root_with_identity(&destination);
    let mut moving = ManagedRootMove::acquire_in(home.path().join(".ctx-control")).unwrap();
    moving.drain().unwrap();
    moving.activate(&destination, ID).unwrap();
    assert_eq!(resolve_in(home.path()).unwrap(), destination);
    fs::rename(&destination, destination.with_file_name("unmounted")).unwrap();
    assert!(resolve_in(home.path()).is_err());
    security::create_private_directory_all(&destination).unwrap();
    assert!(resolve_in(home.path()).is_err());
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
}

#[test]
fn a_second_activation_replaces_the_existing_locator() {
    let home = tempfile::tempdir().unwrap();
    let first = home.path().join("first");
    let second = home.path().join("second");
    root_with_identity(&first);
    root_with_identity(&second);
    let mut moving = ManagedRootMove::acquire_in(home.path().join(".ctx-control")).unwrap();
    moving.drain().unwrap();
    moving.activate(&first, ID).unwrap();
    moving.activate(&second, ID).unwrap();
    assert_eq!(resolve_in(home.path()).unwrap(), second);
    assert!(first.join("install.json").is_file());
}

#[cfg(unix)]
#[test]
fn native_directory_sync_succeeds_and_reports_missing_directories() {
    let directory = tempfile::tempdir().unwrap();
    sync_directory(directory.path()).unwrap();
    assert_eq!(
        sync_directory(&directory.path().join("missing"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn stable_admission_drains_existing_users_and_blocks_new_ones() {
    let home = tempfile::tempdir().unwrap();
    let control = home.path().join(".ctx-control");
    let user = ManagedRootUse::acquire_in(&control).unwrap();
    let mut moving = ManagedRootMove::acquire_in(control.clone()).unwrap();
    assert!(moving
        .drain_with_timeout(std::time::Duration::ZERO)
        .is_err());
    assert!(ManagedRootMove::acquire_in(control.clone()).is_err());
    assert!(ManagedRootUse::acquire_in(&control).is_err());
    drop(user);
    moving.drain().unwrap();
    drop(moving);
    assert!(ManagedRootUse::acquire_in(&control).is_ok());
}

#[test]
fn malformed_unbounded_and_unknown_locator_fields_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let control = home.path().join(".ctx-control");
    security::create_private_directory_all(&control).unwrap();
    for bytes in [
        b"{}".to_vec(),
        vec![b' '; LOCATOR_LIMIT as usize + 1],
        format!(r#"{{"schema_version":1,"path":"relative","install_id":"{ID}"}}"#).into_bytes(),
        format!(r#"{{"schema_version":1,"path":"/missing","install_id":"{ID}","extra":true}}"#)
            .into_bytes(),
    ] {
        let path = control.join("data-root.json");
        let mut file = security::create_private_file_new(&path).unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);
        assert!(resolve_in(home.path()).is_err());
        fs::remove_file(path).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn locator_and_target_links_are_rejected() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let control = home.path().join(".ctx-control");
    security::create_private_directory_all(&control).unwrap();
    let outside = home.path().join("outside");
    fs::write(&outside, "{}").unwrap();
    symlink(&outside, control.join("data-root.json")).unwrap();
    assert!(resolve_in(home.path()).is_err());
    fs::remove_file(control.join("data-root.json")).unwrap();
    let target = home.path().join("target");
    root_with_identity(&target);
    let alias = home.path().join("alias");
    symlink(&target, &alias).unwrap();
    let mut moving = ManagedRootMove::acquire_in(control).unwrap();
    moving.drain().unwrap();
    assert!(moving.activate(&alias, ID).is_err());
    assert!(!home.path().join(".ctx-control/data-root.json").exists());
}

#[test]
fn retired_managed_identity_is_fenced_without_blocking_independent_custom_roots() {
    let home = tempfile::tempdir().unwrap();
    let control = home.path().join(".ctx-control");
    let old = home.path().join(".ctx");
    let active = home.path().join("active");
    let custom = home.path().join("custom");
    for root in [&old, &active, &custom] {
        root_with_identity(root);
    }
    let identity = custom.join("install.json");
    let bytes = fs::read_to_string(&identity)
        .unwrap()
        .replace(ID, "264d8a4d-3c0f-4dbf-b0b3-81a59cedd5e2");
    fs::write(identity, bytes).unwrap();
    let mut moving = ManagedRootMove::acquire_in(control.clone()).unwrap();
    moving.drain().unwrap();
    moving.activate(&active, ID).unwrap();
    assert!(ensure_active_in(&old, &control)
        .unwrap_err()
        .to_string()
        .contains("retired"));
    assert!(ensure_active_in(&active, &control).is_ok());
    assert!(ensure_active_in(&custom, &control).is_ok());
    fs::rename(&active, home.path().join("unmounted")).unwrap();
    assert!(ensure_active_in(&active, &control).is_err());
    assert!(ensure_active_in(&old, &control).is_err());
    assert!(ensure_active_in(&custom, &control).is_ok());
    assert!(ensure_active_in(&home.path().join("new-custom"), &control).is_ok());
}

#[test]
fn durable_directory_syncs_parent_and_propagates_failure_before_publication() {
    let home = tempfile::tempdir().unwrap();
    for name in [".ctx-control", "destination"] {
        let path = home.path().join(name);
        let mut synced = Vec::new();
        let error = create_private_durable_directory_with(&path, |entry| {
            synced.push(entry.to_path_buf());
            if entry == home.path() {
                return Err(io::Error::other("sync failed"));
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "sync failed");
        assert_eq!(synced, [path.clone(), home.path().to_path_buf()]);
        assert!(!path.join("data-root.json").exists());
    }
}

#[test]
fn managed_selection_survives_source_deletion_and_can_bind_an_offline_volume() {
    let home = tempfile::tempdir().unwrap();
    let source = home.path().join(".ctx");
    let bindings = [
        DataRootSelection::select_in(home.path(), None).unwrap(),
        DataRootSelection::select_in(home.path(), Some(source.clone())).unwrap(),
    ];
    let custom =
        DataRootSelection::select_in(home.path(), Some(home.path().join("custom"))).unwrap();
    root_with_identity(&source);
    let destination = home.path().join("destination");
    root_with_identity(&destination);
    let mut moving = ManagedRootMove::acquire_in(home.path().join(".ctx-control")).unwrap();
    moving.drain().unwrap();
    moving.activate(&destination, ID).unwrap();
    fs::remove_dir_all(&source).unwrap();
    for binding in bindings {
        assert!(binding
            .validate_in(home.path())
            .unwrap_err()
            .to_string()
            .contains("retired managed data root"));
    }
    assert!(!source.exists());
    fs::rename(&destination, home.path().join("offline")).unwrap();
    let offline = DataRootSelection::select_in(home.path(), None).unwrap();
    assert_eq!(offline.path(), destination);
    assert!(offline.validate_in(home.path()).is_err());
    assert!(custom.validate_in(home.path()).is_ok());
    assert!(!destination.exists());
}
