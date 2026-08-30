use std::path::Path;

use anyhow::Result;
use ctx_history_refresh::RefreshSelection;

use super::daemon_service_ports::AVAILABILITY;

pub use ctx_daemon_service::{
    pin_active_verified_generation, published_explicit_source_relocation_authority,
    PinnedSourceBackedGeneration, RefreshStatus, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshMode, SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
    SourceBackedRefreshTerminalError,
};
pub use ctx_history_refresh::{open_verified_index, verified_generation_is_query_ready};

pub fn coordinate_source_backed_refresh(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Wait {
        super::finite_worker_owner::with_foreground_guard(|| {
            ctx_daemon_service::coordinate_source_backed_refresh(&AVAILABILITY, data_root, mode)
        })
    } else {
        ctx_daemon_service::coordinate_source_backed_refresh(&AVAILABILITY, data_root, mode)
    }
}

pub fn coordinate_source_backed_refresh_with_progress(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Wait {
        super::finite_worker_owner::with_foreground_guard(|| {
            ctx_daemon_service::coordinate_source_backed_refresh_with_progress(
                &AVAILABILITY,
                data_root,
                mode,
                report_progress,
            )
        })
    } else {
        ctx_daemon_service::coordinate_source_backed_refresh_with_progress(
            &AVAILABILITY,
            data_root,
            mode,
            report_progress,
        )
    }
}

pub fn coordinate_setup_source_backed_refresh_with_progress(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    ctx_daemon_service::coordinate_setup_source_backed_refresh_with_progress(
        &AVAILABILITY,
        data_root,
        mode,
        report_progress,
    )
}

pub fn coordinate_import_source_backed_refresh_with_progress(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    selection: RefreshSelection,
    allow_daemon_autostart: bool,
    report_progress: &mut dyn FnMut(&RefreshStatus) -> Result<()>,
) -> Result<SourceBackedRefreshObservation> {
    super::finite_worker_owner::with_foreground_guard(|| {
        ctx_daemon_service::coordinate_import_source_backed_refresh_with_progress(
            &AVAILABILITY,
            data_root,
            mode,
            selection,
            allow_daemon_autostart,
            report_progress,
        )
    })
}

#[cfg(test)]
mod client_admission_recovery_tests {
    use ctx_daemon_service::testing::{
        recover_wait_refresh_request_for_test, SourceRefreshObservationRecoveryFailed,
    };

    use super::*;

    #[test]
    fn manual_mode_declines_background_availability_without_starting_a_worker() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        std::fs::write(
            data_root.join(crate::config::CONFIG_FILE),
            "[daemon]\nenabled = false\n",
        )
        .unwrap();
        let availability = ctx_daemon_service::DaemonAvailabilityPort::ensure_available(
            &AVAILABILITY,
            &data_root,
            ctx_daemon_service::DaemonTrigger::Search,
            ctx_daemon_service::DaemonAvailabilityDemand::Background,
        )
        .unwrap();

        assert_eq!(
            availability,
            ctx_daemon_service::DaemonAvailability::Disabled
        );
    }

    #[test]
    fn malformed_config_post_ack_recovery_preserves_stable_request_identity() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        std::fs::write(data_root.join(crate::config::CONFIG_FILE), "[daemon\n").unwrap();
        let request_id = "019fcaaa-0000-7000-8000-0000000002b2";

        let error = recover_wait_refresh_request_for_test(&AVAILABILITY, &data_root, request_id)
            .unwrap_err();

        let retained = error
            .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
            .expect("configuration failure remains request-bound after acknowledgement");
        assert_eq!(retained.request_id, request_id);
        assert_eq!(
            retained.disconnect_policy,
            "request_outcome_unknown_after_acknowledgement"
        );
        assert!(format!("{error:#}").contains("load daemon configuration"));
    }
}
