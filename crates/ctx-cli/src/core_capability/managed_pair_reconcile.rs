use std::ffi::OsStr;

use anyhow::{anyhow, bail, Context as _, Result};
use ctx_upgrade_engine::{
    ensure_hosted_transaction_inactive_under_installation_lock,
    inspect_managed_pair_under_installation_lock, managed_install_marker_for_current_exe,
    reconcile_managed_pair_integration_under_installation_lock,
    try_acquire_managed_installation_mutation_at_root, ManagedInstallMarker,
    ManagedPairInstallationStatus,
};

use super::{
    managed_pair_apply::{
        managed_core_destination, marker_channel, normalized_absolute_path, require_directory,
        require_regular_file,
    },
    write_response_frame, CoreManagedPairVerifier,
};

const ARGUMENT_COUNT: usize = 5;
const SUCCESS_RECEIPT: &[u8] = br#"{"schema_version":1,"command":"managed_pair_reconcile_integration","ok":true,"status":"committed"}"#;

pub(super) const fn success_receipt() -> &'static [u8] {
    SUCCESS_RECEIPT
}

pub(super) fn run(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != ARGUMENT_COUNT || arguments[3] != OsStr::new("-") {
        bail!("invalid managed-pair reconciliation invocation");
    }
    let install_root = normalized_absolute_path(&arguments[2], "install root")?;
    let integration = normalized_absolute_path(&arguments[4], "integration ownership")?;
    require_directory(&install_root, "install root")?;
    require_regular_file(&integration, "integration ownership")?;
    let current = std::env::current_exe().context("resolve running managed Core")?;
    if !ctx_upgrade_engine::managed_install_path_identity_matches(
        &current,
        &managed_core_destination(&install_root),
    ) {
        bail!("managed-pair reconciliation must run from the installed Core");
    }
    let _guard = try_acquire_managed_installation_mutation_at_root(&install_root)?
        .ok_or_else(|| anyhow!("managed-pair installation is busy"))?;
    let active_marker = match managed_install_marker_for_current_exe()? {
        ManagedInstallMarker::Valid(marker)
            if ctx_upgrade_engine::managed_install_path_identity_matches(
                &marker.install_path,
                &managed_core_destination(&install_root),
            ) =>
        {
            marker
        }
        ManagedInstallMarker::Valid(_) => {
            bail!("managed-pair reconciliation install root does not own the running Core")
        }
        ManagedInstallMarker::Absent => bail!("managed Core install marker is absent"),
        ManagedInstallMarker::Invalid { reason } => bail!(reason),
    };
    let verifier = CoreManagedPairVerifier::for_channel(marker_channel(&active_marker)?);
    ensure_hosted_transaction_inactive_under_installation_lock(&managed_core_destination(
        &install_root,
    ))?;
    if !matches!(
        inspect_managed_pair_under_installation_lock(&install_root, &verifier)?,
        ManagedPairInstallationStatus::Healthy { .. }
    ) {
        bail!("managed-pair reconciliation requires an installed signed pair");
    }
    reconcile_managed_pair_integration_under_installation_lock(&install_root, &integration)?;
    write_response_frame(std::io::stdout().lock(), success_receipt())
}
