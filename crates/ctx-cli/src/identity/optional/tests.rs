use super::*;

#[test]
fn installation_identity_contention_is_optional_and_does_not_rewrite_identity() {
    let root = tempfile::tempdir().unwrap();
    let id = installation_id(root.path()).unwrap();
    let before = fs::read(install_path(root.path())).unwrap();
    let file = open_identity_file(&install_path(root.path()), false)
        .unwrap()
        .unwrap();
    file.lock_exclusive().unwrap();
    assert!(try_installation_id(root.path()).unwrap().is_none());
    assert!(try_existing_installation_id(root.path()).unwrap().is_none());
    assert_eq!(fs::read(install_path(root.path())).unwrap(), before);
    fs2::FileExt::unlock(&file).unwrap();
    assert_eq!(try_existing_installation_id(root.path()).unwrap(), Some(id));
}

#[test]
fn optional_identity_read_does_not_repair_unsafe_permissions() {
    let root = tempfile::tempdir().unwrap();
    installation_id(root.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = install_path(root.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(try_existing_installation_id(root.path()).is_err());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}

#[test]
fn normal_and_optional_first_profile_writers_share_one_identity() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("device.json");
    let gate = std::sync::Arc::new(std::sync::Barrier::new(32));
    let threads = (0..32)
        .map(|i| {
            let path = path.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                if i % 2 == 0 {
                    device_id_at_path(&path).map(Some)
                } else {
                    access_device_id(&path, true)
                }
            })
        })
        .collect::<Vec<_>>();
    let mut ids = std::collections::BTreeSet::new();
    for thread in threads {
        if let Some(id) = thread.join().unwrap().unwrap() {
            ids.insert(id);
        }
    }
    assert_eq!(ids.len(), 1);
    let canonical = device_id_at_path(&path).unwrap();
    assert!(ids.contains(&canonical));
    assert_eq!(access_device_id(&path, true).unwrap(), Some(canonical));
}
#[test]
fn optional_profile_lookup_preserves_legacy_fields_and_canonicalizes_uuid() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("device.json");
    create_private_file(
        &path,
        br#"{"device_id":"550E8400E29B11D4A716446655440000","legacy_field":"keep"}"#,
    )
    .unwrap();
    let id = access_device_id(&path, true).unwrap().unwrap();
    assert_eq!(id, "550e8400-e29b-11d4-a716-446655440000");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["legacy_field"], "keep");
    assert_eq!(value["device_id"], id);
    verify_private_file(&path).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert_eq!(device_id_at_path(&path).unwrap(), id);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}
