use std::sync::Arc;

use ctx_core::ids::WorkspaceId;
use ctx_provider_runtime::ProviderRuntime;
use ctx_resource_utilization::ResourceSampler;
use ctx_store::Store;
use tokio::sync::Mutex;

use super::state::{ProtectedWorkspaceStoreLookup, WorkspaceStoreAccessError};

#[derive(Clone)]
pub struct ResourceUtilizationHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    providers: Arc<ProviderRuntime>,
    resource_sampler: Arc<Mutex<ResourceSampler>>,
}

impl ResourceUtilizationHandle {
    pub(in crate::daemon) fn new(
        workspace_stores: ProtectedWorkspaceStoreLookup,
        providers: Arc<ProviderRuntime>,
        resource_sampler: Arc<Mutex<ResourceSampler>>,
    ) -> Self {
        Self {
            workspace_stores,
            providers,
            resource_sampler,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        self.workspace_stores.global_store()
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) fn providers(&self) -> &ProviderRuntime {
        self.providers.as_ref()
    }

    pub(in crate::daemon) fn resource_sampler(&self) -> &Mutex<ResourceSampler> {
        self.resource_sampler.as_ref()
    }
}
