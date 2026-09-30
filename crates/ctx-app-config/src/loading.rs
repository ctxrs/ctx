//! Shared configuration parsing with explicit read or migration ownership.
use ctx_history_platform::platform_security::ProviderSourceBoundaryError;

use super::*;

impl AppConfig {
    pub fn load(data_root: &Path) -> Result<Self> {
        let deprecated_controls = DeprecatedControls::detect();
        Self::load_with_deprecated_controls(data_root, &deprecated_controls)
    }

    pub fn load_with_deprecated_controls(
        data_root: &Path,
        deprecated_controls: &DeprecatedControls,
    ) -> Result<Self> {
        let mut config = Self::load_persisted(data_root)?;
        config.apply_env(deprecated_controls)?;
        Ok(config)
    }

    pub(super) fn load_persisted(data_root: &Path) -> Result<Self> {
        Self::load_using(
            data_root,
            mutation::read_config_text_migrating_retired_controls,
        )
    }

    /// Read saved settings without migrations, permission repairs or lock files.
    /// Retired settings are normalized in memory with the same compatibility rules.
    pub fn load_read_only(data_root: &Path) -> Result<Self> {
        let mut config = Self::load_using(data_root, mutation::read_config_text_read_only)?;
        config.apply_env(&DeprecatedControls::detect())?;
        Ok(config)
    }

    fn load_using(data_root: &Path, read: fn(&Path) -> Result<Option<String>>) -> Result<Self> {
        observe_app_config_load();
        let path = data_root.join(CONFIG_FILE);
        match read(&path)? {
            Some(text) => Self::from_saved_text(&path, &text),
            None => Ok(Self::default()),
        }
    }

    pub(super) fn from_saved_text(path: &Path, text: &str) -> Result<Self> {
        let parsed =
            parse_toml_subset(text).with_context(|| format!("parse {}", path.display()))?;
        let mut config = Self::default();
        config
            .apply_values(&parsed)
            .with_context(|| format!("load {}", path.display()))?;
        let data_root = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("config path has no data-root parent"))?;
        for root in config.provider_roots.values() {
            validate_provider_source_outside_data_root(data_root, &root.path)
                .or_else(|error| match error {
                    // Saved roots can be temporarily unavailable. Discovery
                    // retains that source failure while healthy peers run.
                    ProviderSourceBoundaryError::SourceRoot(_) => Ok(()),
                    error => Err(error),
                })
                .with_context(|| {
                    format!(
                        "load {}: configured provider root `{}` must not overlap the ctx data root",
                        path.display(),
                        root.id
                    )
                })?;
        }
        Ok(config)
    }
}

#[cfg(test)]
#[path = "loading_tests.rs"]
mod tests;
