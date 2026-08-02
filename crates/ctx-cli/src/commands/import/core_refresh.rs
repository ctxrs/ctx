use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::{
    config::AppConfig,
    progress::ProgressReporter,
    semantic::{
        autostart_daemon_and_wait, coordinate_core_refresh_without_autostart,
        coordinate_core_refresh_without_autostart_with_progress, coordinate_source_backed_refresh,
        coordinate_source_backed_refresh_with_progress, SourceBackedRefreshMode,
        SourceBackedRefreshObservation,
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
/// Import may start the daemon and waits only for authoritative Core publication.
/// Pro and semantic derived consumers follow independently under daemon scheduling.
pub(super) fn wait_for_import_core_refresh(
    data_root: &Path,
    config: &AppConfig,
    no_daemon: bool,
    request: ImportCoreRefreshRequest<'_>,
    progress: &ProgressReporter,
) -> Result<SourceBackedRefreshObservation> {
    if !no_daemon {
        autostart_daemon_and_wait(data_root, config, DaemonTriggerCommandArg::Import)?;
    }

    let mut observe_progress = |update: &crate::semantic::SourceBackedRefreshProgress| {
        progress.source_refresh(update).map_err(anyhow::Error::new)
    };
    let refresh = match (request, progress.is_enabled()) {
        (ImportCoreRefreshRequest::Automatic, false) if no_daemon => {
            coordinate_core_refresh_without_autostart(data_root, SourceBackedRefreshMode::Wait)
        }
        (ImportCoreRefreshRequest::Automatic, false) => {
            coordinate_source_backed_refresh(data_root, SourceBackedRefreshMode::Wait)
        }
        (ImportCoreRefreshRequest::ExplicitCatalog(authority), false) => {
            SourceBackedRefreshMode::Wait.coordinate_explicit_source_catalog(data_root, authority)
        }
        (ImportCoreRefreshRequest::Automatic, true) if no_daemon => {
            coordinate_core_refresh_without_autostart_with_progress(
                data_root,
                SourceBackedRefreshMode::Wait,
                &mut observe_progress,
            )
        }
        (ImportCoreRefreshRequest::Automatic, true) => {
            coordinate_source_backed_refresh_with_progress(
                data_root,
                SourceBackedRefreshMode::Wait,
                &mut observe_progress,
            )
        }
        (ImportCoreRefreshRequest::ExplicitCatalog(authority), true) => {
            SourceBackedRefreshMode::Wait.coordinate_explicit_source_catalog_with_progress(
                data_root,
                authority,
                &mut observe_progress,
            )
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
    Ok(refresh)
}

#[cfg(test)]
mod tests {
    #[test]
    fn import_control_contains_no_ingestion_provider_read_or_sidecar_implementation() {
        let source = include_str!("core_refresh.rs");
        for forbidden in [
            ["ctx_history_", "capture"].concat(),
            ["SourceBackedRefresh", "Executor"].concat(),
            ["VerifiedIndex", "::open"].concat(),
            ["Store", "::open"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "import Core control contains forbidden foreground implementation `{forbidden}`"
            );
        }
    }
}
