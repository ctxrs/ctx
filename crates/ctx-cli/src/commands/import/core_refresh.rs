use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::{
    config::AppConfig,
    semantic::{
        autostart_daemon_and_wait, converge_source_backed_relational_generation,
        coordinate_source_backed_refresh, SourceBackedRefreshMode, SourceBackedRefreshObservation,
    },
    DaemonTriggerCommandArg,
};

use super::ExplicitSourceCatalogAuthority;

pub(super) enum ImportCoreRefreshRequest<'a> {
    Automatic,
    ExplicitCatalog(&'a ExplicitSourceCatalogAuthority),
}

/// Applies import-specific policy around the one Core refresh control path.
///
/// Import may start the daemon and waits for the required relational frontier,
/// but discovery, acquisition, classification, Core construction, and atomic
/// publication all remain owned by the shared refresh engine.
pub(super) fn wait_for_import_core_refresh(
    data_root: &Path,
    config: &AppConfig,
    no_daemon: bool,
    request: ImportCoreRefreshRequest<'_>,
) -> Result<SourceBackedRefreshObservation> {
    if !no_daemon {
        autostart_daemon_and_wait(data_root, config, DaemonTriggerCommandArg::Import)?;
    }

    let refresh = match request {
        ImportCoreRefreshRequest::Automatic => {
            coordinate_source_backed_refresh(data_root, SourceBackedRefreshMode::Wait)
        }
        ImportCoreRefreshRequest::ExplicitCatalog(authority) => {
            SourceBackedRefreshMode::Wait.coordinate_explicit_source_catalog(data_root, authority)
        }
    }
    .context("publish provider inputs through the Core refresh engine")?;

    let receipt = refresh
        .receipt
        .as_ref()
        .context("Core refresh completed without an authoritative publication receipt")?;
    if refresh.pin.generation_id() != receipt.published_generation {
        bail!(
            "Core refresh receipt names generation {}, but the verified publication pin carries {}",
            receipt.published_generation,
            refresh.pin.generation_id()
        );
    }
    converge_source_backed_relational_generation(data_root, refresh.pin.generation_id())
        .context("converge the required relational projection after Core publication")?;
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    #[test]
    fn import_control_contains_no_ingestion_or_provider_read_implementation() {
        let source = include_str!("core_refresh.rs");
        for forbidden in [
            ["ctx_history_", "capture"].concat(),
            ["SourceBackedRefresh", "Executor"].concat(),
            ["VerifiedIndex", "::open"].concat(),
            ["Store", "::open"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "import Core control contains forbidden implementation `{forbidden}`"
            );
        }
    }
}
