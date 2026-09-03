//! Release upgrade mechanics shared by ctx command and daemon composition.

mod install_marker;
mod upgrade;

pub use ctx_managed_pair_engine::{
    apply_or_resume_managed_pair_under_installation_lock,
    inspect_managed_pair_under_installation_lock,
    resume_pending_managed_pair_under_installation_lock, ManagedPairApplyInput,
    ManagedPairApplyOutcome, ManagedPairComponentIdentity, ManagedPairInstallationStatus,
    ManagedPairTarget, ManagedPairVerifier, VerifiedManagedPairIdentity,
    MANAGED_CORE_INSTALL_MARKER_RELATIVE_PATH, MANAGED_PAIR_ACTIVE_TRANSACTION_RELATIVE_PATH,
    MANAGED_PAIR_ENVELOPE_RELATIVE_PATH, MANAGED_PAIR_INSTALLATION_LOCK_RELATIVE_PATH,
    MANAGED_PAIR_STATE_RELATIVE_PATH,
};

pub use install_marker::{
    current_exe_install_marker, current_exe_is_staging_dogfood, ActiveInstallAttribution,
};
#[cfg(unix)]
pub use upgrade::reconcile_managed_pair_integration_under_installation_lock;
pub use upgrade::{
    active_installation_upgrade_attempt_id, current_exe_has_managed_install_marker_hint,
    current_exe_is_unmanaged, current_install_path, disable_current_man_pages,
    ensure_hosted_transaction_inactive_under_installation_lock,
    installation_daemon_coordination_paths, installation_daemon_coordination_paths_for,
    installation_executable_path, installation_hosted_uninstall_is_active,
    installation_hosted_uninstall_is_active_for_executable,
    installation_interrupted_automatic_upgrade_is_recoverable, installation_upgrade_is_active,
    invalid_install_marker_recovery_guidance, is_valid_install_attempt_id,
    is_valid_upgrade_attempt_id, managed_install_executable,
    managed_install_marker_for_current_exe, managed_install_path_identity_matches, read_state_json,
    reconcile_current_man_pages, run_hosted_transaction, run_hosted_uninstall_after_parent_exit,
    terminal_installation_upgrade_attempt_id, try_acquire_managed_installation_mutation,
    try_acquire_managed_installation_mutation_at_root, unmanaged_install_conversion_guidance,
    upgrade_diagnostics, AutomaticUpgradeObservation, AutomaticUpgradePolicyProvider,
    AutomaticUpgradePolicySnapshot, DaemonRestart, DaemonUpgradeLease, DaemonUpgradePort,
    HostedTransactionAction, HostedTransactionArgs, InstallMarker, ManagedInstallDiagnostic,
    ManagedInstallMarker, ManagedInstallationMutationGuard, ManagedManBundle, ManagedManPage,
    PreparedAutomaticUpgrade, ProductBuildIdentity, ReleaseProcessPort, ReleaseTransport,
    SemanticAccelerator, SemanticLayoutPort, SemanticModelContract, SemanticModelVariant,
    UpgradeDiagnostics, UpgradeEngine, UpgradeFailureKind, UpgradeObserver, UpgradeOutcome,
    UpgradePlan, UpgradePolicy, UpgradeTerminalStatus, HOSTED_UNINSTALL_POST_EXIT_READY,
    STATE_SCHEMA_VERSION,
};
