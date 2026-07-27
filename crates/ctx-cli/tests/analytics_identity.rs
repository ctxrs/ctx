mod support;

use support::*;

#[test]
fn analytics_refuses_device_identity_under_data_root() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let home = data_root.clone();
    let state = data_root.join("state");
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(
        !events_path.exists(),
        "device identity under data root should fail closed before delivery"
    );
    assert!(
        !state.join("ctx").join("device.json").exists(),
        "device identity must not be created under CTX_DATA_ROOT"
    );
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn analytics_refuses_symlinked_state_directory_under_data_root() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let state_link = temp.path().join("state-link");
    let events_path = temp.path().join("analytics.jsonl");
    fs::create_dir_all(&data_root).unwrap();
    symlink(&data_root, &state_link).unwrap();

    ctx(&temp)
        .arg("doctor")
        .env("CTX_DATA_ROOT", &data_root)
        .env("XDG_STATE_HOME", &state_link)
        .env("LOCALAPPDATA", &state_link)
        .env_remove("CTX_ANALYTICS_ENABLED")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(&events_path))
        .assert()
        .success();

    assert!(!events_path.exists());
    assert!(!data_root.join("ctx").exists());
}
