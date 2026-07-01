use std::{fs, path::PathBuf};

use ctx_history_capture::{provider_source_for_path, ProviderSource};
use ctx_history_core::{config_path, database_path, default_data_root, CaptureProvider};
use ctx_history_store::{CatalogCounts, Store};

use crate::error::{Error, Result};

/// Local ctx paths used by a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CtxPaths {
    pub data_root: PathBuf,
    pub database_path: PathBuf,
    pub config_path: PathBuf,
}

/// Builder for [`CtxClient`].
#[derive(Debug, Clone, Default)]
pub struct CtxClientBuilder {
    data_root: Option<PathBuf>,
    home_dir: Option<PathBuf>,
}

impl CtxClientBuilder {
    /// Use a specific ctx data root instead of `CTX_DATA_ROOT` / `~/.ctx`.
    pub fn data_root(mut self, data_root: impl Into<PathBuf>) -> Self {
        self.data_root = Some(data_root.into());
        self
    }

    /// Use a specific home directory for provider source discovery.
    pub fn home_dir(mut self, home_dir: impl Into<PathBuf>) -> Self {
        self.home_dir = Some(home_dir.into());
        self
    }

    pub fn build(self) -> Result<CtxClient> {
        Ok(CtxClient {
            data_root: self.data_root.map(Ok).unwrap_or_else(default_data_root)?,
            home_dir: self.home_dir,
        })
    }
}

/// In-process client for the local ctx history store.
#[derive(Debug, Clone)]
pub struct CtxClient {
    data_root: PathBuf,
    home_dir: Option<PathBuf>,
}

impl CtxClient {
    /// Build a client from `CTX_DATA_ROOT` / `~/.ctx`.
    pub fn new() -> Result<Self> {
        Self::builder().build()
    }

    pub fn builder() -> CtxClientBuilder {
        CtxClientBuilder::default()
    }

    /// Build a client for an explicit data root.
    pub fn with_data_root(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            home_dir: None,
        }
    }

    pub fn paths(&self) -> CtxPaths {
        CtxPaths {
            data_root: self.data_root.clone(),
            database_path: database_path(self.data_root.clone()),
            config_path: config_path(self.data_root.clone()),
        }
    }

    /// Initialize the local store if needed and return its current status.
    pub fn init(&self) -> Result<Status> {
        self.open_store()?;
        self.status()
    }

    /// Return store status without creating the store.
    pub fn status(&self) -> Result<Status> {
        let paths = self.paths();
        let initialized = paths.database_path.exists();
        let (indexed_items, indexed_sources, catalog_counts) = if initialized {
            let store = Store::open_read_only(&paths.database_path)?;
            (
                store.indexed_history_item_count()?,
                store.capture_source_count()?,
                store.catalog_session_counts()?,
            )
        } else {
            (0, 0, CatalogCounts::default())
        };

        Ok(Status {
            initialized,
            paths,
            indexed_items,
            indexed_sources,
            cataloged_sessions: catalog_counts.total,
            indexed_catalog_sessions: catalog_counts.indexed,
            pending_catalog_sessions: catalog_counts.pending,
            failed_catalog_sessions: catalog_counts.failed,
            stale_catalog_sessions: catalog_counts.stale,
            local_only: true,
        })
    }

    /// Discover known provider history sources under the configured home dir.
    pub fn sources(&self) -> Vec<ProviderSource> {
        self.home_dir()
            .as_deref()
            .map(ctx_history_capture::discover_provider_sources)
            .unwrap_or_default()
    }

    /// Discover known provider history sources for one provider.
    pub fn sources_for_provider(&self, provider: CaptureProvider) -> Vec<ProviderSource> {
        self.home_dir()
            .as_deref()
            .map(|home| ctx_history_capture::discover_provider_sources_for_provider(home, provider))
            .unwrap_or_default()
    }

    /// Build a provider source from an explicit path.
    pub fn source_for_path(
        &self,
        provider: CaptureProvider,
        path: impl Into<PathBuf>,
    ) -> ProviderSource {
        provider_source_for_path(provider, path.into())
    }

    /// Run SQLite integrity checks using a read-only connection when possible.
    pub fn doctor(&self) -> Result<DoctorReport> {
        let paths = self.paths();
        let mut findings = Vec::new();
        if !paths.data_root.exists() {
            findings.push(format!(
                "data root does not exist: {}",
                paths.data_root.display()
            ));
        }
        if paths.database_path.exists() {
            let store = Store::open_read_only(&paths.database_path)?;
            findings.extend(store.validate()?);
        } else {
            findings.push(format!(
                "database does not exist: {}",
                paths.database_path.display()
            ));
        }
        Ok(DoctorReport {
            ok: findings.is_empty(),
            findings,
        })
    }

    pub(crate) fn open_store(&self) -> Result<Store> {
        fs::create_dir_all(&self.data_root)?;
        Ok(Store::open(database_path(self.data_root.clone()))?)
    }

    pub(crate) fn open_store_read_only(&self) -> Result<Store> {
        let path = database_path(self.data_root.clone());
        if !path.exists() {
            return Err(Error::StoreNotInitialized(path));
        }
        Ok(Store::open_read_only(path)?)
    }

    pub(crate) fn home_dir(&self) -> Option<PathBuf> {
        self.home_dir
            .clone()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
    }
}

/// Status of the local ctx store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub initialized: bool,
    pub paths: CtxPaths,
    pub indexed_items: usize,
    pub indexed_sources: usize,
    pub cataloged_sessions: usize,
    pub indexed_catalog_sessions: usize,
    pub pending_catalog_sessions: usize,
    pub failed_catalog_sessions: usize,
    pub stale_catalog_sessions: usize,
    pub local_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub ok: bool,
    pub findings: Vec<String>,
}
