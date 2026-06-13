use std::sync::Arc;

use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_resource_utilization::resource_governance::ResourceGovernanceRuntime;
use ctx_resource_utilization::ResourceSampler;
use ctx_store::Store;
use ctx_transport_runtime::terminals::TerminalManager;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct SettingsHandle {
    store: Store,
    telemetry: Telemetry,
    perf_telemetry: PerfTelemetry,
    resource_sampler: Arc<Mutex<ResourceSampler>>,
    resource_governance: Arc<Mutex<ResourceGovernanceRuntime>>,
    providers: Arc<ProviderRuntime>,
    terminals: Arc<TerminalManager>,
}

impl SettingsHandle {
    pub(in crate::daemon) fn new(
        store: Store,
        telemetry: Telemetry,
        perf_telemetry: PerfTelemetry,
        resource_sampler: Arc<Mutex<ResourceSampler>>,
        resource_governance: Arc<Mutex<ResourceGovernanceRuntime>>,
        providers: Arc<ProviderRuntime>,
        terminals: Arc<TerminalManager>,
    ) -> Self {
        Self {
            store,
            telemetry,
            perf_telemetry,
            resource_sampler,
            resource_governance,
            providers,
            terminals,
        }
    }

    pub(in crate::daemon) fn store(&self) -> &Store {
        &self.store
    }

    pub(in crate::daemon) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub(in crate::daemon) fn resource_sampler(&self) -> &Mutex<ResourceSampler> {
        self.resource_sampler.as_ref()
    }

    pub(in crate::daemon) fn resource_governance(&self) -> &Mutex<ResourceGovernanceRuntime> {
        self.resource_governance.as_ref()
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn terminals(&self) -> &TerminalManager {
        self.terminals.as_ref()
    }
}
