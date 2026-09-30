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
fn ctx_data_root_selects_the_complete_managed_root() {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => env::set_var("CTX_DATA_ROOT", value),
                None => env::remove_var("CTX_DATA_ROOT"),
            }
        }
    }
    let _restore = Restore(env::var_os("CTX_DATA_ROOT"));
    let home = dirs::home_dir().expect("test host must provide a home directory");
    for value in [None, Some("")] {
        match value {
            None => env::remove_var("CTX_DATA_ROOT"),
            Some(value) => env::set_var("CTX_DATA_ROOT", value),
        }
        assert_eq!(managed_data_root().unwrap(), home.join(".ctx"));
        assert_eq!(default_data_root().unwrap(), home.join(".ctx"));
    }

    let temp = tempfile::tempdir().unwrap();
    let selected = temp.path().join("managed root-λ");
    env::set_var("CTX_DATA_ROOT", &selected);
    assert_eq!(managed_data_root().unwrap(), selected);
    assert_eq!(default_data_root().unwrap(), selected);
    assert_eq!(
        config_path(default_data_root().unwrap()),
        selected.join("config.toml")
    );
    assert!(!selected.exists(), "resolution must not create the root");

    for value in ["relative", "~/ctx", " ", "/tmp/ctx\nroot", "/tmp/ctx\troot"] {
        env::set_var("CTX_DATA_ROOT", value);
        assert!(managed_data_root()
            .unwrap_err()
            .to_string()
            .starts_with("CTX_DATA_ROOT "));
        assert!(default_data_root().is_err());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        env::set_var(
            "CTX_DATA_ROOT",
            std::ffi::OsString::from_vec(b"/tmp/ctx-\xff".to_vec()),
        );
        assert_eq!(
            managed_data_root().unwrap_err().to_string(),
            "CTX_DATA_ROOT must be a Unicode path"
        );
    }
}
