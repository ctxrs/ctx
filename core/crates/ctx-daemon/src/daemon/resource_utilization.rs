use std::path::Path;
use std::time::Instant;

use ctx_core::ids::WorkspaceId;
use ctx_resource_utilization as resource_utilization;
use ctx_resource_utilization::route_contract::{
    ResourceUtilizationRouteError, ResourceUtilizationRouteQuery, ResourceUtilizationRouteResponse,
};

use crate::daemon::{ResourceUtilizationHandle, WorkspaceStoreAccessError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceUtilizationSnapshotError {
    Disabled,
    WorkspaceNotFound,
    Internal,
}

fn route_error_from_snapshot_error(
    error: ResourceUtilizationSnapshotError,
) -> ResourceUtilizationRouteError {
    match error {
        ResourceUtilizationSnapshotError::Disabled => {
            ResourceUtilizationRouteError::not_found("resource utilization disabled")
        }
        ResourceUtilizationSnapshotError::WorkspaceNotFound => {
            ResourceUtilizationRouteError::not_found("workspace not found")
        }
        ResourceUtilizationSnapshotError::Internal => {
            ResourceUtilizationRouteError::internal("resource utilization unavailable")
        }
    }
}

async fn workspace_resource_utilization_snapshot_with_disabled(
    handle: &ResourceUtilizationHandle,
    workspace_id: WorkspaceId,
    disabled: bool,
) -> Result<resource_utilization::ResourceUtilizationSnapshot, ResourceUtilizationSnapshotError> {
    if disabled {
        return Err(ResourceUtilizationSnapshotError::Disabled);
    }

    let workspace = handle
        .global_store()
        .get_workspace(workspace_id)
        .await
        .map_err(|_| ResourceUtilizationSnapshotError::Internal)?
        .ok_or(ResourceUtilizationSnapshotError::WorkspaceNotFound)?;

    let store = match handle.existing_workspace_store(workspace_id).await {
        Ok(store) => store,
        Err(WorkspaceStoreAccessError::NotFound) => {
            return Err(ResourceUtilizationSnapshotError::WorkspaceNotFound)
        }
        Err(WorkspaceStoreAccessError::Unavailable(_)) => {
            return Err(ResourceUtilizationSnapshotError::Internal)
        }
    };
    let worktrees = store
        .list_worktrees(workspace_id)
        .await
        .map_err(|_| ResourceUtilizationSnapshotError::Internal)?;

    let provider_processes = handle.providers().list_provider_processes().await;

    let (system, disks, cache_age_ms, processes, disk_cache) = {
        let mut sampler = handle.resource_sampler().lock().await;
        let (system, disks, cache_age_ms) = sampler.system_snapshot();
        let processes = sampler.processes_snapshot_light(std::process::id(), &provider_processes);
        let disk_cache = sampler.disk_cache_entry(workspace_id);
        (system, disks, cache_age_ms, processes, disk_cache)
    };

    let disk = resource_utilization::disk_for_path(Path::new(&workspace.root_path), &disks);

    let now = Instant::now();
    let refresh_disk = resource_utilization::should_refresh_disk_cache(now, disk_cache.as_ref());
    let (mut workspace_snapshot, size_cache_age_ms) = if refresh_disk {
        let workspace_clone = workspace.clone();
        let worktrees_clone = worktrees.clone();
        let disk_clone = disk.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            resource_utilization::compute_workspace_disk_snapshot(
                workspace_clone,
                worktrees_clone,
                disk_clone,
                0,
            )
        })
        .await
        .map_err(|_| ResourceUtilizationSnapshotError::Internal)?;
        let mut sampler = handle.resource_sampler().lock().await;
        sampler.update_disk_cache(workspace_id, now, snapshot.clone());
        (snapshot, 0)
    } else {
        let age_ms = resource_utilization::disk_cache_age_ms(now, disk_cache.as_ref());
        let snapshot = disk_cache
            .as_ref()
            .map(|c| c.snapshot.clone())
            .unwrap_or_else(|| {
                resource_utilization::compute_workspace_disk_snapshot(
                    workspace.clone(),
                    worktrees.clone(),
                    disk.clone(),
                    age_ms,
                )
            });
        (snapshot, age_ms)
    };

    workspace_snapshot.disk = disk;
    workspace_snapshot.size_cache_age_ms = size_cache_age_ms;

    Ok(resource_utilization::ResourceUtilizationSnapshot {
        collected_at: chrono::Utc::now().to_rfc3339(),
        cache_age_ms,
        system,
        processes,
        workspace: workspace_snapshot,
    })
}

pub async fn workspace_resource_utilization_snapshot(
    handle: &ResourceUtilizationHandle,
    workspace_id: WorkspaceId,
) -> Result<resource_utilization::ResourceUtilizationSnapshot, ResourceUtilizationSnapshotError> {
    workspace_resource_utilization_snapshot_with_disabled(
        handle,
        workspace_id,
        resource_utilization::resource_utilization_disabled_from_env(),
    )
    .await
}

async fn workspace_resource_utilization_snapshot_for_route_with_disabled(
    handle: &ResourceUtilizationHandle,
    query: ResourceUtilizationRouteQuery,
    disabled: bool,
) -> Result<ResourceUtilizationRouteResponse, ResourceUtilizationRouteError> {
    let workspace_id = query.parse_workspace_id()?;
    workspace_resource_utilization_snapshot_with_disabled(handle, workspace_id, disabled)
        .await
        .map(ResourceUtilizationRouteResponse::new)
        .map_err(route_error_from_snapshot_error)
}

impl ResourceUtilizationHandle {
    pub async fn workspace_resource_utilization_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<resource_utilization::ResourceUtilizationSnapshot, ResourceUtilizationSnapshotError>
    {
        workspace_resource_utilization_snapshot(self, workspace_id).await
    }

    pub async fn workspace_resource_utilization_snapshot_for_route(
        &self,
        query: ResourceUtilizationRouteQuery,
    ) -> Result<ResourceUtilizationRouteResponse, ResourceUtilizationRouteError> {
        workspace_resource_utilization_snapshot_for_route_with_disabled(
            self,
            query,
            resource_utilization::resource_utilization_disabled_from_env(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDaemon;
    use ctx_core::models::VcsKind;
    use ctx_resource_utilization::route_contract::ResourceUtilizationRouteErrorKind;

    fn resource_query(workspace_id: impl ToString) -> ResourceUtilizationRouteQuery {
        serde_json::from_value(serde_json::json!({
            "workspace_id": workspace_id.to_string(),
        }))
        .expect("resource utilization route query")
    }

    #[test]
    fn snapshot_errors_map_to_route_errors() {
        let disabled = route_error_from_snapshot_error(ResourceUtilizationSnapshotError::Disabled);
        assert_eq!(disabled.kind(), ResourceUtilizationRouteErrorKind::NotFound);

        let missing =
            route_error_from_snapshot_error(ResourceUtilizationSnapshotError::WorkspaceNotFound);
        assert_eq!(missing.kind(), ResourceUtilizationRouteErrorKind::NotFound);
        assert_eq!(missing.message(), "workspace not found");

        let internal = route_error_from_snapshot_error(ResourceUtilizationSnapshotError::Internal);
        assert_eq!(internal.kind(), ResourceUtilizationRouteErrorKind::Internal);
    }

    #[tokio::test]
    async fn resource_utilization_route_rejects_invalid_workspace_id_before_snapshot_lookup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");
        let error = daemon
            .resource_utilization_handle_for_test()
            .workspace_resource_utilization_snapshot_for_route(resource_query("not-a-workspace"))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), ResourceUtilizationRouteErrorKind::BadRequest);
        assert_eq!(error.message(), "invalid workspace id");
    }

    #[tokio::test]
    async fn resource_utilization_route_maps_missing_global_workspace_to_not_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");
        let handle = daemon.resource_utilization_handle_for_test();
        let error = workspace_resource_utilization_snapshot_for_route_with_disabled(
            &handle,
            resource_query(WorkspaceId::new().0),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), ResourceUtilizationRouteErrorKind::NotFound);
        assert_eq!(error.message(), "workspace not found");
    }

    #[tokio::test]
    async fn resource_utilization_route_maps_deleting_workspace_store_to_not_found() {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");
        let workspace = daemon
            .seed_workspace_for_test("workspace", temp.path(), VcsKind::Git)
            .await
            .expect("workspace");
        daemon
            .cache_rehydration_begin_workspace_delete_for_test(workspace.id)
            .await;
        let handle = daemon.resource_utilization_handle_for_test();
        let error = workspace_resource_utilization_snapshot_for_route_with_disabled(
            &handle,
            resource_query(workspace.id.0),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), ResourceUtilizationRouteErrorKind::NotFound);
        assert_eq!(error.message(), "workspace not found");
        daemon
            .cache_rehydration_finish_workspace_delete_for_test(workspace.id)
            .await;
    }

    #[tokio::test]
    async fn resource_utilization_route_maps_unavailable_workspace_store_to_internal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let daemon =
            TestDaemon::new_for_test(temp.path().to_path_buf(), "http://127.0.0.1:0".to_string())
                .await
                .expect("test daemon");
        let workspace = daemon
            .seed_workspace_for_test("workspace", temp.path(), VcsKind::Git)
            .await
            .expect("workspace");
        daemon
            .workspace_active_snapshot_make_store_unopenable_for_test(workspace.id)
            .await
            .expect("make workspace store unavailable");
        let handle = daemon.resource_utilization_handle_for_test();
        let error = workspace_resource_utilization_snapshot_for_route_with_disabled(
            &handle,
            resource_query(workspace.id.0),
            false,
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), ResourceUtilizationRouteErrorKind::Internal);
        assert_eq!(error.message(), "resource utilization unavailable");
    }
}
