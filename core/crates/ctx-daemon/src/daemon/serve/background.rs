use std::sync::Arc;

use crate::daemon::{
    lifecycle, managed_auto_update, memleak_debug, merge_queue, mobile_startup,
    provider_child_reclassifier, provider_guard, provider_restart, provider_usage,
    resource_telemetry, storage_guard, DaemonState, ProviderStatusHandle, ProviderUsageHandle,
};

pub(super) fn spawn_daemon_background_services(
    state: Arc<DaemonState>,
    requested_binds: Vec<String>,
    provider_status: ProviderStatusHandle,
    provider_usage: ProviderUsageHandle,
) {
    resource_telemetry::spawn_resource_telemetry(state.clone());
    memleak_debug::spawn_memleak_debug(state.clone());
    storage_guard::spawn_storage_guard(state.clone());
    provider_guard::spawn_provider_guard(Arc::clone(&state.provider_lifecycle_background));
    provider_restart::spawn_provider_restart(Arc::clone(&state.provider_lifecycle_background));
    provider_child_reclassifier::spawn_provider_child_reclassifier(state.clone());
    merge_queue::spawn_merge_queue_runner(state.clone());
    provider_usage::spawn_provider_usage_poller(Arc::new(provider_usage));
    managed_auto_update::spawn_managed_daemon_auto_update(state.clone(), requested_binds);
    lifecycle::spawn_process_shutdown_listener(state.clone());

    mobile_startup::spawn_saved_mobile_tunnel_reconnect(state.clone());

    spawn_startup_provider_status_refresh(provider_status);
}

pub(in crate::daemon) fn spawn_startup_provider_status_refresh(handle: ProviderStatusHandle) {
    tokio::spawn(async move {
        if let Err(err) =
            ctx_provider_runtime::provider_status_service::refresh_provider_statuses(&handle).await
        {
            tracing::warn!("startup provider status refresh failed: {err:#}");
        }
    });
}
