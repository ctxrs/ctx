#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn analytics_refuses_device_identity_under_data_root() {
    let temp = tempdir();
    let data_root = temp.path().join("ctx-data");
    let state = data_root.join("state");
    let events_path = temp.path().join("analytics.jsonl");

    ctx(&temp)
        .arg("status")
        .env("CTX_DATA_ROOT", &data_root)
        .env("XDG_STATE_HOME", &state)
        .env("LOCALAPPDATA", &state)
        .env_remove("CTX_ANALYTICS_OFF")
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
