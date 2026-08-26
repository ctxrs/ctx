use std::{env, ffi::OsString, path::Path, sync::Mutex};

use ctx_history_core::CaptureProvider;

use super::super::ProviderSourceStatus;

pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn tempdir() -> tempfile::TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support test fixtures")
}

pub(super) struct EnvGuard {
    name: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    pub(super) fn remove(name: &'static str) -> Self {
        let original = env::var_os(name);
        env::remove_var(name);
        Self { name, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.name, value);
        } else {
            env::remove_var(self.name);
        }
    }
}

pub(super) fn assert_source_status(
    home: &Path,
    provider: CaptureProvider,
    expected: ProviderSourceStatus,
) {
    let source = super::super::discover_provider_sources(&super::super::TEST_PROVIDER_PROBES, home)
        .into_iter()
        .find(|source| source.provider == provider)
        .unwrap();
    assert_eq!(source.status, expected, "{provider:?}");
}
