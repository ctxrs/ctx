use super::*;

#[test]
fn snapshot_read_normalizes_retired_controls_without_rewriting_or_locking() {
    let root = tempfile::tempdir().unwrap();
    write_default_config(root.path()).unwrap();
    let path = root.path().join(CONFIG_FILE);
    let original =
        "# preserve bytes\n[upgrade]\nallow_rfc2544_fake_ip = true\nchannel = \"beta\"\n";
    crate::durable_write::write_config_durably(&path, original.as_bytes()).unwrap();
    let lock = root.path().join(".config.mutation.lock");
    if lock.exists() {
        fs::remove_file(&lock).unwrap();
    }
    let config = AppConfig::load_using(root.path(), mutation::read_config_text_read_only).unwrap();
    assert_eq!(config.upgrade.channel, "beta");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(!lock.exists());
    let missing = root.path().join("missing");
    AppConfig::load_using(&missing, mutation::read_config_text_read_only).unwrap();
    assert!(!missing.exists());
    // Existing ordinary loads retain their owned compatibility migration.
    AppConfig::load_persisted(root.path()).unwrap();
    assert!(!fs::read_to_string(path)
        .unwrap()
        .contains("allow_rfc2544_fake_ip"));
}

#[test]
fn invalid_snapshot_settings_remain_unchanged() {
    let root = tempfile::tempdir().unwrap();
    write_default_config(root.path()).unwrap();
    let path = root.path().join(CONFIG_FILE);
    let original = "[upgrade]\nallow_rfc2544_fake_ip = true\nunknown_control = true\n";
    crate::durable_write::write_config_durably(&path, original.as_bytes()).unwrap();
    let error =
        AppConfig::load_using(root.path(), mutation::read_config_text_read_only).unwrap_err();
    assert!(format!("{error:#}").contains("unknown_control"));
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn snapshot_read_never_repairs_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(CONFIG_FILE);
    fs::write(&path, "[upgrade]\nchannel = \"beta\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(AppConfig::load_using(root.path(), mutation::read_config_text_read_only).is_err());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(AppConfig::load_using(root.path(), mutation::read_config_text_read_only).is_ok());
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o400
    );
}

#[cfg(unix)]
#[test]
fn saved_unreadable_root_loads_but_mutation_stays_strict() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let parent = fixture.join("providers");
    let provider = parent.join("claude");
    fs::create_dir_all(&provider).unwrap();
    add_claude_root(&data_root, "saved", &provider, None, false).unwrap();
    let path = data_root.join(CONFIG_FILE);
    let original = fs::read(&path).unwrap();
    let permissions = fs::metadata(&parent).unwrap().permissions();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::metadata(&provider).is_err();
    let snapshot = AppConfig::load_read_only(&data_root);
    let ordinary = AppConfig::load_persisted(&data_root);
    let mutation = add_claude_root(&data_root, "saved", &provider, None, true);
    fs::set_permissions(&parent, permissions).unwrap();
    if !unreadable {
        return;
    }

    assert_eq!(snapshot.unwrap().provider_roots["saved"].path, provider);
    assert_eq!(ordinary.unwrap().provider_roots["saved"].path, provider);
    assert!(mutation.is_err());
    assert_eq!(fs::read(path).unwrap(), original);
    add_claude_root(&data_root, "saved", &provider, None, true).unwrap();
}

#[cfg(unix)]
#[test]
fn retired_control_normalization_allows_unreadable_saved_roots_in_both_load_modes() {
    use std::os::unix::fs::PermissionsExt;

    for read_only in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let fixture = fs::canonicalize(temp.path()).unwrap();
        let data_root = fixture.join("data");
        let parent = fixture.join("providers");
        let provider = parent.join("claude");
        fs::create_dir_all(&provider).unwrap();
        add_claude_root(&data_root, "saved", &provider, None, false).unwrap();
        let path = data_root.join(CONFIG_FILE);
        let original = format!(
            "[upgrade]\nallow_rfc2544_fake_ip = true\nchannel = \"beta\"\n{}",
            fs::read_to_string(&path).unwrap()
        );
        crate::durable_write::write_config_durably(&path, original.as_bytes()).unwrap();
        let permissions = fs::metadata(&parent).unwrap().permissions();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o000)).unwrap();
        let inspection = fs::metadata(&provider);
        let mutation = set_semantic_search_enabled(&data_root, true);
        let after_mutation = fs::read_to_string(&path).unwrap();
        let lock = data_root.join(".config.mutation.lock");
        fs::remove_file(&lock).unwrap();
        let loaded = if read_only {
            AppConfig::load_read_only(&data_root)
        } else {
            AppConfig::load_persisted(&data_root)
        };
        let after_load = fs::read_to_string(&path).unwrap();
        fs::set_permissions(&parent, permissions).unwrap();
        let Err(inspection) = inspection else {
            continue;
        };

        assert_eq!(inspection.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(mutation.is_err());
        assert_eq!(after_mutation, original);
        let loaded = loaded.unwrap();
        assert_eq!(loaded.provider_roots["saved"].path, provider);
        assert_eq!(loaded.upgrade.channel, "beta");
        if read_only {
            assert_eq!(after_load, original);
            assert!(!lock.exists());
        } else {
            assert_eq!(
                after_load,
                original.replace("allow_rfc2544_fake_ip = true\n", "")
            );
        }
        set_semantic_search_enabled(&data_root, true).unwrap();
        assert!(!fs::read_to_string(path)
            .unwrap()
            .contains("allow_rfc2544_fake_ip"));
    }
}

#[cfg(unix)]
#[test]
fn saved_roots_still_reject_overlap_and_linked_data_roots() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let data_root = fixture.join("data");
    let provider = fixture.join("provider");
    fs::create_dir(&provider).unwrap();
    add_claude_root(&data_root, "saved", &provider, None, false).unwrap();
    let alias = fixture.join("data-alias");
    symlink(&data_root, &alias).unwrap();
    assert!(AppConfig::load_read_only(&alias).is_err());
    assert!(AppConfig::load_persisted(&alias).is_err());

    let path = data_root.join(CONFIG_FILE);
    let overlapping = format!(
        "[upgrade]\nallow_rfc2544_fake_ip = true\n[sources.roots.saved]\nprovider = \"claude\"\npath = {:?}\n",
        data_root.display().to_string()
    );
    crate::durable_write::write_config_durably(&path, overlapping.as_bytes()).unwrap();
    for result in [
        AppConfig::load_read_only(&data_root),
        AppConfig::load_persisted(&data_root),
    ] {
        assert!(result.unwrap_err().chain().any(|cause| matches!(
            cause.downcast_ref::<ProviderSourceBoundaryError>(),
            Some(ProviderSourceBoundaryError::Overlap)
        )));
    }
    assert_eq!(fs::read_to_string(path).unwrap(), overlapping);
}
