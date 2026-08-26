//! Release upgrade mechanics shared by ctx command and daemon composition.

mod install_marker;
mod upgrade;

pub use ctx_managed_pair_engine::{
    ManagedPairActivation, ManagedPairAttempt, ManagedPairComponentIdentity, ManagedPairEngine,
    ManagedPairPrepared, ManagedPairRecovery, ManagedPairTarget, ManagedPairTransactionStatus,
    ManagedPairUninstallAttempt, ManagedPairVerifier, VerifiedManagedPairIdentity,
    MANAGED_PAIR_ENVELOPE_RELATIVE_PATH, MANAGED_PAIR_STATE_RELATIVE_PATH,
};

pub use install_marker::{
    current_exe_install_marker, current_exe_is_staging_dogfood, ActiveInstallAttribution,
};
pub use upgrade::{
    active_installation_upgrade_attempt_id, automatic_upgrade_check_due, current_install_path,
    installation_daemon_coordination_paths, installation_daemon_coordination_paths_for,
    installation_executable_path, installation_hosted_uninstall_is_active,
    installation_hosted_uninstall_is_active_for_executable, installation_upgrade_is_active,
    invalid_install_marker_recovery_guidance, is_valid_install_attempt_id,
    is_valid_upgrade_attempt_id, managed_install_executable,
    managed_install_marker_for_current_exe, read_state_json, run_hosted_transaction,
    terminal_installation_upgrade_attempt_id, unmanaged_install_conversion_guidance,
    upgrade_diagnostics, AutomaticUpgradeObservation, AutomaticUpgradePolicyProvider,
    AutomaticUpgradePolicySnapshot, DaemonRestart, DaemonUpgradeLease, DaemonUpgradePort,
    HostedTransactionAction, HostedTransactionArgs, InstallMarker, ManagedInstallDiagnostic,
    ManagedInstallMarker, PreparedAutomaticUpgrade, ProductBuildIdentity, ReleaseProcessPort,
    ReleaseTransport, SemanticAccelerator, SemanticLayoutPort, SemanticModelContract,
    SemanticModelVariant, UpgradeDiagnostics, UpgradeEngine, UpgradeFailureKind, UpgradeObserver,
    UpgradeOutcome, UpgradePlan, UpgradePolicy, UpgradeTerminalStatus, STATE_SCHEMA_VERSION,
};
