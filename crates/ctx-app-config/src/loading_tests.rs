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
