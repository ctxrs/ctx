use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_provider_runtime::ProviderRuntime;

use crate::daemon::ProviderAccountsHandle;

#[derive(Clone)]
pub(in crate::daemon::providers) struct ProviderLoginDeps {
    data_root: PathBuf,
    daemon_url: String,
    providers: Arc<ProviderRuntime>,
}

impl ProviderLoginDeps {
    pub(in crate::daemon::providers) fn from_accounts_handle(
        handle: &ProviderAccountsHandle,
    ) -> Self {
        Self::new(
            handle.data_root().to_path_buf(),
            handle.daemon_url().to_string(),
            handle.providers_arc(),
        )
    }

    pub(in crate::daemon::providers) fn new(
        data_root: PathBuf,
        daemon_url: String,
        providers: Arc<ProviderRuntime>,
    ) -> Self {
        Self {
            data_root,
            daemon_url,
            providers,
        }
    }

    pub(in crate::daemon::providers) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon::providers) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon::providers) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }
}
