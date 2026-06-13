#![cfg(feature = "test-support")]

use std::sync::Mutex;

use ctx_avf_linux_runtime::{helper_path, AVF_LINUX_HELPER_PATH_ENV};

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn helper_path_accepts_test_support_builds() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let helper = tempfile::NamedTempFile::new().expect("helper temp file");
    let original = std::env::var_os(AVF_LINUX_HELPER_PATH_ENV);

    std::env::set_var(AVF_LINUX_HELPER_PATH_ENV, helper.path());
    let resolved = helper_path().expect("helper path should resolve with test-support enabled");
    assert_eq!(resolved, helper.path());

    match original {
        Some(value) => std::env::set_var(AVF_LINUX_HELPER_PATH_ENV, value),
        None => std::env::remove_var(AVF_LINUX_HELPER_PATH_ENV),
    }
}
