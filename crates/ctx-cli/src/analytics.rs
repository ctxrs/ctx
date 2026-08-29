use std::cell::Cell;

pub(crate) use ctx_client_observability::analytics::*;

thread_local! {
    static DELIVERY_FAILURE_OUTPUT_QUIET: Cell<bool> = const { Cell::new(false) };
}

struct DeliveryFailureOutputGuard {
    previous: bool,
}

pub(crate) fn quiet_delivery_failure_output(quiet: bool) -> impl Drop {
    let previous = DELIVERY_FAILURE_OUTPUT_QUIET.replace(quiet);
    DeliveryFailureOutputGuard { previous }
}

impl Drop for DeliveryFailureOutputGuard {
    fn drop(&mut self) {
        DELIVERY_FAILURE_OUTPUT_QUIET.set(self.previous);
    }
}

pub(crate) fn send_batch(
    data_root: &std::path::Path,
    config: &crate::config::AppConfig,
    events: &[PublicEventV1],
) {
    send_batch_with_timeout(
        data_root,
        config,
        events,
        crate::net::TELEMETRY_HTTP_TIMEOUT,
    );
}

pub(crate) fn send_daemon_batch(
    data_root: &std::path::Path,
    config: &crate::config::AppConfig,
    events: &[PublicEventV1],
) {
    send_batch_with_timeout(
        data_root,
        config,
        events,
        crate::net::DAEMON_TELEMETRY_HTTP_TIMEOUT,
    );
}

fn send_batch_with_timeout(
    data_root: &std::path::Path,
    config: &crate::config::AppConfig,
    events: &[PublicEventV1],
    timeout: std::time::Duration,
) {
    if let Err(error) = crate::observability_composition::deliver_analytics_batch(
        data_root, config, events, timeout,
    ) {
        let quiet = DELIVERY_FAILURE_OUTPUT_QUIET.get();
        if !quiet && std::env::var_os("CTX_ANALYTICS_DEBUG").is_some() {
            eprintln!("ctx analytics delivery failed: {error:#}");
        }
    }
}
