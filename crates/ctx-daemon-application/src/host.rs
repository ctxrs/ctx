use std::{env, fmt, path::Path, time::Duration};

use ctx_client_observability::analytics::{
    DaemonOperationV1, OperationCompletedV1, Outcome, PublicEventV1,
};

use crate::DaemonApplicationHost;

pub const DAEMON_BACKGROUND_CHILD_ENV: &str = "CTX_DAEMON_BACKGROUND_CHILD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHostStartMode {
    Manual,
    Auto,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DaemonHostRunProfile {
    #[default]
    Persistent,
    FiniteCoreWorker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonHostRunRequest {
    pub loop_interval_seconds: Option<u64>,
    pub max_chunks: Option<usize>,
    pub handle_process_signals: bool,
    pub force: bool,
    pub profile: DaemonHostRunProfile,
    pub start_mode: Option<DaemonHostStartMode>,
    pub trigger: Option<crate::DaemonTrigger>,
}

#[derive(Debug)]
pub enum DaemonHostRunError {
    InternalAutostartMetadata,
    Service(anyhow::Error),
}

impl fmt::Display for DaemonHostRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InternalAutostartMetadata => {
                formatter.write_str("daemon autostart metadata requires a background child")
            }
            Self::Service(error) => fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for DaemonHostRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InternalAutostartMetadata => None,
            Self::Service(error) => Some(error.as_ref()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonObservedOperation {
    Status,
    Enable,
    Disable,
}

pub(super) fn run_daemon_host(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    request: DaemonHostRunRequest,
) -> Result<(), DaemonHostRunError> {
    run_daemon_host_with(
        data_root,
        request,
        || environment_flag(DAEMON_BACKGROUND_CHILD_ENV),
        |data_root, request| host.run_daemon_service(data_root, request),
    )
}

fn run_daemon_host_with(
    data_root: &Path,
    request: DaemonHostRunRequest,
    mut background_child: impl FnMut() -> bool,
    mut run_service: impl FnMut(&Path, DaemonHostRunRequest) -> anyhow::Result<()>,
) -> Result<(), DaemonHostRunError> {
    if (request.start_mode.is_some()
        || request.trigger.is_some()
        || request.profile == DaemonHostRunProfile::FiniteCoreWorker)
        && !background_child()
    {
        return Err(DaemonHostRunError::InternalAutostartMetadata);
    }
    run_service(data_root, request).map_err(DaemonHostRunError::Service)
}

pub(super) fn observe_daemon_operation(
    host: &dyn DaemonApplicationHost,
    data_root: &Path,
    operation: DaemonObservedOperation,
    succeeded: bool,
    elapsed: Duration,
) {
    let event = daemon_operation_event(operation, succeeded, elapsed);
    host.deliver_daemon_events(data_root, std::slice::from_ref(&event));
}

fn daemon_operation_event(
    operation: DaemonObservedOperation,
    succeeded: bool,
    elapsed: Duration,
) -> PublicEventV1 {
    let operation = match operation {
        DaemonObservedOperation::Status => DaemonOperationV1::Status,
        DaemonObservedOperation::Enable => DaemonOperationV1::Enable,
        DaemonObservedOperation::Disable => DaemonOperationV1::Disable,
    };
    PublicEventV1::OperationCompleted(OperationCompletedV1::for_daemon(
        operation,
        if succeeded {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        elapsed,
    ))
}

fn environment_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use ctx_client_observability::{
        analytics::{DurationBucket, Outcome},
        operation_descriptor::OperationDescriptor,
    };

    use super::*;
    use crate::{test_environment_lock, DaemonApplication, TestHost};

    fn manual_request() -> DaemonHostRunRequest {
        DaemonHostRunRequest {
            loop_interval_seconds: Some(2),
            max_chunks: Some(3),
            handle_process_signals: false,
            force: true,
            profile: DaemonHostRunProfile::Persistent,
            start_mode: None,
            trigger: None,
        }
    }

    #[test]
    fn manual_run_skips_background_lookup_and_invokes_the_service_once() {
        let environment_reads = Cell::new(0);
        let service_calls = Cell::new(0);
        let request = manual_request();

        run_daemon_host_with(
            Path::new("data"),
            request,
            || {
                environment_reads.set(environment_reads.get() + 1);
                false
            },
            |data_root, observed| {
                service_calls.set(service_calls.get() + 1);
                assert_eq!(data_root, Path::new("data"));
                assert_eq!(observed, request);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(environment_reads.get(), 0);
        assert_eq!(service_calls.get(), 1);
    }

    #[test]
    fn rejected_internal_metadata_reads_environment_once_and_never_calls_service() {
        let environment_reads = Cell::new(0);
        let service_calls = Cell::new(0);
        let request = DaemonHostRunRequest {
            start_mode: Some(DaemonHostStartMode::Auto),
            trigger: Some(crate::DaemonTrigger::Setup),
            ..manual_request()
        };

        let error = run_daemon_host_with(
            Path::new("data"),
            request,
            || {
                environment_reads.set(environment_reads.get() + 1);
                false
            },
            |_, _| {
                service_calls.set(service_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            DaemonHostRunError::InternalAutostartMetadata
        ));
        assert_eq!(environment_reads.get(), 1);
        assert_eq!(service_calls.get(), 0);
    }

    #[test]
    fn admitted_internal_metadata_invokes_service_once_with_the_same_borrowed_request() {
        let environment_reads = Cell::new(0);
        let service_calls = Cell::new(0);
        let request = DaemonHostRunRequest {
            start_mode: Some(DaemonHostStartMode::Auto),
            trigger: Some(crate::DaemonTrigger::Search),
            ..manual_request()
        };

        run_daemon_host_with(
            Path::new("data"),
            request,
            || {
                environment_reads.set(environment_reads.get() + 1);
                true
            },
            |_, observed| {
                service_calls.set(service_calls.get() + 1);
                assert_eq!(observed, request);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(environment_reads.get(), 1);
        assert_eq!(service_calls.get(), 1);
    }

    #[test]
    fn service_failure_after_admission_is_preserved_without_retry() {
        let service_calls = Cell::new(0);

        let error = run_daemon_host_with(
            Path::new("data"),
            manual_request(),
            || panic!("manual run must not inspect background admission"),
            |_, _| {
                service_calls.set(service_calls.get() + 1);
                Err(anyhow::anyhow!("service failed"))
            },
        )
        .unwrap_err();

        let DaemonHostRunError::Service(error) = error else {
            panic!("service failure must keep its error authority");
        };
        assert_eq!(error.to_string(), "service failed");
        assert_eq!(service_calls.get(), 1);
    }

    #[test]
    fn internal_metadata_is_admitted_only_for_background_children() {
        let _guard = test_environment_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = env::var_os(DAEMON_BACKGROUND_CHILD_ENV);
        env::remove_var(DAEMON_BACKGROUND_CHILD_ENV);
        let host = TestHost;
        let application = DaemonApplication::new(&host);
        let request = DaemonHostRunRequest {
            loop_interval_seconds: None,
            max_chunks: None,
            handle_process_signals: false,
            force: false,
            profile: DaemonHostRunProfile::Persistent,
            start_mode: Some(DaemonHostStartMode::Auto),
            trigger: Some(crate::DaemonTrigger::Search),
        };

        assert!(matches!(
            application.run_daemon_host(Path::new("data"), request),
            Err(DaemonHostRunError::InternalAutostartMetadata)
        ));
        env::set_var(DAEMON_BACKGROUND_CHILD_ENV, "true");
        application
            .run_daemon_host(Path::new("data"), request)
            .unwrap();

        match previous {
            Some(value) => env::set_var(DAEMON_BACKGROUND_CHILD_ENV, value),
            None => env::remove_var(DAEMON_BACKGROUND_CHILD_ENV),
        }
    }

    #[test]
    fn background_child_flag_preserves_the_existing_normalization() {
        let _guard = test_environment_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = env::var_os(DAEMON_BACKGROUND_CHILD_ENV);

        for value in ["1", "true", "TRUE", " yes ", "On"] {
            env::set_var(DAEMON_BACKGROUND_CHILD_ENV, value);
            assert!(environment_flag(DAEMON_BACKGROUND_CHILD_ENV), "{value}");
        }
        for value in ["", "0", "false", "enabled", "truth"] {
            env::set_var(DAEMON_BACKGROUND_CHILD_ENV, value);
            assert!(!environment_flag(DAEMON_BACKGROUND_CHILD_ENV), "{value}");
        }
        env::remove_var(DAEMON_BACKGROUND_CHILD_ENV);
        assert!(!environment_flag(DAEMON_BACKGROUND_CHILD_ENV));

        match previous {
            Some(value) => env::set_var(DAEMON_BACKGROUND_CHILD_ENV, value),
            None => env::remove_var(DAEMON_BACKGROUND_CHILD_ENV),
        }
    }

    #[test]
    fn observed_operations_keep_the_daemon_schema_and_outcome() {
        for (operation, expected) in [
            (DaemonObservedOperation::Status, DaemonOperationV1::Status),
            (DaemonObservedOperation::Enable, DaemonOperationV1::Enable),
            (DaemonObservedOperation::Disable, DaemonOperationV1::Disable),
        ] {
            let PublicEventV1::OperationCompleted(event) =
                daemon_operation_event(operation, true, Duration::from_millis(321))
            else {
                panic!("daemon operation must use the operation-completed event");
            };
            assert!(matches!(
                event.descriptor,
                OperationDescriptor::Daemon(actual) if actual == expected
            ));
            assert_eq!(event.output, None);
            assert_eq!(event.outcome, Outcome::Success);
            assert_eq!(event.duration, DurationBucket::UnderOneSecond);
            assert!(!event.deprecated_daemon_control);
            assert!(!event.deprecated_upgrade_control);
        }

        let PublicEventV1::OperationCompleted(failed) = daemon_operation_event(
            DaemonObservedOperation::Disable,
            false,
            Duration::from_secs(6),
        ) else {
            unreachable!();
        };
        assert_eq!(failed.outcome, Outcome::Failure);
        assert_eq!(failed.duration, DurationBucket::UnderThirtySeconds);
    }
}
