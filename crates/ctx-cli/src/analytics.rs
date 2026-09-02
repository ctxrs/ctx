use std::cell::Cell;

pub(crate) use ctx_client_observability::analytics::*;

/// Final-product analytics policy: persisted consent is subject to the
/// documented process-level opt-out, with malformed overrides failing closed.
pub(crate) fn effective_analytics_enabled(config: &ctx_app_config::AppConfig) -> bool {
    config.analytics.enabled
        && ctx_app_config::normalized_analytics_environment_override() != Some(false)
}

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

pub(crate) fn send_batch(data_root: &std::path::Path, events: &[PublicEventV1]) {
    report_delivery_failure(crate::observability_composition::append_analytics_batch(
        data_root, events,
    ));
}

pub(crate) fn send_daemon_batch(data_root: &std::path::Path, events: &[PublicEventV1]) {
    report_delivery_failure(crate::observability_composition::append_analytics_batch(
        data_root, events,
    ));
    report_delivery_failure(crate::observability_composition::drain_analytics_outbox(
        data_root,
        crate::net::DAEMON_TELEMETRY_HTTP_TIMEOUT,
    ));
}

fn report_delivery_failure(result: anyhow::Result<()>) {
    if let Err(error) = result {
        let quiet = DELIVERY_FAILURE_OUTPUT_QUIET.get();
        if !quiet && std::env::var_os("CTX_ANALYTICS_DEBUG").is_some() {
            eprintln!("ctx analytics delivery failed: {error:#}");
        }
    }
}
