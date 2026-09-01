use std::{env, path::PathBuf};

use crate::{
    config_path, default_data_root, device_path, history_dir, logs_dir, managed_data_root,
    PlatformError,
};

#[test]
fn missing_home_error_preserves_the_default_data_root_message() {
    assert_eq!(
        PlatformError::MissingHome.to_string(),
        "could not determine a home directory for the default ctx data root"
    );
}

#[test]
fn retained_local_layout_paths_are_flat_under_data_root() {
    let root = PathBuf::from("/tmp/ctx-root");
    assert_eq!(history_dir(root.clone()), PathBuf::from("/tmp/ctx-root"));
    assert_eq!(
        config_path(root.clone()),
        PathBuf::from("/tmp/ctx-root/config.toml")
    );
    assert_eq!(logs_dir(root.clone()), PathBuf::from("/tmp/ctx-root/logs"));
    assert_eq!(
        device_path(root),
        PathBuf::from("/tmp/ctx-root/device.json")
    );
}

#[test]
fn managed_data_root_matches_the_home_only_platform_api() {
    let home = dirs::home_dir().expect("test host must provide a home directory");
    assert_eq!(managed_data_root().unwrap(), home.join(".ctx"));
}

#[test]
fn ctx_data_root_env_is_the_ctx_root_itself() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    env::remove_var("CTX_DATA_ROOT");

    let default_root = default_data_root().unwrap();
    assert!(default_root.ends_with(".ctx"));
    let managed_root = managed_data_root().unwrap();
    assert_eq!(managed_root, default_root);

    env::set_var("CTX_DATA_ROOT", "/tmp/custom-ctx-root");

    assert_eq!(
        default_data_root().unwrap(),
        PathBuf::from("/tmp/custom-ctx-root")
    );
    assert_eq!(managed_data_root().unwrap(), managed_root);

    if let Some(previous) = previous {
        env::set_var("CTX_DATA_ROOT", previous);
    } else {
        env::remove_var("CTX_DATA_ROOT");
    }
}
